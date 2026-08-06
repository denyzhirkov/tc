//! Temporary diagnostic: join a channel as a silent listener and analyze who
//! is actually sending voice packets (per-name sequence streams).
//! Usage: cargo run -p tc-client --example probe_listen -- 13.140.128.194:7100 pub-123 20

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use tc_client::tls::{tls_connector, TofuState};
use tc_shared::{
    config, encode_tcp_frame, encode_udp_hello, try_decode_frame, ClientMessage, ServerMessage,
    VoicePacket,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "13.140.128.194:7100".into());
    let channel = args.next().unwrap_or_else(|| "pub-123".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);

    let tofu = TofuState::new(HashMap::new());
    tofu.set_current_server(&addr);
    let stream = TcpStream::connect(&addr).await?;
    let server_ip = stream.peer_addr()?.ip().to_canonical();
    let connector = tls_connector(&tofu);
    let server_name = rustls_pki_types::ServerName::try_from("tc-server")
        .unwrap()
        .to_owned();
    let mut tls = connector.connect(server_name, stream).await?;

    // Hello after Welcome, then join.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut hello_sent = false;
    let mut join_sent = false;
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
                        let hello = encode_tcp_frame(&ClientMessage::Hello {
                            version: "probe".into(),
                            protocol,
                            pubkey: None,
                        })?;
                        tls.write_all(&hello).await?;
                        let join = encode_tcp_frame(&ClientMessage::JoinChannel {
                            channel_id: channel.clone(),
                        })?;
                        tls.write_all(&join).await?;
                        tls.flush().await?;
                        hello_sent = true;
                        join_sent = true;
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
                ServerMessage::Error { message } => {
                    anyhow::bail!("server error: {message}");
                }
                other => println!("tcp: {other:?}"),
            }
        }
        if let Some(g) = got {
            break g;
        }
        anyhow::ensure!(join_sent || !hello_sent, "unreachable");
    };

    // UDP hello so the relay routes voice to us.
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
        println!("WARNING: no UDP hello ACK after 10 attempts — relay may not route to us");
    }
    println!("listening {secs}s on UDP …");

    let cipher =
        XChaCha20Poly1305::new_from_slice(&voice_key).map_err(|_| anyhow::anyhow!("bad key"))?;

    // name → (packets, decrypt_fails, last_seq, backward_jumps, distinct seq ranges seen)
    struct Track {
        packets: u64,
        last_seq: u32,
        backward_jumps: u64,
        min_seq: u32,
        max_seq: u32,
        first: Instant,
        last: Instant,
    }
    let mut tracks: HashMap<String, Track> = HashMap::new();
    let mut decrypt_fail = 0u64;
    let mut udp_buf = vec![0u8; config::MAX_UDP_PACKET + 64];

    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let n = match tokio::time::timeout(timeout, socket.recv(&mut udp_buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                println!("udp recv err: {e}");
                continue;
            }
            Err(_) => break,
        };
        let data = &udp_buf[..n];
        let Some((seq, encrypted)) = VoicePacket::parse_voice_data(data) else {
            continue;
        };
        if encrypted.len() < config::XCHACHA20_NONCE_SIZE + config::POLY1305_TAG_SIZE {
            continue;
        }
        let nonce = XNonce::from_slice(&encrypted[..config::XCHACHA20_NONCE_SIZE]);
        let Ok(plain) = cipher.decrypt(nonce, &encrypted[config::XCHACHA20_NONCE_SIZE..]) else {
            decrypt_fail += 1;
            continue;
        };
        if plain.is_empty() {
            continue;
        }
        let name_len = plain[0] as usize;
        let name = if plain.len() >= 1 + name_len {
            String::from_utf8_lossy(&plain[1..1 + name_len]).to_string()
        } else {
            "<bad>".into()
        };
        let opus_len = plain.len().saturating_sub(1 + name_len);
        let now = Instant::now();
        let t = tracks.entry(name).or_insert(Track {
            packets: 0,
            last_seq: seq,
            backward_jumps: 0,
            min_seq: seq,
            max_seq: seq,
            first: now,
            last: now,
        });
        t.packets += 1;
        t.last = now;
        let diff = seq.wrapping_sub(t.last_seq);
        // backward or huge forward jump = a second interleaved stream
        if diff == 0 || diff > 0x8000_0000 || diff > 5000 {
            t.backward_jumps += 1;
        }
        t.last_seq = seq;
        t.min_seq = t.min_seq.min(seq);
        t.max_seq = t.max_seq.max(seq);
        if t.packets <= 5 || t.packets % 100 == 0 {
            println!(
                "pkt name={:<12} seq={:<8} opus={}B",
                format!(
                    "{:?}",
                    String::from_utf8_lossy(&plain[1..1 + name_len.min(plain.len() - 1)])
                ),
                seq,
                opus_len
            );
        }
    }

    println!("\n── summary ──");
    println!("decrypt failures: {decrypt_fail}");
    for (name, t) in &tracks {
        let dur = t.last.duration_since(t.first).as_secs_f64().max(0.001);
        println!(
            "{name:?}: {} pkts over {:.1}s (~{:.0}/s), seq range {}..{}, stream-interleave jumps: {}",
            t.packets,
            dur,
            t.packets as f64 / dur,
            t.min_seq,
            t.max_seq,
            t.backward_jumps
        );
    }
    Ok(())
}
