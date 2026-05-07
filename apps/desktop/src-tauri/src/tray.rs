//! System tray icon + menu. Provides quick access to mute toggle,
//! show/hide window, and quit.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

/// 22×22 black-on-transparent template PNG. Embedded so we don't depend on the
/// installed bundle's icons/ folder at runtime.
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray-icon@2x.png");

use crate::state::AppCore;

/// Build and install the tray icon. Call once during setup.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let toggle_mute = MenuItemBuilder::with_id("toggle_mute", "Toggle mute").build(app)?;
    let show = MenuItemBuilder::with_id("show", "Show window").build(app)?;
    let hide = MenuItemBuilder::with_id("hide", "Hide window").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit tc_").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&toggle_mute, &show, &hide])
        .separator()
        .item(&quit)
        .build()?;

    let tray_image = Image::from_bytes(TRAY_ICON_PNG)
        .unwrap_or_else(|_| app.default_window_icon().cloned().expect("default icon"));

    TrayIconBuilder::with_id("main")
        .icon(tray_image)
        .icon_as_template(true) // macOS: tinted automatically for light/dark menu bar
        .tooltip("tc_")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle_mute" => toggle_mute_via_state(app.clone()),
            "show" => show_main(app),
            "hide" => hide_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click on the tray icon → toggle window visibility.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let visible = w.is_visible().unwrap_or(false);
                    if visible {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn hide_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

fn toggle_mute_via_state(app: AppHandle) {
    let core: tauri::State<Arc<Mutex<AppCore>>> = app.state();
    let core = (*core).clone();
    tauri::async_runtime::spawn(async move {
        let c = core.lock().await;
        c.muted.fetch_xor(true, Ordering::Relaxed);
    });
}
