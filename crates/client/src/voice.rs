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

use crate::audio::{self, PlayoutSignals};
use crate::codec::{Denoiser, OpusEncoder};
use crate::diagnostics::{AudioDiagnostics, DiagnosticsSnapshot};
use crate::limiter::limit_in_place;
use crate::peer_gain::PeerGains;
use crate::playout::PlayoutPump;

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

    /// Apply this tier to the encoder. In-band FEC is on in every tier, with
    /// the loss hint floored at `FEC_MIN_LOSS_PERC` (see config). Returns the
    /// hint that was applied. Single source of truth for both the initial
    /// setup and later tier changes.
    fn apply(self, encoder: &mut OpusEncoder, measured_loss: u8) -> Result<u8> {
        let loss_hint = fec_loss_hint(measured_loss);
        encoder.apply_quality_settings(self.bitrate(), self.complexity(), true, loss_hint)?;
        Ok(loss_hint)
    }
}

/// Packet-loss hint handed to Opus: the measured loss, never below the floor.
fn fec_loss_hint(measured_loss: u8) -> u8 {
    measured_loss.max(config::FEC_MIN_LOSS_PERC)
}

pub struct NetworkStats {
    received: AtomicU32,
    lost: AtomicU32,
    loss_percent: AtomicU8,
    current_tier: AtomicU8,
}

impl NetworkStats {
    pub(crate) fn new() -> Self {
        Self {
            received: AtomicU32::new(0),
            lost: AtomicU32::new(0),
            loss_percent: AtomicU8::new(0),
            current_tier: AtomicU8::new(QualityTier::High.to_u8()),
        }
    }

    pub(crate) fn record_received(&self) {
        self.received.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_lost(&self) {
        self.lost.fetch_add(1, Ordering::Relaxed);
    }

    /// (received, lost) in the current measurement window.
    #[cfg(test)]
    pub(crate) fn window_counts(&self) -> (u32, u32) {
        (
            self.received.load(Ordering::Relaxed),
            self.lost.load(Ordering::Relaxed),
        )
    }

    pub fn loss_percent(&self) -> u8 {
        self.loss_percent.load(Ordering::Relaxed)
    }

    pub fn current_tier(&self) -> QualityTier {
        QualityTier::from_u8(self.current_tier.load(Ordering::Relaxed))
    }

    /// Check if the measurement window is complete and recalculate tier.
    pub(crate) fn maybe_finalize_window(&self) {
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

/// Swappable endpoint of the capture ring buffer: the capture thread reads
/// through this slot so the supervisor can rebuild the input stream (new
/// device) without restarting the thread. `None` = no capture device.
pub type CaptureSlot = Arc<Mutex<Option<audio::AudioConsumer>>>;
/// Swappable endpoint of the playback ring buffer (see [`CaptureSlot`]).
pub type PlaybackSlot = Arc<Mutex<Option<audio::AudioProducer>>>;

/// One direction of audio: the live cpal stream plus its health flag and the
/// device that was actually opened. Absent (`None` in `VoiceHandle`) when no
/// device was available — the supervisor retries until one appears.
struct AudioHalf {
    _stream: cpal::Stream,
    opened_device: String,
}

/// Health of one audio direction, for the supervisor and the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalfState {
    Ok,
    Failed,
    Absent,
}

/// Start the voice pipeline: capture → encode → UDP send, UDP recv → decode → playback.
/// Returns a handle that keeps the pipeline alive.
///
/// Audio is best-effort per direction: a missing/broken input or output device
/// degrades that half (listen-only / speak-only) instead of failing the call.
/// Only network-level setup errors abort.
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
    // Per-peer local playback gain (sender name → factor), applied per-sender
    // before mixing on the receive path. Shared (ArcSwap) so slider changes
    // apply live mid-call.
    peer_gains: PeerGains,
    // Noise suppression (RNNoise): clean the mic before encoding. Read live on
    // the capture thread so the toggle applies mid-call.
    denoise: Arc<AtomicBool>,
    // Automatic capture level (`agc.rs`): keep speech at a target level instead
    // of multiplying whatever the microphone gives. Read live on the capture
    // thread so the toggle applies mid-call.
    agc: Arc<AtomicBool>,
    // Paranoid (traffic-analysis-resistant) mode: send a constant packet rate
    // of flat-size frames even in silence/mute, so the relay can't infer who is
    // speaking when. Read live on the capture thread so the toggle applies
    // mid-call.
    paranoid: Arc<AtomicBool>,
    // Echo (sound-check) mode: keep our own reflected packets instead of
    // dropping them, so we hear ourselves come back through the server.
    echo_test: bool,
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
    let input_peak = Arc::new(AtomicU32::new(0));

    let cipher = XChaCha20Poly1305::new_from_slice(&voice_key)
        .map_err(|_| anyhow::anyhow!("invalid voice key"))?;

    let capture_failed = Arc::new(AtomicBool::new(false));
    let playback_failed = Arc::new(AtomicBool::new(false));
    let capture_slot: CaptureSlot = Arc::new(Mutex::new(None));
    let playback_slot: PlaybackSlot = Arc::new(Mutex::new(None));

    let mut handle = VoiceHandle {
        capture: None,
        playback: None,
        capture_slot,
        playback_slot,
        capture_failed,
        playback_failed,
        playout: Arc::new(PlayoutSignals::default()),
        diag: Arc::new(AudioDiagnostics::default()),
        output_vol,
        recv_handle: None,
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
    };

    // Best-effort halves: log-and-degrade instead of failing the call.
    if let Err(e) = handle.rebuild_capture(input_device.as_deref()) {
        tracing::warn!("starting without capture: {:#}", e);
    }
    if let Err(e) = handle.rebuild_playback(output_device.as_deref()) {
        tracing::warn!("starting without playback: {:#}", e);
    }

    // Sender pipeline: std::thread (capture → encode) → mpsc → tokio task (UDP send).
    // The channel insulates non-runtime threads from tokio's IOCP-bound try_send().
    let (encoded_tx, encoded_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);

    spawn_capture_thread(CaptureCfg {
        channel_id: channel_id.clone(),
        sender_name: sender_name.clone(),
        cipher: cipher.clone(),
        capture_slot: handle.capture_slot.clone(),
        encoded_tx,
        muted: muted.clone(),
        stats: handle.stats.clone(),
        vad_threshold: vad_threshold.clone(),
        input_gain: input_gain.clone(),
        input_peak: handle.input_peak.clone(),
        stop: handle.stop.clone(),
        stream_failed: handle.capture_failed.clone(),
        denoise: denoise.clone(),
        agc: agc.clone(),
        paranoid: paranoid.clone(),
        diag: handle.diag.clone(),
    });

    spawn_udp_send_task(
        socket.clone(),
        encoded_rx,
        handle.bytes_tx.clone(),
        udp_token,
        Duration::from_secs(config::UDP_KEEPALIVE_INTERVAL_SECS),
    );

    handle.recv_handle = Some(spawn_recv_task(RecvCfg {
        socket: socket.clone(),
        cipher,
        stats: handle.stats.clone(),
        bytes_rx: handle.bytes_rx.clone(),
        speaking: handle.speaking.clone(),
        playback_slot: handle.playback_slot.clone(),
        playout: handle.playout.clone(),
        registered: handle.registered.clone(),
        udp_token,
        own_name: sender_name,
        peer_gains,
        echo_test,
        diag: handle.diag.clone(),
    }));

    Ok(handle)
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
    capture_slot: CaptureSlot,
    encoded_tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    muted: Arc<AtomicBool>,
    stats: Arc<NetworkStats>,
    vad_threshold: Arc<AtomicU32>,
    input_gain: Arc<AtomicU32>,
    input_peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    /// Set when the capture device stops delivering frames (starvation watchdog).
    stream_failed: Arc<AtomicBool>,
    /// Noise suppression (RNNoise) on the captured signal. Read live.
    denoise: Arc<AtomicBool>,
    /// Automatic capture level. Read live.
    agc: Arc<AtomicBool>,
    /// Traffic-analysis-resistant mode (constant rate + flat size). Read live.
    paranoid: Arc<AtomicBool>,
    diag: Arc<AudioDiagnostics>,
}

/// Multiply a captured frame by the user's input gain. A boost can push peaks
/// past full scale, which the denoiser would hard-clip, so a boosted frame goes
/// through the soft limiter when `limit` is set. With AGC on and denoise off
/// the caller clears `limit`: the AGC then attenuates the unclipped frame
/// cleanly and soft-clips its own output. Returns whether the limiter avoided
/// a clip.
fn apply_input_gain(frame: &mut [f32], gain: f32, limit: bool) -> bool {
    if gain == 1.0 {
        return false;
    }
    for s in frame.iter_mut() {
        *s *= gain;
    }
    limit && gain > 1.0 && limit_in_place(frame)
}

fn spawn_capture_thread(cfg: CaptureCfg) {
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

        // A fresh encoder runs at Opus defaults; start at the HIGH tier's
        // settings rather than waiting for a tier change to apply them.
        let mut current_tier = QualityTier::High;
        if let Err(e) = current_tier.apply(&mut encoder, cfg.stats.loss_percent()) {
            tracing::warn!("failed to apply initial quality settings: {}", e);
        }

        let mut sequence: u32 = 1;
        let mut frames_since_check: u32 = 0;
        let mut hangover_remaining: u32 = 0;
        let mut packet_buf = BytesMut::with_capacity(config::MAX_UDP_PACKET);
        let mut pcm_frame = vec![0.0f32; config::FRAME_SIZE];
        let starvation = Duration::from_secs(config::AUDIO_STARVATION_SECS);
        let mut last_frame_at = Instant::now();
        let mut starvation_reported = false;
        let mut preroll = VadPreroll::new();

        // Paranoid mode: a reusable all-zero frame sent (encoded) to keep a
        // constant packet rate during silence/mute, and the last-seen flag so
        // we reconfigure the encoder (CBR + DTX off) only when the toggle flips.
        let silence_pcm = vec![0.0f32; config::FRAME_SIZE];
        let mut last_paranoid = false;

        // RNNoise denoiser, allocated once (heavy boxed model); applied per
        // frame only when the toggle is on.
        let mut denoiser = Denoiser::new();
        // Automatic level, allocated once and kept across frames — its whole
        // job is the memory of what the last second sounded like.
        let mut auto_level = crate::agc::Agc::new();

        // Shared encode→encrypt→send for the paranoid constant-rate paths.
        // Returns false when the pipeline channel is closed (stop). Captures
        // only shared refs, so it coexists with the normal path's borrows.
        let send_one = |encoder: &mut OpusEncoder,
                        packet_buf: &mut BytesMut,
                        sequence: &mut u32,
                        frame: &[f32]|
         -> bool {
            let opus = match encoder.encode(frame) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("encode error: {}", e);
                    return true;
                }
            };
            if !build_packet(
                packet_buf,
                *sequence,
                &cfg.channel_id,
                &name_bytes,
                opus,
                &cfg.cipher,
            ) {
                return true;
            }
            let frozen = packet_buf.split().freeze();
            if cfg.encoded_tx.blocking_send(frozen).is_err() {
                return false;
            }
            *sequence = sequence.wrapping_add(1);
            true
        };

        loop {
            if cfg.stop.load(Ordering::Relaxed) {
                break;
            }
            // The consumer lives behind a swappable slot so the supervisor can
            // rebuild the input stream (new device) without restarting this
            // thread. Uncontended lock, taken at most every 2ms.
            {
                let mut slot = match cfg.capture_slot.lock() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let Some(cons) = slot.as_mut() else {
                    // No capture device right now: keep the meter at zero and
                    // wait for the supervisor to install a new consumer.
                    drop(slot);
                    cfg.input_peak.store(0, Ordering::Relaxed);
                    last_frame_at = Instant::now();
                    starvation_reported = false;
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                if cons.occupied_len() < config::FRAME_SIZE {
                    drop(slot);
                    // Starvation watchdog: a healthy device fills a frame every
                    // 20 ms. Seconds of nothing means the device died without
                    // an error callback (e.g. unplugged headset mic) — flag the
                    // half so the supervisor rebuilds it.
                    if !starvation_reported && last_frame_at.elapsed() > starvation {
                        starvation_reported = true;
                        tracing::warn!(
                            "capture starved for {}s — input device lost, flagging pipeline",
                            config::AUDIO_STARVATION_SECS
                        );
                        cfg.stream_failed.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                last_frame_at = Instant::now();
                starvation_reported = false;
                cons.pop_slice(&mut pcm_frame);
            }

            // Reconfigure the encoder only when the paranoid toggle flips.
            let paranoid = cfg.paranoid.load(Ordering::Relaxed);
            if paranoid != last_paranoid {
                if let Err(e) = encoder.set_constant_bitrate(paranoid) {
                    tracing::warn!("paranoid encoder reconfigure failed: {}", e);
                }
                last_paranoid = paranoid;
            }

            if cfg.muted.load(Ordering::Relaxed) {
                // Zero the published level — otherwise the UI meter freezes at
                // the last pre-mute RMS and waves forever.
                cfg.input_peak.store(0, Ordering::Relaxed);
                // Paranoid: keep the cadence constant with comfort silence
                // (encoded zeros) — the real mic never leaves while muted, but
                // the relay can't tell mute apart from speech.
                if paranoid && !send_one(&mut encoder, &mut packet_buf, &mut sequence, &silence_pcm)
                {
                    break;
                }
                continue;
            }

            let gain = f32::from_bits(cfg.input_gain.load(Ordering::Relaxed));
            let limit = cfg.denoise.load(Ordering::Relaxed) || !cfg.agc.load(Ordering::Relaxed);
            if apply_input_gain(&mut pcm_frame, gain, limit) {
                cfg.diag.limiter_capture.fetch_add(1, Ordering::Relaxed);
            }

            // Noise suppression before the meter/VAD/encode, so the level meter
            // and VAD react to the cleaned signal too.
            if cfg.denoise.load(Ordering::Relaxed) {
                denoiser.process(&mut pcm_frame);
            }

            // After denoise, so the level is measured on speech rather than on
            // room noise; before the meter, so "my voice" shows what is
            // actually being sent.
            if cfg.agc.load(Ordering::Relaxed) {
                auto_level.process(&mut pcm_frame);
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

            // Paranoid: no VAD gating — send every frame so the packet cadence
            // is constant and carries no speech-timing signal.
            if paranoid {
                if !send_one(&mut encoder, &mut packet_buf, &mut sequence, &pcm_frame) {
                    break;
                }
                continue;
            }

            if !pass_vad(&pcm_frame, &cfg.vad_threshold, &mut hangover_remaining) {
                // Buffer gated audio: quiet speech onsets sit below the VAD
                // threshold and would be clipped (audible scratchy starts).
                preroll.push(&pcm_frame);
                continue;
            }

            // VAD open: flush the pre-roll first so the onset plays intact.
            let mut send_failed = false;
            preroll.drain(|frame| {
                if send_failed {
                    return;
                }
                let Ok(opus) = encoder.encode(frame) else {
                    return;
                };
                if build_packet(
                    &mut packet_buf,
                    sequence,
                    &cfg.channel_id,
                    &name_bytes,
                    opus,
                    &cfg.cipher,
                ) {
                    let frozen = packet_buf.split().freeze();
                    if cfg.encoded_tx.blocking_send(frozen).is_err() {
                        send_failed = true;
                        return;
                    }
                    sequence = sequence.wrapping_add(1);
                }
            });
            if send_failed {
                break;
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
    if let Err(e) = current_tier.apply(encoder, loss) {
        tracing::warn!("failed to apply quality settings: {}", e);
    } else {
        tracing::info!(
            "encoder adapted: tier={}, bitrate={}, loss={}%",
            current_tier.as_str(),
            current_tier.bitrate(),
            fec_loss_hint(loss),
        );
    }
}

/// Fixed ring of gated PCM frames flushed when VAD opens — speech onsets
/// (quiet first consonants) sit below the threshold and would be clipped,
/// audible as a "scratchy" first fraction of a second. Pre-allocated slots,
/// zero allocation after construction.
struct VadPreroll {
    slots: Vec<Vec<f32>>,
    start: usize,
    len: usize,
}

impl VadPreroll {
    fn new() -> Self {
        Self {
            slots: (0..config::VAD_PREROLL_FRAMES)
                .map(|_| vec![0.0f32; config::FRAME_SIZE])
                .collect(),
            start: 0,
            len: 0,
        }
    }

    fn push(&mut self, frame: &[f32]) {
        let cap = self.slots.len();
        let idx = (self.start + self.len) % cap;
        let n = frame.len().min(config::FRAME_SIZE);
        self.slots[idx][..n].copy_from_slice(&frame[..n]);
        if self.len == cap {
            self.start = (self.start + 1) % cap; // overwrite oldest
        } else {
            self.len += 1;
        }
    }

    /// Visit buffered frames oldest-first, then reset.
    fn drain(&mut self, mut f: impl FnMut(&[f32])) {
        let cap = self.slots.len();
        for i in 0..self.len {
            let idx = (self.start + i) % cap;
            f(&self.slots[idx]);
        }
        self.start = 0;
        self.len = 0;
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
    speaking: Arc<Mutex<HashMap<String, SpeakerStat>>>,
    playback_slot: PlaybackSlot,
    /// Output level and playing count shared with the output callback.
    playout: Arc<PlayoutSignals>,
    /// Confirmed-inbound flag shared with the re-hello task and the UI.
    registered: Arc<AtomicBool>,
    udp_token: u64,
    /// Our own display name — inbound packets carrying it are echoes and dropped.
    own_name: String,
    /// Per-peer local playback gain (sender name → factor), read on the mix path.
    peer_gains: PeerGains,
    /// Sound-check mode: don't drop our own reflected packets (we want to hear
    /// them come back through the server).
    echo_test: bool,
    diag: Arc<AudioDiagnostics>,
}

/// What woke the receive task.
enum Wake {
    Datagram(std::io::Result<usize>),
    PlayoutTick,
    DiagLog,
}

fn spawn_recv_task(cfg: RecvCfg) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pump = PlayoutPump::new(Instant::now());
        let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];
        let mut decrypt_buf: Vec<u8> = Vec::with_capacity(config::MAX_UDP_PACKET);
        // Output is pulled on this tick as well as on every packet, so a
        // sender's buffer is drained on the device's schedule, not the
        // network's. Armed only while something is buffered or playing: an
        // idle channel wakes for packets and the diagnostics timer only.
        let mut tick = tokio::time::interval(Duration::from_millis(config::PLAYOUT_TICK_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Its own slow timer, so the log fires on time in silence too.
        let diag_interval = Duration::from_secs(config::AUDIO_DIAG_LOG_SECS);
        let mut diag_tick =
            tokio::time::interval_at(tokio::time::Instant::now() + diag_interval, diag_interval);
        diag_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut diag_logged_at = Instant::now();
        let mut diag_logged = cfg.diag.snapshot();

        loop {
            // All three futures are cancel-safe (UdpSocket::recv, Interval::tick).
            let wake = tokio::select! {
                r = cfg.socket.recv(&mut buf) => Wake::Datagram(r),
                _ = tick.tick(), if pump.needs_tick() => Wake::PlayoutTick,
                _ = diag_tick.tick() => Wake::DiagLog,
            };
            match wake {
                Wake::Datagram(Ok(len)) => {
                    cfg.bytes_rx.fetch_add(len as u64, Ordering::Relaxed);
                    ingest(&cfg, &mut pump, &buf[..len], &mut decrypt_buf);
                }
                Wake::Datagram(Err(e)) => {
                    // On Windows, connected UDP sockets surface ICMP errors
                    // (WSAECONNRESET / 10054) on the next recv(). Transient —
                    // retry; the pause stays well inside the output low-water.
                    tracing::warn!("UDP recv error: {}", e);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Wake::PlayoutTick => {}
                Wake::DiagLog => {
                    log_glitches(&cfg.diag, &mut diag_logged, &mut diag_logged_at);
                    continue;
                }
            }

            // Lock the playback slot once per wakeup (uncontended; the
            // supervisor swaps it only on device rebuilds).
            let mut slot = match cfg.playback_slot.lock() {
                Ok(s) => s,
                Err(_) => break,
            };
            pump.pump(
                slot.as_mut(),
                Instant::now(),
                &cfg.playout,
                &cfg.peer_gains,
                &cfg.diag,
                &cfg.stats,
            );
        }
    })
}

/// Warn with the glitch counters accumulated since the previous log, if any
/// of them signals a real defect.
fn log_glitches(
    diag: &AudioDiagnostics,
    logged: &mut DiagnosticsSnapshot,
    logged_at: &mut Instant,
) {
    let window_secs = logged_at.elapsed().as_secs();
    *logged_at = Instant::now();
    let now = diag.snapshot();
    let d = now.delta(logged);
    *logged = now;
    if d.has_glitches() {
        tracing::warn!(
            "voice glitches in last {}s: starved={} expand={} plc={} fec={} late={} accel={} dry={} trimmed={} play_overflow={} cap_overflow={} limiter_play={} limiter_cap={}",
            window_secs,
            d.output_starved,
            d.expand_frames,
            d.plc_frames,
            d.fec_recovered,
            d.late_packets,
            d.accelerate_drops,
            d.playback_dry,
            d.playback_trimmed_samples,
            d.playback_ring_overflow,
            d.capture_ring_overflow,
            d.limiter_playback,
            d.limiter_capture
        );
    }
}

/// Handle one datagram: late hello ACK, or decrypt → sender name → echo guard
/// → speaking meter → playout buffer.
fn ingest(cfg: &RecvCfg, pump: &mut PlayoutPump, datagram: &[u8], decrypt_buf: &mut Vec<u8>) {
    // Late hello ACK (initial handshake timed out, re-hello task running).
    if !cfg.registered.load(Ordering::Relaxed) && decode_udp_hello(datagram) == Some(cfg.udp_token)
    {
        cfg.registered.store(true, Ordering::Relaxed);
        return;
    }

    let Some((sequence, encrypted)) = VoicePacket::parse_voice_data(datagram) else {
        return;
    };
    // Any inbound voice proves the relay knows our address.
    if !cfg.registered.load(Ordering::Relaxed) {
        cfg.registered.store(true, Ordering::Relaxed);
    }
    if !decrypt_packet(&cfg.cipher, encrypted, decrypt_buf) {
        return;
    }
    let Some((name, opus_offset)) = parse_packet_name(decrypt_buf) else {
        return;
    };
    // Echo guard: a relayed copy of our own stream (server-side echo,
    // loopback on a peer) must never reach the speaking map or playback
    // — except during a sound-check, where hearing ourselves back is
    // the whole point.
    if !cfg.echo_test && name == cfg.own_name {
        tracing::trace!("dropping echoed own packet");
        return;
    }
    record_speaker(&cfg.speaking, name, decrypt_buf.len() - opus_offset);
    if pump.on_packet(name, sequence, &decrypt_buf[opus_offset..], Instant::now()) {
        cfg.diag.late_packets.fetch_add(1, Ordering::Relaxed);
    }
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
    capture: Option<AudioHalf>,
    playback: Option<AudioHalf>,
    capture_slot: CaptureSlot,
    playback_slot: PlaybackSlot,
    capture_failed: Arc<AtomicBool>,
    playback_failed: Arc<AtomicBool>,
    playout: Arc<PlayoutSignals>,
    diag: Arc<AudioDiagnostics>,
    output_vol: Arc<AtomicU32>,
    recv_handle: Option<tokio::task::JoinHandle<()>>,
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

    /// Current audio glitch counters (monotonic since the call started).
    pub fn audio_diagnostics(&self) -> DiagnosticsSnapshot {
        self.diag.snapshot()
    }

    /// Whether the server confirmed our UDP registration (hello ACK or any
    /// inbound voice). `false` means we can send but will not hear anyone.
    pub fn is_registered(&self) -> bool {
        self.registered.load(Ordering::Relaxed)
    }

    /// Health of the capture (microphone) half.
    pub fn capture_state(&self) -> HalfState {
        if self.capture.is_none() {
            HalfState::Absent
        } else if self.capture_failed.load(Ordering::Relaxed) {
            HalfState::Failed
        } else {
            HalfState::Ok
        }
    }

    /// Health of the playback (output) half.
    pub fn playback_state(&self) -> HalfState {
        if self.playback.is_none() {
            HalfState::Absent
        } else if self.playback_failed.load(Ordering::Relaxed) {
            HalfState::Failed
        } else {
            HalfState::Ok
        }
    }

    /// Device the capture half actually opened (None when absent).
    pub fn opened_input(&self) -> Option<&str> {
        self.capture.as_ref().map(|h| h.opened_device.as_str())
    }

    /// Device the playback half actually opened (None when absent).
    pub fn opened_output(&self) -> Option<&str> {
        self.playback.as_ref().map(|h| h.opened_device.as_str())
    }

    /// (Re)build the capture half on `device` (None = system default).
    /// The encoder thread keeps running; it just starts reading from the new
    /// ring buffer through the swappable slot. On failure the half is absent.
    pub fn rebuild_capture(&mut self, device: Option<&str>) -> Result<()> {
        // Drop the old stream first: on some hosts two open handles to the
        // same device conflict, and a dead stream is useless anyway.
        self.capture = None;
        *self.capture_slot.lock().unwrap() = None;
        self.capture_failed.store(false, Ordering::Relaxed);
        match audio::start_capture(device, self.capture_failed.clone(), self.diag.clone()) {
            Ok((stream, cons, opened_device)) => {
                *self.capture_slot.lock().unwrap() = Some(cons);
                self.capture = Some(AudioHalf {
                    _stream: stream,
                    opened_device,
                });
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// (Re)build the playback half on `device` (None = system default).
    /// The receive task keeps running; decoded audio flows into the new ring
    /// buffer through the swappable slot. On failure the half is absent.
    pub fn rebuild_playback(&mut self, device: Option<&str>) -> Result<()> {
        self.playback = None;
        *self.playback_slot.lock().unwrap() = None;
        self.playback_failed.store(false, Ordering::Relaxed);
        match audio::start_playback(
            device,
            self.output_vol.clone(),
            self.playback_failed.clone(),
            self.diag.clone(),
            self.playout.clone(),
        ) {
            Ok((stream, prod, opened_device)) => {
                *self.playback_slot.lock().unwrap() = Some(prod);
                self.playback = Some(AudioHalf {
                    _stream: stream,
                    opened_device,
                });
                Ok(())
            }
            Err(e) => Err(e),
        }
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

    /// Total bytes received on the voice socket so far. For the sound-check:
    /// non-zero means our reflected audio made the full round-trip back.
    pub fn bytes_received(&self) -> u64 {
        self.bytes_rx.load(Ordering::Relaxed)
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
        if let Some(h) = &self.recv_handle {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── VAD pre-roll ─────────────────────────────────────────────────

    #[test]
    fn vad_preroll_keeps_last_frames_in_order() {
        let mut pr = VadPreroll::new();
        // Push more frames than capacity; ring must keep the newest N.
        let total = config::VAD_PREROLL_FRAMES + 2;
        for i in 0..total {
            let frame = vec![i as f32; config::FRAME_SIZE];
            pr.push(&frame);
        }
        let mut seen = Vec::new();
        pr.drain(|f| seen.push(f[0] as usize));
        let expect: Vec<usize> = (2..total).collect();
        assert_eq!(seen, expect);
        // Drained: a second drain yields nothing.
        let mut count = 0;
        pr.drain(|_| count += 1);
        assert_eq!(count, 0);
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
    fn fec_is_on_in_every_tier_with_a_floored_loss_hint() {
        assert_eq!(fec_loss_hint(0), config::FEC_MIN_LOSS_PERC);
        assert_eq!(
            fec_loss_hint(config::FEC_MIN_LOSS_PERC + 20),
            config::FEC_MIN_LOSS_PERC + 20
        );
        for tier in [
            QualityTier::High,
            QualityTier::Medium,
            QualityTier::Low,
            QualityTier::Minimum,
        ] {
            let mut enc = OpusEncoder::new().unwrap();
            let hint = tier.apply(&mut enc, 0).unwrap();
            assert_eq!(hint, config::FEC_MIN_LOSS_PERC);
            let (bitrate, complexity, fec, loss) = enc.quality_settings();
            assert_eq!(bitrate, tier.bitrate());
            assert_eq!(complexity, tier.complexity());
            assert!(fec, "{tier:?}");
            assert_eq!(loss, config::FEC_MIN_LOSS_PERC);
        }
    }

    #[test]
    fn encoder_starts_at_high_tier() {
        // A bare encoder runs at Opus defaults; the capture thread applies the
        // HIGH tier through `QualityTier::apply` right after creating it.
        let mut enc = OpusEncoder::new().unwrap();
        assert!(!enc.quality_settings().2, "default encoder has no FEC");
        QualityTier::High.apply(&mut enc, 0).unwrap();
        assert_eq!(
            enc.quality_settings(),
            (
                config::BITRATE_HIGH,
                config::COMPLEXITY_HIGH,
                true,
                config::FEC_MIN_LOSS_PERC
            )
        );
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

    #[test]
    fn input_gain_boost_is_limited_and_counted() {
        let mut frame = vec![0.6f32; config::FRAME_SIZE];
        assert!(apply_input_gain(&mut frame, 4.0, true));
        assert!(frame.iter().all(|s| s.abs() < 1.0));

        // AGC on, denoise off: the frame is left unclipped for the AGC.
        let mut frame = vec![0.6f32; config::FRAME_SIZE];
        assert!(!apply_input_gain(&mut frame, 4.0, false));
        assert!(frame.iter().all(|s| *s == 2.4));

        let mut frame = vec![0.6f32; config::FRAME_SIZE];
        assert!(!apply_input_gain(&mut frame, 1.0, true));
        assert!(frame.iter().all(|s| *s == 0.6));

        // Attenuation can never need the limiter.
        let mut frame = vec![0.9f32; config::FRAME_SIZE];
        assert!(!apply_input_gain(&mut frame, 0.5, true));
        assert!(frame.iter().all(|s| *s == 0.45));
    }
}
