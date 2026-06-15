//! Tauri commands invoked from the Solid frontend.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tc_client::network;
use tc_shared::ClientMessage;
use tokio::sync::Mutex;

use crate::drain;
use crate::events::{emit, ConnState};
use crate::server_registry;
use crate::state::{AppCore, VoiceMode};

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[derive(Serialize)]
pub struct AppStatus {
    pub version: String,
    pub fingerprint: Option<String>,
    pub connected: bool,
    pub server_addr: Option<String>,
    pub channel: Option<String>,
    pub muted: bool,
    pub name: Option<String>,
    pub pubkey: Option<String>,
    pub voice_mode: String,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub input_gain_pct: u32,
    pub output_vol_pct: u32,
    pub vad_level_pct: u32,
    pub notifications: bool,
    pub close_to_tray: bool,
    pub autostart: bool,
    pub language: String,
}

#[tauri::command]
pub async fn app_status(state: CoreState<'_>) -> Result<AppStatus, String> {
    let c = state.lock().await;
    Ok(AppStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        fingerprint: c.fingerprint(),
        pubkey: c.pubkey().map(hex::encode),
        connected: c.conn.is_some(),
        server_addr: c.server_addr.clone(),
        channel: c.channel.clone(),
        muted: c.muted.load(Ordering::Relaxed),
        name: c.name.clone(),
        voice_mode: c.voice_mode.as_str().to_string(),
        input_device: c.input_device.clone(),
        output_device: c.output_device.clone(),
        input_gain_pct: c.input_gain_pct,
        output_vol_pct: c.output_vol_pct,
        vad_level_pct: c.vad_level_pct,
        notifications: c.notifications,
        close_to_tray: c.close_to_tray,
        autostart: c.autostart,
        language: c.language.clone(),
    })
}

/// Build a tc:// invite for the current server (+ channel, if joined).
#[tauri::command]
pub async fn invite_link(state: CoreState<'_>) -> Result<String, String> {
    let c = state.lock().await;
    if c.conn.is_none() {
        return Err("not connected — /server <addr> first".into());
    }
    let addr = c.server_addr.as_deref().ok_or("not connected")?;
    Ok(tc_shared::invite_url(addr, c.channel.as_deref()))
}

#[tauri::command]
pub async fn connect(app: AppHandle, state: CoreState<'_>, addr: String) -> Result<(), String> {
    let addr = if addr.contains(':') {
        addr
    } else {
        format!("{}:{}", addr, tc_shared::config::TCP_PORT)
    };

    emit(&app, "connection_state", ConnState::Connecting);

    // Tear down any existing session BEFORE starting a new one. We must do
    // this atomically so the previous drain task can't fire its disconnect
    // handler against the new state. We:
    //   1. abort the previous drain (so its mpsc::recv is cancelled — it
    //      will not run any state mutations, even when the old reader EOFs);
    //   2. drop the old `ServerConnection`, which closes the writer task
    //      and ultimately the TLS stream;
    //   3. clear channel + voice + pending join.
    let (pubkey, tofu, server_for_drain, voice_to_stop) = {
        let mut c = state.lock().await;
        if let Some(h) = c.drain_handle.take() {
            h.abort();
        }
        c.conn = None;
        c.channel = None;
        c.pending_join = None;
        c.server_addr = Some(addr.clone());
        c.save();
        let voice = c.voice.clone();
        (c.pubkey(), c.tofu.clone(), addr.clone(), voice)
    };
    voice_to_stop.stop().await;

    match network::connect(&addr, &tofu, pubkey).await {
        Ok((conn, rx)) => {
            // Send saved name if we have one + touch the server in registry.
            let saved_name = {
                let mut c = state.lock().await;
                c.conn = Some(conn);
                server_registry::touch(&mut c.servers, &addr);
                c.save();
                c.name.clone()
            };
            if let Some(name) = saved_name {
                if let Some(conn) = state.lock().await.conn.as_ref() {
                    let _ = conn.send(ClientMessage::SetName { name });
                }
            }

            // Spawn drain task. It will emit `connection_state: connected`
            // once it sees the Welcome message (which it always does first).
            let core_arc: Arc<Mutex<AppCore>> = (*state).clone();
            let handle = drain::spawn(app.clone(), core_arc, rx, server_for_drain);
            state.lock().await.drain_handle = Some(handle);
            Ok(())
        }
        Err(e) => {
            state.lock().await.conn = None;
            emit(&app, "connection_state", ConnState::Disconnected);
            Err(format!("connect failed: {:#}", e))
        }
    }
}

#[tauri::command]
pub async fn disconnect(app: AppHandle, state: CoreState<'_>) -> Result<(), String> {
    let voice = {
        let mut c = state.lock().await;
        if let Some(h) = c.drain_handle.take() {
            h.abort();
        }
        c.conn = None;
        c.channel = None;
        c.pending_join = None;
        c.voice.clone()
    };
    voice.stop().await;
    emit(&app, "connection_state", ConnState::Disconnected);
    Ok(())
}

#[tauri::command]
pub async fn list_channels(state: CoreState<'_>) -> Result<(), String> {
    send_msg(&state, ClientMessage::ListChannels).await
}

#[tauri::command]
pub async fn create_channel(state: CoreState<'_>, name: Option<String>) -> Result<(), String> {
    send_msg(&state, ClientMessage::CreateChannel { name }).await
}

#[tauri::command]
pub async fn join_channel(state: CoreState<'_>, channel_id: String) -> Result<(), String> {
    send_msg(&state, ClientMessage::JoinChannel { channel_id }).await
}

#[tauri::command]
pub async fn leave_channel(state: CoreState<'_>) -> Result<(), String> {
    let voice = state.lock().await.voice.clone();
    voice.stop().await;
    send_msg(&state, ClientMessage::LeaveChannel).await
}

/// Begin a sound-check. The drain task starts the echo voice session when the
/// server replies `EchoTestReady`, then ends it after a fixed duration and
/// emits `echo_test_started` / `echo_test_result`. Rejected while in a channel
/// (the server's echo channel can't be joined on top of an existing one).
#[tauri::command]
pub async fn start_echo_test(state: CoreState<'_>) -> Result<(), String> {
    {
        let c = state.lock().await;
        if c.conn.is_none() {
            return Err("not connected".into());
        }
        if c.channel.is_some() {
            return Err("leave the channel first".into());
        }
    }
    send_msg(&state, ClientMessage::StartEchoTest).await
}

/// Abort an in-progress sound-check early (e.g. settings closed). Idempotent.
#[tauri::command]
pub async fn cancel_echo_test(state: CoreState<'_>) -> Result<(), String> {
    let voice = state.lock().await.voice.clone();
    let _ = voice.stop_echo().await;
    // Best-effort teardown of the server-side echo channel.
    let _ = send_msg(&state, ClientMessage::StopEchoTest).await;
    Ok(())
}

#[tauri::command]
pub async fn send_chat(state: CoreState<'_>, text: String) -> Result<(), String> {
    send_msg(&state, ClientMessage::ChatMessage { text }).await
}

#[tauri::command]
pub async fn send_dm(
    state: CoreState<'_>,
    to_pubkey_hex: String,
    text: String,
) -> Result<(), String> {
    let to_pubkey = hex::decode(&to_pubkey_hex).map_err(|e| format!("bad pubkey hex: {}", e))?;
    send_msg(&state, ClientMessage::DirectMessage { to_pubkey, text }).await
}

#[tauri::command]
pub async fn set_name(state: CoreState<'_>, name: String) -> Result<(), String> {
    {
        let mut c = state.lock().await;
        c.name = Some(name.clone());
        c.save();
    }
    send_msg(&state, ClientMessage::SetName { name }).await
}

#[tauri::command]
pub async fn set_mute(state: CoreState<'_>, muted: bool) -> Result<(), String> {
    let c = state.lock().await;
    c.muted.store(muted, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn set_voice_mode(state: CoreState<'_>, mode: String) -> Result<(), String> {
    let parsed = VoiceMode::parse(&mode).ok_or_else(|| format!("unknown mode: {}", mode))?;
    let mut c = state.lock().await;
    c.apply_voice_mode(parsed);
    c.save();
    Ok(())
}

/// Set muted = !muted while held; reset by the runtime on key-up. PTT helper.
#[tauri::command]
pub async fn ptt_press(state: CoreState<'_>) -> Result<(), String> {
    state.lock().await.muted.store(false, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn ptt_release(state: CoreState<'_>) -> Result<(), String> {
    state.lock().await.muted.store(true, Ordering::Relaxed);
    Ok(())
}

/// Export user-facing settings as a JSON string. Excludes the local identity
/// (private key) — that lives in a separate file and is intentionally not
/// shared, so a backup can be safely pasted into chat or stored in cleartext.
#[tauri::command]
pub async fn export_settings(state: CoreState<'_>) -> Result<String, String> {
    let c = state.lock().await;
    let s = tc_client::settings::UserSettings {
        server: c.server_addr.clone(),
        input_device: c.input_device.clone(),
        output_device: c.output_device.clone(),
        vad_level: Some(c.vad_level_pct),
        input_gain: Some(c.input_gain_pct),
        output_vol: Some(c.output_vol_pct),
        name: c.name.clone(),
        voice_mode: Some(c.voice_mode.as_str().to_string()),
        hotkeys: c.hotkeys.clone(),
        notifications: Some(c.notifications),
        autostart: Some(c.autostart),
        close_to_tray: Some(c.close_to_tray),
        language: Some(c.language.clone()),
        trusted_servers: c.tofu.trusted_map(),
        servers: c.servers.clone(),
        dm_peers: c.dm_peers.clone(),
    };
    serde_json::to_string_pretty(&s).map_err(|e| format!("serialize: {}", e))
}

/// Replace current settings (servers, devices, hotkeys, DM peer registry,
/// trusted certs, audio prefs…) from a JSON blob produced by `export_settings`.
/// Identity is left untouched.
#[tauri::command]
pub async fn import_settings(state: CoreState<'_>, json: String) -> Result<(), String> {
    let s: tc_client::settings::UserSettings =
        serde_json::from_str(&json).map_err(|e| format!("invalid backup JSON: {}", e))?;
    let mut c = state.lock().await;
    c.server_addr = s.server.clone();
    c.input_device = s.input_device.clone();
    c.output_device = s.output_device.clone();
    if let Some(v) = s.vad_level {
        c.apply_vad_level(v);
    }
    if let Some(v) = s.input_gain {
        c.apply_input_gain(v);
    }
    if let Some(v) = s.output_vol {
        c.apply_output_vol(v);
    }
    c.name = s.name.clone();
    if let Some(m) = s.voice_mode.as_deref().and_then(VoiceMode::parse) {
        c.apply_voice_mode(m);
    }
    c.hotkeys = s.hotkeys.clone();
    if let Some(v) = s.notifications {
        c.notifications = v;
    }
    if let Some(v) = s.autostart {
        c.autostart = v;
    }
    if let Some(v) = s.close_to_tray {
        c.close_to_tray = v;
    }
    if let Some(v) = s.language.as_deref() {
        if matches!(v, "en" | "ru") {
            c.language = v.to_string();
        }
    }
    c.tofu = tc_client::tls::TofuState::new(s.trusted_servers.clone());
    c.servers = s.servers.clone();
    c.dm_peers = s.dm_peers.clone();
    c.save();
    Ok(())
}

async fn send_msg(state: &CoreState<'_>, msg: ClientMessage) -> Result<(), String> {
    let c = state.lock().await;
    let conn = c.conn.as_ref().ok_or_else(|| "not connected".to_string())?;
    conn.send(msg).map_err(|e| format!("send failed: {:#}", e))
}
