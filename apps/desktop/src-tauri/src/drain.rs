//! Background task that drains incoming server messages and:
//!   1) updates AppCore where state has shifted (channel join/leave),
//!   2) starts/stops the voice handle on join/leave,
//!   3) emits Tauri events to the frontend.
//!
//! Also owns automatic reconnect: when the underlying TCP/TLS connection drops
//! the task tears down channel + voice and retries `network::connect` with a
//! 5s, 5s, then 10s-forever backoff. The user-initiated `disconnect` command
//! aborts this task via its `JoinHandle`, so explicit disconnects never
//! reconnect on their own.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tc_client::network;
use tc_shared::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::events::{
    emit, ChannelListEntry, ChannelListPayload, ChatPayload, ConnState, DmPayload, ErrorPayload,
    JoinedPayload, PeerEvent,
};
use crate::history::{self, ChatLine, LineKind};
use crate::notify;
use crate::state::{AppCore, PendingJoin};
use crate::voice_actor::StartParams;

/// Show OS notification only when notifications are enabled AND the main window
/// is not focused (avoid spamming the user when they're already looking at it).
async fn maybe_notify(app: &AppHandle, core: &Arc<Mutex<AppCore>>, title: &str, body: &str) {
    if !core.lock().await.notifications {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        if w.is_focused().unwrap_or(false) {
            return;
        }
    }
    notify::show(app, title, body);
}

pub fn spawn(
    app: AppHandle,
    core: Arc<Mutex<AppCore>>,
    rx: mpsc::Receiver<Option<ServerMessage>>,
    server_addr: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(app, core, rx, server_addr))
}

/// Top-level loop: drain → on disconnect, reconnect with backoff → drain again.
///
/// We exit only when `tokio::task::JoinHandle::abort` is called from
/// `commands::connect` (user switching servers) or `commands::disconnect`
/// (user-initiated). In both cases the caller already cleared state, so
/// returning from here is the only thing left.
async fn run(
    app: AppHandle,
    core: Arc<Mutex<AppCore>>,
    mut rx: mpsc::Receiver<Option<ServerMessage>>,
    server_addr: String,
) {
    loop {
        // Drain messages until the underlying connection signals disconnect.
        drain_until_disconnect(&app, &core, &server_addr, &mut rx).await;

        // Connection lost. Tear down channel + voice; keep server_addr around
        // so we can reconnect to the same target.
        tear_down_session(&core).await;
        tracing::info!("server disconnected, will attempt reconnect");

        // Backoff schedule: 5s, 5s, then 10s forever (until success or abort).
        let new_rx = match reconnect_loop(&app, &core, &server_addr).await {
            Some(rx) => rx,
            None => return, // unreachable today: we never give up
        };
        rx = new_rx;
    }
}

/// Inner message loop. Returns when the receiver yields `None` (disconnect)
/// or its sender is dropped.
async fn drain_until_disconnect(
    app: &AppHandle,
    core: &Arc<Mutex<AppCore>>,
    server_addr: &str,
    rx: &mut mpsc::Receiver<Option<ServerMessage>>,
) {
    while let Some(maybe_msg) = rx.recv().await {
        let Some(msg) = maybe_msg else {
            return;
        };
        handle(app, core, server_addr, msg).await;
    }
}

/// Clear channel + voice and drop the dead `ServerConnection`.
/// `server_addr` stays in core so reconnect knows where to dial.
async fn tear_down_session(core: &Arc<Mutex<AppCore>>) {
    let voice = {
        let mut c = core.lock().await;
        c.conn = None;
        c.channel = None;
        c.pending_join = None;
        c.voice.clone()
    };
    voice.stop().await;
}

/// Retry connecting to `server_addr` until success.
/// Schedule: 5s, 5s, then 10s indefinitely.
/// Aborting the JoinHandle is the only way out — we never give up on our own.
async fn reconnect_loop(
    app: &AppHandle,
    core: &Arc<Mutex<AppCore>>,
    server_addr: &str,
) -> Option<mpsc::Receiver<Option<ServerMessage>>> {
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        let delay_secs = backoff_seconds(attempt);

        emit(
            app,
            "connection_state",
            ConnState::Reconnecting {
                attempt,
                delay_secs,
            },
        );
        tokio::time::sleep(Duration::from_secs(delay_secs)).await;

        let (pubkey, tofu) = {
            let c = core.lock().await;
            (c.pubkey(), c.tofu.clone())
        };

        match network::connect(server_addr, &tofu, pubkey).await {
            Ok((conn, new_rx)) => {
                // Restore the saved name so the server's view of us is consistent.
                let saved_name = {
                    let mut c = core.lock().await;
                    c.conn = Some(conn);
                    c.name.clone()
                };
                if let Some(name) = saved_name {
                    if let Some(conn) = core.lock().await.conn.as_ref() {
                        let _ = conn.send(ClientMessage::SetName { name });
                    }
                }
                tracing::info!(attempt, "reconnected");
                return Some(new_rx);
            }
            Err(e) => {
                tracing::warn!(attempt, delay_secs, "reconnect failed: {:#}", e);
                // Loop and try again with the next backoff slot.
            }
        }
    }
}

/// 5s for the first two attempts, 10s thereafter.
fn backoff_seconds(attempt: u32) -> u64 {
    if attempt <= 2 {
        5
    } else {
        10
    }
}

async fn handle(
    app: &AppHandle,
    core: &Arc<Mutex<AppCore>>,
    server_addr: &str,
    msg: ServerMessage,
) {
    match msg {
        ServerMessage::Welcome { version, protocol } => {
            tracing::info!(%version, %protocol, "server welcome");
            emit(
                app,
                "connection_state",
                ConnState::Connected {
                    server: server_addr.to_string(),
                },
            );
        }
        ServerMessage::ChannelCreated { channel_id } => {
            // Auto-join freshly created channel.
            if let Some(conn) = core.lock().await.conn.as_ref() {
                let _ = conn.send(tc_shared::ClientMessage::JoinChannel {
                    channel_id: channel_id.clone(),
                });
            }
        }
        ServerMessage::JoinedChannel {
            channel_id,
            participants,
            udp_token,
            voice_key,
        } => {
            // Stash join params, remember the channel in the server registry,
            // and start voice in a separate task (so we don't hold the AppCore
            // lock across voice startup).
            //
            // Self-rename safety net: if we have a local name set but the
            // server's participant list doesn't include it (because SetName
            // raced after the auto-join inside the new channel), re-send
            // SetName now. The server will broadcast NameChanged across the
            // channel and the UI will pick up the correction.
            let resend_name = {
                let mut c = core.lock().await;
                c.channel = Some(channel_id.clone());
                let addr = c.server_addr.clone();
                if let Some(addr) = addr {
                    crate::server_registry::record_channel(&mut c.servers, &addr, &channel_id);
                    c.save();
                }
                c.pending_join = Some(PendingJoin {
                    channel_id: channel_id.clone(),
                    udp_token,
                    voice_key: voice_key.clone(),
                });
                // Default-mute on every channel join so the user never bursts
                // into a new room with a hot mic. They can unmute explicitly.
                c.muted.store(true, Ordering::Relaxed);
                match (c.name.clone(), c.conn.as_ref()) {
                    (Some(name), Some(_)) if !participants.contains(&name) => Some(name),
                    _ => None,
                }
            };
            if let Some(name) = resend_name {
                if let Some(conn) = core.lock().await.conn.as_ref() {
                    let _ = conn.send(tc_shared::ClientMessage::SetName { name });
                }
            }
            emit(
                app,
                "joined_channel",
                JoinedPayload {
                    channel_id: channel_id.clone(),
                    participants,
                },
            );

            spawn_voice(app.clone(), core.clone()).await;
        }
        ServerMessage::PeerJoined { peer_name } => {
            maybe_notify(app, core, "tc_", &format!("{} joined", peer_name)).await;
            emit(app, "peer_joined", PeerEvent { name: peer_name });
        }
        ServerMessage::PeerLeft { peer_name } => {
            emit(app, "peer_left", PeerEvent { name: peer_name });
        }
        ServerMessage::LeftChannel => {
            {
                let mut c = core.lock().await;
                c.channel = None;
                c.voice.stop().await;
            }
            emit(app, "left_channel", serde_json::json!({}));
        }
        ServerMessage::ChatMessage { from, text } => {
            // Record to history while we still have server+channel context.
            {
                let c = core.lock().await;
                if let (Some(addr), Some(channel)) = (c.server_addr.clone(), c.channel.clone()) {
                    history::append(
                        &addr,
                        &channel,
                        &ChatLine {
                            ts_unix: history::unix_now(),
                            kind: LineKind::Chat,
                            from: from.clone(),
                            text: text.clone(),
                        },
                    );
                }
            }
            maybe_notify(app, core, &from, &text).await;
            emit(app, "chat_message", ChatPayload { from, text });
        }
        ServerMessage::DirectMessage {
            from_pubkey,
            from_name,
            text,
        } => {
            let pk_hex = hex::encode(&from_pubkey);
            {
                let mut c = core.lock().await;
                if let Some(addr) = c.server_addr.clone() {
                    history::append(
                        &addr,
                        &format!("@dm:{}", pk_hex),
                        &ChatLine {
                            ts_unix: history::unix_now(),
                            kind: LineKind::Dm,
                            from: from_name.clone(),
                            text: text.clone(),
                        },
                    );
                }
                crate::dm::touch(&mut c.dm_peers, &pk_hex, &from_name);
                c.save();
            }
            maybe_notify(app, core, &format!("DM · {}", from_name), &text).await;
            emit(
                app,
                "direct_message",
                DmPayload {
                    from_pubkey: pk_hex,
                    from_name,
                    text,
                },
            );
        }
        ServerMessage::NameChanged { old_name, new_name } => {
            emit(
                app,
                "name_changed",
                serde_json::json!({ "old_name": old_name, "new_name": new_name }),
            );
        }
        ServerMessage::ChannelList { channels } => {
            let payload = ChannelListPayload {
                channels: channels
                    .into_iter()
                    .map(|c| ChannelListEntry {
                        channel_id: c.channel_id,
                        participant_count: c.participant_count,
                    })
                    .collect(),
            };
            emit(app, "channel_list", payload);
        }
        ServerMessage::EchoTestReady {
            channel_id,
            udp_token,
            voice_key,
        } => {
            spawn_echo(app.clone(), core.clone(), channel_id, udp_token, voice_key).await;
        }
        ServerMessage::Error { message } => {
            emit(app, "error", ErrorPayload { message });
        }
        ServerMessage::Pong => {}
    }
}

/// Start a sound-check voice session and arm the backend-owned timer that ends
/// it after [`ECHO_TEST_DURATION_SECS`], samples the result, and emits it.
/// The frontend is purely reactive: `echo_test_started` → countdown,
/// `echo_test_result` → verdict.
async fn spawn_echo(
    app: AppHandle,
    core: Arc<Mutex<AppCore>>,
    channel_id: String,
    udp_token: u64,
    voice_key: Vec<u8>,
) {
    let (params, voice) = {
        let c = core.lock().await;
        let Some(server_ip) = c.conn.as_ref().map(|conn| conn.peer_addr().ip()) else {
            tracing::warn!("echo test skipped: no live connection");
            return;
        };
        let params = StartParams {
            server_ip,
            channel_id,
            udp_token,
            voice_key,
            // Force the mic live for the test regardless of mute/PTT, but keep
            // VAD as the user configured it — the check exercises that too.
            muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vad_threshold: c.vad_threshold.clone(),
            input_gain: c.input_gain.clone(),
            output_vol: c.output_vol.clone(),
            input_device: c.input_device.clone(),
            output_device: c.output_device.clone(),
            sender_name: c.name.clone().unwrap_or_else(|| "anon".to_string()),
            echo_test: true,
        };
        (params, c.voice.clone())
    };

    if let Err(e) = voice.start_echo(params).await {
        tracing::error!("echo test start failed: {}", e);
        emit(
            &app,
            "error",
            ErrorPayload {
                message: format!("sound check failed: {}", e),
            },
        );
        return;
    }
    emit(&app, "echo_test_started", serde_json::json!({}));

    // Backend owns the timing so the test always tears itself down (server
    // channel + client handle) even if the settings panel is closed early.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(
            tc_shared::config::ECHO_TEST_DURATION_SECS,
        ))
        .await;
        let result = voice.stop_echo().await;
        if let Some(conn) = core.lock().await.conn.as_ref() {
            let _ = conn.send(ClientMessage::StopEchoTest);
        }
        emit(&app, "echo_test_result", result);
    });
}

/// Forward a `StartParams` to the voice actor using params stashed in `pending_join`.
async fn spawn_voice(app: AppHandle, core: Arc<Mutex<AppCore>>) {
    let (params, voice) = {
        let mut c = core.lock().await;
        let Some(p) = c.pending_join.take() else {
            return;
        };
        // UDP must target the same IP the TCP session uses, or the server
        // rejects the hello (source-IP check) and inbound voice never starts.
        let Some(server_ip) = c.conn.as_ref().map(|conn| conn.peer_addr().ip()) else {
            tracing::warn!("voice start skipped: no live connection");
            return;
        };
        let params = StartParams {
            server_ip,
            channel_id: p.channel_id.clone(),
            udp_token: p.udp_token,
            voice_key: p.voice_key,
            muted: c.muted.clone(),
            vad_threshold: c.vad_threshold.clone(),
            input_gain: c.input_gain.clone(),
            output_vol: c.output_vol.clone(),
            input_device: c.input_device.clone(),
            output_device: c.output_device.clone(),
            sender_name: c.name.clone().unwrap_or_else(|| "anon".to_string()),
            echo_test: false,
        };
        (params, c.voice.clone())
    };

    if let Err(e) = voice.start(params).await {
        tracing::error!("voice start failed: {}", e);
        emit(
            &app,
            "error",
            ErrorPayload {
                message: format!("voice failed: {}", e),
            },
        );
    }
}
