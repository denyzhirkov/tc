use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio_rustls::TlsAcceptor;

use bytes::BytesMut;
use tc_shared::config;
use tc_shared::{extract_frames, write_tcp_frame, ChannelId, ClientMessage, ServerMessage};

use crate::rate_limit::RateLimiter;
use crate::state::ServerState;

/// Max concurrent TLS handshakes to prevent resource exhaustion.
const MAX_CONCURRENT_TLS_HANDSHAKES: usize = 128;

/// Timeout for TLS handshake completion.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for idle clients (no data received). 3× the client heartbeat interval.
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

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

    let tls_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TLS_HANDSHAKES));

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("TCP accept error: {}, retrying...", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        tracing::info!(%peer_addr, "new TCP connection");

        let acceptor = acceptor.clone();
        let state = state.clone();
        let senders = senders.clone();
        let permit = tls_semaphore.clone();
        tokio::spawn(async move {
            let _permit = match permit.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return, // semaphore closed — shutting down
            };

            let tls_stream = match tokio::time::timeout(
                TLS_HANDSHAKE_TIMEOUT,
                acceptor.accept(stream),
            )
            .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(%peer_addr, "TLS handshake failed: {}", e);
                    return;
                }
                Err(_) => {
                    tracing::warn!(%peer_addr, "TLS handshake timed out");
                    return;
                }
            };
            drop(_permit);
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
    let total = state.stats().await.clients;
    tracing::info!(%peer_addr, %name, total, "client registered");

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
    let total = state.stats().await.clients;
    tracing::info!(%peer_addr, %name, total, "client disconnected");

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
        let n = match tokio::time::timeout(CLIENT_IDLE_TIMEOUT, reader.read(&mut buf)).await {
            Ok(Ok(0)) => return Ok(()), // Connection closed
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::info!(%peer_addr, "idle timeout, disconnecting");
                return Ok(());
            }
        };

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
            let msg: ClientMessage = match bincode::deserialize(&frame_data) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(%peer_addr, "bincode deserialize error: {}, skipping frame", e);
                    continue;
                }
            };

            // Rate limit: Ping/Hello always allowed, CreateChannel has a stricter limit
            match &msg {
                ClientMessage::Ping | ClientMessage::Hello { .. } => {}
                ClientMessage::CreateChannel { .. } => {
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
        ClientMessage::CreateChannel { name: chan_name } => {
            handle_create_channel(peer_addr, name, chan_name, state, senders).await
        }
        ClientMessage::JoinChannel { channel_id } => {
            handle_join_channel(peer_addr, name, channel_id, state, senders).await
        }
        ClientMessage::LeaveChannel => {
            handle_leave_channel(peer_addr, name, state, senders).await
        }
        ClientMessage::ChatMessage { text } => {
            handle_chat_message(peer_addr, name, text, state, senders).await
        }
        ClientMessage::DirectMessage { to_pubkey, text } => {
            handle_direct_message(peer_addr, name, to_pubkey, text, state, senders).await
        }
        ClientMessage::SetName { name: new_name } => {
            handle_set_name(peer_addr, name, new_name, state, senders).await
        }
        ClientMessage::Hello { version, protocol, pubkey } => {
            tracing::info!(%peer_addr, %name, %version, %protocol, has_pubkey = pubkey.is_some(), "client hello");
            if let Some(pk) = pubkey {
                if pk.len() == 32 {
                    state.set_pubkey(&peer_addr, pk).await;
                } else {
                    tracing::warn!(%peer_addr, len = pk.len(), "ignoring invalid pubkey size");
                }
            }
        }
        ClientMessage::ListChannels => {
            handle_list_channels(peer_addr, name, state, senders).await
        }
        ClientMessage::Ping => {
            send_to(senders, &peer_addr, ServerMessage::Pong).await;
        }
    }

    Ok(())
}

async fn handle_create_channel(
    peer_addr: SocketAddr,
    name: &str,
    chan_name: Option<String>,
    state: &ServerState,
    senders: &ClientSenders,
) {
    match state.create_channel(chan_name.as_deref()).await {
        Ok(channel_id) => {
            let total = state.stats().await.channels;
            tracing::info!(%peer_addr, %name, %channel_id, total_channels = total, "channel created");
            send_to(senders, &peer_addr, ServerMessage::ChannelCreated { channel_id }).await;
        }
        Err(err) => {
            tracing::debug!(%peer_addr, %name, %err, "create channel failed");
            send_to(senders, &peer_addr, ServerMessage::Error { message: err }).await;
        }
    }
}

async fn handle_join_channel(
    peer_addr: SocketAddr,
    name: &str,
    channel_id: ChannelId,
    state: &ServerState,
    senders: &ClientSenders,
) {
    match state.join_channel(&peer_addr, &channel_id).await {
        Ok((participants, udp_token, voice_key)) => {
            tracing::info!(%peer_addr, %name, %channel_id, participants = participants.len(), "joined channel");
            broadcast_to_channel(
                state,
                senders,
                &channel_id,
                Some(&peer_addr),
                ServerMessage::PeerJoined { peer_name: name.to_string() },
            )
            .await;
            send_to(
                senders,
                &peer_addr,
                ServerMessage::JoinedChannel { channel_id, participants, udp_token, voice_key },
            )
            .await;
        }
        Err(err) => {
            tracing::debug!(%peer_addr, %name, %channel_id, %err, "join channel failed");
            send_to(senders, &peer_addr, ServerMessage::Error { message: err }).await;
        }
    }
}

async fn handle_leave_channel(
    peer_addr: SocketAddr,
    name: &str,
    state: &ServerState,
    senders: &ClientSenders,
) {
    if let Some((left_name, channel_id)) = state.leave_channel(&peer_addr).await {
        tracing::info!(%peer_addr, %left_name, %channel_id, "left channel");
        send_to(senders, &peer_addr, ServerMessage::LeftChannel).await;
        broadcast_to_channel(
            state,
            senders,
            &channel_id,
            None,
            ServerMessage::PeerLeft { peer_name: left_name },
        )
        .await;
    } else {
        tracing::debug!(%peer_addr, %name, "leave failed: not in a channel");
        send_to(
            senders,
            &peer_addr,
            ServerMessage::Error { message: "not in a channel".into() },
        )
        .await;
    }
}

async fn handle_chat_message(
    peer_addr: SocketAddr,
    name: &str,
    text: String,
    state: &ServerState,
    senders: &ClientSenders,
) {
    if text.len() > config::MAX_CHAT_LEN {
        tracing::debug!(%peer_addr, %name, len = text.len(), "chat rejected: too long");
        send_to(
            senders,
            &peer_addr,
            ServerMessage::Error { message: "message too long".into() },
        )
        .await;
        return;
    }
    if let Some(client) = state.get_client(&peer_addr).await {
        if let Some(ref channel_id) = client.channel {
            tracing::debug!(%peer_addr, %name, %channel_id, len = text.len(), "chat relayed");
            broadcast_to_channel(
                state,
                senders,
                channel_id,
                Some(&peer_addr),
                ServerMessage::ChatMessage { from: name.to_string(), text },
            )
            .await;
        }
    }
}

async fn handle_direct_message(
    peer_addr: SocketAddr,
    name: &str,
    to_pubkey: Vec<u8>,
    text: String,
    state: &ServerState,
    senders: &ClientSenders,
) {
    if text.len() > config::MAX_CHAT_LEN {
        send_to(senders, &peer_addr, ServerMessage::Error { message: "message too long".into() }).await;
        return;
    }
    if to_pubkey.len() != 32 {
        send_to(senders, &peer_addr, ServerMessage::Error { message: "invalid pubkey".into() }).await;
        return;
    }
    // Resolve sender's pubkey for the from_pubkey field.
    let from_pubkey = match state.get_client(&peer_addr).await.and_then(|c| c.pubkey) {
        Some(pk) => pk,
        None => {
            send_to(senders, &peer_addr, ServerMessage::Error { message: "must send Hello with pubkey first".into() }).await;
            return;
        }
    };
    let target = match state.lookup_pubkey(&to_pubkey).await {
        Some(addr) => addr,
        None => {
            send_to(senders, &peer_addr, ServerMessage::Error { message: "recipient not online".into() }).await;
            return;
        }
    };
    tracing::debug!(%peer_addr, %name, target = %target, len = text.len(), "DM relayed");
    send_to(
        senders,
        &target,
        ServerMessage::DirectMessage { from_pubkey, from_name: name.to_string(), text },
    )
    .await;
}

async fn handle_set_name(
    peer_addr: SocketAddr,
    name: &mut String,
    new_name: String,
    state: &ServerState,
    senders: &ClientSenders,
) {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() || new_name.len() > config::MAX_NAME_LEN {
        tracing::debug!(%peer_addr, %name, "rename rejected: invalid length");
        send_to(
            senders,
            &peer_addr,
            ServerMessage::Error { message: "name must be 1-32 characters".into() },
        )
        .await;
        return;
    }
    if let Some((old_name, channel)) = state.rename_client(&peer_addr, new_name.clone()).await {
        tracing::info!(%peer_addr, %old_name, %new_name, "renamed");
        *name = new_name.clone();
        send_to(
            senders,
            &peer_addr,
            ServerMessage::NameChanged { old_name: old_name.clone(), new_name: new_name.clone() },
        )
        .await;
        if let Some(channel_id) = channel {
            broadcast_to_channel(
                state,
                senders,
                &channel_id,
                Some(&peer_addr),
                ServerMessage::NameChanged { old_name, new_name },
            )
            .await;
        }
    }
}

async fn handle_list_channels(
    peer_addr: SocketAddr,
    name: &str,
    state: &ServerState,
    senders: &ClientSenders,
) {
    let public = state.list_public_channels().await;
    tracing::debug!(%peer_addr, %name, count = public.len(), "channel list requested");
    let channels = public
        .into_iter()
        .map(|(channel_id, participant_count)| tc_shared::ChannelInfo {
            channel_id,
            participant_count,
        })
        .collect();
    send_to(senders, &peer_addr, ServerMessage::ChannelList { channels }).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Limits;

    fn test_addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    async fn setup() -> (ServerState, ClientSenders, SocketAddr, mpsc::Receiver<ServerMessage>) {
        let state = ServerState::new(Limits {
            max_channels: 0,
            max_clients: 0,
        });
        let senders: ClientSenders = Arc::new(RwLock::new(HashMap::new()));
        let peer = test_addr(9000);

        // Register the client in state
        state.register_client(peer).await.unwrap();

        // Create a sender for this client
        let (tx, rx) = mpsc::channel(64);
        senders.write().await.insert(peer, tx);

        (state, senders, peer, rx)
    }

    #[tokio::test]
    async fn handle_ping_responds_pong() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(peer, &mut name, ClientMessage::Ping, &state, &senders)
            .await
            .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::Pong));
    }

    #[tokio::test]
    async fn handle_create_channel() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(
            peer,
            &mut name,
            ClientMessage::CreateChannel { name: None },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::ChannelCreated { .. }));
    }

    #[tokio::test]
    async fn handle_join_nonexistent_channel() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(
            peer,
            &mut name,
            ClientMessage::JoinChannel {
                channel_id: "nonexistent".into(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::Error { .. }));
    }

    #[tokio::test]
    async fn handle_create_and_join_channel() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();

        // Create
        handle_message(
            peer,
            &mut name,
            ClientMessage::CreateChannel { name: None },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let channel_id = match rx.try_recv().unwrap() {
            ServerMessage::ChannelCreated { channel_id } => channel_id,
            other => panic!("expected ChannelCreated, got {:?}", other),
        };

        // Join
        handle_message(
            peer,
            &mut name,
            ClientMessage::JoinChannel { channel_id },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::JoinedChannel { .. }));
    }

    #[tokio::test]
    async fn handle_leave_without_channel() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(peer, &mut name, ClientMessage::LeaveChannel, &state, &senders)
            .await
            .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::Error { .. }));
    }

    #[tokio::test]
    async fn handle_set_name_valid() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(
            peer,
            &mut name,
            ClientMessage::SetName {
                name: "bob".to_string(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::NameChanged { .. }));
        assert_eq!(name, "bob");
    }

    #[tokio::test]
    async fn handle_set_name_empty_rejected() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(
            peer,
            &mut name,
            ClientMessage::SetName {
                name: "   ".to_string(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::Error { .. }));
        assert_eq!(name, "alice"); // unchanged
    }

    #[tokio::test]
    async fn handle_chat_not_in_channel() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(
            peer,
            &mut name,
            ClientMessage::ChatMessage {
                text: "hello".into(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();
        // No response expected when not in a channel (no error sent for chat)
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_chat_too_long() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        let long_text = "x".repeat(config::MAX_CHAT_LEN + 1);
        handle_message(
            peer,
            &mut name,
            ClientMessage::ChatMessage { text: long_text },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::Error { .. }));
    }

    #[tokio::test]
    async fn handle_list_channels() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();
        handle_message(peer, &mut name, ClientMessage::ListChannels, &state, &senders)
            .await
            .unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMessage::ChannelList { .. }));
    }

    #[tokio::test]
    async fn handle_chat_relayed_to_peer() {
        let (state, senders, peer, mut rx) = setup().await;
        let mut name = "alice".to_string();

        // Create + join channel
        handle_message(
            peer,
            &mut name,
            ClientMessage::CreateChannel { name: None },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let channel_id = match rx.try_recv().unwrap() {
            ServerMessage::ChannelCreated { channel_id } => channel_id,
            other => panic!("expected ChannelCreated, got {:?}", other),
        };
        handle_message(
            peer,
            &mut name,
            ClientMessage::JoinChannel {
                channel_id: channel_id.clone(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();
        let _ = rx.try_recv().unwrap(); // JoinedChannel

        // Add a second client
        let peer2 = test_addr(9001);
        state.register_client(peer2).await.unwrap();
        let (tx2, mut rx2) = mpsc::channel(64);
        senders.write().await.insert(peer2, tx2);
        state.join_channel(&peer2, &channel_id).await.unwrap();

        // Alice sends chat
        handle_message(
            peer,
            &mut name,
            ClientMessage::ChatMessage {
                text: "hi bob".into(),
            },
            &state,
            &senders,
        )
        .await
        .unwrap();

        // Bob should receive it
        let msg = rx2.try_recv().unwrap();
        match msg {
            ServerMessage::ChatMessage { from, text } => {
                assert_eq!(from, "alice");
                assert_eq!(text, "hi bob");
            }
            other => panic!("expected ChatMessage, got {:?}", other),
        }
        // Alice should NOT receive her own chat
        assert!(rx.try_recv().is_err());
    }
}

/// Broadcast a message to all clients in a channel, optionally excluding one.
/// Acquires both locks in a single scope to avoid intermediate Vec allocation
/// and reduce total lock hold time.
async fn broadcast_to_channel(
    state: &ServerState,
    senders: &ClientSenders,
    channel_id: &str,
    exclude: Option<&SocketAddr>,
    msg: ServerMessage,
) {
    let senders = senders.read().await;
    state.broadcast_channel(channel_id, exclude, &msg, |addr, m| {
        if let Some(tx) = senders.get(addr) {
            if tx.try_send(m).is_err() {
                tracing::debug!(%addr, "client queue full, dropping broadcast");
            }
        }
    }).await;
}
