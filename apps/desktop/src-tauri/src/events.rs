//! Tauri event names and payloads pushed from backend to frontend.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected {
        server: String,
    },
    /// Connection dropped; backoff scheduled before next attempt.
    Reconnecting {
        attempt: u32,
        delay_secs: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerEvent {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatPayload {
    pub from: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DmPayload {
    pub from_pubkey: String,
    pub from_name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinedPayload {
    pub channel_id: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelListEntry {
    pub channel_id: String,
    pub participant_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelListPayload {
    pub channels: Vec<ChannelListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakerLevel {
    pub name: String,
    pub level: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceLevelPayload {
    pub input_peak: f32,
    pub speakers: Vec<SpeakerLevel>,
    pub loss_percent: u8,
    pub tier: String,
    pub tx_kbps: f64,
    pub rx_kbps: f64,
    pub muted: bool,
    /// `false` while the server has not confirmed UDP registration (no inbound voice).
    pub registered: bool,
    /// Microphone half is live (false = listen-only call).
    pub capture_ok: bool,
    /// Output half is live (false = speak-only call).
    pub playback_ok: bool,
}

/// A new audio device was hot-plugged; the frontend asks whether to use it.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceAddedPayload {
    /// "input" or "output".
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

/// Pushed to the overlay window when its config changes so it can react live
/// (e.g. show/hide by visibility mode) without polling `app_status`.
#[derive(Debug, Clone, Serialize)]
pub struct OverlayConfigPayload {
    pub enabled: bool,
    pub position: String,
    pub visibility: String,
}

/// Helper: emit and ignore the result (frontend may not have listeners yet).
pub fn emit<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: P) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!("failed to emit {}: {}", event, e);
    }
}
