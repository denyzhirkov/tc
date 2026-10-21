//! Tauri event names and payloads pushed from backend to frontend.

use serde::ser::{SerializeSeq, SerializeStruct, Serializer};
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

/// The fast level lane (`voice_levels`, ~30 Hz): our own microphone and every
/// sender's per-frame loudness, 0..1. Borrows the emitter's reused buffer, so
/// a tick allocates nothing beyond the serialized event.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct VoiceLevelsPayload<'a> {
    pub me: f32,
    pub peers: PeerLevels<'a>,
}

/// `[(name, level)]`, serialized as `[{ name, level }]` like `SpeakerLevel`.
#[derive(Debug, Clone, Copy)]
pub struct PeerLevels<'a>(pub &'a [(String, f32)]);

impl Serialize for PeerLevels<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Entry<'b>(&'b str, f32);
        impl Serialize for Entry<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut s = serializer.serialize_struct("SpeakerLevel", 2)?;
                s.serialize_field("name", self.0)?;
                s.serialize_field("level", &self.1)?;
                s.end()
            }
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (name, level) in self.0 {
            seq.serialize_element(&Entry(name, *level))?;
        }
        seq.end()
    }
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
