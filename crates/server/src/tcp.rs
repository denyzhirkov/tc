use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;

use bytes::BytesMut;
use tc_shared::config;
use tc_shared::{extract_frames, write_tcp_frame, ClientMessage, ServerMessage};

use crate::rate_limit::RateLimiter;
use crate::state::ServerState;

/// Capacity for per-client outgoing message queue.
const CLIENT_QUEUE_CAPACITY: usize = 128;

/// Per-client sender for outgoing TCP messages.
pub type ClientSender = mpsc::Sender<ServerMessage>;

/// Shared map of all connected clients' senders, keyed by TCP address.
pub type ClientSenders = Arc<RwLock<HashMap<SocketAddr, ClientSender>>>;

/// Start the TCP control server.
pub async fn run_tcp_server(
    state: ServerState,
    senders: ClientSenders,
    addr: String,
    acceptor: TlsAcceptor,
) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("TCP+TLS listening on {}", addr);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        tracing::info!(%peer_addr, "new TCP connection");

        let acceptor = acceptor.clone();
        let state = state.clone();
        let senders = senders.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(%peer_addr, "TLS handshake failed: {}", e);
                    return;
                }
            };
            tracing::info!(%peer_addr, "TLS handshake complete");

            if let Err(e) = handle_client(tls_stream, peer_addr, state, senders).await {
                tracing::warn!(%peer_addr, "client error: {}", e);
            }
        });
    }
}

async fn handle_client(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    peer_addr: SocketAddr,
    state: ServerState,
    senders: ClientSenders,
) -> Result<()> {
    let (reader, writer) = tokio::io::split(stream);

    // Register client
    let name = match state.register_client(peer_addr).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(%peer_addr, "rejected: {}", e);
            return Ok(());
        }
    };
    tracing::info!(%peer_addr, %name, "client registered");

    // Create outgoing message channel (bounded to prevent memory growth)
    let (tx, rx) = mpsc::channel::<ServerMessage>(CLIENT_QUEUE_CAPACITY);
    senders.write().await.insert(peer_addr, tx.clone());

    // Send Welcome with server version as first message
    let _ = tx.try_send(ServerMessage::Welcome {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: config::PROTOCOL_VERSION,
    });

    // Writer task: sends queued ServerMessages to this client
    let writer_handle = tokio::spawn(writer_task(writer, rx));

    // Reader loop: reads incoming ClientMessages
    let result = reader_loop(reader, peer_addr, name.clone(), &state, &senders).await;

    // Cleanup on disconnect
    senders.write().await.remove(&peer_addr);
    if let Some((left_name, Some(channel_id))) = state.remove_client(&peer_addr).await {
        broadcast_to_channel(
            &state,
            &senders,
            &channel_id,
            None,
            ServerMessage::PeerLeft {
                peer_name: left_name,
            },
        )
        .await;
    }
    tracing::info!(%peer_addr, %name, "client disconnected");

    writer_handle.abort();
    result
}

async fn writer_task<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::Receiver<ServerMessage>,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = write_tcp_frame(&mut writer, &msg).await {
            tracing::debug!("writer stopped: {}", e);
            break;
        }
    }
}

async fn reader_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    peer_addr: SocketAddr,
    mut name: String,
    state: &ServerState,
    senders: &ClientSenders,
) -> Result<()> {
    let mut buf = vec![0u8; config::TCP_READ_BUF];
    let mut pending = BytesMut::new();
    let mut cmd_limiter = RateLimiter::new(config::RATE_LIMIT_CMD_PER_SEC, config::RATE_LIMIT_CMD_BURST);
    let mut create_limiter = RateLimiter::new(config::RATE_LIMIT_CREATE_PER_SEC, config::RATE_LIMIT_CREATE_BURST);

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // Connection closed
        }

        pending.extend_from_slice(&buf[..n]);

        if pending.len() > config::MAX_PENDING_BUF {
            tracing::warn!(%peer_addr, "pending buffer overflow ({} bytes), disconnecting", pending.len());
            anyhow::bail!("pending buffer overflow");
        }

        let frames = match extract_frames(&mut pending) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(%peer_addr, "frame error: {}", e);
                anyhow::bail!("protocol error: {}", e);
            }
        };
        for frame_data in frames {
            let msg: ClientMessage = bincode::deserialize(&frame_data)?;

            // Rate limit: Ping/Hello always allowed, CreateChannel has a stricter limit
            match &msg {
                ClientMessage::Ping | ClientMessage::Hello { .. } => {}
                ClientMessage::CreateChannel => {
                    if !cmd_limiter.check() || !create_limiter.check() {
                        tracing::debug!(%peer_addr, "rate limited (create)");
                        send_to(senders, &peer_addr, ServerMessage::Error {
                            message: "rate limited, slow down".into(),
                        }).await;
                        continue;
                    }
                }
                _ => {
                    if !cmd_limiter.check() {
                        tracing::debug!(%peer_addr, "rate limited");
                        send_to(senders, &peer_addr, ServerMessage::Error {
                            message: "rate limited, slow down".into(),
                        }).await;
                        continue;
                    }
                }
            }

            handle_message(peer_addr, &mut name, msg, state, senders).await?;
        }
    }
}

async fn handle_message(
    peer_addr: SocketAddr,
    name: &mut String,
    msg: ClientMessage,
    state: &ServerState,
    senders: &ClientSenders,
) -> Result<()> {
    match msg {
        ClientMessage::CreateChannel => {
            match state.create_channel().await {
                Ok(channel_id) => {
                    tracing::info!(%peer_addr, %name, %channel_id, "channel created");
                    send_to(senders, &peer_addr, ServerMessage::ChannelCreated { channel_id }).await;
                }
                Err(err) => {
                    send_to(senders, &peer_addr, ServerMessage::Error { message: err }).await;
                }
            }
        }

        ClientMessage::JoinChannel { channel_id } => {
            match state.join_channel(&peer_addr, &channel_id).await {
                Ok((participants, udp_token, voice_key)) => {
                    tracing::info!(%peer_addr, %name, %channel_id, "joined channel");

                    // Notify existing participants
                    broadcast_to_channel(
                        state,
                        senders,
                        &channel_id,
                        Some(&peer_addr),
                        ServerMessage::PeerJoined {
                            peer_name: name.to_string(),
                        },
                    )
                    .await;

                    // Confirm join to the client
                    send_to(
                        senders,
                        &peer_addr,
                        ServerMessage::JoinedChannel {
                            channel_id,
                            participants,
                            udp_token,
                            voice_key,
                        },
                    )
                    .await;
                }
                Err(err) => {
                    send_to(senders, &peer_addr, ServerMessage::Error { message: err }).await;
                }
            }
        }

        ClientMessage::LeaveChannel => {
            if let Some((left_name, channel_id)) = state.leave_channel(&peer_addr).await {
                tracing::info!(%peer_addr, %left_name, %channel_id, "left channel");
                send_to(senders, &peer_addr, ServerMessage::LeftChannel).await;
                broadcast_to_channel(
                    state,
                    senders,
                    &channel_id,
                    None,
                    ServerMessage::PeerLeft {
                        peer_name: left_name,
                    },
                )
                .await;
            } else {
                send_to(
                    senders,
                    &peer_addr,
                    ServerMessage::Error {
                        message: "not in a channel".into(),
                    },
                )
                .await;
            }
        }

        ClientMessage::ChatMessage { text } => {
            if text.len() > config::MAX_CHAT_LEN {
                send_to(
                    senders,
                    &peer_addr,
                    ServerMessage::Error {
                        message: "message too long".into(),
                    },
                )
                .await;
            } else if let Some(client) = state.get_client(&peer_addr).await {
                if let Some(ref channel_id) = client.channel {
                    broadcast_to_channel(
                        state,
                        senders,
                        channel_id,
                        Some(&peer_addr),
                        ServerMessage::ChatMessage {
                            from: name.to_string(),
                            text,
                        },
                    )
                    .await;
                }
            }
        }

        ClientMessage::SetName { name: new_name } => {
            let new_name = new_name.trim().to_string();
            if new_name.is_empty() || new_name.len() > config::MAX_NAME_LEN {
                send_to(
                    senders,
                    &peer_addr,
                    ServerMessage::Error {
                        message: "name must be 1-32 characters".into(),
                    },
                )
                .await;
            } else if let Some((old_name, channel)) = state.rename_client(&peer_addr, new_name.clone()).await {
                tracing::info!(%peer_addr, %old_name, %new_name, "renamed");
                *name = new_name.clone();
                send_to(
                    senders,
                    &peer_addr,
                    ServerMessage::NameChanged {
                        old_name: old_name.clone(),
                        new_name: new_name.clone(),
                    },
                )
                .await;
                if let Some(channel_id) = channel {
                    broadcast_to_channel(
                        state,
                        senders,
                        &channel_id,
                        Some(&peer_addr),
                        ServerMessage::NameChanged {
                            old_name,
                            new_name,
                        },
                    )
                    .await;
                }
            }
        }

        ClientMessage::Hello { version, protocol } => {
            tracing::info!(%peer_addr, %name, %version, %protocol, "client hello");
        }

        ClientMessage::Ping => {
            send_to(senders, &peer_addr, ServerMessage::Pong).await;
        }
    }

    Ok(())
}

/// Send a message to a specific client (non-blocking, drops on full queue).
async fn send_to(senders: &ClientSenders, addr: &SocketAddr, msg: ServerMessage) {
    let senders = senders.read().await;
    if let Some(tx) = senders.get(addr) {
        if tx.try_send(msg).is_err() {
            tracing::debug!(%addr, "client queue full, dropping message");
        }
    }
}

/// Broadcast a message to all clients in a channel, optionally excluding one.
async fn broadcast_to_channel(
    state: &ServerState,
    senders: &ClientSenders,
    channel_id: &str,
    exclude: Option<&SocketAddr>,
    msg: ServerMessage,
) {
    let addrs = state.get_channel_tcp_addrs(channel_id).await;
    let senders = senders.read().await;
    for addr in &addrs {
        if exclude.is_some_and(|ex| ex == addr) {
            continue;
        }
        if let Some(tx) = senders.get(addr) {
            if tx.try_send(msg.clone()).is_err() {
                tracing::debug!(%addr, "client queue full, dropping broadcast");
            }
        }
    }
}
