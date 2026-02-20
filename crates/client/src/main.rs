use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use tokio::sync::{broadcast, mpsc};

mod audio;
mod codec;
mod network;
mod settings;
mod tls;
mod tui;
mod voice;
mod web;

use tc_shared::config;
use tc_shared::{ClientMessage, ServerMessage};
use tls::TofuState;
use tui::ConnectionState;
use web::WebState;

#[derive(Parser)]
#[command(name = "tc-client", about = "termicall voice chat client")]
struct Args {
    /// Web UI port (default: 17300)
    #[arg(long = "web-port", default_value_t = config::WEB_PORT)]
    web_port: u16,

    /// Disable the web UI
    #[arg(long = "no-web")]
    no_web: bool,
}

/// Auto-reconnect backoff parameters.
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 30_000;

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

    let args = Args::parse();

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
    if let Some(pct) = user_settings.input_gain {
        app.input_gain
            .store(vol_from_percent(pct).to_bits(), Ordering::Relaxed);
    }
    if let Some(pct) = user_settings.output_vol {
        app.output_vol
            .store(vol_from_percent(pct).to_bits(), Ordering::Relaxed);
    }
    let mut terminal = tui::init_terminal()?;

    // TOFU state for TLS certificate verification
    let tofu = TofuState::new(user_settings.trusted_servers);

    // Web UI channels
    let (web_cmd_tx, mut web_cmd_rx) = mpsc::channel::<web::WebCommand>(64);
    let (web_state_tx, _) = broadcast::channel::<WebState>(16);

    if !args.no_web {
        let port = args.web_port;
        let tx = web_cmd_tx.clone();
        let state_tx = web_state_tx.clone();
        tokio::spawn(web::start_web_server(port, tx, state_tx));
        app.add_message(format!("web UI at http://127.0.0.1:{}", port));
    }

    // Try initial connection
    let mut conn: Option<network::ServerConnection> = None;
    let mut server_rx: Option<mpsc::Receiver<Option<ServerMessage>>> = None;

    app.add_message(format!("connecting to {}...", server_addr));
    terminal.draw(|f| tui::draw(f, &app))?;

    match network::connect(&server_addr, &tofu).await {
        Ok((c, rx)) => {
            handle_tofu_result(&tofu, &mut app);
            app.add_message("connected!".into());
            app.conn_state = ConnectionState::Connected;
            if let Some(name) = app.name.clone() {
                let _ = c.send(ClientMessage::SetName { name });
            }
            conn = Some(c);
            server_rx = Some(rx);
            save_tofu(&tofu, &app);
        }
        Err(_) => {
            app.conn_state = ConnectionState::Disconnected;
            app.add_message("server not configured, connection failed".into());
            app.add_message("use /server <ip> to set address, then /reconnect".into());
        }
    }

    // Voice handle — set when we join a channel
    let mut voice_handle: Option<voice::VoiceHandle> = None;

    // Auto-reconnect state
    let mut reconnect_attempt: u32 = 0;
    let mut next_reconnect: Option<Instant> = None;

    let mut last_stats_tick = Instant::now();

    loop {
        // Periodic voice stats update (~100ms)
        if app.voice_active && last_stats_tick.elapsed() >= Duration::from_millis(100) {
            app.voice_quality = voice_handle.as_ref().map(|vh| {
                let (loss, tier) = vh.quality_info();
                (loss, tier.to_string())
            });
            app.voice_traffic = voice_handle.as_ref().map(|vh| vh.traffic_info());
            app.active_speakers = voice_handle
                .as_ref()
                .map(|vh| vh.active_speakers())
                .unwrap_or_default();
            app.dirty = true;
            last_stats_tick = Instant::now();
        }

        // Tick matrix rain if active
        if app.matrix_mode {
            let size = terminal.size()?;
            let rate = app.voice_traffic.map(|(tx, rx, _)| tx + rx).unwrap_or(0.0);
            app.matrix_state.tick(size.width, size.height, rate);
            app.dirty = true;
        }

        // Draw only when state changed + broadcast to web UI
        if app.dirty {
            terminal.draw(|f| tui::draw(f, &app))?;
            let _ = web_state_tx.send(WebState::from_app(&app));
            app.dirty = false;
        }

        // Adaptive poll timeout: fast when animating, slow when idle
        let poll_timeout = if app.voice_active || app.matrix_mode {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(200)
        };
        let action = tui::poll_event(&mut app, poll_timeout);

        match action {
            tui::Action::Command(input) => {
                handle_command(
                    &input,
                    &mut conn,
                    &mut server_rx,
                    &mut app,
                    &muted,
                    &mut voice_handle,
                    &mut reconnect_attempt,
                    &mut next_reconnect,
                    &tofu,
                    &web_cmd_tx,
                    &web_state_tx,
                )
                .await;
            }
            tui::Action::Quit => break,
            tui::Action::None => {}
        }

        // Drain web UI commands
        while let Ok(cmd) = web_cmd_rx.try_recv() {
            match cmd {
                web::WebCommand::Command { text } => {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        handle_command(
                            &trimmed,
                            &mut conn,
                            &mut server_rx,
                            &mut app,
                            &muted,
                            &mut voice_handle,
                            &mut reconnect_attempt,
                            &mut next_reconnect,
                            &tofu,
                            &web_cmd_tx,
                            &web_state_tx,
                        )
                        .await;
                    }
                }
            }
        }

        // Drain server messages
        if let Some(ref mut rx) = server_rx {
            while let Ok(msg) = rx.try_recv() {
                app.dirty = true;
                match msg {
                    Some(msg) => {
                        handle_server_message(msg, &mut conn, &mut app, &muted, &mut voice_handle).await;
                    }
                    None => {
                        // Save current channel for auto-rejoin
                        if let Some(ch) = app.channel.take() {
                            app.rejoin_channel = Some(ch);
                        }
                        conn = None;
                        voice_handle.take();
                        app.voice_active = false;
                        app.participants.clear();
                        app.voice_join_params = None;
                        app.add_message("disconnected from server".into());
                        // Start auto-reconnect
                        reconnect_attempt = 0;
                        next_reconnect = Some(Instant::now() + Duration::from_millis(RECONNECT_BASE_MS));
                        app.conn_state = ConnectionState::Reconnecting { attempt: 1 };
                    }
                }
            }
        }

        // Auto-reconnect logic
        if let Some(deadline) = next_reconnect {
            if Instant::now() >= deadline {
                reconnect_attempt += 1;
                app.conn_state = ConnectionState::Reconnecting { attempt: reconnect_attempt };

                match network::connect(&app.server_addr, &tofu).await {
                    Ok((c, rx)) => {
                        handle_tofu_result(&tofu, &mut app);
                        app.add_message("reconnected!".into());
                        app.conn_state = ConnectionState::Connected;
                        if let Some(name) = app.name.clone() {
                            let _ = c.send(ClientMessage::SetName { name });
                        }
                        // Auto-rejoin previous channel
                        if let Some(ref channel_id) = app.rejoin_channel.take() {
                            app.add_message(format!("rejoining #{}...", channel_id));
                            let _ = c.send(ClientMessage::JoinChannel {
                                channel_id: channel_id.clone(),
                            });
                        }
                        conn = Some(c);
                        server_rx = Some(rx);
                        next_reconnect = None;
                        reconnect_attempt = 0;
                        save_tofu(&tofu, &app);
                    }
                    Err(_) => {
                        // Stop auto-reconnect on TOFU mismatch — user must /trust
                        if matches!(tofu.last_result(), Some(tls::TofuResult::Mismatch { .. })) {
                            if let Some(tls::TofuResult::Mismatch { expected, actual }) = tofu.last_result() {
                                app.add_message("WARNING: server certificate has changed!".into());
                                app.add_message(format!("  expected: {}", expected));
                                app.add_message(format!("  actual:   {}", actual));
                                app.add_message("use /trust to accept the new certificate".into());
                            }
                            app.conn_state = ConnectionState::Disconnected;
                            next_reconnect = None;
                        } else {
                            let delay_ms = (RECONNECT_BASE_MS * 2u64.saturating_pow(reconnect_attempt.min(5)))
                                .min(RECONNECT_MAX_MS);
                            tracing::debug!("reconnect attempt {} failed, next in {}ms", reconnect_attempt, delay_ms);
                            next_reconnect = Some(Instant::now() + Duration::from_millis(delay_ms));
                        }
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
            app.conn_state = ConnectionState::Disconnected;
            app.add_message("disconnected from server".into());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    input: &str,
    conn: &mut Option<network::ServerConnection>,
    server_rx: &mut Option<mpsc::Receiver<Option<ServerMessage>>>,
    app: &mut tui::App,
    muted: &Arc<AtomicBool>,
    voice_handle: &mut Option<voice::VoiceHandle>,
    reconnect_attempt: &mut u32,
    next_reconnect: &mut Option<Instant>,
    tofu: &TofuState,
    web_cmd_tx: &mpsc::Sender<web::WebCommand>,
    web_state_tx: &broadcast::Sender<WebState>,
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
        "/trust" => {
            tofu.remove_server(&app.server_addr);
            app.add_message(format!("cleared fingerprint for {}, reconnecting...", app.server_addr));
            save_tofu(tofu, app);
            do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
            return;
        }
        "/server" | "/s" => {
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
                    if app.conn_state.is_disconnected() {
                        do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
                    }
                } else {
                    app.add_message(format!("current server: {}", app.server_addr));
                }
            } else {
                app.add_message(format!("current server: {}", app.server_addr));
            }
            return;
        }
        "/reconnect" | "/r" => {
            do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
            return;
        }
        "/config" | "/cfg" => {
            handle_config(parts.get(1).copied(), app, muted, voice_handle).await;
            return;
        }
        "/name" | "/n" => {
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
        "/web" => {
            if let Some(port_str) = parts.get(1).map(|s| s.trim()) {
                if let Ok(port) = port_str.parse::<u16>() {
                    let tx = web_cmd_tx.clone();
                    let state_tx = web_state_tx.clone();
                    tokio::spawn(web::start_web_server(port, tx, state_tx));
                    app.add_message(format!("web UI at http://127.0.0.1:{}", port));
                } else {
                    app.add_message("usage: /web <port>".into());
                }
            } else {
                app.add_message("usage: /web <port>".into());
            }
            return;
        }
        "/quit" | "/exit" | "/q" => {
            app.should_quit = true;
            return;
        }
        "/help" | "/h" => {
            app.add_message("── commands (short aliases in parens) ──".into());
            app.add_message("  /config (/cfg)     list audio config".into());
            app.add_message("  /config input <N>  select input device".into());
            app.add_message("  /config output <N> select output device".into());
            app.add_message("  /config gain <0-200> mic gain (default 100)".into());
            app.add_message("  /config vol <0-200>  incoming volume (default 100)".into());
            app.add_message("  /config vad <0-100>  vad level (0/off = disable, default 10)".into());
            app.add_message("  /create (/c)       create a new channel".into());
            app.add_message("  /connect (/j) <id> join a channel (or just #<id>)".into());
            app.add_message("  /leave (/l)        leave current channel".into());
            app.add_message("  /mute (/m)         toggle mute".into());
            app.add_message("  /name (/n) <name>  set your display name".into());
            app.add_message("  /server (/s) <ip>  set server address".into());
            app.add_message("  /reconnect (/r)    reconnect to server".into());
            app.add_message("  /web <port>        start web UI on port".into());
            app.add_message("  /quit (/q)         exit".into());
            app.add_message("  <text>             send chat message".into());
            app.add_message("── keys ──".into());
            app.add_message("  Up/Down            command history".into());
            app.add_message("  Ctrl+C / Esc       quit".into());
            return;
        }
        _ => {}
    }

    // Commands that require a connection
    if app.conn_state.is_disconnected() {
        app.add_message("not connected — use /server <ip> and /reconnect".into());
        return;
    }

    match parts[0] {
        "/create" | "/c" => {
            app.add_message("> /create".into());
            send_or_disconnect(conn, app, ClientMessage::CreateChannel);
        }
        "/connect" | "/join" | "/j" => {
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
        "/leave" | "/l" => {
            app.add_message("> /leave".into());
            send_or_disconnect(conn, app, ClientMessage::LeaveChannel);
            *voice_handle = None;
            app.voice_active = false;
            app.channel = None;
            app.participants.clear();
            app.voice_join_params = None;
            app.rejoin_channel = None;
        }
        _ => {
            if let Some(id) = input.strip_prefix('#') {
                let id = id.trim();
                if !id.is_empty() {
                    app.add_message(format!("> /connect {}", id));
                    send_or_disconnect(conn, app, ClientMessage::JoinChannel {
                        channel_id: id.to_string(),
                    });
                } else {
                    app.add_message("usage: #<channel_id>".into());
                }
            } else if !input.starts_with('/') {
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
    server_rx: &mut Option<mpsc::Receiver<Option<ServerMessage>>>,
    app: &mut tui::App,
    reconnect_attempt: &mut u32,
    next_reconnect: &mut Option<Instant>,
    tofu: &TofuState,
) {
    // Stop auto-reconnect if running
    *next_reconnect = None;
    *reconnect_attempt = 0;

    // Drop old connection
    conn.take();
    server_rx.take();
    app.voice_active = false;
    app.channel = None;
    app.participants.clear();
    app.voice_join_params = None;

    app.conn_state = ConnectionState::Reconnecting { attempt: 1 };
    app.add_message(format!("connecting to {}...", app.server_addr));

    match network::connect(&app.server_addr, tofu).await {
        Ok((c, rx)) => {
            handle_tofu_result(tofu, app);
            app.add_message("connected!".into());
            app.conn_state = ConnectionState::Connected;
            if let Some(name) = app.name.clone() {
                let _ = c.send(ClientMessage::SetName { name });
            }
            // Auto-rejoin if we had a channel
            if let Some(ref channel_id) = app.rejoin_channel.take() {
                app.add_message(format!("rejoining #{}...", channel_id));
                let _ = c.send(ClientMessage::JoinChannel {
                    channel_id: channel_id.clone(),
                });
            }
            *conn = Some(c);
            *server_rx = Some(rx);
            save_tofu(tofu, app);
        }
        Err(e) => {
            // Check if it was a TOFU mismatch
            if let Some(tls::TofuResult::Mismatch { expected, actual }) = tofu.last_result() {
                app.add_message("WARNING: server certificate has changed!".into());
                app.add_message(format!("  expected: {}", expected));
                app.add_message(format!("  actual:   {}", actual));
                app.add_message("use /trust to accept the new certificate".into());
            }
            app.conn_state = ConnectionState::Disconnected;
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
        ServerMessage::Welcome { version, protocol } => {
            let my_version = env!("CARGO_PKG_VERSION");
            let my_protocol = config::PROTOCOL_VERSION;
            if protocol != my_protocol {
                app.add_message(format!(
                    "WARNING: protocol mismatch! server={} (v{}) client={} (v{})",
                    protocol, version, my_protocol, my_version
                ));
                app.add_message("please update your client to match the server".into());
            } else if version != my_version {
                app.add_message(format!(
                    "server version {} (you have {}), consider updating",
                    version, my_version
                ));
            }
        }
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
            let my_name = app.name.clone().unwrap_or_else(|| "user".into());
            match voice::start_voice(
                &app.server_addr,
                channel_id,
                udp_token,
                muted.clone(),
                voice_key,
                app.input_device.clone(),
                app.output_device.clone(),
                app.vad_threshold.clone(),
                app.input_gain.clone(),
                app.output_vol.clone(),
                my_name,
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
            app.rejoin_channel = None;
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
        app.add_message("── volume ──".into());
        let gain = f32::from_bits(app.input_gain.load(Ordering::Relaxed));
        let vol = f32::from_bits(app.output_vol.load(Ordering::Relaxed));
        app.add_message(format!("  gain (mic): {}%", percent_from_vol(gain)));
        app.add_message(format!("  vol (recv): {}%", percent_from_vol(vol)));
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
        "gain" => {
            let gain = app.input_gain.clone();
            if handle_config_vol(index_str, &gain, "gain (mic)", app) {
                save_settings(app);
            }
        }
        "vol" => {
            let vol = app.output_vol.clone();
            if handle_config_vol(index_str, &vol, "vol (recv)", app) {
                save_settings(app);
            }
        }
        _ => {
            app.add_message("usage: /config [input|output|gain|vol|vad] <value>".into());
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

fn vol_from_percent(pct: u32) -> f32 {
    pct as f32 / 100.0
}

fn percent_from_vol(vol: f32) -> u32 {
    (vol * 100.0).round() as u32
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

fn handle_config_vol(value: &str, atomic: &Arc<AtomicU32>, label: &str, app: &mut tui::App) -> bool {
    if value.is_empty() {
        let current = f32::from_bits(atomic.load(Ordering::Relaxed));
        app.add_message(format!("{}: {}%", label, percent_from_vol(current)));
        return false;
    }
    match value.parse::<u32>() {
        Ok(pct @ 0..=200) => {
            let vol = vol_from_percent(pct);
            atomic.store(vol.to_bits(), Ordering::Relaxed);
            app.add_message(format!("{} set to {}%", label, pct));
            true
        }
        _ => {
            app.add_message(format!("usage: /config {} <0-200> (default 100)", label.split_whitespace().next().unwrap_or(label)));
            false
        }
    }
}

fn handle_tofu_result(tofu: &TofuState, app: &mut tui::App) {
    if let Some(result) = tofu.last_result() {
        match result {
            tls::TofuResult::TrustedNew(fp) => {
                app.add_message(format!("new server fingerprint: {}", fp));
            }
            tls::TofuResult::TrustedKnown => {}
            tls::TofuResult::Mismatch { expected, actual } => {
                app.add_message("WARNING: server certificate has changed!".into());
                app.add_message(format!("  expected: {}", expected));
                app.add_message(format!("  actual:   {}", actual));
            }
        }
    }
}

fn save_tofu(tofu: &TofuState, app: &tui::App) {
    let trusted = tofu.trusted_map();
    if !trusted.is_empty() {
        let mut s = build_settings(app);
        s.trusted_servers = trusted;
        s.save();
    }
}

fn save_settings(app: &tui::App) {
    build_settings(app).save();
}

fn build_settings(app: &tui::App) -> settings::UserSettings {
    let default_server = format!("127.0.0.1:{}", config::TCP_PORT);
    let threshold = f32::from_bits(app.vad_threshold.load(Ordering::Relaxed));
    let level = vad_level_from_threshold(threshold);
    let default_level = vad_level_from_threshold(config::VAD_RMS_THRESHOLD);
    let gain_pct = percent_from_vol(f32::from_bits(app.input_gain.load(Ordering::Relaxed)));
    let vol_pct = percent_from_vol(f32::from_bits(app.output_vol.load(Ordering::Relaxed)));

    settings::UserSettings {
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
        input_gain: if gain_pct != 100 { Some(gain_pct) } else { None },
        output_vol: if vol_pct != 100 { Some(vol_pct) } else { None },
        name: app.name.clone(),
        trusted_servers: Default::default(),
    }
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

    let my_name = app.name.clone().unwrap_or_else(|| "user".into());
    match voice::start_voice(
        &app.server_addr,
        params.channel_id,
        params.udp_token,
        muted.clone(),
        params.voice_key,
        app.input_device.clone(),
        app.output_device.clone(),
        app.vad_threshold.clone(),
        app.input_gain.clone(),
        app.output_vol.clone(),
        my_name,
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
