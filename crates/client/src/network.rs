use std::time::Duration;

use anyhow::{Context, Result};
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use tc_shared::{config, extract_frames, write_tcp_frame, ClientMessage, ServerMessage};

use crate::tls;

/// Handle to send commands to the server.
#[derive(Clone)]
pub struct ServerConnection {
    tx: mpsc::UnboundedSender<ClientMessage>,
}

impl ServerConnection {
    pub fn send(&self, msg: ClientMessage) -> Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("connection closed"))
    }
}

/// Connect to the server and return a handle + receiver for incoming messages.
pub async fn connect(
    server_addr: &str,
) -> Result<(ServerConnection, mpsc::UnboundedReceiver<Option<ServerMessage>>)> {
    let addr = if server_addr.contains(':') {
        server_addr.to_string()
    } else {
        format!("{}:{}", server_addr, config::TCP_PORT)
    };

    let stream = TcpStream::connect(&addr)
        .await
        .with_context(|| format!("failed to connect to {}", addr))?;

    let connector = tls::tls_connector();
    let server_name: ServerName<'static> = "localhost"
        .try_into()
        .expect("valid server name");
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .with_context(|| format!("TLS handshake failed with {}", addr))?;

    tracing::info!("connected to server at {} (TLS)", addr);

    let (reader, writer) = tokio::io::split(tls_stream);

    // Channel for outgoing commands (client → server)
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ClientMessage>();

    // Channel for incoming messages (server → client)
    // None signals disconnect
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<Option<ServerMessage>>();

    // Writer task
    tokio::spawn(writer_task(writer, cmd_rx));

    // Reader task
    tokio::spawn(reader_task(reader, msg_tx));

    // Heartbeat task — sends Ping every HEARTBEAT_INTERVAL_SECS
    let heartbeat_tx = cmd_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(config::HEARTBEAT_INTERVAL_SECS)).await;
            if heartbeat_tx.send(ClientMessage::Ping).is_err() {
                break;
            }
        }
    });

    Ok((ServerConnection { tx: cmd_tx }, msg_rx))
}

async fn writer_task<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::UnboundedReceiver<ClientMessage>,
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
    tx: mpsc::UnboundedSender<Option<ServerMessage>>,
) {
    let mut buf = vec![0u8; config::TCP_READ_BUF];
    let mut pending = Vec::new();

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => {
                tracing::info!("server connection closed");
                let _ = tx.send(None);
                break;
            }
            Ok(n) => n,
        };

        pending.extend_from_slice(&buf[..n]);

        let frames = match extract_frames(&mut pending) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("frame error: {}", e);
                let _ = tx.send(None);
                return;
            }
        };
        for frame_data in frames {
            match bincode::deserialize::<ServerMessage>(&frame_data) {
                Ok(msg) => {
                    if tx.send(Some(msg)).is_err() {
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
