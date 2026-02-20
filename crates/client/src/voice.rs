use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use tokio::net::UdpSocket;

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
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

    fn to_u8(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
            Self::Minimum => 3,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Medium,
            2 => Self::Low,
            3 => Self::Minimum,
            _ => Self::High,
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
    received: AtomicU32,
    lost: AtomicU32,
    loss_percent: AtomicU8,
    current_tier: AtomicU8,
}

impl NetworkStats {
    fn new() -> Self {
        Self {
            received: AtomicU32::new(0),
            lost: AtomicU32::new(0),
            loss_percent: AtomicU8::new(0),
            current_tier: AtomicU8::new(QualityTier::High.to_u8()),
        }
    }

    fn record_received(&self) {
        self.received.fetch_add(1, Ordering::Relaxed);
    }

    fn record_lost(&self) {
        self.lost.fetch_add(1, Ordering::Relaxed);
    }

    pub fn loss_percent(&self) -> u8 {
        self.loss_percent.load(Ordering::Relaxed)
    }

    pub fn current_tier(&self) -> QualityTier {
        QualityTier::from_u8(self.current_tier.load(Ordering::Relaxed))
    }

    /// Check if the measurement window is complete and recalculate tier.
    fn maybe_finalize_window(&self) {
        let received = self.received.load(Ordering::Relaxed);
        let lost = self.lost.load(Ordering::Relaxed);
        let total = received + lost;
        if total >= config::STATS_WINDOW_PACKETS {
            let loss = ((lost as f32 / total as f32) * 100.0) as u8;
            self.loss_percent.store(loss, Ordering::Relaxed);
            self.current_tier.store(QualityTier::from_loss(loss).to_u8(), Ordering::Relaxed);
            self.received.store(0, Ordering::Relaxed);
            self.lost.store(0, Ordering::Relaxed);
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
/// Pre-allocated slots avoid per-packet heap allocation on the hot path.
struct JitterBuffer {
    /// Fixed-size ring of (seq, data) slots. seq=0 means slot is empty.
    /// Vecs are pre-allocated and reused — no heap alloc after warmup.
    slots: Vec<(u32, Vec<u8>)>,
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
        let slots = (0..capacity)
            .map(|_| (0u32, Vec::with_capacity(256)))
            .collect();
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

    /// Push a packet into the buffer. Copies opus_data into a pre-allocated slot.
    fn push(&mut self, seq: u32, opus_data: &[u8]) {
        if !self.started {
            self.next_seq = seq;
            self.started = true;
        }

        // Drop packets that are too old (already played)
        if seq_before(seq, self.next_seq) {
            // Detect sender restart: a large backward jump means the remote
            // peer likely left and rejoined, resetting their sequence counter.
            // Without this, all new packets would be silently dropped until
            // the sequence catches up to the old next_seq.
            let gap = self.next_seq.wrapping_sub(seq);
            if gap > self.capacity as u32 * 2 {
                tracing::debug!(
                    "jitter buffer: stream restart detected (seq={}, next_seq={}, gap={}), resetting",
                    seq, self.next_seq, gap
                );
                self.reset_to(seq);
            } else {
                return;
            }
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

        // Copy into pre-allocated slot (reuses existing Vec capacity)
        let idx = seq as usize % self.capacity;
        self.slots[idx].0 = seq;
        self.slots[idx].1.clear();
        self.slots[idx].1.extend_from_slice(opus_data);

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

    /// Pop the next packet. Data is swapped into `out` (zero-alloc after warmup).
    fn pop(&mut self, out: &mut Vec<u8>) -> PopResult {
        if !self.started {
            return PopResult::Empty;
        }

        let idx = self.next_seq as usize % self.capacity;
        if self.slots[idx].0 == self.next_seq {
            // Swap data out: slot gets the (empty) out buffer, caller gets the data.
            // Both Vecs keep their capacity — no allocation.
            std::mem::swap(&mut self.slots[idx].1, out);
            self.slots[idx].0 = 0; // mark empty
            self.next_seq = self.next_seq.wrapping_add(1);
            return PopResult::Packet;
        }

        // Slot empty or stale — check if any future packets exist
        let has_future = (1..self.capacity).any(|offset| {
            let future_seq = self.next_seq.wrapping_add(offset as u32);
            let future_idx = future_seq as usize % self.capacity;
            let slot = &self.slots[future_idx];
            slot.0 == future_seq && slot.0 != 0
        });
        if has_future {
            // Clear stale data at current slot
            self.slots[idx].0 = 0;
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
                self.slots[idx].0 == seq && seq != 0
            })
            .count()
    }

    /// Skip next_seq forward to the oldest available packet in the ring.
    fn skip_to_oldest_available(&mut self) {
        for offset in 0..self.capacity {
            let seq = self.next_seq.wrapping_add(offset as u32);
            let idx = seq as usize % self.capacity;
            if self.slots[idx].0 == seq && seq != 0 {
                self.next_seq = seq;
                return;
            }
        }
    }

    /// Reset the buffer to accept a new stream starting at the given sequence.
    /// Called when a large backward sequence jump is detected (peer rejoined).
    fn reset_to(&mut self, seq: u32) {
        self.next_seq = seq;
        for slot in &mut self.slots {
            slot.0 = 0; // mark empty (Vecs keep their capacity)
        }
        self.jitter_ms = 0.0;
        self.last_arrival = None;
        self.packets_since_adapt = 0;
    }

    /// Current jitter estimate in milliseconds.
    fn jitter_ms(&self) -> f64 {
        self.jitter_ms
    }
}

enum PopResult {
    Packet,  // Data available in the swap buffer
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
    input_gain: Arc<AtomicU32>,
    output_vol: Arc<AtomicU32>,
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
    let stats = Arc::new(NetworkStats::new());
    let bytes_tx = Arc::new(AtomicU64::new(0));
    let bytes_rx = Arc::new(AtomicU64::new(0));

    let cipher = XChaCha20Poly1305::new_from_slice(&voice_key)
        .map_err(|_| anyhow::anyhow!("invalid voice key"))?;
    let send_cipher = cipher.clone();

    // Adaptive playback buffer cap (samples), shared with audio output callback
    let playback_cap = Arc::new(AtomicU32::new(config::MAX_PLAYBACK_BUF as u32));

    // Start audio I/O (lock-free ring buffers, zero allocation on audio threads)
    let (_capture_stream, mut capture_cons) = audio::start_capture(input_device.as_deref())?;
    let (_playback_stream, playback_prod) = audio::start_playback(
        output_device.as_deref(),
        playback_cap.clone(),
        output_vol.clone(),
    )?;

    // Sender pipeline: std::thread (capture → encode) → channel → tokio task (UDP send)
    // Using a channel avoids calling tokio try_send() from a non-runtime thread,
    // which fails on Windows IOCP because readiness isn't tracked outside async context.
    // Channel carries Bytes (ref-counted) to avoid cloning the packet buffer each frame.
    let (encoded_tx, mut encoded_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);

    let send_channel = channel_id.clone();
    // Truncate sender name to fit in a u8 length prefix
    let send_name_bytes: Vec<u8> = sender_name.as_bytes().iter().take(255).copied().collect();
    let send_muted = muted.clone();
    let send_stats = stats.clone();
    let send_vad_threshold = vad_threshold.clone();
    let send_input_gain = input_gain.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let send_stop = stop.clone();
    let _send_handle = std::thread::spawn(move || {
        use ringbuf::traits::{Consumer, Observer};

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
        // Reusable BytesMut for building UDP packets: [header | nonce | ciphertext | tag]
        // .freeze() produces a ref-counted Bytes — zero-copy send through channel.
        let mut packet_buf = BytesMut::with_capacity(config::MAX_UDP_PACKET);
        // Pre-allocated frame buffer — reads from ring buffer, no allocation per frame
        let mut pcm_frame = vec![0.0f32; config::FRAME_SIZE];

        loop {
            if send_stop.load(Ordering::Relaxed) {
                break;
            }
            // Poll ring buffer for a full frame (capture callback fires every ~20ms)
            if capture_cons.occupied_len() < config::FRAME_SIZE {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            capture_cons.pop_slice(&mut pcm_frame);
            if send_muted.load(Ordering::Relaxed) {
                continue;
            }

            // Apply microphone gain
            let gain = f32::from_bits(send_input_gain.load(Ordering::Relaxed));
            if gain != 1.0 {
                for s in &mut pcm_frame {
                    *s *= gain;
                }
            }

            // Periodically check if quality tier changed
            frames_since_check += 1;
            if frames_since_check >= config::STATS_WINDOW_PACKETS {
                frames_since_check = 0;
                let new_tier = send_stats.current_tier();
                if new_tier != current_tier {
                    let loss = send_stats.loss_percent();
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
            packet_buf.put_slice(&sequence.to_be_bytes());
            let id_bytes = send_channel.as_bytes();
            packet_buf.put_u8(id_bytes.len() as u8);
            packet_buf.put_slice(id_bytes);

            let nonce_bytes: [u8; config::XCHACHA20_NONCE_SIZE] = rand::random();
            let nonce = XNonce::from(nonce_bytes);
            packet_buf.put_slice(&nonce_bytes);
            let plaintext_start = packet_buf.len();
            // Plaintext: [name_len:1][name:M][opus_data]
            packet_buf.put_u8(send_name_bytes.len() as u8);
            packet_buf.put_slice(&send_name_bytes);
            packet_buf.put_slice(opus_data);

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
            packet_buf.put_slice(tag.as_slice());

            // freeze() → Bytes (ref-counted, zero-copy clone for channel send)
            let frozen = packet_buf.split().freeze();
            if encoded_tx.blocking_send(frozen).is_err() {
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

    // Receiver task (UDP → decode → ring buffer → playback)
    let recv_socket = socket.clone();
    let recv_stats = stats.clone();
    let recv_bytes_rx = bytes_rx.clone();
    let recv_playback_cap = playback_cap.clone();
    let recv_speaking = speaking.clone();
    let mut playback_prod = playback_prod;
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
        // Reusable buffer for in-place decryption (avoids per-packet Vec allocation)
        let mut decrypt_buf: Vec<u8> = Vec::with_capacity(config::MAX_UDP_PACKET);
        // Reusable swap buffer for jitter pop (zero-alloc after warmup)
        let mut opus_swap: Vec<u8> = Vec::with_capacity(256);

        loop {
            match recv_socket.recv(&mut buf).await {
                Ok(len) => {
                    recv_bytes_rx.fetch_add(len as u64, Ordering::Relaxed);
                    // Zero-copy parse: get (sequence, encrypted_payload) without allocating
                    if let Some((sequence, encrypted)) = VoicePacket::parse_voice_data(&buf[..len]) {
                        // Decrypt in-place: [nonce:24][ciphertext+tag:16]
                        if encrypted.len() < config::XCHACHA20_NONCE_SIZE + config::POLY1305_TAG_SIZE {
                            continue;
                        }
                        let nonce = XNonce::from_slice(&encrypted[..config::XCHACHA20_NONCE_SIZE]);
                        // Copy ciphertext+tag into reusable buffer, decrypt in-place
                        decrypt_buf.clear();
                        decrypt_buf.extend_from_slice(&encrypted[config::XCHACHA20_NONCE_SIZE..]);
                        if cipher.decrypt_in_place(nonce, b"", &mut decrypt_buf).is_err() {
                            tracing::trace!("decrypt failed, dropping packet");
                            continue;
                        }

                        // Parse sender name from decrypted plaintext: [name_len:1][name:M][opus_data]
                        if decrypt_buf.is_empty() { continue; }
                        let name_len = decrypt_buf[0] as usize;
                        if decrypt_buf.len() < 1 + name_len { continue; }
                        if name_len > 0 {
                            if let Ok(name) = std::str::from_utf8(&decrypt_buf[1..1 + name_len]) {
                                if let Ok(mut sp) = recv_speaking.lock() {
                                    sp.insert(name.to_string(), Instant::now());
                                }
                            }
                        }
                        jitter.push(sequence, &decrypt_buf[1 + name_len..]);

                        // Sync playback buffer cap with jitter estimate
                        let jitter_cap = ((jitter.jitter_ms() * 2.0 / jitter.expected_interval_ms).ceil() as usize)
                            .clamp(config::JITTER_BUF_MIN, config::JITTER_BUF_MAX);
                        let cap_samples = (jitter_cap * config::FRAME_SIZE) as u32;
                        recv_playback_cap.store(cap_samples.max(config::MAX_PLAYBACK_BUF as u32), Ordering::Relaxed);

                        // Drain available packets — push decoded PCM directly to
                        // ring buffer (zero alloc; volume applied in playback callback)
                        loop {
                            match jitter.pop(&mut opus_swap) {
                                PopResult::Packet => {
                                    recv_stats.record_received();
                                    recv_stats.maybe_finalize_window();
                                    match decoder.decode(&opus_swap) {
                                        Ok(pcm) => {
                                            use ringbuf::traits::Producer;
                                            playback_prod.push_slice(pcm);
                                        }
                                        Err(e) => {
                                            tracing::trace!("decode error: {}", e);
                                        }
                                    }
                                }
                                PopResult::Missing => {
                                    recv_stats.record_lost();
                                    recv_stats.maybe_finalize_window();
                                    match decoder.decode_plc() {
                                        Ok(pcm) => {
                                            use ringbuf::traits::Producer;
                                            playback_prod.push_slice(pcm);
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
    stats: Arc<NetworkStats>,
    stop: Arc<AtomicBool>,
    bytes_tx: Arc<AtomicU64>,
    bytes_rx: Arc<AtomicU64>,
    speaking: Arc<Mutex<HashMap<String, Instant>>>,
    traffic: Mutex<TrafficSnapshot>,
}

impl VoiceHandle {
    /// Returns `(loss_percent, tier_name)` for display in the TUI.
    pub fn quality_info(&self) -> (u8, &'static str) {
        (self.stats.loss_percent(), self.stats.current_tier().as_str())
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
