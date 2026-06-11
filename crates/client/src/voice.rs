use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
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
            self.current_tier
                .store(QualityTier::from_loss(loss).to_u8(), Ordering::Relaxed);
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
                self.max_size,
                new_size,
                self.jitter_ms,
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
    server_ip: std::net::IpAddr,
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
    let (socket, acked) = udp_connect_and_handshake(server_ip, udp_token).await?;
    let socket = Arc::new(socket);

    let stats = Arc::new(NetworkStats::new());
    let bytes_tx = Arc::new(AtomicU64::new(0));
    let bytes_rx = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let registered = Arc::new(AtomicBool::new(acked));
    if !acked {
        // Exits on its own via the `registered`/`stop` flags.
        drop(spawn_rehello_task(
            socket.clone(),
            udp_token,
            registered.clone(),
            stop.clone(),
        ));
    }
    let speaking: Arc<Mutex<HashMap<String, SpeakerStat>>> = Arc::new(Mutex::new(HashMap::new()));
    let playback_cap = Arc::new(AtomicU32::new(config::MAX_PLAYBACK_BUF as u32));
    let input_peak = Arc::new(AtomicU32::new(0));

    let cipher = XChaCha20Poly1305::new_from_slice(&voice_key)
        .map_err(|_| anyhow::anyhow!("invalid voice key"))?;

    let (_capture_stream, capture_cons) = audio::start_capture(input_device.as_deref())?;
    let (_playback_stream, playback_prod) = audio::start_playback(
        output_device.as_deref(),
        playback_cap.clone(),
        output_vol.clone(),
    )?;

    // Sender pipeline: std::thread (capture → encode) → mpsc → tokio task (UDP send).
    // The channel insulates non-runtime threads from tokio's IOCP-bound try_send().
    let (encoded_tx, encoded_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);

    spawn_capture_thread(CaptureCfg {
        channel_id: channel_id.clone(),
        sender_name: sender_name.clone(),
        cipher: cipher.clone(),
        capture_cons,
        encoded_tx,
        muted: muted.clone(),
        stats: stats.clone(),
        vad_threshold: vad_threshold.clone(),
        input_gain: input_gain.clone(),
        input_peak: input_peak.clone(),
        stop: stop.clone(),
    });

    spawn_udp_send_task(
        socket.clone(),
        encoded_rx,
        bytes_tx.clone(),
        udp_token,
        Duration::from_secs(config::UDP_KEEPALIVE_INTERVAL_SECS),
    );

    let recv_handle = spawn_recv_task(RecvCfg {
        socket: socket.clone(),
        cipher,
        stats: stats.clone(),
        bytes_rx: bytes_rx.clone(),
        playback_cap: playback_cap.clone(),
        speaking: speaking.clone(),
        playback_prod,
        registered: registered.clone(),
        udp_token,
        own_name: sender_name,
    });

    Ok(VoiceHandle {
        _capture_stream,
        _playback_stream,
        recv_handle,
        stats,
        stop,
        registered,
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
        input_peak,
    })
}

/// Connect the UDP voice socket to the server.
///
/// `server_ip` is the resolved IP of the live TCP connection — never re-resolved
/// from the server string. The server requires the UDP hello to arrive from the
/// same IP as the TCP session, so both transports must leave through the same
/// address family (a hostname resolving to IPv6 for TCP while UDP went IPv4
/// caused a permanent registration reject → one-way audio).
/// Returns the connected socket and whether the hello was ACK'd. `false` means
/// registration is unconfirmed — the caller must keep re-helloing in the
/// background, or inbound voice will silently never arrive (one-way audio).
async fn udp_connect_and_handshake(
    server_ip: std::net::IpAddr,
    udp_token: u64,
) -> Result<(UdpSocket, bool)> {
    let server_ip = server_ip.to_canonical();
    let udp_addr = std::net::SocketAddr::new(server_ip, config::UDP_PORT);
    let bind_addr: std::net::SocketAddr = if server_ip.is_ipv6() {
        "[::]:0".parse().unwrap()
    } else {
        "0.0.0.0:0".parse().unwrap()
    };

    let socket = UdpSocket::bind(bind_addr).await?;
    socket.connect(&udp_addr).await?;
    tracing::info!("UDP voice connected to {}", udp_addr);

    let hello_pkt = encode_udp_hello(udp_token);
    let mut ack_buf = vec![0u8; 16];
    for attempt in 0..config::UDP_HELLO_RETRIES {
        socket.send(&hello_pkt).await?;
        match tokio::time::timeout(
            Duration::from_millis(config::UDP_HELLO_INTERVAL_MS),
            socket.recv(&mut ack_buf),
        )
        .await
        {
            Ok(Ok(len)) if decode_udp_hello(&ack_buf[..len]) == Some(udp_token) => {
                tracing::debug!("UDP hello ACK received on attempt {}", attempt + 1);
                return Ok((socket, true));
            }
            _ => {
                tracing::trace!("UDP hello attempt {} — no ACK, retrying", attempt + 1);
            }
        }
    }
    tracing::warn!(
        "UDP hello: no ACK after {} attempts — voice rx unconfirmed, re-helloing in background",
        config::UDP_HELLO_RETRIES
    );
    Ok((socket, false))
}

/// Keep sending UDP hellos with backoff until registration is confirmed
/// (ACK or first inbound voice packet flips `registered`) or the pipeline stops.
fn spawn_rehello_task(
    socket: Arc<UdpSocket>,
    udp_token: u64,
    registered: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let hello_pkt = encode_udp_hello(udp_token);
        let mut delay = config::UDP_REHELLO_BASE_MS;
        loop {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if registered.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                break;
            }
            if let Err(e) = socket.send(&hello_pkt).await {
                tracing::debug!("re-hello send failed: {}", e);
            }
            delay = (delay * 2).min(config::UDP_REHELLO_MAX_MS);
        }
        if registered.load(Ordering::Relaxed) {
            tracing::info!("UDP voice registration confirmed");
        }
    })
}

struct CaptureCfg {
    channel_id: ChannelId,
    sender_name: String,
    cipher: XChaCha20Poly1305,
    capture_cons: audio::AudioConsumer,
    encoded_tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    muted: Arc<AtomicBool>,
    stats: Arc<NetworkStats>,
    vad_threshold: Arc<AtomicU32>,
    input_gain: Arc<AtomicU32>,
    input_peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

fn spawn_capture_thread(mut cfg: CaptureCfg) {
    // Truncate sender name to fit in a u8 length prefix
    let name_bytes: Vec<u8> = cfg
        .sender_name
        .as_bytes()
        .iter()
        .take(255)
        .copied()
        .collect();
    std::thread::spawn(move || {
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
        let mut packet_buf = BytesMut::with_capacity(config::MAX_UDP_PACKET);
        let mut pcm_frame = vec![0.0f32; config::FRAME_SIZE];

        loop {
            if cfg.stop.load(Ordering::Relaxed) {
                break;
            }
            if cfg.capture_cons.occupied_len() < config::FRAME_SIZE {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            cfg.capture_cons.pop_slice(&mut pcm_frame);
            if cfg.muted.load(Ordering::Relaxed) {
                // Zero the published level — otherwise the UI meter freezes at
                // the last pre-mute RMS and waves forever.
                cfg.input_peak.store(0, Ordering::Relaxed);
                continue;
            }

            let gain = f32::from_bits(cfg.input_gain.load(Ordering::Relaxed));
            if gain != 1.0 {
                for s in &mut pcm_frame {
                    *s *= gain;
                }
            }

            // Publish RMS for UI level meter.
            let rms =
                (pcm_frame.iter().map(|s| s * s).sum::<f32>() / pcm_frame.len() as f32).sqrt();
            cfg.input_peak.store(rms.to_bits(), Ordering::Relaxed);

            frames_since_check += 1;
            if frames_since_check >= config::STATS_WINDOW_PACKETS {
                frames_since_check = 0;
                maybe_adapt_encoder(&mut encoder, &cfg.stats, &mut current_tier);
            }

            if !pass_vad(&pcm_frame, &cfg.vad_threshold, &mut hangover_remaining) {
                continue;
            }

            let opus_data = match encoder.encode(&pcm_frame) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("encode error: {}", e);
                    continue;
                }
            };

            if !build_packet(
                &mut packet_buf,
                sequence,
                &cfg.channel_id,
                &name_bytes,
                opus_data,
                &cfg.cipher,
            ) {
                continue;
            }

            let frozen = packet_buf.split().freeze();
            if cfg.encoded_tx.blocking_send(frozen).is_err() {
                break;
            }
            sequence = sequence.wrapping_add(1);
        }
    });
}

fn maybe_adapt_encoder(
    encoder: &mut OpusEncoder,
    stats: &NetworkStats,
    current_tier: &mut QualityTier,
) {
    let new_tier = stats.current_tier();
    if new_tier == *current_tier {
        return;
    }
    let loss = stats.loss_percent();
    *current_tier = new_tier;
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

/// Returns true if the frame should be sent.
fn pass_vad(pcm: &[f32], vad_threshold: &Arc<AtomicU32>, hangover_remaining: &mut u32) -> bool {
    let threshold = f32::from_bits(vad_threshold.load(Ordering::Relaxed));
    if threshold <= 0.0 {
        return true;
    }
    let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
    if rms >= threshold {
        *hangover_remaining = config::VAD_HANGOVER_FRAMES;
        true
    } else if *hangover_remaining > 0 {
        *hangover_remaining -= 1;
        true
    } else {
        false
    }
}

/// Build [seq:4][id_len:1][channel_id:N][nonce:24][ciphertext][tag:16] in `packet_buf`.
/// Returns false on encryption failure.
fn build_packet(
    packet_buf: &mut BytesMut,
    sequence: u32,
    channel_id: &str,
    name_bytes: &[u8],
    opus_data: &[u8],
    cipher: &XChaCha20Poly1305,
) -> bool {
    packet_buf.clear();
    packet_buf.put_slice(&sequence.to_be_bytes());
    let id_bytes = channel_id.as_bytes();
    packet_buf.put_u8(id_bytes.len() as u8);
    packet_buf.put_slice(id_bytes);

    let nonce_bytes: [u8; config::XCHACHA20_NONCE_SIZE] = rand::random();
    let nonce = XNonce::from(nonce_bytes);
    packet_buf.put_slice(&nonce_bytes);
    let plaintext_start = packet_buf.len();
    packet_buf.put_u8(name_bytes.len() as u8);
    packet_buf.put_slice(name_bytes);
    packet_buf.put_slice(opus_data);

    match cipher.encrypt_in_place_detached(&nonce, b"", &mut packet_buf[plaintext_start..]) {
        Ok(tag) => {
            packet_buf.put_slice(tag.as_slice());
            true
        }
        Err(_) => {
            tracing::warn!("encrypt error");
            false
        }
    }
}

fn spawn_udp_send_task(
    socket: Arc<UdpSocket>,
    mut encoded_rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    bytes_tx: Arc<AtomicU64>,
    udp_token: u64,
    idle: Duration,
) {
    tokio::spawn(async move {
        // During VAD silence no voice packets flow, and NAT mappings /
        // stateful-firewall entries for the UDP flow expire — killing the
        // inbound path. The keepalive is a *re-hello with the real token*:
        // it refreshes the NAT mapping AND re-registers our current source
        // address on the relay, so a NAT rebind self-heals within one
        // interval instead of leaving the client permanently deaf (the
        // server relays voice to the address recorded at registration).
        let keepalive = encode_udp_hello(udp_token);
        loop {
            match tokio::time::timeout(idle, encoded_rx.recv()).await {
                Ok(Some(bytes)) => {
                    let bytes_len = bytes.len();
                    match socket.send(&bytes).await {
                        Ok(_) => {
                            bytes_tx.fetch_add(bytes_len as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::trace!("UDP send error: {}", e);
                        }
                    }
                }
                Ok(None) => break, // pipeline stopped
                Err(_) => {
                    if let Err(e) = socket.send(&keepalive).await {
                        tracing::trace!("UDP keepalive send error: {}", e);
                    }
                }
            }
        }
    });
}

struct RecvCfg {
    socket: Arc<UdpSocket>,
    cipher: XChaCha20Poly1305,
    stats: Arc<NetworkStats>,
    bytes_rx: Arc<AtomicU64>,
    playback_cap: Arc<AtomicU32>,
    speaking: Arc<Mutex<HashMap<String, SpeakerStat>>>,
    playback_prod: audio::AudioProducer,
    /// Confirmed-inbound flag shared with the re-hello task and the UI.
    registered: Arc<AtomicBool>,
    udp_token: u64,
    /// Our own display name — inbound packets carrying it are echoes and dropped.
    own_name: String,
}

/// Per-sender receive state. Sequence counters of different senders are
/// independent (each starts at 1), so sharing one jitter buffer across
/// senders would make every interleaved packet look like a stream restart
/// and constantly reset the buffer — concurrent speakers require one
/// buffer + decoder per sender, mixed into playback per frame.
struct SenderStream {
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    last_packet: Instant,
}

fn spawn_recv_task(mut cfg: RecvCfg) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut streams: HashMap<String, SenderStream> = HashMap::new();
        let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];
        let mut decrypt_buf: Vec<u8> = Vec::with_capacity(config::MAX_UDP_PACKET);
        let mut opus_swap: Vec<u8> = Vec::with_capacity(256);
        let mut mix_buf = vec![0.0f32; config::FRAME_SIZE];
        let stale_ttl = Duration::from_secs(config::SENDER_STREAM_TTL_SECS);
        let mut packets_since_prune: u32 = 0;

        loop {
            let len = match cfg.socket.recv(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    // On Windows, connected UDP sockets surface ICMP errors
                    // (WSAECONNRESET / 10054) on the next recv(). Transient — retry.
                    tracing::warn!("UDP recv error: {}", e);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            };
            cfg.bytes_rx.fetch_add(len as u64, Ordering::Relaxed);

            // Late hello ACK (initial handshake timed out, re-hello task running).
            if !cfg.registered.load(Ordering::Relaxed)
                && decode_udp_hello(&buf[..len]) == Some(cfg.udp_token)
            {
                cfg.registered.store(true, Ordering::Relaxed);
                continue;
            }

            let Some((sequence, encrypted)) = VoicePacket::parse_voice_data(&buf[..len]) else {
                continue;
            };
            // Any inbound voice proves the relay knows our address.
            if !cfg.registered.load(Ordering::Relaxed) {
                cfg.registered.store(true, Ordering::Relaxed);
            }
            if !decrypt_packet(&cfg.cipher, encrypted, &mut decrypt_buf) {
                continue;
            }
            let Some((name, opus_offset)) = parse_packet_name(&decrypt_buf) else {
                continue;
            };
            // Echo guard: a relayed copy of our own stream (server-side echo,
            // loopback on a peer) must never reach the speaking map or playback.
            if name == cfg.own_name {
                tracing::trace!("dropping echoed own packet");
                continue;
            }
            record_speaker(&cfg.speaking, name, decrypt_buf.len() - opus_offset);

            packets_since_prune += 1;
            if packets_since_prune >= config::STATS_WINDOW_PACKETS {
                packets_since_prune = 0;
                streams.retain(|_, s| s.last_packet.elapsed() < stale_ttl);
            }

            if !streams.contains_key(name) {
                if streams.len() >= config::MAX_SENDER_STREAMS {
                    streams.retain(|_, s| s.last_packet.elapsed() < stale_ttl);
                }
                if streams.len() >= config::MAX_SENDER_STREAMS {
                    tracing::warn!("sender stream limit reached, dropping packet");
                    continue;
                }
                let decoder = match OpusDecoder::new() {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("failed to create decoder: {}", e);
                        continue;
                    }
                };
                streams.insert(
                    name.to_string(),
                    SenderStream {
                        jitter: JitterBuffer::new(),
                        decoder,
                        last_packet: Instant::now(),
                    },
                );
            }
            let stream = streams.get_mut(name).expect("stream just ensured");
            stream.last_packet = Instant::now();
            stream.jitter.push(sequence, &decrypt_buf[opus_offset..]);

            // Sync playback buffer cap with this sender's jitter estimate
            let jitter = &stream.jitter;
            let jitter_cap = ((jitter.jitter_ms() * 2.0 / jitter.expected_interval_ms).ceil()
                as usize)
                .clamp(config::JITTER_BUF_MIN, config::JITTER_BUF_MAX);
            let cap_samples = (jitter_cap * config::FRAME_SIZE) as u32;
            cfg.playback_cap.store(
                cap_samples.max(config::MAX_PLAYBACK_BUF as u32),
                Ordering::Relaxed,
            );

            drain_streams_to_playback(
                &mut streams,
                &mut opus_swap,
                &mut mix_buf,
                &cfg.stats,
                &mut cfg.playback_prod,
            );
        }
    })
}

/// Decrypt `encrypted` ([nonce:24][ciphertext+tag:16]) into `out` (reused buffer).
fn decrypt_packet(cipher: &XChaCha20Poly1305, encrypted: &[u8], out: &mut Vec<u8>) -> bool {
    if encrypted.len() < config::XCHACHA20_NONCE_SIZE + config::POLY1305_TAG_SIZE {
        return false;
    }
    let nonce = XNonce::from_slice(&encrypted[..config::XCHACHA20_NONCE_SIZE]);
    out.clear();
    out.extend_from_slice(&encrypted[config::XCHACHA20_NONCE_SIZE..]);
    if cipher.decrypt_in_place(nonce, b"", out).is_err() {
        tracing::trace!("decrypt failed, dropping packet");
        return false;
    }
    true
}

/// Parse [name_len:1][name:M][opus_data], return (sender name, opus_offset).
fn parse_packet_name(buf: &[u8]) -> Option<(&str, usize)> {
    if buf.is_empty() {
        return None;
    }
    let name_len = buf[0] as usize;
    if buf.len() < 1 + name_len {
        return None;
    }
    let name = std::str::from_utf8(&buf[1..1 + name_len]).ok()?;
    Some((name, 1 + name_len))
}

/// Record voice activity for the UI meter.
///
/// Level proxy: opus_data length normalised against ~200 bytes (a typical
/// high-bitrate frame). It's not RMS — but it tracks talker loudness well
/// enough for a UI meter and is free to compute (no second decode).
fn record_speaker(
    speaking: &Arc<Mutex<HashMap<String, SpeakerStat>>>,
    name: &str,
    opus_len: usize,
) {
    if name.is_empty() {
        return;
    }
    let level = (opus_len as f32 / 200.0).clamp(0.0, 1.0);
    if let Ok(mut sp) = speaking.lock() {
        // get_mut first: no String allocation on the per-packet path.
        if let Some(entry) = sp.get_mut(name) {
            entry.last_seen = Instant::now();
            // EMA smoothing so the bar doesn't flicker per-frame.
            entry.level = entry.level * 0.5 + level * 0.5;
        } else {
            sp.insert(
                name.to_string(),
                SpeakerStat {
                    last_seen: Instant::now(),
                    level,
                },
            );
        }
    }
}

/// Pop one frame per sender per round, mix into `mix_buf`, push to playback.
/// Rounds repeat until every sender's jitter buffer is empty. Single-talker
/// rounds skip the clamp pass and behave exactly like the pre-mixing path.
fn drain_streams_to_playback(
    streams: &mut HashMap<String, SenderStream>,
    opus_swap: &mut Vec<u8>,
    mix_buf: &mut [f32],
    stats: &NetworkStats,
    playback_prod: &mut audio::AudioProducer,
) {
    use ringbuf::traits::Producer;
    loop {
        let mut contributors = 0usize;
        let mut mixed_len = 0usize;
        for stream in streams.values_mut() {
            let pcm = match stream.jitter.pop(opus_swap) {
                PopResult::Packet => {
                    stats.record_received();
                    stats.maybe_finalize_window();
                    match stream.decoder.decode(opus_swap) {
                        Ok(pcm) => pcm,
                        Err(e) => {
                            tracing::trace!("decode error: {}", e);
                            continue;
                        }
                    }
                }
                PopResult::Missing => {
                    stats.record_lost();
                    stats.maybe_finalize_window();
                    match stream.decoder.decode_plc() {
                        Ok(pcm) => pcm,
                        Err(e) => {
                            tracing::trace!("PLC error: {}", e);
                            continue;
                        }
                    }
                }
                PopResult::Empty => continue,
            };
            let n = pcm.len().min(mix_buf.len());
            if contributors == 0 {
                mix_buf[..n].copy_from_slice(&pcm[..n]);
            } else {
                if n > mixed_len {
                    mix_buf[mixed_len..n].fill(0.0);
                }
                for (m, s) in mix_buf[..n].iter_mut().zip(pcm) {
                    *m += *s;
                }
            }
            mixed_len = mixed_len.max(n);
            contributors += 1;
        }
        if contributors == 0 {
            break;
        }
        if contributors > 1 {
            for m in &mut mix_buf[..mixed_len] {
                *m = m.clamp(-1.0, 1.0);
            }
        }
        playback_prod.push_slice(&mix_buf[..mixed_len]);
    }
}

struct TrafficSnapshot {
    instant: Instant,
    bytes_tx: u64,
    bytes_rx: u64,
    tx_rate: f64,
    rx_rate: f64,
}

/// Per-sender voice activity sample.
#[derive(Debug, Clone, Copy)]
pub struct SpeakerStat {
    pub last_seen: Instant,
    /// Loudness proxy in 0.0–1.0, derived from opus packet size.
    pub level: f32,
}

/// Keeps the voice pipeline alive. Drop to stop all tasks cleanly.
pub struct VoiceHandle {
    _capture_stream: cpal::Stream,
    _playback_stream: cpal::Stream,
    recv_handle: tokio::task::JoinHandle<()>,
    stats: Arc<NetworkStats>,
    stop: Arc<AtomicBool>,
    registered: Arc<AtomicBool>,
    bytes_tx: Arc<AtomicU64>,
    bytes_rx: Arc<AtomicU64>,
    speaking: Arc<Mutex<HashMap<String, SpeakerStat>>>,
    traffic: Mutex<TrafficSnapshot>,
    /// RMS of the most recent capture frame (f32 bits in atomic), updated 50×/sec.
    input_peak: Arc<AtomicU32>,
}

impl VoiceHandle {
    /// Returns `(loss_percent, tier_name)` for display in the TUI.
    pub fn quality_info(&self) -> (u8, &'static str) {
        (
            self.stats.loss_percent(),
            self.stats.current_tier().as_str(),
        )
    }

    /// Whether the server confirmed our UDP registration (hello ACK or any
    /// inbound voice). `false` means we can send but will not hear anyone.
    pub fn is_registered(&self) -> bool {
        self.registered.load(Ordering::Relaxed)
    }

    /// Returns names of speakers active within the last 300ms.
    pub fn active_speakers(&self) -> Vec<String> {
        let cutoff = Duration::from_millis(300);
        if let Ok(mut sp) = self.speaking.lock() {
            sp.retain(|_, s| s.last_seen.elapsed() < cutoff);
            sp.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Returns `(name, level)` for each speaker active within the last 300ms.
    /// `level` is a 0.0–1.0 loudness proxy derived from opus packet size.
    pub fn speaker_levels(&self) -> Vec<(String, f32)> {
        let cutoff = Duration::from_millis(300);
        if let Ok(mut sp) = self.speaking.lock() {
            sp.retain(|_, s| s.last_seen.elapsed() < cutoff);
            sp.iter().map(|(n, s)| (n.clone(), s.level)).collect()
        } else {
            Vec::new()
        }
    }

    /// Returns the most recent input RMS (0.0–1.0).
    pub fn input_peak(&self) -> f32 {
        f32::from_bits(self.input_peak.load(Ordering::Relaxed))
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
            (
                snap.tx_rate / 1024.0,
                snap.rx_rate / 1024.0,
                cur_tx + cur_rx,
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── per-sender streams & mixing ──────────────────────────────────

    fn encoded_frame(encoder: &mut OpusEncoder, value: f32) -> Vec<u8> {
        let pcm = vec![value; config::FRAME_SIZE];
        encoder.encode(&pcm).unwrap().to_vec()
    }

    fn sender_stream() -> SenderStream {
        SenderStream {
            jitter: JitterBuffer::new(),
            decoder: OpusDecoder::new().unwrap(),
            last_packet: Instant::now(),
        }
    }

    /// Two senders with independent sequence counters (each starts near its own
    /// base) must both decode and mix — the old shared jitter buffer treated the
    /// interleaving as endless stream restarts and produced almost nothing.
    #[test]
    fn drain_mixes_two_concurrent_sender_streams() {
        use ringbuf::traits::{Consumer, Observer, Split};

        let mut enc_a = OpusEncoder::new().unwrap();
        let mut enc_b = OpusEncoder::new().unwrap();

        let mut streams: HashMap<String, SenderStream> = HashMap::new();
        streams.insert("alice".into(), sender_stream());
        streams.insert("bob".into(), sender_stream());

        // Interleaved arrival, disjoint sequence spaces (bob "joined earlier").
        const FRAMES: u32 = 5;
        for i in 0..FRAMES {
            let pkt_a = encoded_frame(&mut enc_a, 0.0);
            let pkt_b = encoded_frame(&mut enc_b, 0.0);
            streams.get_mut("alice").unwrap().jitter.push(1 + i, &pkt_a);
            streams
                .get_mut("bob")
                .unwrap()
                .jitter
                .push(100_000 + i, &pkt_b);
        }

        let rb = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE * 16);
        let (mut prod, mut cons) = rb.split();
        let stats = NetworkStats::new();
        let mut opus_swap = Vec::new();
        let mut mix_buf = vec![0.0f32; config::FRAME_SIZE];

        drain_streams_to_playback(
            &mut streams,
            &mut opus_swap,
            &mut mix_buf,
            &stats,
            &mut prod,
        );

        // Mixed output: one frame per round, not one per packet.
        assert_eq!(cons.occupied_len(), config::FRAME_SIZE * FRAMES as usize);
        // Every packet from both senders was consumed as received, none "lost".
        assert_eq!(stats.received.load(Ordering::Relaxed), FRAMES * 2);
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);
        let mut sink = vec![0.0f32; config::FRAME_SIZE * FRAMES as usize];
        cons.pop_slice(&mut sink);
    }

    #[test]
    fn drain_single_sender_passthrough() {
        use ringbuf::traits::{Observer, Split};

        let mut enc = OpusEncoder::new().unwrap();
        let mut streams: HashMap<String, SenderStream> = HashMap::new();
        streams.insert("alice".into(), sender_stream());
        for i in 0..3u32 {
            let pkt = encoded_frame(&mut enc, 0.0);
            streams.get_mut("alice").unwrap().jitter.push(1 + i, &pkt);
        }

        let rb = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE * 8);
        let (mut prod, cons) = rb.split();
        let stats = NetworkStats::new();
        let mut opus_swap = Vec::new();
        let mut mix_buf = vec![0.0f32; config::FRAME_SIZE];

        drain_streams_to_playback(
            &mut streams,
            &mut opus_swap,
            &mut mix_buf,
            &stats,
            &mut prod,
        );

        assert_eq!(cons.occupied_len(), config::FRAME_SIZE * 3);
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn parse_packet_name_extracts_name_and_offset() {
        let mut buf = vec![5u8];
        buf.extend_from_slice(b"alice");
        buf.extend_from_slice(&[1, 2, 3]);
        let (name, offset) = parse_packet_name(&buf).unwrap();
        assert_eq!(name, "alice");
        assert_eq!(offset, 6);
        assert_eq!(&buf[offset..], &[1, 2, 3]);

        assert!(parse_packet_name(&[]).is_none());
        assert!(parse_packet_name(&[10, b'x']).is_none()); // name_len beyond buf
    }

    /// The idle keepalive must be a re-hello carrying the real token, so the
    /// relay refreshes the client's registered address (NAT-rebind self-heal),
    /// not the inert token-0 keepalive.
    #[tokio::test]
    async fn send_task_keepalive_rehellos_with_real_token() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        let (_tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(4);
        spawn_udp_send_task(
            Arc::new(client),
            rx,
            Arc::new(AtomicU64::new(0)),
            777,
            Duration::from_millis(50),
        );

        let mut buf = [0u8; 16];
        let (len, _) = tokio::time::timeout(Duration::from_secs(5), server.recv_from(&mut buf))
            .await
            .expect("no keepalive arrived")
            .unwrap();
        assert_eq!(decode_udp_hello(&buf[..len]), Some(777));
    }

    // ── re-hello ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn rehello_resends_until_registered_then_exits() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        let registered = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_rehello_task(Arc::new(client), 42, registered.clone(), stop.clone());

        // The loop must keep sending hellos while unregistered.
        let mut buf = [0u8; 16];
        let (len, _) = tokio::time::timeout(Duration::from_secs(5), server.recv_from(&mut buf))
            .await
            .expect("no re-hello arrived")
            .unwrap();
        assert_eq!(decode_udp_hello(&buf[..len]), Some(42));

        // Once registration is confirmed the task exits on its own.
        registered.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(15), handle)
            .await
            .expect("re-hello task did not exit after registration")
            .unwrap();
    }

    #[tokio::test]
    async fn rehello_exits_on_stop() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server.local_addr().unwrap()).await.unwrap();

        let registered = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(true));
        let handle = spawn_rehello_task(Arc::new(client), 7, registered, stop);
        tokio::time::timeout(Duration::from_secs(15), handle)
            .await
            .expect("re-hello task did not exit on stop")
            .unwrap();
    }

    // ── seq_before ───────────────────────────────────────────────────

    #[test]
    fn seq_before_basic() {
        assert!(seq_before(1, 2));
        assert!(seq_before(0, 1));
        assert!(!seq_before(2, 1));
        assert!(!seq_before(5, 5)); // equal
    }

    #[test]
    fn seq_before_wrapping() {
        // Near u32::MAX boundary
        assert!(seq_before(u32::MAX - 1, u32::MAX));
        assert!(seq_before(u32::MAX, 0)); // wrap around
        assert!(seq_before(u32::MAX, 1)); // wrap around
        assert!(!seq_before(1, u32::MAX)); // 1 is "after" MAX in wrapping
    }

    // ── QualityTier ──────────────────────────────────────────────────

    #[test]
    fn quality_tier_from_loss_thresholds() {
        assert_eq!(QualityTier::from_loss(0), QualityTier::High);
        assert_eq!(QualityTier::from_loss(4), QualityTier::High);
        assert_eq!(QualityTier::from_loss(5), QualityTier::Medium);
        assert_eq!(QualityTier::from_loss(14), QualityTier::Medium);
        assert_eq!(QualityTier::from_loss(15), QualityTier::Low);
        assert_eq!(QualityTier::from_loss(29), QualityTier::Low);
        assert_eq!(QualityTier::from_loss(30), QualityTier::Minimum);
        assert_eq!(QualityTier::from_loss(100), QualityTier::Minimum);
    }

    #[test]
    fn quality_tier_u8_roundtrip() {
        for tier in [
            QualityTier::High,
            QualityTier::Medium,
            QualityTier::Low,
            QualityTier::Minimum,
        ] {
            assert_eq!(QualityTier::from_u8(tier.to_u8()), tier);
        }
    }

    #[test]
    fn quality_tier_unknown_u8_defaults_high() {
        assert_eq!(QualityTier::from_u8(255), QualityTier::High);
    }

    #[test]
    fn quality_tier_bitrate_decreasing() {
        assert!(QualityTier::High.bitrate() > QualityTier::Medium.bitrate());
        assert!(QualityTier::Medium.bitrate() > QualityTier::Low.bitrate());
        assert!(QualityTier::Low.bitrate() > QualityTier::Minimum.bitrate());
    }

    #[test]
    fn quality_tier_complexity_decreasing() {
        assert!(QualityTier::High.complexity() > QualityTier::Medium.complexity());
        assert!(QualityTier::Medium.complexity() > QualityTier::Low.complexity());
        assert!(QualityTier::Low.complexity() > QualityTier::Minimum.complexity());
    }

    #[test]
    fn quality_tier_fec_only_when_lossy() {
        assert!(!QualityTier::High.fec());
        assert!(QualityTier::Medium.fec());
        assert!(QualityTier::Low.fec());
        assert!(QualityTier::Minimum.fec());
    }

    #[test]
    fn quality_tier_as_str() {
        assert_eq!(QualityTier::High.as_str(), "high");
        assert_eq!(QualityTier::Medium.as_str(), "medium");
        assert_eq!(QualityTier::Low.as_str(), "low");
        assert_eq!(QualityTier::Minimum.as_str(), "minimum");
    }

    // ── NetworkStats ─────────────────────────────────────────────────

    #[test]
    fn network_stats_initial_state() {
        let stats = NetworkStats::new();
        assert_eq!(stats.loss_percent(), 0);
        assert_eq!(stats.current_tier(), QualityTier::High);
    }

    #[test]
    fn network_stats_finalize_no_loss() {
        let stats = NetworkStats::new();
        for _ in 0..config::STATS_WINDOW_PACKETS {
            stats.record_received();
        }
        stats.maybe_finalize_window();
        assert_eq!(stats.loss_percent(), 0);
        assert_eq!(stats.current_tier(), QualityTier::High);
    }

    #[test]
    fn network_stats_finalize_high_loss() {
        let stats = NetworkStats::new();
        // 50% loss: 75 received, 75 lost
        let half = config::STATS_WINDOW_PACKETS / 2;
        for _ in 0..half {
            stats.record_received();
        }
        for _ in 0..half {
            stats.record_lost();
        }
        stats.maybe_finalize_window();
        assert!(stats.loss_percent() >= 40); // ~50%
        assert_eq!(stats.current_tier(), QualityTier::Minimum);
    }

    #[test]
    fn network_stats_window_resets_after_finalize() {
        let stats = NetworkStats::new();
        for _ in 0..config::STATS_WINDOW_PACKETS {
            stats.record_received();
        }
        stats.maybe_finalize_window();

        // Window should be reset — not enough data to finalize again
        stats.record_received();
        stats.maybe_finalize_window(); // should not change since total < window
                                       // Still reports previous window's result
        assert_eq!(stats.loss_percent(), 0);
    }

    // ── JitterBuffer ─────────────────────────────────────────────────

    #[test]
    fn jitter_buffer_push_pop_in_order() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[10, 20]);
        jb.push(2, &[30, 40]);

        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![10, 20]);
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![30, 40]);
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_buffer_missing_packet_detected() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[10]);
        // Skip seq 2
        jb.push(3, &[30]);

        assert!(matches!(jb.pop(&mut out), PopResult::Packet)); // seq 1
        assert_eq!(out, vec![10]);
        assert!(matches!(jb.pop(&mut out), PopResult::Missing)); // seq 2 gap
        assert!(matches!(jb.pop(&mut out), PopResult::Packet)); // seq 3
        assert_eq!(out, vec![30]);
    }

    #[test]
    fn jitter_buffer_late_packet_dropped() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(5, &[50]);
        jb.pop(&mut out); // consume seq 5, next_seq=6

        // Push seq 5 again (late) — should be silently dropped
        jb.push(5, &[99]);
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_buffer_count_buffered() {
        let mut jb = JitterBuffer::new();
        jb.push(1, &[10]);
        jb.push(2, &[20]);
        jb.push(3, &[30]);
        assert_eq!(jb.count_buffered(), 3);
    }

    #[test]
    fn jitter_buffer_reset_to() {
        let mut jb = JitterBuffer::new();
        jb.push(100, &[1]);
        jb.push(101, &[2]);

        jb.reset_to(200);
        assert_eq!(jb.count_buffered(), 0);
        assert_eq!(jb.jitter_ms(), 0.0);

        // Can push and pop from new sequence
        let mut out = Vec::new();
        jb.push(200, &[42]);
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![42]);
    }

    #[test]
    fn jitter_buffer_stream_restart_detection() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        // Simulate an advanced stream
        jb.push(1000, &[1]);
        jb.pop(&mut out); // next_seq = 1001

        // Sender restarts at seq 1 — large backward jump
        jb.push(1, &[99]);

        // Buffer should have auto-reset
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![99]);
    }

    #[test]
    fn jitter_buffer_far_ahead_packet_dropped() {
        let mut jb = JitterBuffer::new();
        jb.push(1, &[10]);
        // Push way ahead (beyond capacity)
        jb.push(1 + config::JITTER_BUF_MAX as u32, &[99]);

        let mut out = Vec::new();
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![10]);
        // The far-ahead packet should have been dropped
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_buffer_adapt_size() {
        let mut jb = JitterBuffer::new();
        // Jitter is 0 initially — adapt should set to minimum
        jb.adapt_size();
        assert!(jb.max_size >= config::JITTER_BUF_MIN);
        assert!(jb.max_size <= config::JITTER_BUF_MAX);
    }

    #[test]
    fn jitter_buffer_skip_to_oldest() {
        let mut jb = JitterBuffer::new();
        jb.push(1, &[10]); // start
        let mut out = Vec::new();
        jb.pop(&mut out); // consume seq 1, next_seq = 2

        // Push 5 and 6, skip 2,3,4
        jb.push(5, &[50]);
        jb.push(6, &[60]);

        jb.skip_to_oldest_available();
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![50]);
    }
}
