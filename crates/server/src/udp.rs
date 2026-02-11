use anyhow::Result;
use tokio::net::UdpSocket;

use tc_shared::config;
use tc_shared::{decode_udp_hello, VoicePacket};

use crate::state::ServerState;

/// Start the UDP voice relay server.
pub async fn run_udp_relay(state: ServerState, addr: String) -> Result<()> {
    let socket = UdpSocket::bind(&addr).await?;
    tracing::info!("UDP listening on {}", addr);

    let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];

    loop {
        let (len, src_addr) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("UDP recv error: {}", e);
                continue;
            }
        };

        let data = &buf[..len];

        // Check for hello packet (token-based UDP registration)
        if let Some(token) = decode_udp_hello(data) {
            if state.register_udp_by_token(token, src_addr).await {
                tracing::debug!(%src_addr, "UDP hello registered via token");
            } else {
                tracing::debug!(%src_addr, "UDP hello with invalid token");
            }
            continue;
        }

        // Parse only the channel_id without copying opus_data
        let channel_id = match VoicePacket::parse_channel_id(data) {
            Some(id) => id,
            None => continue,
        };

        // Relay raw bytes to all other participants in the channel
        let peers = state.get_channel_peers(channel_id, &src_addr).await;
        for peer_addr in peers {
            if let Err(e) = socket.send_to(data, peer_addr).await {
                tracing::trace!("UDP send to {} failed: {}", peer_addr, e);
            }
        }
    }
}
