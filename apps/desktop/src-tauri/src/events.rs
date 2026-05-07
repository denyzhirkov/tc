//! Tauri event names and payloads pushed from backend to frontend.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected { server: String },
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
pub struct VoiceQualityPayload {
    pub loss_percent: u8,
    pub tier: String,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

/// Helper: emit and ignore the result (frontend may not have listeners yet).
pub fn emit<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: P) {
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!("failed to emit {}: {}", event, e);
    }
}
