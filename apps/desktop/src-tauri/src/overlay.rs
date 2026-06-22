//! Room overlay: a transparent, click-through, always-on-top HUD window that
//! shows the current channel roster + who's speaking over a fullscreen game.
//!
//! The window is a second webview loading `overlay.html` — a pure subscriber to
//! the same Tauri events the main window listens to (`AppCore` stays the single
//! source of truth). The backend only owns the window's lifecycle (create on
//! enable, destroy on disable) and placement; what it renders is decided by the
//! overlay frontend from the live event stream.
//!
//! Platform reality: this floats over *borderless-windowed* games. Exclusive
//! fullscreen (DirectX exclusive) cannot be overlaid by an ordinary window.

use std::sync::Arc;

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;

use crate::events::{emit, OverlayConfigPayload};
use crate::state::{is_overlay_position, AppCore};

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

pub const LABEL: &str = "overlay";

/// Logical size of the HUD window and its margin from the screen edge.
const WIDTH: f64 = 232.0;
const HEIGHT: f64 = 360.0;
const MARGIN: f64 = 16.0;

/// Create/destroy + place the overlay window to match the current config.
/// Safe to call repeatedly (idempotent) and on startup.
pub async fn sync(app: &AppHandle, core: &Arc<Mutex<AppCore>>) {
    let (enabled, position) = {
        let c = core.lock().await;
        (c.overlay_enabled, c.overlay_position.clone())
    };
    if enabled {
        ensure(app, &position);
    } else if let Some(w) = app.get_webview_window(LABEL) {
        // destroy() (not close()) bypasses the close-to-tray CloseRequested path.
        let _ = w.destroy();
    }
}

fn ensure(app: &AppHandle, position: &str) {
    if let Some(w) = app.get_webview_window(LABEL) {
        place(&w, position);
        let _ = w.show();
        return;
    }
    match build(app) {
        Ok(w) => place(&w, position),
        Err(e) => tracing::warn!("overlay window create failed: {}", e),
    }
}

fn build(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
        .title("tc_ overlay")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(true)
        .build()?;
    // Pure HUD: never intercept clicks; ride along onto the game's space.
    // Both are best-effort — unsupported on some Wayland compositors.
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.set_visible_on_all_workspaces(true);
    Ok(win)
}

/// Anchor the window to a screen-edge preset on the primary monitor.
fn place(win: &tauri::WebviewWindow, preset: &str) {
    let Ok(Some(mon)) = win.primary_monitor() else {
        return; // headless / no monitor — leave at default placement.
    };
    let scale = mon.scale_factor();
    let size = mon.size();
    let origin = mon.position();
    let ow = (WIDTH * scale) as i32;
    let oh = (HEIGHT * scale) as i32;
    let m = (MARGIN * scale) as i32;
    let mw = size.width as i32;
    let mh = size.height as i32;

    let left = origin.x + m;
    let right = origin.x + mw - ow - m;
    let top = origin.y + m;
    let bottom = origin.y + mh - oh - m;
    let vcenter = origin.y + (mh - oh) / 2;

    let (x, y) = match preset {
        "tl" => (left, top),
        "tr" => (right, top),
        "bl" => (left, bottom),
        "br" => (right, bottom),
        "lc" => (left, vcenter),
        "rc" => (right, vcenter),
        _ => (right, top),
    };
    let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
}

async fn emit_config(app: &AppHandle, core: &Arc<Mutex<AppCore>>) {
    let c = core.lock().await;
    emit(
        app,
        "overlay_config",
        OverlayConfigPayload {
            enabled: c.overlay_enabled,
            position: c.overlay_position.clone(),
            visibility: c.overlay_visibility.clone(),
        },
    );
}

#[tauri::command]
pub async fn set_overlay_enabled(
    app: AppHandle,
    state: CoreState<'_>,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut c = state.lock().await;
        c.overlay_enabled = enabled;
        c.save();
    }
    let core = (*state).clone();
    sync(&app, &core).await;
    emit_config(&app, &core).await;
    Ok(())
}

#[tauri::command]
pub async fn set_overlay_position(
    app: AppHandle,
    state: CoreState<'_>,
    position: String,
) -> Result<(), String> {
    if !is_overlay_position(&position) {
        return Err(format!("invalid overlay position: {}", position));
    }
    {
        let mut c = state.lock().await;
        c.overlay_position = position.clone();
        c.save();
    }
    if let Some(w) = app.get_webview_window(LABEL) {
        place(&w, &position);
    }
    let core = (*state).clone();
    emit_config(&app, &core).await;
    Ok(())
}

#[tauri::command]
pub async fn set_overlay_visibility(
    app: AppHandle,
    state: CoreState<'_>,
    visibility: String,
) -> Result<(), String> {
    if !matches!(visibility.as_str(), "always" | "in_call") {
        return Err(format!("invalid overlay visibility: {}", visibility));
    }
    {
        let mut c = state.lock().await;
        c.overlay_visibility = visibility;
        c.save();
    }
    let core = (*state).clone();
    emit_config(&app, &core).await;
    Ok(())
}
