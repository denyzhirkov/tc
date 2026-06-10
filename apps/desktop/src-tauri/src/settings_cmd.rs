//! User-toggleable platform integration: notifications, autostart, close-to-tray.

use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::Mutex;

use crate::state::AppCore;

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[tauri::command]
pub async fn set_notifications(state: CoreState<'_>, enabled: bool) -> Result<(), String> {
    let mut c = state.lock().await;
    c.notifications = enabled;
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_autostart(
    app: AppHandle,
    state: CoreState<'_>,
    enabled: bool,
) -> Result<(), String> {
    let mgr = app.autolaunch();
    let res = if enabled { mgr.enable() } else { mgr.disable() };
    res.map_err(|e| format!("autostart: {}", e))?;
    let mut c = state.lock().await;
    c.autostart = enabled;
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_close_to_tray(state: CoreState<'_>, enabled: bool) -> Result<(), String> {
    let mut c = state.lock().await;
    c.close_to_tray = enabled;
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_language(state: CoreState<'_>, lang: String) -> Result<(), String> {
    let lang = match lang.as_str() {
        "en" | "ru" => lang,
        other => return Err(format!("unsupported language: {}", other)),
    };
    let mut c = state.lock().await;
    c.language = lang;
    c.save();
    Ok(())
}
