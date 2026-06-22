//! tc_ desktop — Tauri backend.

mod audio_cmd;
mod commands;
mod deeplink;
mod dev_log;
mod dm;
mod drain;
mod events;
mod history;
mod hotkeys;
mod level_pump;
mod notify;
mod overlay;
mod server_registry;
mod settings_cmd;
mod state;
mod tray;
mod update_check;
mod voice_actor;

use std::sync::Arc;

use tauri::Manager;
use tokio::sync::Mutex;

use crate::state::AppCore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::Layer;

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("tc=info,tc_desktop=info"));
        // Console output keeps the env filter; the dev-log layer sees DEBUG+
        // so /show_dev_logs can stream details without restarting with RUST_LOG.
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
            .with(dev_log::DevLogLayer.with_filter(tracing_subscriber::filter::LevelFilter::DEBUG))
            .init();
    }

    // Required for the rustls-based TLS stack used by `tc-client`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tauri::Builder::default()
        // Must be the first plugin: on Windows/Linux a tc:// click spawns a
        // second process; this forwards it here (the deep-link feature already
        // re-triggers on_open_url from argv) and we just surface the window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(hotkeys::build_plugin())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // Wire up tc:// deep-link handler. Forwards parsed invites as a
            // Tauri event the frontend listens for; until the frontend is
            // ready they are buffered in the Gate (cold-start links).
            {
                app.manage(deeplink::Gate::default());
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        deeplink::forward(&handle, url.as_str());
                    }
                });

                // Installers register the scheme on macOS/Windows; in dev and
                // for Linux AppImages it must be (re-)registered at runtime.
                #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
                if let Err(e) = app.deep_link().register_all() {
                    tracing::warn!("tc:// scheme registration failed: {}", e);
                }
            }

            dev_log::set_app(app.handle().clone());

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

            // Create the room overlay window if it was left enabled.
            let handle = app.handle().clone();
            let core_for_overlay: Arc<Mutex<AppCore>> =
                app.state::<Arc<Mutex<AppCore>>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                overlay::sync(&handle, &core_for_overlay).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept the close button: hide instead of quit when close_to_tray.
            // Only for the main window — the overlay manages its own lifecycle.
            if window.label() != "main" {
                return;
            }
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
            commands::set_paranoid,
            commands::set_denoise,
            commands::ptt_press,
            commands::ptt_release,
            audio_cmd::list_input_devices,
            audio_cmd::list_output_devices,
            audio_cmd::set_input_device,
            audio_cmd::set_output_device,
            audio_cmd::set_input_gain,
            audio_cmd::set_output_volume,
            audio_cmd::set_peer_volume,
            audio_cmd::list_peer_volumes,
            audio_cmd::set_vad_level,
            audio_cmd::play_test_signal,
            commands::start_echo_test,
            commands::cancel_echo_test,
            hotkeys::set_hotkey,
            hotkeys::unset_hotkey,
            hotkeys::list_hotkeys,
            settings_cmd::set_notifications,
            settings_cmd::set_autostart,
            settings_cmd::set_close_to_tray,
            settings_cmd::set_language,
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
            deeplink::take_pending_invite,
            commands::invite_link,
            commands::export_settings,
            commands::import_settings,
            update_check::check_for_update,
            update_check::open_release,
            dev_log::set_dev_logs,
            overlay::set_overlay_enabled,
            overlay::set_overlay_position,
            overlay::set_overlay_visibility,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
