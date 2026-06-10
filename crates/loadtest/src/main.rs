//! tc-loadtest — local load generator for the tc voice server.
//!
//! Spawns N simulated clients across K channels. Each client:
//!   1. Performs a TLS handshake (cert verification disabled — dev only).
//!   2. Sends Hello + JoinChannel and waits for `JoinedChannel { udp_token }`.
//!   3. Opens a UDP socket, sends a hello with the token, awaits the ACK.
//!   4. Pumps voice packets at `--pps` for `--duration` seconds.
//!   5. Concurrently receives forwarded packets from peers in its channel.
//!
//! At the end the tool prints per-client and aggregate stats (sent / received,
//! bytes, expected fan-out, drop rate). Use it to sanity-check throughput
//! changes or measure capacity ceilings on the local box.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use clap::Parser;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tc_shared::{
    config, encode_tcp_frame, encode_udp_hello, extract_frames, ClientMessage, ServerMessage,
    VoicePacket,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::TlsConnector;

#[derive(Parser, Debug)]
#[command(name = "tc-loadtest", about = "Local load generator for tc-server")]
struct Args {
    /// Server hostname or IP.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// TCP control port.
    #[arg(long, default_value_t = config::TCP_PORT)]
    tcp_port: u16,

    /// UDP voice port.
    #[arg(long, default_value_t = config::UDP_PORT)]
    udp_port: u16,

    /// Number of simulated clients.
    #[arg(long, default_value_t = 10)]
    clients: usize,

    /// Number of distinct channels to spread clients across.
    #[arg(long, default_value_t = 1)]
    channels: usize,

    /// Voice packets per second per client.
    #[arg(long, default_value_t = 50)]
    pps: u32,

    /// Voice payload bytes per packet (excluding header).
    #[arg(long, default_value_t = 200)]
    payload: usize,

    /// Test duration in seconds (excluding warmup).
    #[arg(long, default_value_t = 30)]
    duration: u64,

    /// Warmup before counting in seconds.
    #[arg(long, default_value_t = 2)]
    warmup: u64,

    /// Stagger client start by this many millis to avoid thundering herd.
    #[arg(long, default_value_t = 5)]
    stagger_ms: u64,

    /// Log level.
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[derive(Default)]
struct ClientStats {
    udp_sent: AtomicU64,
    udp_sent_bytes: AtomicU64,
    udp_recv: AtomicU64,
    udp_recv_bytes: AtomicU64,
    join_failed: AtomicU64,
    udp_handshake_failed: AtomicU64,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let directive = format!("tc_loadtest={}", args.log_level);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(directive.parse()?),
        )
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("rustls provider already installed"))?;

    if args.channels == 0 || args.clients == 0 {
        return Err(anyhow!("--channels and --clients must be > 0"));
    }

    let tcp_addr: SocketAddr = format!("{}:{}", args.host, args.tcp_port).parse()?;
    let udp_addr: SocketAddr = format!("{}:{}", args.host, args.udp_port).parse()?;

    let connector = make_tls_connector()?;

    // Pre-create one channel per slot using a throwaway client connection.
    // Each loadtest run uses freshly-created channels so we don't collide with
    // anything else hitting the server.
    tracing::info!("creating {} channel(s)…", args.channels);
    let channel_ids = create_channels(&connector, &args, tcp_addr, args.channels).await?;
    tracing::info!(channels = ?channel_ids, "channels ready");

    let stats = Arc::new(ClientStats::default());
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let total_secs = args.warmup + args.duration;
    tracing::info!(
        clients = args.clients,
        per_channel = args.clients as f64 / args.channels as f64,
        pps = args.pps,
        payload = args.payload,
        duration_s = args.duration,
        warmup_s = args.warmup,
        "starting clients"
    );

    let mut handles = Vec::with_capacity(args.clients);
    for i in 0..args.clients {
        let channel = channel_ids[i % args.channels].clone();
        let connector = connector.clone();
        let stats = stats.clone();
        let stop = stop_flag.clone();
        let host = args.host.clone();
        let pps = args.pps;
        let payload = args.payload;
        let warmup = args.warmup;
        let stagger = args.stagger_ms;
        handles.push(tokio::spawn(async move {
            // Spread out start so we don't slam the listener.
            tokio::time::sleep(Duration::from_millis(stagger * i as u64)).await;
            if let Err(e) = run_client(
                i, host, tcp_addr, udp_addr, channel, connector, pps, payload, warmup, stop, stats,
            )
            .await
            {
                tracing::warn!(client = i, "client failed: {:#}", e);
            }
        }));
    }

    // Run for the full duration, then signal stop.
    tokio::time::sleep(Duration::from_secs(total_secs)).await;
    stop_flag.store(true, Ordering::Relaxed);

    // Give clients a moment to finish and tally results.
    let _ = tokio::time::timeout(Duration::from_secs(5), futures_join(handles)).await;

    print_summary(&args, &stats);
    Ok(())
}

/// Drive the per-client lifecycle.
#[allow(clippy::too_many_arguments)]
async fn run_client(
    idx: usize,
    host: String,
    tcp_addr: SocketAddr,
    udp_addr: SocketAddr,
    channel: String,
    connector: TlsConnector,
    pps: u32,
    payload: usize,
    warmup_secs: u64,
    stop: Arc<std::sync::atomic::AtomicBool>,
    stats: Arc<ClientStats>,
) -> Result<()> {
    // 1. TCP + TLS
    let stream = TcpStream::connect(tcp_addr).await?;
    let server_name = ServerName::try_from(host).map_err(|e| anyhow!("invalid SNI: {e}"))?;
    let mut tls = connector.connect(server_name, stream).await?;

    // 2. Hello (no pubkey for loadtest)
    write_msg(
        &mut tls,
        &ClientMessage::Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: config::PROTOCOL_VERSION,
            pubkey: None,
        },
    )
    .await?;

    // 3. Join the assigned channel
    write_msg(
        &mut tls,
        &ClientMessage::JoinChannel {
            channel_id: channel.clone(),
        },
    )
    .await?;

    // 4. Read frames until JoinedChannel arrives (also drain Welcome etc.)
    let udp_token = match read_until_joined(&mut tls).await {
        Ok(t) => t,
        Err(e) => {
            stats.join_failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    };

    // 5. UDP socket + hello with token, await ACK
    let udp = UdpSocket::bind("0.0.0.0:0").await?;
    udp.connect(udp_addr).await?;
    let hello = encode_udp_hello(udp_token);
    udp.send(&hello).await?;
    let mut ack_buf = [0u8; 64];
    match tokio::time::timeout(Duration::from_secs(2), udp.recv(&mut ack_buf)).await {
        Ok(Ok(_)) => {}
        _ => {
            stats.udp_handshake_failed.fetch_add(1, Ordering::Relaxed);
            return Err(anyhow!("UDP handshake timed out"));
        }
    }

    // 6. After warmup, start counting. Run sender + receiver concurrently.
    let warmup_until = Instant::now() + Duration::from_secs(warmup_secs);

    let recv_stats = stats.clone();
    let recv_stop = stop.clone();
    let udp_recv = Arc::new(udp);
    let udp_send = udp_recv.clone();
    let recv_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            if recv_stop.load(Ordering::Relaxed) {
                return;
            }
            // recv errors and timeouts both just mean "retry".
            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(200), udp_recv.recv(&mut buf)).await
            {
                if Instant::now() >= warmup_until {
                    recv_stats.udp_recv.fetch_add(1, Ordering::Relaxed);
                    recv_stats
                        .udp_recv_bytes
                        .fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    });

    // 7. Sender: timer at fixed rate, send VoicePacket each tick.
    let interval_ns = 1_000_000_000u64 / pps.max(1) as u64;
    let mut tick = tokio::time::interval(Duration::from_nanos(interval_ns));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut seq: u32 = 1;
    let opus_payload = vec![0xAB; payload];
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        let pkt = VoicePacket {
            sequence: seq,
            channel_id: channel.clone(),
            opus_data: opus_payload.clone(),
        };
        let bytes = pkt.encode();
        match udp_send.send(&bytes).await {
            Ok(_) => {
                if Instant::now() >= warmup_until {
                    stats.udp_sent.fetch_add(1, Ordering::Relaxed);
                    stats
                        .udp_sent_bytes
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                }
            }
            Err(e) => {
                tracing::debug!(client = idx, "UDP send error: {}", e);
            }
        }
        seq = seq.wrapping_add(1);
        if seq == 0 {
            seq = 1;
        }
    }

    recv_task.abort();
    Ok(())
}

/// Send a length-prefixed bincode frame.
async fn write_msg(
    tls: &mut tokio_rustls::client::TlsStream<TcpStream>,
    msg: &ClientMessage,
) -> Result<()> {
    let frame = encode_tcp_frame(msg)?;
    tls.write_all(&frame).await?;
    tls.flush().await?;
    Ok(())
}

/// Drain the TCP stream until we see `JoinedChannel`, returning the udp_token.
async fn read_until_joined(tls: &mut tokio_rustls::client::TlsStream<TcpStream>) -> Result<u64> {
    let mut pending = BytesMut::new();
    let mut buf = vec![0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for JoinedChannel"));
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        let n = tokio::time::timeout(timeout, tls.read(&mut buf))
            .await
            .map_err(|_| anyhow!("read timeout"))?
            .context("tls read")?;
        if n == 0 {
            return Err(anyhow!("server closed before JoinedChannel"));
        }
        pending.extend_from_slice(&buf[..n]);
        let frames = extract_frames(&mut pending)?;
        for frame in frames {
            let msg: ServerMessage = bincode::deserialize(&frame)?;
            match msg {
                ServerMessage::JoinedChannel { udp_token, .. } => return Ok(udp_token),
                ServerMessage::Error { message } => return Err(anyhow!("server error: {message}")),
                _ => {} // welcome, peer events, etc — ignore
            }
        }
    }
}

/// Create N channels by issuing CreateChannel from a single helper connection.
async fn create_channels(
    connector: &TlsConnector,
    args: &Args,
    tcp_addr: SocketAddr,
    n: usize,
) -> Result<Vec<String>> {
    let stream = TcpStream::connect(tcp_addr).await?;
    let server_name =
        ServerName::try_from(args.host.clone()).map_err(|e| anyhow!("invalid SNI: {e}"))?;
    let mut tls = connector.connect(server_name, stream).await?;

    write_msg(
        &mut tls,
        &ClientMessage::Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: config::PROTOCOL_VERSION,
            pubkey: None,
        },
    )
    .await?;

    let mut ids = Vec::with_capacity(n);
    let mut pending = BytesMut::new();
    let mut buf = vec![0u8; 4096];

    for _ in 0..n {
        write_msg(&mut tls, &ClientMessage::CreateChannel { name: None }).await?;

        // Read until ChannelCreated comes back. Server's create rate limit
        // is strict (0.1/s burst 3) so for n>3 we wait between requests.
        loop {
            let n_read = tls.read(&mut buf).await?;
            if n_read == 0 {
                return Err(anyhow!("server closed during CreateChannel"));
            }
            pending.extend_from_slice(&buf[..n_read]);
            let frames = extract_frames(&mut pending)?;
            let mut got = false;
            for frame in frames {
                let msg: ServerMessage = bincode::deserialize(&frame)?;
                match msg {
                    ServerMessage::ChannelCreated { channel_id } => {
                        ids.push(channel_id);
                        got = true;
                    }
                    ServerMessage::Error { message } => {
                        // Rate limited — back off and retry. The per-IP limiter
                        // means we have to wait for refill on the *server* clock.
                        tracing::debug!("create rate limited: {message}, backing off 11s");
                        tokio::time::sleep(Duration::from_secs(11)).await;
                        write_msg(&mut tls, &ClientMessage::CreateChannel { name: None }).await?;
                    }
                    _ => {}
                }
            }
            if got {
                break;
            }
        }
    }

    let _ = tls.shutdown().await;
    Ok(ids)
}

fn print_summary(args: &Args, stats: &ClientStats) {
    let sent = stats.udp_sent.load(Ordering::Relaxed);
    let sent_bytes = stats.udp_sent_bytes.load(Ordering::Relaxed);
    let recv = stats.udp_recv.load(Ordering::Relaxed);
    let recv_bytes = stats.udp_recv_bytes.load(Ordering::Relaxed);
    let join_failed = stats.join_failed.load(Ordering::Relaxed);
    let udp_failed = stats.udp_handshake_failed.load(Ordering::Relaxed);

    let dur = args.duration.max(1) as f64;
    // Expected: each client sends pps × duration, each peer in same channel receives one.
    // Per-channel peers ≈ ceil(clients / channels). Each client receives (peers - 1) × pps × dur.
    let per_chan = (args.clients as f64 / args.channels as f64).ceil() as u64;
    let expected_recv_per_client = per_chan.saturating_sub(1) * args.pps as u64 * args.duration;
    let expected_recv_total = expected_recv_per_client * args.clients as u64;

    println!();
    println!("================ tc-loadtest summary ================");
    println!("clients          : {}", args.clients);
    println!(
        "channels         : {} ({:.1} clients/chan)",
        args.channels,
        args.clients as f64 / args.channels as f64
    );
    println!("target pps/client: {}", args.pps);
    println!(
        "duration         : {}s (warmup {}s)",
        args.duration, args.warmup
    );
    println!("payload bytes    : {}", args.payload);
    println!("-----------------------------------------------------");
    println!(
        "UDP sent         : {} pkts ({:.2} MB) ≈ {:.0} pps total",
        sent,
        sent_bytes as f64 / 1e6,
        sent as f64 / dur
    );
    println!(
        "UDP recv         : {} pkts ({:.2} MB) ≈ {:.0} pps total",
        recv,
        recv_bytes as f64 / 1e6,
        recv as f64 / dur
    );
    println!(
        "expected recv    : {} pkts (per-client {})",
        expected_recv_total, expected_recv_per_client
    );
    if expected_recv_total > 0 {
        let pct = recv as f64 / expected_recv_total as f64 * 100.0;
        println!("delivery ratio   : {:.2}%", pct);
    }
    if join_failed > 0 || udp_failed > 0 {
        println!(
            "failures         : join={} udp_hello={}",
            join_failed, udp_failed
        );
    }
    println!("=====================================================");
}

// ── TLS plumbing ────────────────────────────────────────────────────

fn make_tls_connector() -> Result<TlsConnector> {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

// ── futures_join: minimal join-all without bringing in `futures` ────

async fn futures_join<T>(handles: Vec<tokio::task::JoinHandle<T>>) {
    for h in handles {
        let _ = h.await;
    }
}
