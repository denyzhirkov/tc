use std::time::Duration;

use anyhow::{Context, Result};
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use bytes::BytesMut;
use tc_shared::{config, extract_frames, write_tcp_frame, ClientMessage, ServerMessage};

use crate::tls;

/// Capacity for outgoing client→server command queue.
const CMD_QUEUE_CAPACITY: usize = 64;
/// Capacity for incoming server→client message queue.
const MSG_QUEUE_CAPACITY: usize = 256;

/// Handle to send commands to the server.
#[derive(Clone)]
pub struct ServerConnection {
    tx: mpsc::Sender<ClientMessage>,
}

impl ServerConnection {
    pub fn send(&self, msg: ClientMessage) -> Result<()> {
        self.tx
            .try_send(msg)
            .map_err(|_| anyhow::anyhow!("connection closed"))
    }
}

/// Connect to the server and return a handle + receiver for incoming messages.
pub async fn connect(
    server_addr: &str,
    tofu: &tls::TofuState,
) -> Result<(ServerConnection, mpsc::Receiver<Option<ServerMessage>>)> {
    let addr = if server_addr.contains(':') {
        server_addr.to_string()
    } else {
        format!("{}:{}", server_addr, config::TCP_PORT)
    };

    let stream = TcpStream::connect(&addr)
        .await
        .with_context(|| format!("failed to connect to {}", addr))?;

    tofu.set_current_server(&addr);
    let connector = tls::tls_connector(tofu);
    let host = addr.split(':').next().unwrap_or("localhost");
    let server_name: ServerName<'static> = match ServerName::try_from(host) {
        Ok(name) => name.to_owned(),
        Err(_) => "localhost".try_into().unwrap(),
    };
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .with_context(|| format!("TLS handshake failed with {}", addr))?;

    tracing::info!("connected to server at {} (TLS)", addr);

    let (reader, writer) = tokio::io::split(tls_stream);

    // Channel for outgoing commands (client → server)
    let (cmd_tx, cmd_rx) = mpsc::channel::<ClientMessage>(CMD_QUEUE_CAPACITY);

    // Channel for incoming messages (server → client)
    // None signals disconnect
    let (msg_tx, msg_rx) = mpsc::channel::<Option<ServerMessage>>(MSG_QUEUE_CAPACITY);

    // Writer task
    tokio::spawn(writer_task(writer, cmd_rx));

    // Reader task
    tokio::spawn(reader_task(reader, msg_tx));

    // Send Hello as first message
    let _ = cmd_tx.try_send(ClientMessage::Hello {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: config::PROTOCOL_VERSION,
    });

    // Heartbeat task — sends Ping every HEARTBEAT_INTERVAL_SECS
    let heartbeat_tx = cmd_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(config::HEARTBEAT_INTERVAL_SECS)).await;
            if heartbeat_tx.try_send(ClientMessage::Ping).is_err() {
                break;
            }
        }
    });

    Ok((ServerConnection { tx: cmd_tx }, msg_rx))
}

async fn writer_task<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::Receiver<ClientMessage>,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = write_tcp_frame(&mut writer, &msg).await {
            tracing::debug!("writer stopped: {}", e);
            break;
        }
    }
}

async fn reader_task<R: AsyncRead + Unpin>(
    mut reader: R,
    tx: mpsc::Sender<Option<ServerMessage>>,
) {
    let mut buf = vec![0u8; config::TCP_READ_BUF];
    let mut pending = BytesMut::new();

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => {
                tracing::info!("server connection closed");
                let _ = tx.try_send(None);
                break;
            }
            Ok(n) => n,
        };

        pending.extend_from_slice(&buf[..n]);

        if pending.len() > config::MAX_PENDING_BUF {
            tracing::warn!("pending buffer overflow ({} bytes), disconnecting", pending.len());
            let _ = tx.try_send(None);
            return;
        }

        let frames = match extract_frames(&mut pending) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("frame error: {}", e);
                let _ = tx.try_send(None);
                return;
            }
        };
        for frame_data in frames {
            match bincode::deserialize::<ServerMessage>(&frame_data) {
                Ok(msg) => {
                    if tx.try_send(Some(msg)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to decode server message: {}", e);
                }
            }
        }
    }
}
