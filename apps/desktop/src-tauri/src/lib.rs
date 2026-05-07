//! tc_ desktop — Tauri backend.

mod audio_cmd;
mod commands;
mod deeplink;
mod dm;
mod drain;
mod events;
mod history;
mod hotkeys;
mod level_pump;
mod notify;
mod server_registry;
mod settings_cmd;
mod state;
mod tray;
mod voice_actor;

use std::sync::Arc;

use tauri::Manager;
use tokio::sync::Mutex;

use crate::state::AppCore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("tc=info,tc_desktop=info")),
        )
        .init();

    // Required for the rustls-based TLS stack used by `tc-client`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tauri::Builder::default()
        .plugin(hotkeys::build_plugin())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // Wire up tc:// deep-link handler. Forwards parsed invites as a
            // Tauri event the frontend listens for.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        deeplink::forward(&handle, url.as_str());
                    }
                });
            }

            let core = Arc::new(Mutex::new(AppCore::new()));
            app.manage(core.clone());
            level_pump::spawn(app.handle().clone(), core);

            // Tray icon (best-effort — log if unavailable, e.g. headless CI).
            if let Err(e) = tray::install(app.handle()) {
                tracing::warn!("tray install failed: {}", e);
            }

            // Restore hotkey bindings asynchronously after setup.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                hotkeys::restore_from_settings(&handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept the close button: hide instead of quit when close_to_tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let core: tauri::State<Arc<Mutex<AppCore>>> = app.state();
                let core_arc = (*core).clone();
                let close_to_tray = core_arc
                    .try_lock()
                    .map(|c| c.close_to_tray)
                    .unwrap_or(false);
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_status,
            commands::connect,
            commands::disconnect,
            commands::list_channels,
            commands::create_channel,
            commands::join_channel,
            commands::leave_channel,
            commands::send_chat,
            commands::send_dm,
            commands::set_name,
            commands::set_mute,
            commands::set_voice_mode,
            commands::ptt_press,
            commands::ptt_release,
            audio_cmd::list_input_devices,
            audio_cmd::list_output_devices,
            audio_cmd::set_input_device,
            audio_cmd::set_output_device,
            audio_cmd::set_input_gain,
            audio_cmd::set_output_volume,
            audio_cmd::set_vad_level,
            audio_cmd::play_test_signal,
            hotkeys::set_hotkey,
            hotkeys::unset_hotkey,
            hotkeys::list_hotkeys,
            settings_cmd::set_notifications,
            settings_cmd::set_autostart,
            settings_cmd::set_close_to_tray,
            server_registry::list_servers,
            server_registry::forget_server,
            server_registry::set_server_favourite,
            server_registry::label_server,
            history::get_history,
            history::clear_history,
            dm::list_dm_peers,
            dm::forget_dm_peer,
            dm::get_dm_history,
            dm::send_dm_to,
            dm::resolve_peer,
            commands::export_settings,
            commands::import_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
