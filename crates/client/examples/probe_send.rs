//! Temporary diagnostic: join a channel and TRANSMIT a real encrypted Opus
//! tone for N seconds as "probe-bot" — verifies the full receive path of the
//! other clients in the channel without needing a second human.
//! Usage: cargo run -p tc-client --example probe_send -- 13.140.128.194:7100 pub-123 6

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use tc_client::codec::OpusEncoder;
use tc_client::tls::{tls_connector, TofuState};
use tc_shared::{
    config, encode_tcp_frame, encode_udp_hello, try_decode_frame, ClientMessage, ServerMessage,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "13.140.128.194:7100".into());
    let channel = args.next().unwrap_or_else(|| "pub-123".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);

    let tofu = TofuState::new(HashMap::new());
    tofu.set_current_server(&addr);
    let stream = TcpStream::connect(&addr).await?;
    let server_ip = stream.peer_addr()?.ip().to_canonical();
    let connector = tls_connector(&tofu);
    let server_name = rustls_pki_types::ServerName::try_from("tc-server")
        .unwrap()
        .to_owned();
    let mut tls = connector.connect(server_name, stream).await?;

    // Welcome → Hello + SetName + Join → JoinedChannel.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut hello_sent = false;
    let (udp_token, voice_key) = loop {
        let n = tls.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "server closed connection");
        buf.extend_from_slice(&chunk[..n]);
        let mut got: Option<(u64, Vec<u8>)> = None;
        while let Some((frame, consumed)) = try_decode_frame(&buf)? {
            buf.drain(..consumed);
            match bincode::deserialize::<ServerMessage>(&frame)? {
                ServerMessage::Welcome { version, protocol } => {
                    println!("server: v{version} protocol {protocol}");
                    if !hello_sent {
                        for msg in [
                            ClientMessage::Hello {
                                version: "probe".into(),
                                protocol,
                                pubkey: None,
                            },
                            ClientMessage::SetName {
                                name: "probe-bot".into(),
                            },
                            ClientMessage::JoinChannel {
                                channel_id: channel.clone(),
                            },
                        ] {
                            tls.write_all(&encode_tcp_frame(&msg)?).await?;
                        }
                        tls.flush().await?;
                        hello_sent = true;
                    }
                }
                ServerMessage::JoinedChannel {
                    channel_id,
                    participants,
                    udp_token,
                    voice_key,
                } => {
                    println!(
                        "joined {channel_id}; participants ({}): {participants:?}",
                        participants.len()
                    );
                    got = Some((udp_token, voice_key));
                }
                ServerMessage::Error { message } => anyhow::bail!("server error: {message}"),
                other => println!("tcp: {other:?}"),
            }
        }
        if let Some(g) = got {
            break g;
        }
    };

    // UDP hello with ACK retry.
    let udp_addr = std::net::SocketAddr::new(server_ip, config::UDP_PORT);
    let bind: std::net::SocketAddr = if server_ip.is_ipv6() {
        "[::]:0".parse().unwrap()
    } else {
        "0.0.0.0:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind).await?;
    socket.connect(udp_addr).await?;
    let hello = encode_udp_hello(udp_token);
    let mut ack = [0u8; 16];
    let mut registered = false;
    for attempt in 1..=10 {
        socket.send(&hello).await?;
        match tokio::time::timeout(Duration::from_millis(500), socket.recv(&mut ack)).await {
            Ok(Ok(n)) if tc_shared::decode_udp_hello(&ack[..n]) == Some(udp_token) => {
                println!("UDP registered (ACK on attempt {attempt})");
                registered = true;
                break;
            }
            _ => {}
        }
    }
    if !registered {
        println!("WARNING: no hello ACK — sending anyway");
    }

    // Transmit a 440 Hz tone: FRAME_SIZE @ 48 kHz mono, 50 packets/s.
    let cipher =
        XChaCha20Poly1305::new_from_slice(&voice_key).map_err(|_| anyhow::anyhow!("bad key"))?;
    let mut encoder = OpusEncoder::new()?;
    let name = b"probe-bot";
    let mut pcm = vec![0.0f32; config::FRAME_SIZE];
    let mut phase = 0.0f32;
    let step = 440.0 / config::SAMPLE_RATE as f32;

    let frames = secs * 50;
    println!("transmitting {secs}s of 440 Hz tone as \"probe-bot\" …");
    let started = Instant::now();
    let mut sent = 0u64;
    for seq in 1..=frames as u32 {
        for s in pcm.iter_mut() {
            *s = (phase * std::f32::consts::TAU).sin() * 0.25;
            phase = (phase + step) % 1.0;
        }
        let opus = encoder.encode(&pcm)?;

        // [seq:4][id_len:1][channel_id][nonce:24][enc(name_len+name+opus)][tag:16]
        let mut pkt = Vec::with_capacity(config::MAX_UDP_PACKET);
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.push(channel.len() as u8);
        pkt.extend_from_slice(channel.as_bytes());
        let nonce_bytes: [u8; config::XCHACHA20_NONCE_SIZE] = rand::random();
        pkt.extend_from_slice(&nonce_bytes);
        let plaintext_start = pkt.len();
        pkt.push(name.len() as u8);
        pkt.extend_from_slice(name);
        pkt.extend_from_slice(opus);
        let tag = cipher
            .encrypt_in_place_detached(&XNonce::from(nonce_bytes), b"", &mut pkt[plaintext_start..])
            .map_err(|_| anyhow::anyhow!("encrypt failed"))?;
        pkt.extend_from_slice(tag.as_slice());

        socket.send(&pkt).await?;
        sent += 1;

        let target = started + Duration::from_millis(20 * seq as u64);
        if let Some(d) = target.checked_duration_since(Instant::now()) {
            tokio::time::sleep(d).await;
        }
    }
    println!(
        "done: {sent} packets sent over {:.1}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
