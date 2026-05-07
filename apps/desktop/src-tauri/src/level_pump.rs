//! Periodic emitter that polls the voice actor and pushes a `voice_level`
//! event to the frontend roughly 5×/sec while voice is active.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::events::{emit, SpeakerLevel, VoiceLevelPayload};
use crate::state::AppCore;

const TICK: Duration = Duration::from_millis(200);

pub fn spawn(app: AppHandle, core: Arc<Mutex<AppCore>>) {
    tauri::async_runtime::spawn(async move {
        let mut last_muted: Option<bool> = None;
        loop {
            tokio::time::sleep(TICK).await;
            let (voice, muted) = {
                let c = core.lock().await;
                (c.voice.clone(), c.muted.load(Ordering::Relaxed))
            };

            // Emit a lightweight mute event whenever it flips, even if voice is
            // off — so the header reflects hotkey-driven mute toggles.
            if last_muted != Some(muted) {
                emit(&app, "muted_changed", serde_json::json!({ "muted": muted }));
                last_muted = Some(muted);
            }

            let snap = voice.snapshot().await;
            if !snap.active {
                continue;
            }
            emit(
                &app,
                "voice_level",
                VoiceLevelPayload {
                    input_peak: snap.input_peak,
                    speakers: snap
                        .speaker_levels
                        .into_iter()
                        .map(|(name, level)| SpeakerLevel { name, level })
                        .collect(),
                    loss_percent: snap.loss_percent,
                    tier: snap.tier,
                    tx_kbps: snap.tx_rate / 1024.0,
                    rx_kbps: snap.rx_rate / 1024.0,
                    muted,
                },
            );
        }
    });
}
