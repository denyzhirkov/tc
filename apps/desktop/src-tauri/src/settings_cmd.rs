//! User-toggleable platform integration: notifications, autostart, close-to-tray.

use std::sync::Arc;

use tauri::{AppHandle, State};
#[cfg(not(windows))]
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
    #[cfg(windows)]
    set_autostart_windows(&app, enabled).map_err(|e| format!("autostart: {}", e))?;
    #[cfg(not(windows))]
    {
        let mgr = app.autolaunch();
        let res = if enabled { mgr.enable() } else { mgr.disable() };
        res.map_err(|e| format!("autostart: {}", e))?;
    }
    let mut c = state.lock().await;
    c.autostart = enabled;
    c.save();
    Ok(())
}

/// Windows autostart via the registry, done directly instead of through the
/// plugin: the bundled `auto-launch` 0.5 *opens* `HKCU\…\Run` with
/// `KEY_SET_VALUE` rather than creating it, so on profiles where that key does
/// not yet exist `enable()` fails with "The system cannot find the file
/// specified (os error 2)" — and `disable()` fails the same way when the value
/// is already absent. `create_subkey` is idempotent and we tolerate a missing
/// value on delete, so both directions are robust. The executable path is
/// quoted so paths with spaces (e.g. `C:\Program Files\…`) launch correctly.
#[cfg(windows)]
fn set_autostart_windows(app: &AppHandle, enabled: bool) -> std::io::Result<()> {
    use tauri::Manager;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    // Task Manager's "Startup" tab records per-entry enable/disable here; a
    // "disabled" record shadows the Run key, so the app silently won't launch
    // even though the Run value exists.
    const APPROVED_KEY: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    let value_name = app.package_info().name.clone();

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu.create_subkey(RUN_KEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        run.set_value(&value_name, &format!("\"{}\"", exe.display()))?;
        // Clear any prior "disabled" shadow so the entry actually fires. The
        // user just opted in via our settings, so honour that over a stale
        // Task Manager toggle. Best-effort: the key/value may not exist.
        if let Ok(approved) = hkcu.open_subkey_with_flags(APPROVED_KEY, KEY_SET_VALUE) {
            let _ = approved.delete_value(&value_name);
        }
    } else if let Err(e) = run.delete_value(&value_name) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e);
        }
    }
    Ok(())
}

/// Re-apply the saved autostart state to the OS at launch so the setting stays
/// the source of truth. On Windows this self-heals a stale `Run` path (e.g.
/// written from a different install location, or a now-moved dev build) by
/// rewriting the current executable path, and re-clears any disabled shadow.
/// Windows-only: macOS/Linux autostart is persisted by tauri-plugin-autostart
/// (LaunchAgent / .desktop entry), which survives across launches on its own.
pub fn reconcile_autostart_at_startup(app: &AppHandle, enabled: bool) {
    #[cfg(windows)]
    {
        match set_autostart_windows(app, enabled) {
            Ok(()) => tracing::info!(enabled, "autostart reconciled with Run key"),
            Err(e) => tracing::warn!("autostart reconcile failed: {}", e),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, enabled);
    }
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
