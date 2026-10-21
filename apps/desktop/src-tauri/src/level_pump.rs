//! Periodic emitter that polls the voice actor and pushes a `voice_level`
//! event to the frontend roughly 5×/sec while voice is active. Each poll also
//! hands the current call's live-levels handle to the fast lane
//! (`fast_levels`), which then runs without touching `AppCore`.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::{watch, Mutex};

use crate::events::{emit, DeviceAddedPayload, SpeakerLevel, VoiceLevelPayload};
use crate::state::AppCore;

const TICK: Duration = Duration::from_millis(200);

pub fn spawn(app: AppHandle, core: Arc<Mutex<AppCore>>) {
    let (live_tx, live_rx) = watch::channel(None);
    crate::fast_levels::spawn(app.clone(), live_rx);
    tauri::async_runtime::spawn(async move {
        let mut last_muted: Option<bool> = None;
        let mut was_active = false;
        let mut last_restarts: u32 = 0;
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

            let mut snap = voice.snapshot().await;
            // A new call, a restarted pipeline or the end of the call: only
            // then does the fast lane need to re-point.
            let live = snap.live_levels.take();
            live_tx.send_if_modified(|cur| {
                let same = match (cur.as_ref(), live.as_ref()) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    *cur = live;
                }
                !same
            });
            // Hot-plug prompts are relevant both in and out of calls.
            for d in &snap.new_devices {
                emit(
                    &app,
                    "device_added",
                    DeviceAddedPayload {
                        kind: d.kind.to_string(),
                        name: d.name.clone(),
                    },
                );
            }
            // Before the idle `continue` below: a device list goes stale just
            // as easily with no call running, and that is exactly when someone
            // is sitting in settings picking one.
            if snap.devices_changed {
                emit(&app, "devices_changed", serde_json::json!({}));
            }
            if !snap.active {
                // The frontend must learn that voice stopped, or it keeps
                // animating the last speaker levels forever (phantom waves).
                if was_active {
                    was_active = false;
                    emit(&app, "voice_stopped", serde_json::json!({}));
                }
                continue;
            }
            was_active = true;
            if snap.device_restarts != last_restarts {
                last_restarts = snap.device_restarts;
                emit(
                    &app,
                    "log",
                    serde_json::json!({
                        "text": "audio device changed — stream rebuilt automatically"
                    }),
                );
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
                    registered: snap.registered,
                    capture_ok: snap.capture_ok,
                    playback_ok: snap.playback_ok,
                },
            );
        }
    });
}
