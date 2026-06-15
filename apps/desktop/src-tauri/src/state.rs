//! Backend application state shared across Tauri commands.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tc_client::identity::Identity;
use tc_client::network::ServerConnection;
use tc_client::settings::{DmPeer, ServerEntry, UserSettings};
use tc_client::tls::TofuState;
use tc_shared::ChannelId;

use crate::voice_actor::VoiceManager;

/// Voice capture mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceMode {
    /// Always send (no VAD).
    Open,
    /// Send only when RMS > threshold.
    Vad,
    /// Muted by default; hotkey unmutes while held.
    Ptt,
}

impl VoiceMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "vad" => Some(Self::Vad),
            "ptt" => Some(Self::Ptt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Vad => "vad",
            Self::Ptt => "ptt",
        }
    }
}

/// Top-level mutable state. Behind a `tokio::sync::Mutex` so commands can
/// hold the lock across .await (e.g. while connecting).
pub struct AppCore {
    pub identity: Option<Identity>,
    pub tofu: TofuState,
    pub server_addr: Option<String>,
    pub conn: Option<ServerConnection>,
    pub voice: VoiceManager,
    pub channel: Option<ChannelId>,
    pub name: Option<String>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub voice_mode: VoiceMode,
    /// Last value the user picked for VAD level (kept across mode toggles).
    pub vad_level_pct: u32,
    pub input_gain_pct: u32,
    pub output_vol_pct: u32,
    /// Hotkey bindings: action name ("mute"/"ptt"/"quick_join") → Tauri accelerator string.
    pub hotkeys: std::collections::HashMap<String, String>,
    pub notifications: bool,
    pub autostart: bool,
    pub close_to_tray: bool,
    /// UI language: "en" or "ru". Defaults to "en".
    pub language: String,
    pub servers: Vec<ServerEntry>,
    pub dm_peers: Vec<DmPeer>,
    /// Atomics shared with the audio capture thread.
    pub muted: Arc<AtomicBool>,
    /// Paranoid mode: constant packet rate + flat frame size (read live by the
    /// capture thread; survives across calls).
    pub paranoid: Arc<AtomicBool>,
    /// Noise suppression (RNNoise) on the captured mic (read live).
    pub denoise: Arc<AtomicBool>,
    pub vad_threshold: Arc<AtomicU32>,
    pub input_gain: Arc<AtomicU32>,
    pub output_vol: Arc<AtomicU32>,
    /// UDP token + voice key from the most recent JoinedChannel — fed to
    /// `voice::start_voice` once the drain task picks it up.
    pub pending_join: Option<PendingJoin>,
    /// Handle to the currently-running drain task. We hold it so a fresh
    /// `connect` can `.abort()` the previous drain before spawning a new
    /// one — without that, the old drain's "disconnected" emit would race
    /// the new connection's state and wipe the freshly-set channel/conn.
    pub drain_handle: Option<tokio::task::JoinHandle<()>>,
}

pub struct PendingJoin {
    pub channel_id: ChannelId,
    pub udp_token: u64,
    pub voice_key: Vec<u8>,
}

impl AppCore {
    pub fn new() -> Self {
        let identity = match Identity::load_or_generate() {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!("identity unavailable: {:#}", e);
                None
            }
        };

        let saved = UserSettings::load();
        let voice_mode = saved
            .voice_mode
            .as_deref()
            .and_then(VoiceMode::parse)
            .unwrap_or(VoiceMode::Vad);
        let vad_level_pct = saved.vad_level.unwrap_or(15);
        let input_gain_pct = saved.input_gain.unwrap_or(100);
        let output_vol_pct = saved.output_vol.unwrap_or(100);

        let muted = Arc::new(AtomicBool::new(matches!(voice_mode, VoiceMode::Ptt)));
        let paranoid = Arc::new(AtomicBool::new(saved.paranoid.unwrap_or(false)));
        let denoise = Arc::new(AtomicBool::new(saved.denoise.unwrap_or(false)));
        let vad_threshold = Arc::new(AtomicU32::new(
            vad_threshold_for(voice_mode, vad_level_pct).to_bits(),
        ));
        let input_gain = Arc::new(AtomicU32::new(pct_to_float(input_gain_pct).to_bits()));
        let output_vol = Arc::new(AtomicU32::new(pct_to_float(output_vol_pct).to_bits()));

        Self {
            identity,
            tofu: TofuState::new(saved.trusted_servers.clone()),
            server_addr: saved.server.clone(),
            conn: None,
            voice: VoiceManager::spawn(),
            channel: None,
            name: saved.name.clone(),
            input_device: saved.input_device.clone(),
            output_device: saved.output_device.clone(),
            voice_mode,
            vad_level_pct,
            input_gain_pct,
            output_vol_pct,
            hotkeys: saved.hotkeys.clone(),
            notifications: saved.notifications.unwrap_or(true),
            autostart: saved.autostart.unwrap_or(false),
            close_to_tray: saved.close_to_tray.unwrap_or(false),
            language: saved
                .language
                .as_deref()
                .filter(|s| matches!(*s, "en" | "ru"))
                .unwrap_or("en")
                .to_string(),
            servers: saved.servers.clone(),
            dm_peers: saved.dm_peers.clone(),
            muted,
            paranoid,
            denoise,
            vad_threshold,
            input_gain,
            output_vol,
            pending_join: None,
            drain_handle: None,
        }
    }

    pub fn pubkey(&self) -> Option<Vec<u8>> {
        self.identity.as_ref().map(|i| i.pubkey().to_vec())
    }

    pub fn fingerprint(&self) -> Option<String> {
        self.identity.as_ref().map(|i| i.fingerprint())
    }

    /// Persist current state into `tc.toml`. Called after any user-visible setting change.
    pub fn save(&self) {
        let s = UserSettings {
            server: self.server_addr.clone(),
            input_device: self.input_device.clone(),
            output_device: self.output_device.clone(),
            vad_level: Some(self.vad_level_pct),
            input_gain: Some(self.input_gain_pct),
            output_vol: Some(self.output_vol_pct),
            name: self.name.clone(),
            voice_mode: Some(self.voice_mode.as_str().to_string()),
            paranoid: Some(self.paranoid.load(Ordering::Relaxed)),
            denoise: Some(self.denoise.load(Ordering::Relaxed)),
            hotkeys: self.hotkeys.clone(),
            notifications: Some(self.notifications),
            autostart: Some(self.autostart),
            close_to_tray: Some(self.close_to_tray),
            language: Some(self.language.clone()),
            trusted_servers: self.tofu.trusted_map(),
            servers: self.servers.clone(),
            dm_peers: self.dm_peers.clone(),
        };
        s.save();
    }

    /// Apply a new voice mode: update muted + vad_threshold atomics accordingly.
    pub fn apply_voice_mode(&mut self, mode: VoiceMode) {
        self.voice_mode = mode;
        let threshold = vad_threshold_for(mode, self.vad_level_pct);
        self.vad_threshold
            .store(threshold.to_bits(), Ordering::Relaxed);
        self.muted
            .store(matches!(mode, VoiceMode::Ptt), Ordering::Relaxed);
    }

    pub fn apply_vad_level(&mut self, level_pct: u32) {
        self.vad_level_pct = level_pct.clamp(0, 100);
        let threshold = vad_threshold_for(self.voice_mode, self.vad_level_pct);
        self.vad_threshold
            .store(threshold.to_bits(), Ordering::Relaxed);
    }

    pub fn apply_input_gain(&mut self, pct: u32) {
        self.input_gain_pct = pct.clamp(0, 400);
        self.input_gain.store(
            pct_to_float(self.input_gain_pct).to_bits(),
            Ordering::Relaxed,
        );
    }

    pub fn apply_output_vol(&mut self, pct: u32) {
        self.output_vol_pct = pct.clamp(0, 400);
        self.output_vol.store(
            pct_to_float(self.output_vol_pct).to_bits(),
            Ordering::Relaxed,
        );
    }
}

fn pct_to_float(pct: u32) -> f32 {
    (pct.min(400) as f32) / 100.0
}

/// Map (mode, vad_level_pct) → RMS threshold for the capture loop.
/// Open/PTT disable VAD; VAD maps 0..100 → ~0.001..0.05 RMS.
fn vad_threshold_for(mode: VoiceMode, level_pct: u32) -> f32 {
    match mode {
        VoiceMode::Open | VoiceMode::Ptt => 0.0,
        VoiceMode::Vad => {
            let n = level_pct.clamp(0, 100) as f32;
            if n == 0.0 {
                0.0
            } else {
                0.001 + (n / 100.0) * 0.049
            }
        }
    }
}
