// All tunable parameters in one place.
// Grouped by subsystem for clarity.

// ── Version ─────────────────────────────────────────────────────────────

/// Protocol version — bump on every breaking protocol change.
pub const PROTOCOL_VERSION: u16 = 3;

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
/// Initial delay of the background re-hello loop after a failed handshake (milliseconds).
pub const UDP_REHELLO_BASE_MS: u64 = 1000;
/// Cap for the re-hello backoff (milliseconds).
pub const UDP_REHELLO_MAX_MS: u64 = 5000;
/// Send a UDP keepalive after this much voice-send silence (seconds).
/// Must stay below typical NAT/stateful-firewall UDP timeouts (~30–60 s).
pub const UDP_KEEPALIVE_INTERVAL_SECS: u64 = 20;
/// Max accumulated unparsed TCP data per client (bytes). Disconnect on overflow.
pub const MAX_PENDING_BUF: usize = 2 * MAX_FRAME_SIZE;
/// Interval between TCP heartbeat pings (seconds).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;
/// Default port for the embedded web UI.
pub const WEB_PORT: u16 = 17300;

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
/// Prefix for public (named) channels.
pub const PUBLIC_CHANNEL_PREFIX: &str = "pub-";
/// Max length of a public channel name (without prefix).
pub const MAX_CHANNEL_NAME_LEN: usize = 20;
/// Prefix for ephemeral echo-test channels. The UDP relay reflects voice
/// packets on these channels back to the sender (loopback) instead of fanning
/// them out — so a sound check exercises the real round-trip through the server
/// without the server ever decrypting or storing anything.
pub const ECHO_CHANNEL_PREFIX: &str = "echo-";

// ── Echo test (sound check) ────────────────────────────────────────────────

/// Duration of the live echo sound-check (seconds): mic → server → playback.
pub const ECHO_TEST_DURATION_SECS: u64 = 5;
/// How long the client waits for the server's `EchoTestReady` before giving up
/// (e.g. the server is old and ignores the request, or is unreachable).
pub const ECHO_TEST_SETUP_TIMEOUT_SECS: u64 = 5;

// ── Audio ─────────────────────────────────────────────────────────────────

/// Opus frame size: 20ms at 48kHz mono = 960 samples.
pub const FRAME_SIZE: usize = 960;
/// Audio sample rate (Hz).
pub const SAMPLE_RATE: u32 = 48000;
/// Number of audio channels (mono).
pub const AUDIO_CHANNELS: u16 = 1;
/// Max encoded Opus packet size (bytes).
pub const MAX_OPUS_PACKET: usize = 4000;
/// Safety-net latency cap of the output callback's queue, in 48 kHz samples
/// (300 ms). The playout loop only renders while the output holds less than
/// `PLAYOUT_OUTPUT_LOW_WATER`, so in normal operation the queue never gets near
/// this; it exists so a stalled-then-resumed device or any future overproducer
/// cannot grow latency without bound. Set above the largest jitter target
/// (`PLAYOUT_TARGET_MAX_FRAMES`) so it can never fight the playout engine. A
/// trim at this cap is a defect and counts as a glitch.
pub const PLAYBACK_SAFETY_CAP: usize = FRAME_SIZE * 15;
/// Length of the click-avoiding ramps on the playback path, in milliseconds.
/// Every edit of the sample timeline is ramped over this window: the crossfade
/// over a frame dropped by playout acceleration, the crossfade across a
/// safety-cap trim, and the silence↔data boundary when the output runs dry
/// between phrases.
pub const PLAYBACK_FADE_MS: u32 = 5;

/// How often (seconds) the receive task logs the audio glitch counters,
/// when any of them moved since the previous log.
pub const AUDIO_DIAG_LOG_SECS: u64 = 10;

// ── Playout (receive side, `client/playout.rs`) ─────────────────────────
// Playback is pulled by the output device's clock: the receive task renders
// one mixed 20 ms frame whenever the output runs low, and each sender's jitter
// buffer is held at an adaptive depth in front of that pull.

/// Wakeup period of the receive task's playout loop (milliseconds), on top of
/// waking for every inbound packet. Several ticks per 20 ms frame so the output
/// is topped up well before the device needs it. On Windows the default timer
/// resolution is ~15.6 ms, so this may tick at ~16 ms there; the low-water
/// mark below is sized to survive that, and `output_starved` is the evidence.
pub const PLAYOUT_TICK_MS: u64 = 5;
/// The playout loop renders frames while the output (decode → playback ring
/// plus the callback's queue) holds fewer than this many 48 kHz samples.
/// Two frames: one for the device to take next, one to cover a late wakeup
/// (a 16 ms Windows tick plus scheduling slack). A device whose callback asks
/// for more than a frame raises it to one request plus one frame.
pub const PLAYOUT_OUTPUT_LOW_WATER: usize = 2 * FRAME_SIZE;
/// Bounds of a sender's target jitter-buffer depth, in frames. The target is
/// `ceil((2 × jitter_ms + 20) / 20)`: two mean deviations of arrival time plus
/// one frame for the arrival/pull phase. 2 frames (40 ms) is the floor even on
/// a perfect link; 10 frames (200 ms) is where waiting stops being worth it.
pub const PLAYOUT_TARGET_MIN_FRAMES: usize = 2;
pub const PLAYOUT_TARGET_MAX_FRAMES: usize = 10;
/// Frames of concealment (expand) a playing sender gets when its buffer runs
/// empty before it is considered finished and goes idle. 60 ms bridges a late
/// packet without stretching a talkspurt's tail into an audible smear.
pub const PLAYOUT_HOLD_FRAMES: u32 = 3;
/// Window (frames, ≈1 s) over which the minimum buffered depth is taken when
/// deciding to accelerate. Using the minimum, not the mean, means only latency
/// the network never needed over a whole second is removed.
pub const PLAYOUT_ACCEL_WINDOW_FRAMES: usize = 50;
/// Frames of depth history needed before the first acceleration of a talkspurt
/// or after the previous one. Lets the onset (the VAD pre-roll burst) play out
/// intact and keeps one excess frame from being removed twice.
pub const PLAYOUT_ACCEL_MIN_OBSERVED: usize = 10;
/// A frame may be dropped to reduce latency only when its RMS is below this:
/// the default VAD threshold (≈ −36 dBFS), i.e. a level the sender itself
/// treats as not-speech — pauses, breaths, hangover tails. AGC aims speech at
/// `AGC_TARGET_RMS` (≈ −21 dBFS), 15 dB above it.
pub const PLAYOUT_ACCEL_MAX_RMS: f32 = 0.015;
/// Excess depth (frames beyond target + 1) at which acceleration stops waiting
/// for a quiet frame. Content that is never quiet (music, a loud room in
/// paranoid mode) would otherwise let sender clock drift walk the buffer into
/// the ring's edge and force a resync; a crossfaded drop is far gentler.
pub const PLAYOUT_ACCEL_FORCE_EXCESS: usize = 3;
/// Jitter ring capacity per sender (packets, 300 ms). Holds the largest target
/// plus the acceleration margin; anything further ahead is out of window.
pub const JITTER_BUF_MAX: usize = 15;
/// Consecutive packets beyond the jitter ring (forward seq gap) before the
/// buffer resyncs onto the new position. >1 so a single stray/reordered
/// packet cannot reset a healthy stream.
pub const JITTER_RESYNC_AFTER: u32 = 3;
/// Capture starvation watchdog: if the input stream delivers no frames for
/// this long, the device is considered dead (unplugged / lost) and the voice
/// pipeline is flagged unhealthy so a supervisor can rebuild it.
pub const AUDIO_STARVATION_SECS: u64 = 3;
/// Minimum interval between automatic voice-pipeline restarts after a device
/// failure (avoids a tight rebuild loop while no device is available).
pub const VOICE_RESTART_MIN_INTERVAL_MS: u64 = 2000;
/// How often the desktop supervisor polls audio devices: follow-the-default
/// checks, hot-plug detection, and rebuild retries for absent halves.
pub const DEVICE_POLL_INTERVAL_MS: u64 = 2000;
/// Server-side TTL for *orphaned* UDP hello tokens (sessions gone or out of
/// the token's channel). Tokens of live in-channel sessions never expire.
pub const UDP_TOKEN_TTL_SECS: u64 = 30;
/// Max concurrent per-sender receive streams (each owns a jitter buffer +
/// Opus decoder). Packets from further senders are dropped until one goes stale.
pub const MAX_SENDER_STREAMS: usize = 16;
/// Drop a sender's receive stream after this long without a packet from them.
pub const SENDER_STREAM_TTL_SECS: u64 = 10;

/// Per-peer local playback volume (listener-side gain applied before mixing).
/// 100 = unchanged (1.0×); 0 mutes the peer locally; 200 doubles them. Purely
/// local — never sent to the relay or other peers.
pub const PEER_VOL_DEFAULT_PCT: u32 = 100;
pub const PEER_VOL_MAX_PCT: u32 = 200;

// ── Capture downmix ──────────────────────────────────────────────────────
// A capture device we did not get to choose the format of (WASAPI shared mode
// hands us the mixer format or nothing) often presents a mono microphone as a
// multi-channel stream with silence in the spare channels. Averaging across the
// raw channel count then throws away 6 dB per silent channel, which no gain
// slider should have to pay back.
/// A channel counts as carrying signal while its smoothed magnitude is within
/// this fraction of the loudest channel's — about −30 dB.
pub const DOWNMIX_ACTIVE_RATIO: f32 = 0.03;
/// Below this smoothed magnitude the whole stream is treated as silent and the
/// previous decision is kept, so a pause between words cannot flip the divisor.
pub const DOWNMIX_SILENCE_FLOOR: f32 = 1.0e-5;
/// Weight of each callback in the per-channel magnitude envelope. Slow on
/// purpose: this answers "how is this device wired", not "who is talking now".
pub const DOWNMIX_SMOOTHING: f32 = 0.05;

// ── Automatic capture level (AGC) ────────────────────────────────────────
// Opt-in. A manual gain multiplies the noise floor along with the voice, so a
// quiet microphone stays quiet at any slider position; this keeps speech at a
// target level instead. See `client/agc.rs`.
/// Where speech is aimed, as frame RMS. About −21 dBFS — loud enough to match
/// a laptop's own processed microphone, short of the headroom a shout needs.
pub const AGC_TARGET_RMS: f32 = 0.09;
/// Most the gain may ever help (≈ +18 dB) and most it may hold back (−12 dB).
pub const AGC_MAX_GAIN: f32 = 8.0;
pub const AGC_MIN_GAIN: f32 = 0.25;
/// Per-frame approach when coming down. Fast: the next syllable would arrive
/// over-amplified otherwise.
pub const AGC_ATTACK: f32 = 0.35;
/// Per-frame approach when going up — about a second, so nobody hears it.
pub const AGC_RELEASE: f32 = 0.03;
/// Below this frame RMS the gain does not move at all. An AGC that adapts in a
/// pause is a machine for amplifying room noise between sentences.
pub const AGC_FLOOR_RMS: f32 = 0.004;
/// Where the soft limiter (AGC output, capture gain stage, playback output)
/// starts compressing instead of passing through.
pub const LIMITER_KNEE: f32 = 0.8;

// ── VAD ──────────────────────────────────────────────────────────────────

/// RMS energy threshold for voice activity detection.
/// Frames below this are silent and not sent. Set to 0.0 to disable.
/// Maps to VAD level 15 in the TUI (`level * 0.001`) — the default level in
/// both clients.
pub const VAD_RMS_THRESHOLD: f32 = 0.015;
/// Number of frames to keep sending after the last loud frame (hangover).
/// At 20ms per frame, 10 frames ≈ 200ms of trailing audio.
pub const VAD_HANGOVER_FRAMES: u32 = 10;
/// Frames of gated audio kept and flushed when VAD opens, so quiet speech
/// onsets (first consonants) aren't clipped. 5 frames = 100ms of pre-roll.
pub const VAD_PREROLL_FRAMES: usize = 5;

// ── Adaptive Quality ────────────────────────────────────────────────

/// Measurement window size in packets (~3 seconds at 50 pps).
pub const STATS_WINDOW_PACKETS: u32 = 150;

/// Loss thresholds (percent) for quality tier transitions.
pub const LOSS_THRESH_MEDIUM: u8 = 5;
pub const LOSS_THRESH_LOW: u8 = 15;
pub const LOSS_THRESH_MINIMUM: u8 = 30;

/// Bitrates (bits/sec) per quality tier. The HIGH tier is "HD voice": at
/// 64 kbit/s mono VoIP Opus encodes full 20 kHz fullband automatically, well
/// above typical speech-chat quality. Lower tiers degrade gracefully on loss.
pub const BITRATE_HIGH: i32 = 64_000;
pub const BITRATE_MEDIUM: i32 = 40_000;
pub const BITRATE_LOW: i32 = 24_000;
pub const BITRATE_MINIMUM: i32 = 12_000;

/// Encoder complexity per quality tier (0–10).
pub const COMPLEXITY_HIGH: u8 = 10;

/// Floor for the Opus packet-loss hint, in percent. In-band FEC is always on,
/// but the encoder only emits LBRR redundancy when the loss hint is above 0,
/// and the sender sees nothing but its own inbound loss (there is no receiver
/// feedback yet), which is 0 on a clean link. The floor keeps every packet
/// carrying a cheap copy of the previous frame so a single loss is recoverable.
pub const FEC_MIN_LOSS_PERC: u8 = 5;
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

// ── Rate Limiting ───────────────────────────────────────────────────────

/// General command rate limit (commands per second per client).
pub const RATE_LIMIT_CMD_PER_SEC: f64 = 10.0;
/// Maximum burst for general commands.
pub const RATE_LIMIT_CMD_BURST: f64 = 20.0;
/// Channel creation rate limit (per second per client).
pub const RATE_LIMIT_CREATE_PER_SEC: f64 = 0.1;
/// Maximum burst for channel creation.
pub const RATE_LIMIT_CREATE_BURST: f64 = 3.0;

/// General command rate limit per source IP (defends against multi-connection
/// abuse from a single attacker that would otherwise multiply the per-client cap).
/// Sized higher than per-client to leave headroom for legitimate NAT'd users.
pub const RATE_LIMIT_IP_CMD_PER_SEC: f64 = 30.0;
/// Maximum burst per source IP.
pub const RATE_LIMIT_IP_CMD_BURST: f64 = 60.0;
/// Idle entries in the per-IP limiter map are cleared after this many seconds
/// when no connections from that IP remain.
pub const IP_LIMITER_IDLE_TTL_SECS: u64 = 300;
