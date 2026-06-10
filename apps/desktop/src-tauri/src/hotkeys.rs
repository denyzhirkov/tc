//! Global hotkeys: bind a Tauri-style accelerator to one of three actions.
//!
//! Actions:
//!   - mute       — toggle muted (one-shot on key-down)
//!   - ptt        — push-to-talk: muted while released, unmuted while held
//!   - quick_join — emit `quick_join` event so the frontend can prompt
//!
//! Bindings live in `UserSettings.hotkeys` (action → accelerator string) and
//! are restored on startup. `set_hotkey` / `unset_hotkey` re-register at runtime.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::Mutex;

use crate::events::emit;
use crate::state::AppCore;

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    Mute,
    Ptt,
    QuickJoin,
}

impl HotkeyAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mute" => Some(Self::Mute),
            "ptt" => Some(Self::Ptt),
            "quick_join" | "quickjoin" | "join" => Some(Self::QuickJoin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Ptt => "ptt",
            Self::QuickJoin => "quick_join",
        }
    }
}

/// Build the plugin and wire the dispatching handler. Call once during setup.
pub fn build_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            let action = match resolve_action(app, shortcut) {
                Some(a) => a,
                None => return,
            };
            handle(app.clone(), action, event.state());
        })
        .build()
}

/// Restore the hotkey bindings stored in `AppCore` on launch.
pub async fn restore_from_settings(app: &AppHandle) {
    let core: State<Arc<Mutex<AppCore>>> = app.state();
    let bindings: Vec<(HotkeyAction, String)> = {
        let c = core.lock().await;
        c.hotkeys
            .iter()
            .filter_map(|(a, k)| HotkeyAction::parse(a).map(|aa| (aa, k.clone())))
            .collect()
    };
    for (action, accel) in bindings {
        if let Err(e) = register_internal(app, action, &accel) {
            tracing::warn!("failed to restore hotkey {:?} = {}: {}", action, accel, e);
        }
    }
}

#[tauri::command]
pub async fn set_hotkey(
    app: AppHandle,
    state: CoreState<'_>,
    action: String,
    accel: String,
) -> Result<(), String> {
    let parsed =
        HotkeyAction::parse(&action).ok_or_else(|| format!("unknown action: {}", action))?;

    // Unregister whatever this action was previously bound to, so a rebind
    // (or retry after a register failure) can claim the same key.
    let prev = {
        let c = state.lock().await;
        c.hotkeys.get(parsed.as_str()).cloned()
    };
    if let Some(old_accel) = prev {
        if let Ok(s) = old_accel.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(s);
        }
    }

    register_internal(&app, parsed, &accel)?;
    let mut c = state.lock().await;
    c.hotkeys.insert(parsed.as_str().to_string(), accel);
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn unset_hotkey(
    app: AppHandle,
    state: CoreState<'_>,
    action: String,
) -> Result<(), String> {
    let parsed =
        HotkeyAction::parse(&action).ok_or_else(|| format!("unknown action: {}", action))?;
    let mut c = state.lock().await;
    if let Some(accel) = c.hotkeys.remove(parsed.as_str()) {
        let shortcut: Shortcut = accel
            .parse()
            .map_err(|e| format!("invalid accelerator: {}", e))?;
        let _ = app.global_shortcut().unregister(shortcut);
    }
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn list_hotkeys(state: CoreState<'_>) -> Result<Vec<(String, String)>, String> {
    let c = state.lock().await;
    Ok(c.hotkeys
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}

/// Register a single hotkey synchronously (no AppCore mutation).
fn register_internal(app: &AppHandle, _action: HotkeyAction, accel: &str) -> Result<(), String> {
    let shortcut: Shortcut = accel
        .parse()
        .map_err(|e| format!("invalid accelerator '{}': {}", accel, e))?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| format!("register failed: {}", e))?;
    Ok(())
}

/// Find which action this shortcut is bound to. Cheap (a handful of entries).
fn resolve_action(app: &AppHandle, shortcut: &Shortcut) -> Option<HotkeyAction> {
    let core: State<Arc<Mutex<AppCore>>> = app.state();
    // Sync read: we only do a non-blocking try_lock — if the lock is held we
    // skip this event rather than block the event loop.
    let c = core.try_lock().ok()?;
    for (action_str, accel) in c.hotkeys.iter() {
        if let Ok(s) = accel.parse::<Shortcut>() {
            if &s == shortcut {
                return HotkeyAction::parse(action_str);
            }
        }
    }
    None
}

fn handle(app: AppHandle, action: HotkeyAction, state: ShortcutState) {
    let core: State<Arc<Mutex<AppCore>>> = app.state();
    let core = (*core).clone();
    tauri::async_runtime::spawn(async move {
        match (action, state) {
            (HotkeyAction::Mute, ShortcutState::Pressed) => {
                let c = core.lock().await;
                let was = c.muted.fetch_xor(true, Ordering::Relaxed);
                emit(
                    &app,
                    "log",
                    serde_json::json!({ "text": if was { "unmuted (hotkey)" } else { "muted (hotkey)" } }),
                );
            }
            (HotkeyAction::Ptt, ShortcutState::Pressed) => {
                core.lock().await.muted.store(false, Ordering::Relaxed);
            }
            (HotkeyAction::Ptt, ShortcutState::Released) => {
                core.lock().await.muted.store(true, Ordering::Relaxed);
            }
            (HotkeyAction::QuickJoin, ShortcutState::Pressed) => {
                emit(&app, "quick_join", serde_json::json!({}));
            }
            _ => {}
        }
    });
}
