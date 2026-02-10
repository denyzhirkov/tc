use serde::{Deserialize, Serialize};

/// Default server TCP port.
pub const DEFAULT_TCP_PORT: u16 = 7100;
/// Default server UDP port.
pub const DEFAULT_UDP_PORT: u16 = 7101;
/// Length of generated channel IDs.
pub const CHANNEL_ID_LEN: usize = 5;
/// Max audio packet payload size (bytes).
pub const MAX_AUDIO_PAYLOAD: usize = 1500;

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
    (0..CHANNEL_ID_LEN)
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
    /// Create a new channel.
    CreateChannel,
    /// Join an existing channel.
    JoinChannel { channel_id: ChannelId },
    /// Leave the current channel.
    LeaveChannel,
    /// Send a text chat message.
    ChatMessage { text: String },
    /// Ping (keep-alive).
    Ping,
}

// ---------------------------------------------------------------------------
// TCP Control Messages  (server → client)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Channel created successfully.
    ChannelCreated { channel_id: ChannelId },
    /// Joined channel successfully.
    JoinedChannel {
        channel_id: ChannelId,
        participants: Vec<String>,
    },
    /// Another participant joined your channel.
    PeerJoined { peer_name: String },
    /// Another participant left your channel.
    PeerLeft { peer_name: String },
    /// You left the channel.
    LeftChannel,
    /// Incoming text chat from a peer.
    ChatMessage { from: String, text: String },
    /// Error from server.
    Error { message: String },
    /// Pong (keep-alive response).
    Pong,
}

// ---------------------------------------------------------------------------
// UDP Voice Packet (binary, not serde — keep it lean)
// ---------------------------------------------------------------------------

/// Header for a UDP voice packet.
///
/// Wire format (big-endian):
/// ```text
/// [0..4]   sequence: u32
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
/// Returns `Some((message_bytes, consumed))` if a complete frame is available.
pub fn try_decode_frame(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let total = 4 + len;
    if buf.len() < total {
        return None;
    }
    Some((buf[4..total].to_vec(), total))
}

