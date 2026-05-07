use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use tokio::sync::{broadcast, mpsc};

use tc_client::{audio, identity, network, settings, tls, tui, voice, web};
use tc_shared::config;
use tc_shared::{ClientMessage, ServerMessage};
use tls::TofuState;
use tui::ConnectionState;
use web::WebState;

#[derive(Parser)]
#[command(name = "tc-client", about = "tc voice chat client")]
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
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

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
    let local_identity = match identity::Identity::load_or_generate() {
        Ok(id) => {
            tracing::info!(fingerprint = %id.fingerprint(), "loaded local identity");
            Some(id)
        }
        Err(e) => {
            tracing::warn!("identity unavailable, continuing anonymously: {:#}", e);
            None
        }
    };
    let local_pubkey: Option<Vec<u8>> = local_identity.as_ref().map(|i| i.pubkey().to_vec());
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

    match network::connect(&server_addr, &tofu, local_pubkey.clone()).await {
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
        Err(e) => {
            handle_tofu_result(&tofu, &mut app);
            app.conn_state = ConnectionState::Disconnected;
            app.add_message(format!("connection failed: {:#}", e));
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

                match network::connect(&app.server_addr, &tofu, local_pubkey.clone()).await {
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
                        let delay_ms = (RECONNECT_BASE_MS * 2u64.saturating_pow(reconnect_attempt.min(5)))
                            .min(RECONNECT_MAX_MS);
                        tracing::debug!("reconnect attempt {} failed, next in {}ms", reconnect_attempt, delay_ms);
                        next_reconnect = Some(Instant::now() + Duration::from_millis(delay_ms));
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
    let arg = parts.get(1).copied();

    // Commands that work without a connection
    match parts[0] {
        "/matrix" => return cmd_matrix(app),
        "/trust" => {
            return cmd_trust(app, tofu, conn, server_rx, reconnect_attempt, next_reconnect).await;
        }
        "/server" | "/s" => {
            return cmd_server(app, arg, conn, server_rx, reconnect_attempt, next_reconnect, tofu).await;
        }
        "/reconnect" | "/r" => {
            do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
            return;
        }
        "/config" | "/cfg" => {
            handle_config(arg, app, muted, voice_handle).await;
            return;
        }
        "/name" | "/n" => return cmd_name(app, arg, conn),
        "/mute" | "/m" => return cmd_mute(app, muted),
        "/web" => return cmd_web(app, arg, web_cmd_tx, web_state_tx),
        "/quit" | "/exit" | "/q" => {
            app.should_quit = true;
            return;
        }
        "/help" | "/h" => return cmd_help(app),
        _ => {}
    }

    // Commands that require a connection
    if app.conn_state.is_disconnected() {
        app.add_message("not connected — use /server <ip> and /reconnect".into());
        return;
    }

    match parts[0] {
        "/create" | "/c" => cmd_create(app, arg, conn),
        "/list" | "/ls" => cmd_list(app, conn),
        "/connect" | "/join" | "/j" => cmd_connect(app, arg, conn),
        "/leave" | "/l" => cmd_leave(app, conn, voice_handle),
        _ => cmd_default(app, input, conn, parts[0]),
    }
}

fn cmd_matrix(app: &mut tui::App) {
    app.matrix_mode = !app.matrix_mode;
    if app.matrix_mode {
        app.matrix_state = tui::MatrixState::new();
    }
}

async fn cmd_trust(
    app: &mut tui::App,
    tofu: &TofuState,
    conn: &mut Option<network::ServerConnection>,
    server_rx: &mut Option<mpsc::Receiver<Option<ServerMessage>>>,
    reconnect_attempt: &mut u32,
    next_reconnect: &mut Option<Instant>,
) {
    tofu.remove_server(&app.server_addr);
    app.add_message(format!("cleared fingerprint for {}, reconnecting...", app.server_addr));
    save_tofu(tofu, app);
    do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
}

async fn cmd_server(
    app: &mut tui::App,
    arg: Option<&str>,
    conn: &mut Option<network::ServerConnection>,
    server_rx: &mut Option<mpsc::Receiver<Option<ServerMessage>>>,
    reconnect_attempt: &mut u32,
    next_reconnect: &mut Option<Instant>,
    tofu: &TofuState,
) {
    let addr = arg.map(str::trim).filter(|s| !s.is_empty());
    let Some(addr) = addr else {
        app.add_message(format!("current server: {}", app.server_addr));
        return;
    };
    app.server_addr = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{}:{}", addr, config::TCP_PORT)
    };
    save_settings(app);
    app.add_message(format!("server address set to {}", app.server_addr));
    if app.conn_state.is_disconnected() {
        do_reconnect(conn, server_rx, app, reconnect_attempt, next_reconnect, tofu).await;
    }
}

fn cmd_name(app: &mut tui::App, arg: Option<&str>, conn: &mut Option<network::ServerConnection>) {
    let Some(new_name) = arg.map(sanitize_name).filter(|n| !n.is_empty()) else {
        app.add_message("usage: /name <name>".into());
        return;
    };
    app.add_message(format!("> /name {}", new_name));
    send_or_disconnect(conn, app, ClientMessage::SetName { name: new_name.clone() });
    app.name = Some(new_name);
    save_settings(app);
}

fn cmd_mute(app: &mut tui::App, muted: &Arc<AtomicBool>) {
    let was_muted = muted.fetch_xor(true, Ordering::Relaxed);
    app.add_message(if was_muted { "unmuted".into() } else { "muted".into() });
}

fn cmd_web(
    app: &mut tui::App,
    arg: Option<&str>,
    web_cmd_tx: &mpsc::Sender<web::WebCommand>,
    web_state_tx: &broadcast::Sender<WebState>,
) {
    let port = arg.map(str::trim).and_then(|s| s.parse::<u16>().ok());
    let Some(port) = port else {
        app.add_message("usage: /web <port>".into());
        return;
    };
    tokio::spawn(web::start_web_server(port, web_cmd_tx.clone(), web_state_tx.clone()));
    app.add_message(format!("web UI at http://127.0.0.1:{}", port));
}

fn cmd_help(app: &mut tui::App) {
    for line in [
        "── commands (short aliases in parens) ──",
        "  /config (/cfg)     list audio config",
        "  /config input <N>  select input device",
        "  /config output <N> select output device",
        "  /config gain <0-200> mic gain (default 100)",
        "  /config vol <0-200>  incoming volume (default 100)",
        "  /config vad <0-100>  vad level (0/off = disable, default 10)",
        "  /create (/c)       create a private channel",
        "  /create (/c) <name> create a public channel",
        "  /list (/ls)        list public channels",
        "  /connect (/j) <id> join a channel (or just #<id>)",
        "  /leave (/l)        leave current channel",
        "  /mute (/m)         toggle mute",
        "  /name (/n) <name>  set your display name",
        "  /server (/s) <ip>  set server address",
        "  /reconnect (/r)    reconnect to server",
        "  /web <port>        start web UI on port",
        "  /quit (/q)         exit",
        "  <text>             send chat message",
        "── keys ──",
        "  Up/Down            command history",
        "  Ctrl+C / Esc       quit",
    ] {
        app.add_message(line.into());
    }
}

fn cmd_create(app: &mut tui::App, arg: Option<&str>, conn: &mut Option<network::ServerConnection>) {
    let raw = arg.map(str::trim).filter(|s| !s.is_empty());
    let Some(raw_name) = raw else {
        app.add_message("> /create".into());
        send_or_disconnect(conn, app, ClientMessage::CreateChannel { name: None });
        return;
    };
    let name = sanitize_channel_name(raw_name);
    if name.is_empty() {
        app.add_message("channel name must contain [a-z0-9-]".into());
    } else if name.len() > config::MAX_CHANNEL_NAME_LEN {
        app.add_message(format!("channel name too long (max {})", config::MAX_CHANNEL_NAME_LEN));
    } else {
        app.add_message(format!("> /create {}", name));
        send_or_disconnect(conn, app, ClientMessage::CreateChannel { name: Some(name) });
    }
}

fn cmd_list(app: &mut tui::App, conn: &mut Option<network::ServerConnection>) {
    app.add_message("> /list".into());
    send_or_disconnect(conn, app, ClientMessage::ListChannels);
}

fn cmd_connect(app: &mut tui::App, arg: Option<&str>, conn: &mut Option<network::ServerConnection>) {
    let id = arg.map(str::trim).filter(|s| !s.is_empty());
    let Some(id) = id else {
        app.add_message("usage: /connect <channel_id>".into());
        return;
    };
    app.add_message(format!("> /connect {}", id));
    send_or_disconnect(conn, app, ClientMessage::JoinChannel { channel_id: id.to_string() });
}

fn cmd_leave(
    app: &mut tui::App,
    conn: &mut Option<network::ServerConnection>,
    voice_handle: &mut Option<voice::VoiceHandle>,
) {
    app.add_message("> /leave".into());
    send_or_disconnect(conn, app, ClientMessage::LeaveChannel);
    *voice_handle = None;
    app.voice_active = false;
    app.channel = None;
    app.participants.clear();
    app.voice_join_params = None;
    app.rejoin_channel = None;
}

fn cmd_default(
    app: &mut tui::App,
    input: &str,
    conn: &mut Option<network::ServerConnection>,
    head: &str,
) {
    if let Some(id) = input.strip_prefix('#') {
        let id = id.trim();
        if id.is_empty() {
            app.add_message("usage: #<channel_id>".into());
        } else {
            app.add_message(format!("> /connect {}", id));
            send_or_disconnect(conn, app, ClientMessage::JoinChannel { channel_id: id.to_string() });
        }
    } else if !input.starts_with('/') {
        send_or_disconnect(conn, app, ClientMessage::ChatMessage { text: input.to_string() });
        app.add_message(format!("you: {}", input));
    } else {
        app.add_message(format!("unknown command: {} (try /help)", head));
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

    let pk = identity::Identity::load_or_generate().ok().map(|i| i.pubkey().to_vec());
    match network::connect(&app.server_addr, tofu, pk).await {
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
            app.conn_state = ConnectionState::Disconnected;
            app.add_message(format!("connection failed: {:#}", e));
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
        ServerMessage::DirectMessage { from_pubkey: _, from_name, text } => {
            app.add_message(format!("(dm) {}: {}", from_name, text));
        }
        ServerMessage::ChannelList { channels } => {
            if channels.is_empty() {
                app.add_message("no public channels".into());
            } else {
                app.add_message("── public channels ──".into());
                for ch in channels {
                    app.add_message(format!("  {} ({} users)", ch.channel_id, ch.participant_count));
                }
            }
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

/// Sanitize a public channel name: lowercase, keep only [a-z0-9-].
fn sanitize_channel_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
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
                app.add_message("server certificate changed (auto-trusted)".into());
                app.add_message(format!("  old: {}", expected));
                app.add_message(format!("  new: {}", actual));
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
        voice_mode: None,
        hotkeys: Default::default(),
        notifications: None,
        autostart: None,
        close_to_tray: None,
        language: None,
        trusted_servers: Default::default(),
        servers: Vec::new(),
        dm_peers: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_level_roundtrip() {
        for level in [0, 1, 10, 50, 100] {
            let threshold = vad_threshold_from_level(level);
            let back = vad_level_from_threshold(threshold);
            assert_eq!(back, level);
        }
    }

    #[test]
    fn vol_roundtrip() {
        for pct in [0, 50, 100, 150, 200] {
            let vol = vol_from_percent(pct);
            let back = percent_from_vol(vol);
            assert_eq!(back, pct);
        }
    }

    #[test]
    fn sanitize_name_strips_control_chars() {
        assert_eq!(sanitize_name("hello\nworld"), "helloworld");
        assert_eq!(sanitize_name("  alice\t "), "alice");
        assert_eq!(sanitize_name("bob"), "bob");
    }

    #[test]
    fn sanitize_channel_name_valid() {
        assert_eq!(sanitize_channel_name("My Channel!"), "mychannel");
        assert_eq!(sanitize_channel_name("test-room"), "test-room");
        assert_eq!(sanitize_channel_name("--edge--"), "edge");
        assert_eq!(sanitize_channel_name("UPPER123"), "upper123");
    }

    #[test]
    fn sanitize_name_empty_and_whitespace() {
        assert_eq!(sanitize_name(""), "");
        assert_eq!(sanitize_name("   "), "");
        assert_eq!(sanitize_name("\n\r\t"), "");
    }

    #[test]
    fn sanitize_name_preserves_unicode() {
        assert_eq!(sanitize_name("Денис"), "Денис");
        assert_eq!(sanitize_name("  日本語  "), "日本語");
    }

    #[test]
    fn sanitize_channel_name_empty() {
        assert_eq!(sanitize_channel_name(""), "");
        assert_eq!(sanitize_channel_name("!!!"), "");
        assert_eq!(sanitize_channel_name("---"), "");
    }

    #[test]
    fn sanitize_channel_name_unicode_stripped() {
        assert_eq!(sanitize_channel_name("канал-1"), "1");
        assert_eq!(sanitize_channel_name("café"), "caf");
    }

    #[test]
    fn vad_boundary_values() {
        assert_eq!(vad_level_from_threshold(0.0), 0);
        assert_eq!(vad_level_from_threshold(0.1), 100);
        assert_eq!(vad_level_from_threshold(0.001), 1);
        assert_eq!(vad_threshold_from_level(0), 0.0);
        assert_eq!(vad_threshold_from_level(1), 0.001);
        assert_eq!(vad_threshold_from_level(100), 0.1);
    }

    #[test]
    fn vol_boundary_values() {
        assert_eq!(vol_from_percent(0), 0.0);
        assert_eq!(vol_from_percent(100), 1.0);
        assert_eq!(vol_from_percent(200), 2.0);
        assert_eq!(percent_from_vol(0.0), 0);
        assert_eq!(percent_from_vol(1.0), 100);
        assert_eq!(percent_from_vol(2.0), 200);
    }
}
