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
    pubkey: Option<Vec<u8>>,
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
        pubkey: pubkey.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tc_shared::{encode_tcp_frame, ServerMessage};

    #[tokio::test]
    async fn writer_task_encodes_messages() {
        let (tx, rx) = mpsc::channel::<ClientMessage>(8);
        let buf = tokio::io::duplex(4096);
        let (_, reader) = buf;
        let (writer, mut verify_reader) = tokio::io::duplex(4096);

        tokio::spawn(writer_task(writer, rx));

        tx.send(ClientMessage::Ping).await.unwrap();
        drop(tx); // close channel so writer_task finishes

        // Read what was written
        let mut out = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut verify_reader, &mut out)
            .await
            .unwrap();
        drop(reader);

        // Should be a valid length-prefixed frame
        assert!(out.len() >= 4, "output too short: {} bytes", out.len());
        let len = u32::from_be_bytes([out[0], out[1], out[2], out[3]]) as usize;
        assert_eq!(len, out.len() - 4);
        let msg: ClientMessage = bincode::deserialize(&out[4..]).unwrap();
        assert!(matches!(msg, ClientMessage::Ping));
    }

    #[tokio::test]
    async fn reader_task_decodes_single_message() {
        let msg = ServerMessage::Pong;
        let frame = encode_tcp_frame(&msg).unwrap();

        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::io::AsyncWriteExt::write_all(&mut writer, &frame)
            .await
            .unwrap();
        drop(writer); // EOF after the frame

        let (tx, mut rx) = mpsc::channel::<Option<ServerMessage>>(8);
        reader_task(reader, tx).await;

        let received = rx.recv().await.unwrap();
        assert!(matches!(received, Some(ServerMessage::Pong)));

        // Should get None (disconnect signal)
        let disconnect = rx.recv().await.unwrap();
        assert!(disconnect.is_none());
    }

    #[tokio::test]
    async fn reader_task_decodes_multiple_messages() {
        let msg1 = ServerMessage::Pong;
        let msg2 = ServerMessage::ChannelCreated {
            channel_id: "abc12".into(),
        };
        let frame1 = encode_tcp_frame(&msg1).unwrap();
        let frame2 = encode_tcp_frame(&msg2).unwrap();

        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::io::AsyncWriteExt::write_all(&mut writer, &frame1)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &frame2)
            .await
            .unwrap();
        drop(writer);

        let (tx, mut rx) = mpsc::channel::<Option<ServerMessage>>(8);
        reader_task(reader, tx).await;

        let r1 = rx.recv().await.unwrap();
        assert!(matches!(r1, Some(ServerMessage::Pong)));

        let r2 = rx.recv().await.unwrap();
        assert!(matches!(r2, Some(ServerMessage::ChannelCreated { .. })));
    }

    #[tokio::test]
    async fn reader_task_handles_empty_stream() {
        let (_writer, reader) = tokio::io::duplex(4096);
        drop(_writer); // immediate EOF

        let (tx, mut rx) = mpsc::channel::<Option<ServerMessage>>(8);
        reader_task(reader, tx).await;

        // Should get disconnect signal
        let received = rx.recv().await.unwrap();
        assert!(received.is_none());
    }

    #[test]
    fn server_connection_send_closed_channel() {
        let (tx, rx) = mpsc::channel::<ClientMessage>(1);
        drop(rx);
        let conn = ServerConnection { tx };
        let result = conn.send(ClientMessage::Ping);
        assert!(result.is_err());
    }
}
