// All tunable parameters in one place.
// Grouped by subsystem for clarity.

// ── Network ──────────────────────────────────────────────────────────────

/// Default TCP port for control channel.
pub const TCP_PORT: u16 = 7100;
/// Default UDP port for voice relay.
pub const UDP_PORT: u16 = 7101;
/// Max TCP frame payload (bytes). Frames larger than this are rejected.
pub const MAX_FRAME_SIZE: usize = 65536;
/// TCP read buffer size (bytes).
pub const TCP_READ_BUF: usize = 8192;
/// Max UDP packet size (bytes).
pub const MAX_UDP_PACKET: usize = 1500;
/// Number of UDP hello retries on voice connect.
pub const UDP_HELLO_RETRIES: u32 = 3;
/// Interval between UDP hello retries (milliseconds).
pub const UDP_HELLO_INTERVAL_MS: u64 = 200;
/// Interval between TCP heartbeat pings (seconds).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;

// ── Chat ─────────────────────────────────────────────────────────────────

/// Max chat message length (bytes).
pub const MAX_CHAT_LEN: usize = 2048;
/// Max display name length (bytes).
pub const MAX_NAME_LEN: usize = 32;
/// Max messages kept in TUI history.
pub const MAX_MESSAGE_HISTORY: usize = 500;

// ── Channel ──────────────────────────────────────────────────────────────

/// Length of generated channel IDs.
pub const CHANNEL_ID_LEN: usize = 5;

// ── Audio ─────────────────────────────────────────────────────────────────

/// Opus frame size: 20ms at 48kHz mono = 960 samples.
pub const FRAME_SIZE: usize = 960;
/// Audio sample rate (Hz).
pub const SAMPLE_RATE: u32 = 48000;
/// Number of audio channels (mono).
pub const AUDIO_CHANNELS: u16 = 1;
/// Max encoded Opus packet size (bytes).
pub const MAX_OPUS_PACKET: usize = 4000;
/// Max playback buffer in samples (~100ms). Excess is dropped to cap latency.
pub const MAX_PLAYBACK_BUF: usize = FRAME_SIZE * 5;
/// Jitter buffer min/max capacity (packets) for adaptive sizing.
pub const JITTER_BUF_MIN: usize = 2;
pub const JITTER_BUF_MAX: usize = 15;
/// Initial jitter buffer size before adaptation kicks in.
pub const JITTER_BUF_INITIAL: usize = 4;
/// How often to recalculate adaptive jitter buffer size (packets).
pub const JITTER_ADAPT_INTERVAL: u32 = 50;

// ── VAD ──────────────────────────────────────────────────────────────────

/// RMS energy threshold for voice activity detection.
/// Frames below this are silent and not sent. Set to 0.0 to disable.
pub const VAD_RMS_THRESHOLD: f32 = 0.01;
/// Number of frames to keep sending after the last loud frame (hangover).
/// At 20ms per frame, 10 frames ≈ 200ms of trailing audio.
pub const VAD_HANGOVER_FRAMES: u32 = 10;

// ── Adaptive Quality ────────────────────────────────────────────────

/// Measurement window size in packets (~3 seconds at 50 pps).
pub const STATS_WINDOW_PACKETS: u32 = 150;

/// Loss thresholds (percent) for quality tier transitions.
pub const LOSS_THRESH_MEDIUM: u8 = 5;
pub const LOSS_THRESH_LOW: u8 = 15;
pub const LOSS_THRESH_MINIMUM: u8 = 30;

/// Bitrates (bits/sec) per quality tier.
pub const BITRATE_HIGH: i32 = 32_000;
pub const BITRATE_MEDIUM: i32 = 24_000;
pub const BITRATE_LOW: i32 = 16_000;
pub const BITRATE_MINIMUM: i32 = 10_000;

/// Encoder complexity per quality tier (0–10).
pub const COMPLEXITY_HIGH: u8 = 10;
pub const COMPLEXITY_MEDIUM: u8 = 7;
pub const COMPLEXITY_LOW: u8 = 5;
pub const COMPLEXITY_MINIMUM: u8 = 3;

// ── Crypto ──────────────────────────────────────────────────────────────

/// Length of per-channel voice encryption key (bytes).
pub const VOICE_KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce size (bytes).
pub const XCHACHA20_NONCE_SIZE: usize = 24;
/// Poly1305 authentication tag size (bytes).
pub const POLY1305_TAG_SIZE: usize = 16;

// ── Server ───────────────────────────────────────────────────────────────

/// Interval between maintenance sweeps (empty channel cleanup, etc).
pub const MAINTENANCE_INTERVAL_SECS: u64 = 30;
