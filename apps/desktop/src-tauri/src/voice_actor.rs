//! Voice actor — runs on a dedicated OS thread because `VoiceHandle` (and the
//! underlying `cpal::Stream`s it owns) is `!Send`. The thread spins up its own
//! `current_thread` Tokio runtime so we can still call the async
//! `voice::start_voice`. Other parts of the app talk to the actor only via the
//! Send-safe `VoiceManager` handle.

use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;

use anyhow::Result;
use tc_client::audio;
use tc_client::voice::{self, VoiceHandle};
use tc_shared::ChannelId;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub struct StartParams {
    /// Resolved IP of the live TCP connection — the UDP voice target.
    pub server_ip: std::net::IpAddr,
    pub channel_id: ChannelId,
    pub udp_token: u64,
    pub voice_key: Vec<u8>,
    pub muted: Arc<AtomicBool>,
    pub vad_threshold: Arc<AtomicU32>,
    pub input_gain: Arc<AtomicU32>,
    pub output_vol: Arc<AtomicU32>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub sender_name: String,
    /// Echo (sound-check) session — hear our own audio reflected by the server.
    pub echo_test: bool,
}

/// Outcome of a sound-check, sampled just before the echo handle is dropped.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct EchoResult {
    /// Microphone half opened successfully (a capture device was available).
    pub mic_ok: bool,
    /// Output half opened successfully (a playback device was available).
    pub output_ok: bool,
    /// Server confirmed our UDP registration (control path round-trip worked).
    pub registered: bool,
    /// Reflected audio actually came back — the full mic→server→ear loop works.
    pub roundtrip_ok: bool,
    /// Bytes received back (diagnostic; 0 with VAD on usually means silence).
    pub bytes_received: u64,
}

/// A newly hot-plugged audio device, reported once for the frontend prompt.
#[derive(Clone, Debug)]
pub struct NewDevice {
    /// "input" or "output".
    pub kind: &'static str,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct VoiceSnapshot {
    pub active: bool,
    pub loss_percent: u8,
    pub tier: String,
    /// `(name, level)` for each speaker active in the last 300ms.
    pub speaker_levels: Vec<(String, f32)>,
    pub tx_rate: f64,
    pub rx_rate: f64,
    /// 0.0–1.0 RMS of the most recent input frame.
    pub input_peak: f32,
    /// Server confirmed our UDP registration; `false` = sending but deaf.
    pub registered: bool,
    /// Number of automatic half rebuilds after device failures this call.
    pub device_restarts: u32,
    /// Microphone half is live (false = listen-only).
    pub capture_ok: bool,
    /// Output half is live (false = speak-only).
    pub playback_ok: bool,
    /// Hot-plugged devices detected since the last snapshot (drained on read).
    pub new_devices: Vec<NewDevice>,
}

impl VoiceSnapshot {
    pub fn idle() -> Self {
        Self {
            active: false,
            loss_percent: 0,
            tier: "—".into(),
            speaker_levels: Vec::new(),
            tx_rate: 0.0,
            rx_rate: 0.0,
            input_peak: 0.0,
            registered: true,
            device_restarts: 0,
            capture_ok: true,
            playback_ok: true,
            new_devices: Vec::new(),
        }
    }
}

enum Cmd {
    Start(StartParams, oneshot::Sender<Result<(), String>>),
    Stop,
    Snapshot(oneshot::Sender<VoiceSnapshot>),
    /// Apply a new input device immediately (None = system default).
    SetInputDevice(Option<String>),
    /// Apply a new output device immediately (None = system default).
    SetOutputDevice(Option<String>),
    /// Start a sound-check session (separate handle from a live call).
    EchoStart(StartParams, oneshot::Sender<Result<(), String>>),
    /// Stop the sound-check, sampling the result first. Idempotent.
    EchoStop(oneshot::Sender<EchoResult>),
}

#[derive(Clone)]
pub struct VoiceManager {
    tx: mpsc::Sender<Cmd>,
}

impl VoiceManager {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>(16);
        std::thread::Builder::new()
            .name("voice-actor".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("voice-actor runtime");
                rt.block_on(actor_loop(rx));
            })
            .expect("spawn voice-actor thread");
        Self { tx }
    }

    pub async fn start(&self, params: StartParams) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Start(params, reply_tx))
            .await
            .map_err(|_| "voice actor gone".to_string())?;
        reply_rx
            .await
            .map_err(|_| "voice actor dropped reply".to_string())?
    }

    pub async fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop).await;
    }

    /// Switch the input device now (mid-call) and for future starts.
    pub async fn set_input_device(&self, name: Option<String>) {
        let _ = self.tx.send(Cmd::SetInputDevice(name)).await;
    }

    /// Switch the output device now (mid-call) and for future starts.
    pub async fn set_output_device(&self, name: Option<String>) {
        let _ = self.tx.send(Cmd::SetOutputDevice(name)).await;
    }

    pub async fn snapshot(&self) -> VoiceSnapshot {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(Cmd::Snapshot(reply_tx)).await.is_err() {
            return VoiceSnapshot::idle();
        }
        reply_rx.await.unwrap_or_else(|_| VoiceSnapshot::idle())
    }

    /// Start a sound-check session. Runs alongside (but separate from) any live
    /// call handle; in practice the caller blocks echo while in a channel.
    pub async fn start_echo(&self, params: StartParams) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::EchoStart(params, reply_tx))
            .await
            .map_err(|_| "voice actor gone".to_string())?;
        reply_rx
            .await
            .map_err(|_| "voice actor dropped reply".to_string())?
    }

    /// Stop the sound-check and read its result. Safe to call when none runs.
    pub async fn stop_echo(&self) -> EchoResult {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(Cmd::EchoStop(reply_tx)).await.is_err() {
            return EchoResult::default();
        }
        reply_rx.await.unwrap_or_default()
    }
}

async fn start_from(p: &StartParams) -> anyhow::Result<VoiceHandle> {
    voice::start_voice(
        p.server_ip,
        p.channel_id.clone(),
        p.udp_token,
        p.muted.clone(),
        p.voice_key.clone(),
        p.input_device.clone(),
        p.output_device.clone(),
        p.vad_threshold.clone(),
        p.input_gain.clone(),
        p.output_vol.clone(),
        p.sender_name.clone(),
        p.echo_test,
    )
    .await
}

/// Supervisor state for the audio device lifecycle.
struct Supervisor {
    /// Desired devices (None = follow the system default). Seeded from
    /// `StartParams`, updated live by `SetInputDevice`/`SetOutputDevice`.
    desired_input: Option<String>,
    desired_output: Option<String>,
    last_capture_attempt: Option<std::time::Instant>,
    last_playback_attempt: Option<std::time::Instant>,
    /// Device polling (follow-default + hot-plug) throttle.
    last_device_poll: Option<std::time::Instant>,
    /// Known device names for hot-plug diffing (None until first poll).
    known_inputs: Option<std::collections::HashSet<String>>,
    known_outputs: Option<std::collections::HashSet<String>>,
    rebuilds: u32,
    pending_new_devices: Vec<NewDevice>,
}

impl Supervisor {
    fn new(input: Option<String>, output: Option<String>) -> Self {
        Self {
            desired_input: input,
            desired_output: output,
            last_capture_attempt: None,
            last_playback_attempt: None,
            last_device_poll: None,
            known_inputs: None,
            known_outputs: None,
            rebuilds: 0,
            pending_new_devices: Vec::new(),
        }
    }

    /// One supervision pass. Called on every snapshot tick (~5/s); the device
    /// polling parts self-throttle to DEVICE_POLL_INTERVAL_MS. `handle` is
    /// None outside a call — hot-plug detection still runs.
    fn tick(&mut self, mut handle: Option<&mut VoiceHandle>) {
        let retry =
            std::time::Duration::from_millis(tc_shared::config::VOICE_RESTART_MIN_INTERVAL_MS);

        // 1. Rebuild dead/absent halves (throttled per half).
        if let Some(h) = handle.as_deref_mut() {
            if h.capture_state() != voice::HalfState::Ok
                && self
                    .last_capture_attempt
                    .is_none_or(|t| t.elapsed() >= retry)
            {
                self.last_capture_attempt = Some(std::time::Instant::now());
                match h.rebuild_capture(self.desired_input.as_deref()) {
                    Ok(()) => {
                        self.rebuilds += 1;
                        tracing::warn!("capture half rebuilt (device lost or changed)");
                    }
                    Err(e) => tracing::debug!("capture rebuild attempt failed: {:#}", e),
                }
            }
            if h.playback_state() != voice::HalfState::Ok
                && self
                    .last_playback_attempt
                    .is_none_or(|t| t.elapsed() >= retry)
            {
                self.last_playback_attempt = Some(std::time::Instant::now());
                match h.rebuild_playback(self.desired_output.as_deref()) {
                    Ok(()) => {
                        self.rebuilds += 1;
                        tracing::warn!("playback half rebuilt (device lost or changed)");
                    }
                    Err(e) => tracing::debug!("playback rebuild attempt failed: {:#}", e),
                }
            }
        }

        // 2. Device polling: follow the system default + hot-plug detection.
        let poll = std::time::Duration::from_millis(tc_shared::config::DEVICE_POLL_INTERVAL_MS);
        if self.last_device_poll.is_some_and(|t| t.elapsed() < poll) {
            return;
        }
        self.last_device_poll = Some(std::time::Instant::now());

        // Follow-default: when the user picked "Default", a default-device
        // change must move the live call (streams are bound to the physical
        // device they were opened on and never follow on their own).
        if let Some(h) = handle.as_deref_mut() {
            if self.desired_input.is_none() {
                if let Some(def) = audio::default_input_name() {
                    if h.opened_input().is_some_and(|cur| cur != def)
                        && h.rebuild_capture(None).is_ok()
                    {
                        self.rebuilds += 1;
                        tracing::info!("capture moved to new system default: {}", def);
                    }
                }
            }
            if self.desired_output.is_none() {
                if let Some(def) = audio::default_output_name() {
                    if h.opened_output().is_some_and(|cur| cur != def)
                        && h.rebuild_playback(None).is_ok()
                    {
                        self.rebuilds += 1;
                        tracing::info!("playback moved to new system default: {}", def);
                    }
                }
            }
        }

        // Hot-plug detection: diff device name sets. First poll is the
        // baseline — never prompt for devices that were already present.
        let inputs: std::collections::HashSet<String> = audio::list_input_devices()
            .map(|v| v.into_iter().map(|d| d.name).collect())
            .unwrap_or_default();
        let outputs: std::collections::HashSet<String> = audio::list_output_devices()
            .map(|v| v.into_iter().map(|d| d.name).collect())
            .unwrap_or_default();
        if let (Some(known_in), Some(known_out)) =
            (self.known_inputs.take(), self.known_outputs.take())
        {
            for name in inputs.difference(&known_in) {
                self.on_new_device("input", name, handle.as_deref_mut());
            }
            for name in outputs.difference(&known_out) {
                self.on_new_device("output", name, handle.as_deref_mut());
            }
        }
        self.known_inputs = Some(inputs);
        self.known_outputs = Some(outputs);
    }

    /// A device appeared. If it is the user's configured device — switch to it
    /// automatically (they already chose it); otherwise queue a UI prompt.
    fn on_new_device(&mut self, kind: &'static str, name: &str, handle: Option<&mut VoiceHandle>) {
        let configured = match kind {
            "input" => self.desired_input.as_deref() == Some(name),
            _ => self.desired_output.as_deref() == Some(name),
        };
        if configured {
            if let Some(h) = handle {
                let res = if kind == "input" {
                    h.rebuild_capture(Some(name))
                } else {
                    h.rebuild_playback(Some(name))
                };
                if res.is_ok() {
                    self.rebuilds += 1;
                    tracing::info!("configured {} device reconnected: {}", kind, name);
                }
            }
        } else {
            self.pending_new_devices.push(NewDevice {
                kind,
                name: name.to_string(),
            });
        }
    }
}

/// Sample a sound-check result from a live echo handle.
fn echo_result(h: &VoiceHandle) -> EchoResult {
    let bytes = h.bytes_received();
    EchoResult {
        mic_ok: h.capture_state() == voice::HalfState::Ok,
        output_ok: h.playback_state() == voice::HalfState::Ok,
        registered: h.is_registered(),
        roundtrip_ok: bytes > 0,
        bytes_received: bytes,
    }
}

async fn actor_loop(mut rx: mpsc::Receiver<Cmd>) {
    let mut handle: Option<VoiceHandle> = None;
    // Sound-check handle, kept separate from a live call so the call's restart
    // supervisor never touches it. Mutually exclusive with `handle` in practice.
    let mut echo_handle: Option<VoiceHandle> = None;
    // Kept while in a call so the pipeline can be rebuilt after an audio
    // device failure (unplugged headset, default-device switch, …).
    let mut last_params: Option<StartParams> = None;
    let mut last_restart: Option<std::time::Instant> = None;
    let mut sup = Supervisor::new(None, None);
    let restart_interval =
        std::time::Duration::from_millis(tc_shared::config::VOICE_RESTART_MIN_INTERVAL_MS);

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Start(p, reply) => {
                // Drop any prior handle first.
                handle = None;
                last_restart = None;
                sup = Supervisor::new(p.input_device.clone(), p.output_device.clone());
                let res = start_from(&p).await;
                last_params = Some(p);
                match res {
                    Ok(h) => {
                        handle = Some(h);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(format!("{:#}", e)));
                    }
                }
            }
            Cmd::Stop => {
                handle = None;
                last_params = None;
            }
            Cmd::EchoStart(p, reply) => {
                echo_handle = None;
                match start_from(&p).await {
                    Ok(h) => {
                        echo_handle = Some(h);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(format!("{:#}", e)));
                    }
                }
            }
            Cmd::EchoStop(reply) => {
                let result = echo_handle.as_ref().map(echo_result).unwrap_or_default();
                echo_handle = None;
                let _ = reply.send(result);
            }
            Cmd::SetInputDevice(name) => {
                sup.desired_input = name;
                if let Some(h) = handle.as_mut() {
                    match h.rebuild_capture(sup.desired_input.as_deref()) {
                        Ok(()) => tracing::info!("input device switched mid-call"),
                        Err(e) => tracing::warn!("input switch failed: {:#}", e),
                    }
                }
            }
            Cmd::SetOutputDevice(name) => {
                sup.desired_output = name;
                if let Some(h) = handle.as_mut() {
                    match h.rebuild_playback(sup.desired_output.as_deref()) {
                        Ok(()) => tracing::info!("output device switched mid-call"),
                        Err(e) => tracing::warn!("output switch failed: {:#}", e),
                    }
                }
            }
            Cmd::Snapshot(reply) => {
                // Network-level safety net: start_voice itself failed (socket
                // error) — retry the whole pipeline, throttled. Audio halves
                // never fail the start anymore; they degrade and are handled
                // by the supervisor below.
                if handle.is_none()
                    && echo_handle.is_none()
                    && last_params.is_some()
                    && last_restart.is_none_or(|t| t.elapsed() >= restart_interval)
                {
                    if let Some(p) = &last_params {
                        last_restart = Some(std::time::Instant::now());
                        match start_from(p).await {
                            Ok(h) => {
                                tracing::warn!("voice pipeline restarted after start failure");
                                handle = Some(h);
                            }
                            Err(e) => {
                                tracing::warn!("voice pipeline restart failed: {:#}", e);
                            }
                        }
                    }
                }

                sup.tick(handle.as_mut());

                // Report the call handle if present, otherwise a running
                // sound-check — so the frontend's level meter works in both.
                let snap = match handle.as_ref().or(echo_handle.as_ref()) {
                    Some(h) => {
                        let (loss, tier) = h.quality_info();
                        let (tx_rate, rx_rate, _bytes_total) = h.traffic_info();
                        VoiceSnapshot {
                            active: true,
                            loss_percent: loss,
                            tier: tier.to_string(),
                            speaker_levels: h.speaker_levels(),
                            tx_rate,
                            rx_rate,
                            input_peak: h.input_peak(),
                            registered: h.is_registered(),
                            device_restarts: sup.rebuilds,
                            capture_ok: h.capture_state() == voice::HalfState::Ok,
                            playback_ok: h.playback_state() == voice::HalfState::Ok,
                            new_devices: std::mem::take(&mut sup.pending_new_devices),
                        }
                    }
                    None => {
                        let mut s = VoiceSnapshot::idle();
                        // Hot-plug prompts work outside calls too ("yes" just
                        // selects the device for future joins).
                        s.new_devices = std::mem::take(&mut sup.pending_new_devices);
                        s
                    }
                };
                let _ = reply.send(snap);
            }
        }
    }
}
