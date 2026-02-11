use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

mod audio;
mod codec;
mod network;
mod settings;
mod tls;
mod tui;
mod voice;

use tc_shared::config;
use tc_shared::{ClientMessage, ServerMessage};

#[tokio::main]
async fn main() -> Result<()> {
    // Log to file so it doesn't mess up the TUI
    let log_file = std::fs::File::create("/tmp/tc-client.log").ok();
    if let Some(file) = log_file {
        tracing_subscriber::fmt()
            .with_writer(file)
            .with_env_filter("tc=debug")
            .init();
    }

    let user_settings = settings::UserSettings::load();
    let server_addr = user_settings
        .server
        .clone()
        .unwrap_or_else(|| format!("127.0.0.1:{}", config::TCP_PORT));
    let muted = Arc::new(AtomicBool::new(false));

    let mut app = tui::App::new(muted.clone(), server_addr.clone());
    app.input_device = user_settings.input_device;
    app.output_device = user_settings.output_device;
    app.name = user_settings.name.map(|n| sanitize_name(&n)).filter(|n| !n.is_empty());
    if let Some(level) = user_settings.vad_level {
        let threshold = vad_threshold_from_level(level);
        app.vad_threshold
            .store(threshold.to_bits(), Ordering::Relaxed);
    }
    let mut terminal = tui::init_terminal()?;

    // Try initial connection
    let mut conn: Option<network::ServerConnection> = None;
    let mut server_rx: Option<mpsc::UnboundedReceiver<Option<ServerMessage>>> = None;

    app.add_message(format!("connecting to {}...", server_addr));
    terminal.draw(|f| tui::draw(f, &app))?;

    match network::connect(&server_addr).await {
        Ok((c, rx)) => {
            app.add_message("connected!".into());
            if let Some(name) = app.name.clone() {
                let _ = c.send(ClientMessage::SetName { name });
            }
            conn = Some(c);
            server_rx = Some(rx);
        }
        Err(_) => {
            app.disconnected = true;
            app.add_message("server not configured, connection failed".into());
            app.add_message("use /server <ip> to set address, then /reconnect".into());
        }
    }

    // Voice handle — set when we join a channel
    let mut voice_handle: Option<voice::VoiceHandle> = None;

    loop {
        // Update voice quality info
        app.voice_quality = voice_handle.as_ref().map(|vh| {
            let (loss, tier) = vh.quality_info();
            (loss, tier.to_string())
        });
        app.voice_traffic = voice_handle.as_ref().map(|vh| vh.traffic_info());

        // Tick matrix rain if active
        if app.matrix_mode {
            let size = terminal.size()?;
            let rate = app.voice_traffic.map(|(tx, rx, _)| tx + rx).unwrap_or(0.0);
            app.matrix_state.tick(size.width, size.height, rate);
        }

        // Draw
        terminal.draw(|f| tui::draw(f, &app))?;

        // Poll keyboard events (non-blocking, 50ms timeout)
        let action = tui::poll_event(&mut app, Duration::from_millis(50));

        match action {
            tui::Action::Command(input) => {
                handle_command(
                    &input,
                    &mut conn,
                    &mut server_rx,
                    &mut app,
                    &muted,
                    &mut voice_handle,
                )
                .await;
            }
            tui::Action::Quit => break,
            tui::Action::None => {}
        }

        // Drain server messages
        if let Some(ref mut rx) = server_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Some(msg) => {
                        handle_server_message(msg, &mut conn, &mut app, &muted, &mut voice_handle).await;
                    }
                    None => {
                        app.disconnected = true;
                        conn = None;
                        voice_handle.take();
                        app.voice_active = false;
                        app.channel = None;
                        app.participants.clear();
                        app.voice_join_params = None;
                        app.add_message("disconnected from server".into());
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Cleanup
    drop(voice_handle);
    tui::restore_terminal();
    Ok(())
}

fn send_or_disconnect(
    conn: &mut Option<network::ServerConnection>,
    app: &mut tui::App,
    msg: ClientMessage,
) {
    if let Some(ref c) = conn {
        if c.send(msg).is_err() {
            *conn = None;
            app.disconnected = true;
            app.add_message("disconnected from server".into());
        }
    }
}

async fn handle_command(
    input: &str,
    conn: &mut Option<network::ServerConnection>,
    server_rx: &mut Option<mpsc::UnboundedReceiver<Option<ServerMessage>>>,
    app: &mut tui::App,
    muted: &Arc<AtomicBool>,
    voice_handle: &mut Option<voice::VoiceHandle>,
) {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();

    // Commands that work without a connection
    match parts[0] {
        "/matrix" => {
            app.matrix_mode = !app.matrix_mode;
            if app.matrix_mode {
                app.matrix_state = tui::MatrixState::new();
            }
            return;
        }
        "/server" => {
            if let Some(addr) = parts.get(1).map(|s| s.trim()) {
                if !addr.is_empty() {
                    app.server_addr = if addr.contains(':') {
                        addr.to_string()
                    } else {
                        format!("{}:{}", addr, config::TCP_PORT)
                    };
                    save_settings(app);
                    app.add_message(format!("server address set to {}", app.server_addr));
                    // Auto-reconnect if not connected
                    if app.disconnected {
                        do_reconnect(conn, server_rx, app).await;
                    }
                } else {
                    app.add_message(format!("current server: {}", app.server_addr));
                }
            } else {
                app.add_message(format!("current server: {}", app.server_addr));
            }
            return;
        }
        "/reconnect" => {
            do_reconnect(conn, server_rx, app).await;
            return;
        }
        "/config" => {
            handle_config(parts.get(1).copied(), app, muted, voice_handle).await;
            return;
        }
        "/name" => {
            if let Some(new_name) = parts.get(1).map(|s| sanitize_name(s)) {
                if !new_name.is_empty() {
                    app.add_message(format!("> /name {}", new_name));
                    send_or_disconnect(conn, app, ClientMessage::SetName {
                        name: new_name.to_string(),
                    });
                    app.name = Some(new_name);
                    save_settings(app);
                } else {
                    app.add_message("usage: /name <name>".into());
                }
            } else {
                app.add_message("usage: /name <name>".into());
            }
            return;
        }
        "/mute" | "/m" => {
            let was_muted = muted.fetch_xor(true, Ordering::Relaxed);
            if was_muted {
                app.add_message("unmuted".into());
            } else {
                app.add_message("muted".into());
            }
            return;
        }
        "/quit" | "/exit" => {
            app.should_quit = true;
            return;
        }
        "/help" => {
            app.add_message("── commands ──".into());
            app.add_message("  /config            list audio config".into());
            app.add_message("  /config input <N>  select input device".into());
            app.add_message("  /config output <N> select output device".into());
            app.add_message("  /config vad <0-100> set vad level (0/off = disable, default 10)".into());
            app.add_message("  /create            create a new channel".into());
            app.add_message("  /connect <id>      join a channel".into());
            app.add_message("  /leave             leave current channel".into());
            app.add_message("  /mute              toggle mute".into());
            app.add_message("  /name <name>       set your display name".into());
            app.add_message("  /server <ip>       set server address".into());
            app.add_message("  /reconnect         reconnect to server".into());
            app.add_message("  /quit              exit".into());
            app.add_message("  <text>             send chat message".into());
            app.add_message("── keys ──".into());
            app.add_message("  Up/Down            command history".into());
            app.add_message("  Ctrl+C / Esc       quit".into());
            return;
        }
        _ => {}
    }

    // Commands that require a connection
    if app.disconnected {
        app.add_message("not connected — use /server <ip> and /reconnect".into());
        return;
    }

    match parts[0] {
        "/create" => {
            app.add_message("> /create".into());
            send_or_disconnect(conn, app, ClientMessage::CreateChannel);
        }
        "/connect" | "/join" => {
            if let Some(id) = parts.get(1).map(|s| s.trim()) {
                if !id.is_empty() {
                    app.add_message(format!("> /connect {}", id));
                    send_or_disconnect(conn, app, ClientMessage::JoinChannel {
                        channel_id: id.to_string(),
                    });
                } else {
                    app.add_message("usage: /connect <channel_id>".into());
                }
            } else {
                app.add_message("usage: /connect <channel_id>".into());
            }
        }
        "/leave" => {
            app.add_message("> /leave".into());
            send_or_disconnect(conn, app, ClientMessage::LeaveChannel);
            *voice_handle = None;
            app.voice_active = false;
            app.channel = None;
            app.participants.clear();
            app.voice_join_params = None;
        }
        _ => {
            if !input.starts_with('/') {
                // Chat message
                send_or_disconnect(conn, app, ClientMessage::ChatMessage {
                    text: input.to_string(),
                });
                app.add_message(format!("you: {}", input));
            } else {
                app.add_message(format!("unknown command: {} (try /help)", parts[0]));
            }
        }
    }
}

async fn do_reconnect(
    conn: &mut Option<network::ServerConnection>,
    server_rx: &mut Option<mpsc::UnboundedReceiver<Option<ServerMessage>>>,
    app: &mut tui::App,
) {
    // Drop old connection
    conn.take();
    server_rx.take();
    app.disconnected = true;
    app.voice_active = false;
    app.channel = None;
    app.participants.clear();
    app.voice_join_params = None;

    app.add_message(format!("connecting to {}...", app.server_addr));

    match network::connect(&app.server_addr).await {
        Ok((c, rx)) => {
            app.add_message("connected!".into());
            app.disconnected = false;
            if let Some(name) = app.name.clone() {
                let _ = c.send(ClientMessage::SetName { name });
            }
            *conn = Some(c);
            *server_rx = Some(rx);
        }
        Err(e) => {
            app.add_message(format!("connection failed: {}", e));
        }
    }
}

async fn handle_server_message(
    msg: ServerMessage,
    conn: &mut Option<network::ServerConnection>,
    app: &mut tui::App,
    muted: &Arc<AtomicBool>,
    voice_handle: &mut Option<voice::VoiceHandle>,
) {
    match msg {
        ServerMessage::ChannelCreated { channel_id } => {
            app.add_message(format!("channel created: {}, joining...", channel_id));
            send_or_disconnect(conn, app, ClientMessage::JoinChannel {
                channel_id: channel_id.to_string(),
            });
        }
        ServerMessage::JoinedChannel {
            channel_id,
            participants,
            udp_token,
            voice_key,
        } => {
            app.add_message(format!(
                "joined #{} ({})",
                channel_id,
                participants.join(", ")
            ));
            app.participants = participants;
            app.channel = Some(channel_id.clone());

            // Store join params for potential restart
            app.voice_join_params = Some(tui::VoiceJoinParams {
                channel_id: channel_id.clone(),
                udp_token,
                voice_key: voice_key.clone(),
            });

            // Start voice
            match voice::start_voice(
                &app.server_addr,
                channel_id,
                udp_token,
                muted.clone(),
                voice_key,
                app.input_device.clone(),
                app.output_device.clone(),
                app.vad_threshold.clone(),
            )
            .await
            {
                Ok(handle) => {
                    *voice_handle = Some(handle);
                    app.voice_active = true;
                    app.add_message("voice active".into());
                }
                Err(e) => {
                    app.add_message(format!("voice error: {}", e));
                }
            }
        }
        ServerMessage::PeerJoined { peer_name } => {
            app.add_message(format!("{} joined", peer_name));
            app.participants.push(peer_name);
        }
        ServerMessage::PeerLeft { peer_name } => {
            app.add_message(format!("{} left", peer_name));
            app.participants.retain(|p| p != &peer_name);
        }
        ServerMessage::LeftChannel => {
            *voice_handle = None;
            app.voice_active = false;
            app.channel = None;
            app.participants.clear();
            app.voice_join_params = None;
            app.add_message("left channel".into());
        }
        ServerMessage::NameChanged { old_name, new_name } => {
            app.add_message(format!("{} is now {}", old_name, new_name));
            // Update participants list
            if let Some(p) = app.participants.iter_mut().find(|p| *p == &old_name) {
                *p = new_name;
            }
        }
        ServerMessage::ChatMessage { from, text } => {
            app.add_message(format!("{}: {}", from, text));
        }
        ServerMessage::Error { message } => {
            app.add_message(format!("error: {}", message));
        }
        ServerMessage::Pong => {}
    }
}

async fn handle_config(
    args: Option<&str>,
    app: &mut tui::App,
    muted: &Arc<AtomicBool>,
    voice_handle: &mut Option<voice::VoiceHandle>,
) {
    let args = args.unwrap_or("").trim();

    if args.is_empty() {
        // List all devices
        app.add_message("── input devices ──".into());
        match audio::list_input_devices() {
            Ok(devices) => {
                for d in &devices {
                    let marker = if app.input_device.as_deref() == Some(&d.name) {
                        " [selected]"
                    } else if d.is_default {
                        " [default]"
                    } else {
                        ""
                    };
                    app.add_message(format!("  {}. {}{}", d.index, d.name, marker));
                }
            }
            Err(e) => app.add_message(format!("  error: {}", e)),
        }
        app.add_message("── output devices ──".into());
        match audio::list_output_devices() {
            Ok(devices) => {
                for d in &devices {
                    let marker = if app.output_device.as_deref() == Some(&d.name) {
                        " [selected]"
                    } else if d.is_default {
                        " [default]"
                    } else {
                        ""
                    };
                    app.add_message(format!("  {}. {}{}", d.index, d.name, marker));
                }
            }
            Err(e) => app.add_message(format!("  error: {}", e)),
        }
        app.add_message("── vad ──".into());
        let threshold = f32::from_bits(app.vad_threshold.load(Ordering::Relaxed));
        if threshold == 0.0 {
            app.add_message("  level: off (0)".into());
        } else {
            app.add_message(format!("  level: {} (threshold {:.4})", vad_level_from_threshold(threshold), threshold));
        }
        app.add_message(format!("  hangover: {} frames", config::VAD_HANGOVER_FRAMES));
        return;
    }

    let sub_parts: Vec<&str> = args.splitn(2, ' ').collect();
    let sub_cmd = sub_parts[0];
    let index_str = sub_parts.get(1).map(|s| s.trim()).unwrap_or("");

    match sub_cmd {
        "input" => {
            let Ok(idx) = index_str.parse::<usize>() else {
                app.add_message("usage: /config input <N>".into());
                return;
            };
            match audio::list_input_devices() {
                Ok(devices) => {
                    if let Some(d) = devices.into_iter().find(|d| d.index == idx) {
                        app.add_message(format!("input device set to: {}", d.name));
                        app.input_device = Some(d.name);
                        save_settings(app);
                        restart_voice(app, muted, voice_handle).await;
                    } else {
                        app.add_message(format!("no input device with index {}", idx));
                    }
                }
                Err(e) => app.add_message(format!("error listing devices: {}", e)),
            }
        }
        "output" => {
            let Ok(idx) = index_str.parse::<usize>() else {
                app.add_message("usage: /config output <N>".into());
                return;
            };
            match audio::list_output_devices() {
                Ok(devices) => {
                    if let Some(d) = devices.into_iter().find(|d| d.index == idx) {
                        app.add_message(format!("output device set to: {}", d.name));
                        app.output_device = Some(d.name);
                        save_settings(app);
                        restart_voice(app, muted, voice_handle).await;
                    } else {
                        app.add_message(format!("no output device with index {}", idx));
                    }
                }
                Err(e) => app.add_message(format!("error listing devices: {}", e)),
            }
        }
        "vad" => {
            if handle_config_vad(index_str, app) {
                save_settings(app);
            }
        }
        _ => {
            app.add_message("usage: /config [input|output|vad] <value>".into());
        }
    }
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\n' && *c != '\r' && *c != '\t')
        .collect::<String>()
        .trim()
        .to_string()
}

fn vad_level_from_threshold(threshold: f32) -> u32 {
    (threshold * 1000.0).round() as u32
}

fn vad_threshold_from_level(level: u32) -> f32 {
    level as f32 * 0.001
}

fn handle_config_vad(value: &str, app: &mut tui::App) -> bool {
    if value.is_empty() {
        let current = f32::from_bits(app.vad_threshold.load(Ordering::Relaxed));
        if current == 0.0 {
            app.add_message("vad: off (0)".into());
        } else {
            app.add_message(format!("vad level: {} (threshold {:.4})", vad_level_from_threshold(current), current));
        }
        app.add_message(format!("  hangover: {} frames", config::VAD_HANGOVER_FRAMES));
        return false;
    }

    if value == "off" {
        app.vad_threshold.store(0.0_f32.to_bits(), Ordering::Relaxed);
        app.add_message("vad disabled".into());
        return true;
    }

    match value.parse::<u32>() {
        Ok(0) => {
            app.vad_threshold.store(0.0_f32.to_bits(), Ordering::Relaxed);
            app.add_message("vad disabled".into());
            true
        }
        Ok(level @ 1..=100) => {
            let threshold = vad_threshold_from_level(level);
            app.vad_threshold.store(threshold.to_bits(), Ordering::Relaxed);
            app.add_message(format!("vad level set to {} (threshold {:.4})", level, threshold));
            true
        }
        _ => {
            app.add_message("usage: /config vad <0-100|off> (default 10)".into());
            false
        }
    }
}

fn save_settings(app: &tui::App) {
    let default_server = format!("127.0.0.1:{}", config::TCP_PORT);
    let threshold = f32::from_bits(app.vad_threshold.load(Ordering::Relaxed));
    let level = vad_level_from_threshold(threshold);
    let default_level = vad_level_from_threshold(config::VAD_RMS_THRESHOLD);

    let s = settings::UserSettings {
        server: if app.server_addr != default_server {
            Some(app.server_addr.clone())
        } else {
            None
        },
        input_device: app.input_device.clone(),
        output_device: app.output_device.clone(),
        vad_level: if level != default_level {
            Some(level)
        } else {
            None
        },
        name: app.name.clone(),
    };
    s.save();
}

async fn restart_voice(
    app: &mut tui::App,
    muted: &Arc<AtomicBool>,
    voice_handle: &mut Option<voice::VoiceHandle>,
) {
    let Some(params) = app.voice_join_params.clone() else {
        return;
    };

    // Drop old handle
    voice_handle.take();
    app.voice_active = false;

    match voice::start_voice(
        &app.server_addr,
        params.channel_id,
        params.udp_token,
        muted.clone(),
        params.voice_key,
        app.input_device.clone(),
        app.output_device.clone(),
        app.vad_threshold.clone(),
    )
    .await
    {
        Ok(handle) => {
            *voice_handle = Some(handle);
            app.voice_active = true;
            app.add_message("voice restarted".into());
        }
        Err(e) => {
            app.add_message(format!("voice restart error: {}", e));
        }
    }
}
