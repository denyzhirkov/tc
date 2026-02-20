use bytes::{Buf, BytesMut};
use serde::{Deserialize, Serialize};

use crate::config;

// ---------------------------------------------------------------------------
// Channel ID
// ---------------------------------------------------------------------------

/// Short alphanumeric channel identifier.
pub type ChannelId = String;

/// Generate a random short channel ID.
pub fn generate_channel_id() -> ChannelId {
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..config::CHANNEL_ID_LEN)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

// ---------------------------------------------------------------------------
// TCP Control Messages  (client → server)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after connect — announces client version.
    Hello {
        version: String,
        protocol: u16,
    },
    /// Create a new channel. If `name` is set, creates a public channel "pub-<name>".
    CreateChannel { name: Option<String> },
    /// Join an existing channel.
    JoinChannel { channel_id: ChannelId },
    /// Leave the current channel.
    LeaveChannel,
    /// List public channels.
    ListChannels,
    /// Send a text chat message.
    ChatMessage { text: String },
    /// Set display name.
    SetName { name: String },
    /// Ping (keep-alive).
    Ping,
}

// ---------------------------------------------------------------------------
// TCP Control Messages  (server → client)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// First message after connect — announces server version.
    Welcome {
        version: String,
        protocol: u16,
    },
    /// Channel created successfully.
    ChannelCreated { channel_id: ChannelId },
    /// Joined channel successfully.
    JoinedChannel {
        channel_id: ChannelId,
        participants: Vec<String>,
        /// Token to send in UDP hello to bind UDP address to this TCP session.
        udp_token: u64,
        /// 32-byte XChaCha20-Poly1305 channel encryption key.
        voice_key: Vec<u8>,
    },
    /// Another participant joined your channel.
    PeerJoined { peer_name: String },
    /// Another participant left your channel.
    PeerLeft { peer_name: String },
    /// You left the channel.
    LeftChannel,
    /// Incoming text chat from a peer.
    ChatMessage { from: String, text: String },
    /// Name was changed.
    NameChanged { old_name: String, new_name: String },
    /// List of public channels.
    ChannelList { channels: Vec<ChannelInfo> },
    /// Error from server.
    Error { message: String },
    /// Pong (keep-alive response).
    Pong,
}

/// Info about a public channel (for /list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub channel_id: ChannelId,
    pub participant_count: u32,
}

// ---------------------------------------------------------------------------
// UDP Packets (binary, not serde — keep it lean)
// ---------------------------------------------------------------------------

/// UDP hello packet size: `[seq=0: u32][token: u64]` = 12 bytes.
const UDP_HELLO_SIZE: usize = 12;

/// Encode a UDP hello packet with the given token.
pub fn encode_udp_hello(token: u64) -> Vec<u8> {
    let mut buf = vec![0u8; UDP_HELLO_SIZE];
    buf[4..12].copy_from_slice(&token.to_be_bytes());
    buf
}

/// Try to decode a UDP hello packet. Returns the token if valid.
pub fn decode_udp_hello(data: &[u8]) -> Option<u64> {
    if data.len() != UDP_HELLO_SIZE {
        return None;
    }
    let seq = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if seq != 0 {
        return None;
    }
    Some(u64::from_be_bytes([
        data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
    ]))
}

/// Voice packet.
///
/// Wire format (big-endian):
/// ```text
/// [0..4]   sequence: u32 (always > 0 for voice)
/// [4..5]   channel_id_len: u8
/// [5..5+N] channel_id: UTF-8 bytes
/// [rest]   opus_data
/// ```
#[derive(Debug, Clone)]
pub struct VoicePacket {
    pub sequence: u32,
    pub channel_id: ChannelId,
    pub opus_data: Vec<u8>,
}

impl VoicePacket {
    /// Serialize to bytes for UDP transmission.
    pub fn encode(&self) -> Vec<u8> {
        let id_bytes = self.channel_id.as_bytes();
        let mut buf = Vec::with_capacity(4 + 1 + id_bytes.len() + self.opus_data.len());
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.push(id_bytes.len() as u8);
        buf.extend_from_slice(id_bytes);
        buf.extend_from_slice(&self.opus_data);
        buf
    }

    /// Deserialize from UDP bytes.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 6 {
            return None;
        }
        let sequence = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if sequence == 0 {
            return None;
        }
        let id_len = data[4] as usize;
        if data.len() < 5 + id_len + 1 {
            return None;
        }
        let channel_id = String::from_utf8(data[5..5 + id_len].to_vec()).ok()?;
        let opus_data = data[5 + id_len..].to_vec();
        Some(VoicePacket {
            sequence,
            channel_id,
            opus_data,
        })
    }

    /// Zero-copy parse: extract (sequence, encrypted_payload) without allocating.
    /// Used by the client receiver where channel_id is not needed.
    pub fn parse_voice_data(data: &[u8]) -> Option<(u32, &[u8])> {
        if data.len() < 6 {
            return None;
        }
        let sequence = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if sequence == 0 {
            return None;
        }
        let id_len = data[4] as usize;
        if data.len() < 5 + id_len + 1 {
            return None;
        }
        Some((sequence, &data[5 + id_len..]))
    }

    /// Parse only the channel_id from raw bytes without copying opus_data.
    pub fn parse_channel_id(data: &[u8]) -> Option<&str> {
        if data.len() < 6 {
            return None;
        }
        let id_len = data[4] as usize;
        if data.len() < 5 + id_len {
            return None;
        }
        std::str::from_utf8(&data[5..5 + id_len]).ok()
    }
}

// ---------------------------------------------------------------------------
// TCP framing helpers (length-prefixed messages)
// ---------------------------------------------------------------------------

/// Encode a TCP message as length-prefixed bytes: [u32 len][bincode payload].
pub fn encode_tcp_frame<T: Serialize>(msg: &T) -> anyhow::Result<Vec<u8>> {
    let payload = bincode::serialize(msg)?;
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Try to extract one complete frame from a buffer.
/// Returns `Ok(Some((message_bytes, consumed)))` if a complete frame is available.
/// Returns `Err` if the frame exceeds the configured max frame size.
pub fn try_decode_frame(buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>, FrameError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > config::MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge(len));
    }
    let total = 4 + len;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some((buf[4..total].to_vec(), total)))
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame too large: {0} bytes (max {MAX_FRAME_SIZE})", MAX_FRAME_SIZE = config::MAX_FRAME_SIZE)]
    TooLarge(usize),
}

/// Extract all complete frames from a pending buffer, advancing consumed bytes.
/// Uses BytesMut for O(1) advance instead of O(n) Vec::drain.
pub fn extract_frames(pending: &mut BytesMut) -> Result<Vec<Vec<u8>>, FrameError> {
    let mut frames = Vec::new();
    loop {
        match try_decode_frame(pending) {
            Ok(Some((data, consumed))) => {
                frames.push(data);
                pending.advance(consumed);
            }
            Ok(None) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(frames)
}

/// Encode a message and write it as a length-prefixed frame, then flush.
pub async fn write_tcp_frame<W, T>(writer: &mut W, msg: &T) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    use tokio::io::AsyncWriteExt;
    let frame = encode_tcp_frame(msg)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}
