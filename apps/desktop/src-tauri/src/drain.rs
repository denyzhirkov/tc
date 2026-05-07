//! Background task that drains incoming server messages and:
//!   1) updates AppCore where state has shifted (channel join/leave),
//!   2) starts/stops the voice handle on join/leave,
//!   3) emits Tauri events to the frontend.

use std::sync::Arc;

use tauri::{AppHandle, Manager};
use tc_shared::ServerMessage;
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
    mut rx: mpsc::Receiver<Option<ServerMessage>>,
    server_addr: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(maybe_msg) = rx.recv().await {
            let Some(msg) = maybe_msg else {
                // Disconnect: writer task returned None.
                {
                    let mut c = core.lock().await;
                    c.conn = None;
                    c.channel = None;
                    c.voice.stop().await;
                }
                emit(&app, "connection_state", ConnState::Disconnected);
                tracing::info!("server disconnected");
                break;
            };
            handle(&app, &core, &server_addr, msg).await;
        }
    })
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
                ConnState::Connected { server: server_addr.to_string() },
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
                JoinedPayload { channel_id: channel_id.clone(), participants },
            );

            spawn_voice(app.clone(), core.clone(), server_addr.to_string()).await;
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
        ServerMessage::DirectMessage { from_pubkey, from_name, text } => {
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
        ServerMessage::Error { message } => {
            emit(app, "error", ErrorPayload { message });
        }
        ServerMessage::Pong => {}
    }
}

/// Forward a `StartParams` to the voice actor using params stashed in `pending_join`.
async fn spawn_voice(app: AppHandle, core: Arc<Mutex<AppCore>>, server_addr: String) {
    let (params, voice) = {
        let mut c = core.lock().await;
        let Some(p) = c.pending_join.take() else { return };
        let params = StartParams {
            server_addr,
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
        };
        (params, c.voice.clone())
    };

    if let Err(e) = voice.start(params).await {
        tracing::error!("voice start failed: {}", e);
        emit(
            &app,
            "error",
            ErrorPayload { message: format!("voice failed: {}", e) },
        );
    }
}
