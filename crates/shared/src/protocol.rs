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

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn udp_hello_roundtrip() {
        let token: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let encoded = encode_udp_hello(token);
        assert_eq!(encoded.len(), 12);
        assert_eq!(decode_udp_hello(&encoded), Some(token));
    }

    #[test]
    fn udp_hello_rejects_wrong_size() {
        assert_eq!(decode_udp_hello(&[0u8; 5]), None);
        assert_eq!(decode_udp_hello(&[0u8; 13]), None);
        assert_eq!(decode_udp_hello(&[]), None);
    }

    #[test]
    fn udp_hello_rejects_nonzero_seq() {
        let mut data = encode_udp_hello(42);
        data[0] = 1;
        assert_eq!(decode_udp_hello(&data), None);
    }

    #[test]
    fn voice_packet_roundtrip() {
        let pkt = VoicePacket {
            sequence: 42,
            channel_id: "abc12".into(),
            opus_data: vec![1, 2, 3, 4, 5],
        };
        let encoded = pkt.encode();
        let decoded = VoicePacket::decode(&encoded).unwrap();
        assert_eq!(decoded.sequence, 42);
        assert_eq!(decoded.channel_id, "abc12");
        assert_eq!(decoded.opus_data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn voice_packet_parse_voice_data() {
        let pkt = VoicePacket {
            sequence: 7,
            channel_id: "ch".into(),
            opus_data: vec![10, 20, 30],
        };
        let encoded = pkt.encode();
        let (seq, payload) = VoicePacket::parse_voice_data(&encoded).unwrap();
        assert_eq!(seq, 7);
        assert_eq!(payload, &[10, 20, 30]);
    }

    #[test]
    fn voice_packet_parse_channel_id() {
        let pkt = VoicePacket {
            sequence: 1,
            channel_id: "room1".into(),
            opus_data: vec![0xFF],
        };
        let encoded = pkt.encode();
        assert_eq!(VoicePacket::parse_channel_id(&encoded), Some("room1"));
    }

    #[test]
    fn voice_packet_rejects_short_data() {
        assert!(VoicePacket::decode(&[0u8; 3]).is_none());
        assert!(VoicePacket::decode(&[0u8; 5]).is_none());
    }

    #[test]
    fn voice_packet_rejects_seq_zero() {
        let mut data = VoicePacket {
            sequence: 1,
            channel_id: "ch".into(),
            opus_data: vec![1],
        }
        .encode();
        data[0] = 0;
        data[1] = 0;
        data[2] = 0;
        data[3] = 0;
        assert!(VoicePacket::decode(&data).is_none());
    }

    #[test]
    fn tcp_frame_roundtrip() {
        let msg = ClientMessage::Ping;
        let frame = encode_tcp_frame(&msg).unwrap();
        let (payload, consumed) = try_decode_frame(&frame).unwrap().unwrap();
        assert_eq!(consumed, frame.len());
        let decoded: ClientMessage = bincode::deserialize(&payload).unwrap();
        assert!(matches!(decoded, ClientMessage::Ping));
    }

    #[test]
    fn tcp_frame_rejects_oversized() {
        let len = (config::MAX_FRAME_SIZE + 1) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        assert!(matches!(try_decode_frame(&buf), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn extract_frames_partial() {
        let msg = ClientMessage::Ping;
        let frame = encode_tcp_frame(&msg).unwrap();

        let half = frame.len() / 2;
        let mut pending = BytesMut::from(&frame[..half]);
        let frames = extract_frames(&mut pending).unwrap();
        assert!(frames.is_empty());
        assert_eq!(pending.len(), half);

        pending.extend_from_slice(&frame[half..]);
        let frames = extract_frames(&mut pending).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(pending.is_empty());
    }

    #[test]
    fn generate_channel_id_valid() {
        let id = generate_channel_id();
        assert_eq!(id.len(), config::CHANNEL_ID_LEN);
        const CHARSET: &str = "abcdefghjkmnpqrstuvwxyz23456789";
        for c in id.chars() {
            assert!(CHARSET.contains(c), "invalid char: {}", c);
        }
    }

    // ── serde round-trips for all message variants ───────────────────

    fn roundtrip_client(msg: &ClientMessage) {
        let bytes = bincode::serialize(msg).unwrap();
        let back: ClientMessage = bincode::deserialize(&bytes).unwrap();
        // Use debug repr for comparison
        assert_eq!(format!("{:?}", msg), format!("{:?}", back));
    }

    fn roundtrip_server(msg: &ServerMessage) {
        let bytes = bincode::serialize(msg).unwrap();
        let back: ServerMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(format!("{:?}", msg), format!("{:?}", back));
    }

    #[test]
    fn client_message_serde_hello() {
        roundtrip_client(&ClientMessage::Hello {
            version: "1.0".into(),
            protocol: 2,
        });
    }

    #[test]
    fn client_message_serde_create_channel() {
        roundtrip_client(&ClientMessage::CreateChannel { name: None });
        roundtrip_client(&ClientMessage::CreateChannel { name: Some("lobby".into()) });
    }

    #[test]
    fn client_message_serde_join_channel() {
        roundtrip_client(&ClientMessage::JoinChannel { channel_id: "abc12".into() });
    }

    #[test]
    fn client_message_serde_leave_channel() {
        roundtrip_client(&ClientMessage::LeaveChannel);
    }

    #[test]
    fn client_message_serde_list_channels() {
        roundtrip_client(&ClientMessage::ListChannels);
    }

    #[test]
    fn client_message_serde_chat() {
        roundtrip_client(&ClientMessage::ChatMessage { text: "hello".into() });
    }

    #[test]
    fn client_message_serde_set_name() {
        roundtrip_client(&ClientMessage::SetName { name: "alice".into() });
    }

    #[test]
    fn client_message_serde_ping() {
        roundtrip_client(&ClientMessage::Ping);
    }

    #[test]
    fn server_message_serde_welcome() {
        roundtrip_server(&ServerMessage::Welcome {
            version: "1.0".into(),
            protocol: 2,
        });
    }

    #[test]
    fn server_message_serde_channel_created() {
        roundtrip_server(&ServerMessage::ChannelCreated { channel_id: "abc".into() });
    }

    #[test]
    fn server_message_serde_joined_channel() {
        roundtrip_server(&ServerMessage::JoinedChannel {
            channel_id: "abc".into(),
            participants: vec!["user-1".into(), "user-2".into()],
            udp_token: 42,
            voice_key: vec![0u8; 32],
        });
    }

    #[test]
    fn server_message_serde_peer_joined() {
        roundtrip_server(&ServerMessage::PeerJoined { peer_name: "alice".into() });
    }

    #[test]
    fn server_message_serde_peer_left() {
        roundtrip_server(&ServerMessage::PeerLeft { peer_name: "bob".into() });
    }

    #[test]
    fn server_message_serde_left_channel() {
        roundtrip_server(&ServerMessage::LeftChannel);
    }

    #[test]
    fn server_message_serde_chat() {
        roundtrip_server(&ServerMessage::ChatMessage {
            from: "alice".into(),
            text: "hi".into(),
        });
    }

    #[test]
    fn server_message_serde_name_changed() {
        roundtrip_server(&ServerMessage::NameChanged {
            old_name: "user-1".into(),
            new_name: "alice".into(),
        });
    }

    #[test]
    fn server_message_serde_channel_list() {
        roundtrip_server(&ServerMessage::ChannelList {
            channels: vec![
                ChannelInfo { channel_id: "pub-a".into(), participant_count: 3 },
                ChannelInfo { channel_id: "pub-b".into(), participant_count: 0 },
            ],
        });
    }

    #[test]
    fn server_message_serde_error() {
        roundtrip_server(&ServerMessage::Error { message: "oops".into() });
    }

    #[test]
    fn server_message_serde_pong() {
        roundtrip_server(&ServerMessage::Pong);
    }

    #[test]
    fn channel_info_serde() {
        let info = ChannelInfo { channel_id: "test".into(), participant_count: 5 };
        let bytes = bincode::serialize(&info).unwrap();
        let back: ChannelInfo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.channel_id, "test");
        assert_eq!(back.participant_count, 5);
    }

    #[test]
    fn frame_error_display() {
        let e = FrameError::TooLarge(99999);
        let s = format!("{}", e);
        assert!(s.contains("99999"));
        assert!(s.contains("too large"));
    }

    // ── TCP frame: multiple frames in one buffer ─────────────────────

    #[test]
    fn extract_multiple_frames() {
        let msg1 = ClientMessage::Ping;
        let msg2 = ClientMessage::LeaveChannel;
        let f1 = encode_tcp_frame(&msg1).unwrap();
        let f2 = encode_tcp_frame(&msg2).unwrap();

        let mut pending = BytesMut::new();
        pending.extend_from_slice(&f1);
        pending.extend_from_slice(&f2);

        let frames = extract_frames(&mut pending).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(pending.is_empty());
    }

    #[test]
    fn try_decode_frame_incomplete() {
        // Less than 4 bytes header
        assert!(try_decode_frame(&[0u8; 2]).unwrap().is_none());
        // Header says 100 bytes but only 10 provided
        let mut buf = vec![0, 0, 0, 100];
        buf.extend_from_slice(&[0u8; 6]);
        assert!(try_decode_frame(&buf).unwrap().is_none());
    }

    // ── write_tcp_frame async ────────────────────────────────────────

    #[tokio::test]
    async fn write_tcp_frame_produces_valid_frame() {
        let msg = ServerMessage::Pong;
        let mut buf = Vec::new();
        write_tcp_frame(&mut buf, &msg).await.unwrap();

        let (payload, consumed) = try_decode_frame(&buf).unwrap().unwrap();
        assert_eq!(consumed, buf.len());
        let decoded: ServerMessage = bincode::deserialize(&payload).unwrap();
        assert!(matches!(decoded, ServerMessage::Pong));
    }
}
