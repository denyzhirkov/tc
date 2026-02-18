use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::net::UdpSocket;

use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use tc_shared::{config, decode_udp_hello, encode_udp_hello, ChannelId, VoicePacket};

use crate::audio;
use crate::codec::{OpusDecoder, OpusEncoder};

// ── Adaptive Quality ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    High,
    Medium,
    Low,
    Minimum,
}

impl QualityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Minimum => "minimum",
        }
    }

    fn from_loss(loss_percent: u8) -> Self {
        if loss_percent >= config::LOSS_THRESH_MINIMUM {
            Self::Minimum
        } else if loss_percent >= config::LOSS_THRESH_LOW {
            Self::Low
        } else if loss_percent >= config::LOSS_THRESH_MEDIUM {
            Self::Medium
        } else {
            Self::High
        }
    }

    fn bitrate(self) -> i32 {
        match self {
            Self::High => config::BITRATE_HIGH,
            Self::Medium => config::BITRATE_MEDIUM,
            Self::Low => config::BITRATE_LOW,
            Self::Minimum => config::BITRATE_MINIMUM,
        }
    }

    fn complexity(self) -> u8 {
        match self {
            Self::High => config::COMPLEXITY_HIGH,
            Self::Medium => config::COMPLEXITY_MEDIUM,
            Self::Low => config::COMPLEXITY_LOW,
            Self::Minimum => config::COMPLEXITY_MINIMUM,
        }
    }

    fn fec(self) -> bool {
        self != Self::High
    }
}

pub struct NetworkStats {
    received: u32,
    lost: u32,
    pub loss_percent: u8,
    pub current_tier: QualityTier,
}

impl NetworkStats {
    fn new() -> Self {
        Self {
            received: 0,
            lost: 0,
            loss_percent: 0,
            current_tier: QualityTier::High,
        }
    }

    fn record_received(&mut self) {
        self.received += 1;
    }

    fn record_lost(&mut self) {
        self.lost += 1;
    }

    /// Check if the measurement window is complete and recalculate tier.
    fn maybe_finalize_window(&mut self) {
        let total = self.received + self.lost;
        if total >= config::STATS_WINDOW_PACKETS {
            self.loss_percent = ((self.lost as f32 / total as f32) * 100.0) as u8;
            self.current_tier = QualityTier::from_loss(self.loss_percent);
            self.received = 0;
            self.lost = 0;
        }
    }
}

/// Wrapping-aware sequence comparison: returns true if a is "before" b.
fn seq_before(a: u32, b: u32) -> bool {
    // Treat the difference as signed: if b - a is in [1, 2^31), a is before b.
    let diff = b.wrapping_sub(a);
    diff > 0 && diff < 0x8000_0000
}

/// Adaptive jitter buffer backed by a fixed-size ring.
/// Uses RFC 3550-style jitter estimation to pick optimal buffer depth.
/// Ring buffer avoids HashMap allocation overhead and improves cache locality.
struct JitterBuffer {
    /// Fixed-size ring of slots: (sequence, opus_data).
    /// Indexed by `seq % capacity`. Storing seq validates freshness.
    slots: Vec<Option<(u32, Vec<u8>)>>,
    capacity: usize,
    next_seq: u32,
    started: bool,
    max_size: usize,
    /// Smoothed jitter estimate (exponential moving average, in ms).
    jitter_ms: f64,
    /// Last packet arrival time.
    last_arrival: Option<Instant>,
    /// Expected inter-packet interval (ms).
    expected_interval_ms: f64,
    /// Counter for periodic adaptation.
    packets_since_adapt: u32,
}

impl JitterBuffer {
    fn new() -> Self {
        let capacity = config::JITTER_BUF_MAX;
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        Self {
            slots,
            capacity,
            next_seq: 0,
            started: false,
            max_size: config::JITTER_BUF_INITIAL,
            jitter_ms: 0.0,
            last_arrival: None,
            expected_interval_ms: (config::FRAME_SIZE as f64 / config::SAMPLE_RATE as f64) * 1000.0, // 20ms
            packets_since_adapt: 0,
        }
    }

    fn push(&mut self, seq: u32, opus_data: Vec<u8>) {
        if !self.started {
            self.next_seq = seq;
            self.started = true;
        }

        // Drop packets that are too old (already played)
        if seq_before(seq, self.next_seq) {
            return;
        }

        // Drop packets too far ahead (would alias in the ring)
        let ahead = seq.wrapping_sub(self.next_seq) as usize;
        if ahead >= self.capacity {
            return;
        }

        // Track inter-arrival jitter (RFC 3550 style)
        let now = Instant::now();
        if let Some(prev) = self.last_arrival {
            let actual_interval_ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            let deviation = (actual_interval_ms - self.expected_interval_ms).abs();
            // Exponential moving average: alpha = 1/16 (same as RFC 3550)
            self.jitter_ms += (deviation - self.jitter_ms) / 16.0;
        }
        self.last_arrival = Some(now);

        let idx = seq as usize % self.capacity;
        self.slots[idx] = Some((seq, opus_data));

        // Periodically adapt buffer size
        self.packets_since_adapt += 1;
        if self.packets_since_adapt >= config::JITTER_ADAPT_INTERVAL {
            self.packets_since_adapt = 0;
            self.adapt_size();
        }

        // If too many packets buffered, skip ahead to drain faster
        if self.count_buffered() > self.max_size {
            self.skip_to_oldest_available();
        }
    }

    /// Adapt max_size based on observed jitter.
    /// target = jitter_ms * 2 / frame_duration_ms, clamped to [MIN, MAX].
    fn adapt_size(&mut self) {
        let target_ms = self.jitter_ms * 2.0;
        let target_packets = (target_ms / self.expected_interval_ms).ceil() as usize;
        let new_size = target_packets.clamp(config::JITTER_BUF_MIN, config::JITTER_BUF_MAX);
        if new_size != self.max_size {
            tracing::debug!(
                "jitter buffer adapted: {} → {} packets (jitter={:.1}ms)",
                self.max_size, new_size, self.jitter_ms,
            );
            self.max_size = new_size;
        }
    }

    /// Returns the next packet if available, or None to signal a gap.
    fn pop(&mut self) -> PopResult {
        if !self.started {
            return PopResult::Empty;
        }

        let idx = self.next_seq as usize % self.capacity;
        if let Some((seq, _)) = &self.slots[idx] {
            if *seq == self.next_seq {
                let (_, data) = self.slots[idx].take().unwrap();
                self.next_seq = self.next_seq.wrapping_add(1);
                return PopResult::Packet(data);
            }
        }

        // Slot empty or stale — check if any future packets exist
        let has_future = (1..self.capacity).any(|offset| {
            let future_seq = self.next_seq.wrapping_add(offset as u32);
            let future_idx = future_seq as usize % self.capacity;
            matches!(&self.slots[future_idx], Some((s, _)) if *s == future_seq)
        });
        if has_future {
            // Clear stale data at current slot
            self.slots[idx] = None;
            self.next_seq = self.next_seq.wrapping_add(1);
            PopResult::Missing
        } else {
            PopResult::Empty
        }
    }

    /// Count valid buffered packets in the current window.
    fn count_buffered(&self) -> usize {
        (0..self.capacity)
            .filter(|&offset| {
                let seq = self.next_seq.wrapping_add(offset as u32);
                let idx = seq as usize % self.capacity;
                matches!(&self.slots[idx], Some((s, _)) if *s == seq)
            })
            .count()
    }

    /// Skip next_seq forward to the oldest available packet in the ring.
    fn skip_to_oldest_available(&mut self) {
        for offset in 0..self.capacity {
            let seq = self.next_seq.wrapping_add(offset as u32);
            let idx = seq as usize % self.capacity;
            if matches!(&self.slots[idx], Some((s, _)) if *s == seq) {
                self.next_seq = seq;
                return;
            }
        }
    }

    /// Current jitter estimate in milliseconds.
    fn jitter_ms(&self) -> f64 {
        self.jitter_ms
    }
}

enum PopResult {
    Packet(Vec<u8>),
    Missing, // Gap detected — use PLC
    Empty,   // Nothing available
}

/// Start the voice pipeline: capture → encode → UDP send, UDP recv → decode → playback.
/// Returns a handle that keeps the pipeline alive.
#[allow(clippy::too_many_arguments)]
pub async fn start_voice(
    server_addr: &str,
    channel_id: ChannelId,
    udp_token: u64,
    muted: Arc<AtomicBool>,
    voice_key: Vec<u8>,
    input_device: Option<String>,
    output_device: Option<String>,
    vad_threshold: Arc<AtomicU32>,
    sender_name: String,
) -> Result<VoiceHandle> {
    let udp_addr = if server_addr.contains(':') {
        // Replace TCP port with UDP port
        let parts: Vec<&str> = server_addr.rsplitn(2, ':').collect();
        format!("{}:{}", parts[1], config::UDP_PORT)
    } else {
        format!("{}:{}", server_addr, config::UDP_PORT)
    };

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(&udp_addr).await?;
    tracing::info!("UDP voice connected to {}", udp_addr);

    // Send hello packets with token, wait for ACK from server
    let hello_pkt = encode_udp_hello(udp_token);
    let mut ack_buf = vec![0u8; 16];
    let mut registered = false;
    for attempt in 0..config::UDP_HELLO_RETRIES {
        socket.send(&hello_pkt).await?;
        match tokio::time::timeout(
            Duration::from_millis(config::UDP_HELLO_INTERVAL_MS),
            socket.recv(&mut ack_buf),
        )
        .await
        {
            Ok(Ok(len)) => {
                // Verify ACK: server echoes back the same hello packet
                if decode_udp_hello(&ack_buf[..len]) == Some(udp_token) {
                    tracing::debug!("UDP hello ACK received on attempt {}", attempt + 1);
                    registered = true;
                    break;
                }
            }
            _ => {
                tracing::trace!("UDP hello attempt {} — no ACK, retrying", attempt + 1);
            }
        }
    }
    if !registered {
        tracing::warn!("UDP hello: no ACK after {} attempts, proceeding anyway", config::UDP_HELLO_RETRIES);
    }

    let socket = Arc::new(socket);
    let stats = Arc::new(Mutex::new(NetworkStats::new()));
    let bytes_tx = Arc::new(AtomicU64::new(0));
    let bytes_rx = Arc::new(AtomicU64::new(0));

    let cipher = XChaCha20Poly1305::new_from_slice(&voice_key)
        .map_err(|_| anyhow::anyhow!("invalid voice key"))?;
    let send_cipher = cipher.clone();

    // Adaptive playback buffer cap (samples), shared with audio output callback
    let playback_cap = Arc::new(AtomicU32::new(config::MAX_PLAYBACK_BUF as u32));

    // Start audio I/O
    let (_capture_stream, capture_rx) = audio::start_capture(input_device.as_deref())?;
    let (_playback_stream, playback_tx) = audio::start_playback(output_device.as_deref(), playback_cap.clone())?;

    // Sender pipeline: std::thread (capture → encode) → channel → tokio task (UDP send)
    // Using a channel avoids calling tokio try_send() from a non-runtime thread,
    // which fails on Windows IOCP because readiness isn't tracked outside async context.
    let (encoded_tx, mut encoded_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let send_channel = channel_id.clone();
    // Truncate sender name to fit in a u8 length prefix
    let send_name_bytes: Vec<u8> = sender_name.as_bytes().iter().take(255).copied().collect();
    let send_muted = muted.clone();
    let send_stats = stats.clone();
    let send_vad_threshold = vad_threshold.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let send_stop = stop.clone();
    let _send_handle = std::thread::spawn(move || {
        let mut encoder = match OpusEncoder::new() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("failed to create encoder: {}", e);
                return;
            }
        };

        let mut sequence: u32 = 1;
        let mut current_tier = QualityTier::High;
        let mut frames_since_check: u32 = 0;
        let mut hangover_remaining: u32 = 0;
        // Reusable buffer for building UDP packets: [header | nonce | ciphertext | tag]
        let mut packet_buf = Vec::with_capacity(config::MAX_UDP_PACKET);

        loop {
            if send_stop.load(Ordering::Relaxed) {
                break;
            }
            let pcm_frame = match capture_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(f) => f,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            };
            if send_muted.load(Ordering::Relaxed) {
                continue;
            }

            // Periodically check if quality tier changed
            frames_since_check += 1;
            if frames_since_check >= config::STATS_WINDOW_PACKETS {
                frames_since_check = 0;
                if let Ok(s) = send_stats.lock() {
                    let new_tier = s.current_tier;
                    if new_tier != current_tier {
                        let loss = s.loss_percent;
                        drop(s);
                        current_tier = new_tier;
                        let loss_hint = if current_tier.fec() { loss } else { 0 };
                        if let Err(e) = encoder.apply_quality_settings(
                            current_tier.bitrate(),
                            current_tier.complexity(),
                            current_tier.fec(),
                            loss_hint,
                        ) {
                            tracing::warn!("failed to apply quality settings: {}", e);
                        } else {
                            tracing::info!(
                                "encoder adapted: tier={}, bitrate={}, loss={}%",
                                current_tier.as_str(),
                                current_tier.bitrate(),
                                loss_hint,
                            );
                        }
                    }
                }
            }

            // Energy-based VAD with hangover: skip silent frames but keep
            // sending for a few frames after the last loud one to avoid clipping.
            let threshold = f32::from_bits(send_vad_threshold.load(Ordering::Relaxed));
            if threshold > 0.0 {
                let rms = (pcm_frame.iter().map(|s| s * s).sum::<f32>()
                    / pcm_frame.len() as f32)
                    .sqrt();
                if rms >= threshold {
                    hangover_remaining = config::VAD_HANGOVER_FRAMES;
                } else if hangover_remaining > 0 {
                    hangover_remaining -= 1;
                } else {
                    continue;
                }
            }

            let opus_data = match encoder.encode(&pcm_frame) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("encode error: {}", e);
                    continue;
                }
            };

            // Build UDP packet directly in reusable buffer:
            // [seq:4][id_len:1][channel_id:N][nonce:24][ciphertext][tag:16]
            packet_buf.clear();
            packet_buf.extend_from_slice(&sequence.to_be_bytes());
            let id_bytes = send_channel.as_bytes();
            packet_buf.push(id_bytes.len() as u8);
            packet_buf.extend_from_slice(id_bytes);

            let nonce_bytes: [u8; config::XCHACHA20_NONCE_SIZE] = rand::random();
            let nonce = XNonce::from(nonce_bytes);
            packet_buf.extend_from_slice(&nonce_bytes);
            let plaintext_start = packet_buf.len();
            // Plaintext: [name_len:1][name:M][opus_data]
            packet_buf.push(send_name_bytes.len() as u8);
            packet_buf.extend_from_slice(&send_name_bytes);
            packet_buf.extend_from_slice(opus_data);

            // Encrypt in-place (avoids intermediate Vec allocations)
            let tag = match send_cipher.encrypt_in_place_detached(
                &nonce,
                b"",
                &mut packet_buf[plaintext_start..],
            ) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!("encrypt error");
                    continue;
                }
            };
            packet_buf.extend_from_slice(tag.as_slice());

            if encoded_tx.blocking_send(packet_buf.clone()).is_err() {
                break; // receiver dropped, pipeline shutting down
            }
            sequence = sequence.wrapping_add(1);
        }
    });

    // Tokio task that drains encoded packets and sends via UDP (IOCP-safe on Windows)
    let send_socket = socket.clone();
    let send_bytes_tx = bytes_tx.clone();
    let _send_task = tokio::spawn(async move {
        while let Some(bytes) = encoded_rx.recv().await {
            let bytes_len = bytes.len();
            match send_socket.send(&bytes).await {
                Ok(_) => {
                    send_bytes_tx.fetch_add(bytes_len as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::trace!("UDP send error: {}", e);
                }
            }
        }
    });

    // Active speakers: name → last heard timestamp
    let speaking: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    // Receiver task (UDP → decode → playback)
    let recv_socket = socket.clone();
    let recv_stats = stats.clone();
    let recv_bytes_rx = bytes_rx.clone();
    let recv_playback_cap = playback_cap.clone();
    let recv_speaking = speaking.clone();
    let recv_handle = tokio::spawn(async move {
        let mut decoder = match OpusDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("failed to create decoder: {}", e);
                return;
            }
        };

        let mut jitter = JitterBuffer::new();
        let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];

        loop {
            match recv_socket.recv(&mut buf).await {
                Ok(len) => {
                    recv_bytes_rx.fetch_add(len as u64, Ordering::Relaxed);
                    if let Some(packet) = VoicePacket::decode(&buf[..len]) {
                        // Decrypt: [nonce][ciphertext+tag]
                        if packet.opus_data.len() < config::XCHACHA20_NONCE_SIZE + config::POLY1305_TAG_SIZE {
                            continue;
                        }
                        let nonce = XNonce::from_slice(&packet.opus_data[..config::XCHACHA20_NONCE_SIZE]);
                        let plaintext = match cipher.decrypt(nonce, &packet.opus_data[config::XCHACHA20_NONCE_SIZE..]) {
                            Ok(pt) => pt,
                            Err(_) => {
                                tracing::trace!("decrypt failed, dropping packet");
                                continue;
                            }
                        };

                        // Parse sender name: [name_len:1][name:M][opus_data]
                        if plaintext.is_empty() { continue; }
                        let name_len = plaintext[0] as usize;
                        if plaintext.len() < 1 + name_len { continue; }
                        if name_len > 0 {
                            if let Ok(name) = std::str::from_utf8(&plaintext[1..1 + name_len]) {
                                if let Ok(mut sp) = recv_speaking.lock() {
                                    sp.insert(name.to_string(), Instant::now());
                                }
                            }
                        }
                        let opus_data = plaintext[1 + name_len..].to_vec();
                        jitter.push(packet.sequence, opus_data);

                        // Sync playback buffer cap with jitter estimate
                        let jitter_cap = ((jitter.jitter_ms() * 2.0 / jitter.expected_interval_ms).ceil() as usize)
                            .clamp(config::JITTER_BUF_MIN, config::JITTER_BUF_MAX);
                        let cap_samples = (jitter_cap * config::FRAME_SIZE) as u32;
                        recv_playback_cap.store(cap_samples.max(config::MAX_PLAYBACK_BUF as u32), Ordering::Relaxed);

                        // Drain available packets
                        loop {
                            match jitter.pop() {
                                PopResult::Packet(opus_data) => {
                                    if let Ok(mut s) = recv_stats.lock() {
                                        s.record_received();
                                        s.maybe_finalize_window();
                                    }
                                    match decoder.decode(&opus_data) {
                                        Ok(pcm) => {
                                            let _ = playback_tx.send(pcm);
                                        }
                                        Err(e) => {
                                            tracing::trace!("decode error: {}", e);
                                        }
                                    }
                                }
                                PopResult::Missing => {
                                    if let Ok(mut s) = recv_stats.lock() {
                                        s.record_lost();
                                        s.maybe_finalize_window();
                                    }
                                    // Use Opus PLC for lost packet
                                    match decoder.decode_plc() {
                                        Ok(pcm) => {
                                            let _ = playback_tx.send(pcm);
                                        }
                                        Err(e) => {
                                            tracing::trace!("PLC error: {}", e);
                                        }
                                    }
                                }
                                PopResult::Empty => break,
                            }
                        }
                    }
                }
                Err(e) => {
                    // On Windows, connected UDP sockets surface ICMP errors
                    // (WSAECONNRESET / 10054) on the next recv(). This is
                    // transient — just retry instead of killing the receiver.
                    tracing::warn!("UDP recv error: {}", e);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            }
        }
    });

    Ok(VoiceHandle {
        _capture_stream,
        _playback_stream,
        recv_handle,
        stats,
        stop,
        bytes_tx,
        bytes_rx,
        speaking,
        traffic: Mutex::new(TrafficSnapshot {
            instant: Instant::now(),
            bytes_tx: 0,
            bytes_rx: 0,
            tx_rate: 0.0,
            rx_rate: 0.0,
        }),
    })
}

struct TrafficSnapshot {
    instant: Instant,
    bytes_tx: u64,
    bytes_rx: u64,
    tx_rate: f64,
    rx_rate: f64,
}

/// Keeps the voice pipeline alive. Drop to stop all tasks cleanly.
pub struct VoiceHandle {
    _capture_stream: cpal::Stream,
    _playback_stream: cpal::Stream,
    recv_handle: tokio::task::JoinHandle<()>,
    stats: Arc<Mutex<NetworkStats>>,
    stop: Arc<AtomicBool>,
    bytes_tx: Arc<AtomicU64>,
    bytes_rx: Arc<AtomicU64>,
    speaking: Arc<Mutex<HashMap<String, Instant>>>,
    traffic: Mutex<TrafficSnapshot>,
}

impl VoiceHandle {
    /// Returns `(loss_percent, tier_name)` for display in the TUI.
    pub fn quality_info(&self) -> (u8, &'static str) {
        if let Ok(s) = self.stats.lock() {
            (s.loss_percent, s.current_tier.as_str())
        } else {
            (0, "high")
        }
    }

    /// Returns names of speakers active within the last 300ms.
    pub fn active_speakers(&self) -> Vec<String> {
        let cutoff = Duration::from_millis(300);
        if let Ok(mut sp) = self.speaking.lock() {
            sp.retain(|_, t| t.elapsed() < cutoff);
            sp.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Returns `(tx_kbps, rx_kbps, total_bytes)` — rates in KB/s and cumulative session total.
    pub fn traffic_info(&self) -> (f64, f64, u64) {
        let cur_tx = self.bytes_tx.load(Ordering::Relaxed);
        let cur_rx = self.bytes_rx.load(Ordering::Relaxed);
        if let Ok(mut snap) = self.traffic.lock() {
            let elapsed = snap.instant.elapsed().as_secs_f64();
            if elapsed >= 0.2 {
                let tx_delta = cur_tx.saturating_sub(snap.bytes_tx) as f64;
                let rx_delta = cur_rx.saturating_sub(snap.bytes_rx) as f64;
                snap.tx_rate = tx_delta / elapsed;
                snap.rx_rate = rx_delta / elapsed;
                snap.bytes_tx = cur_tx;
                snap.bytes_rx = cur_rx;
                snap.instant = Instant::now();
            }
            (snap.tx_rate / 1024.0, snap.rx_rate / 1024.0, cur_tx + cur_rx)
        } else {
            (0.0, 0.0, cur_tx + cur_rx)
        }
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        // Signal sender thread to stop, then abort the receiver task.
        self.stop.store(true, Ordering::Relaxed);
        self.recv_handle.abort();
    }
}
