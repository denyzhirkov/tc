//! End-to-end test: two clients connect to a real server, join a channel,
//! exchange chat messages, and relay voice packets via UDP.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::{Buf, BytesMut};
use tokio::io::AsyncReadExt;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use tc_shared::{
    encode_udp_hello, try_decode_frame, write_tcp_frame, ClientMessage, ServerMessage, VoicePacket,
};

// ── Helpers ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct TestVerifier;

impl rustls::client::danger::ServerCertVerifier for TestVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

async fn read_server_msg<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    pending: &mut BytesMut,
) -> ServerMessage {
    loop {
        match try_decode_frame(pending) {
            Ok(Some((data, consumed))) => {
                pending.advance(consumed);
                return bincode::deserialize(&data).unwrap();
            }
            Ok(None) => {
                let mut buf = [0u8; 4096];
                let n = reader.read(&mut buf).await.unwrap();
                assert!(n > 0, "connection closed unexpectedly");
                pending.extend_from_slice(&buf[..n]);
            }
            Err(e) => panic!("frame error: {}", e),
        }
    }
}

struct TestClient<R, W> {
    reader: R,
    writer: W,
    pending: BytesMut,
}

impl<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin> TestClient<R, W> {
    async fn send(&mut self, msg: &ClientMessage) {
        write_tcp_frame(&mut self.writer, msg).await.unwrap();
    }

    async fn recv(&mut self) -> ServerMessage {
        read_server_msg(&mut self.reader, &mut self.pending).await
    }
}

// ── Server setup ─────────────────────────────────────────────────────

struct TestServer {
    tcp_addr: std::net::SocketAddr,
    udp_addr: std::net::SocketAddr,
    _tcp_handle: tokio::task::JoinHandle<()>,
    _udp_handle: tokio::task::JoinHandle<()>,
}

async fn start_server(bind_ip: std::net::IpAddr) -> TestServer {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Generate TLS cert
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = certified.cert.der().clone();
    let key_der = certified.key_pair.serialize_der();

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    // Find free ports by binding and immediately releasing. Binding on the
    // given IP family (127.0.0.1 or ::1) keeps TCP and UDP on the same family,
    // exactly as the client now does — the mismatch is what caused one-way audio.
    let tcp_listener = tokio::net::TcpListener::bind((bind_ip, 0)).await.unwrap();
    let tcp_addr = tcp_listener.local_addr().unwrap();
    drop(tcp_listener);

    let udp_sock = UdpSocket::bind((bind_ip, 0)).await.unwrap();
    let udp_addr = udp_sock.local_addr().unwrap();
    drop(udp_sock);

    let state = tc_server::state::ServerState::new(tc_server::state::Limits::default());
    let senders: tc_server::tcp::ClientSenders = Arc::new(RwLock::new(HashMap::new()));

    let tcp_state = state.clone();
    let tcp_senders = senders.clone();
    let ip_limiter = Arc::new(tc_server::rate_limit::IpRateLimiter::new(
        tc_shared::config::RATE_LIMIT_IP_CMD_PER_SEC,
        tc_shared::config::RATE_LIMIT_IP_CMD_BURST,
    ));
    let tcp_handle = tokio::spawn(async move {
        let _ = tc_server::tcp::run_tcp_server(
            tcp_state,
            tcp_senders,
            ip_limiter,
            tcp_addr.to_string(),
            acceptor,
        )
        .await;
    });

    let udp_state = state.clone();
    let udp_metrics = state.metrics().clone();
    let udp_handle = tokio::spawn(async move {
        let _ =
            tc_server::udp::run_udp_relay(udp_state, udp_metrics, udp_addr.to_string(), 1).await;
    });

    // Wait for TCP server to be ready (retry connect)
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(tcp_addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    TestServer {
        tcp_addr,
        udp_addr,
        _tcp_handle: tcp_handle,
        _udp_handle: udp_handle,
    }
}

async fn connect_client(
    tcp_addr: std::net::SocketAddr,
) -> TestClient<
    tokio::io::ReadHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
> {
    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TestVerifier))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

    let stream = tokio::net::TcpStream::connect(tcp_addr).await.unwrap();
    let domain = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(domain.to_owned(), stream).await.unwrap();
    let (reader, writer) = tokio::io::split(tls);

    TestClient {
        reader,
        writer,
        pending: BytesMut::new(),
    }
}

// ── Tests ────────────────────────────────────────────────────────────

/// IPv4 loopback: the original happy path.
#[tokio::test]
async fn two_clients_chat_and_voice() {
    run_two_client_voice("127.0.0.1".parse().unwrap()).await;
}

/// IPv6 loopback (`::1`): exercises the whole TCP+UDP register+relay path over
/// IPv6 — the environment where the Windows one-way-audio bug lived. Skipped
/// gracefully if the host has no usable IPv6 loopback (some CI runners).
#[tokio::test]
async fn two_clients_chat_and_voice_ipv6() {
    let ip: std::net::IpAddr = "::1".parse().unwrap();
    if tokio::net::TcpListener::bind((ip, 0)).await.is_err() {
        eprintln!("skipping IPv6 e2e: ::1 loopback unavailable on this host");
        return;
    }
    run_two_client_voice(ip).await;
}

/// Sound-check: one client starts an echo test and a voice packet it sends is
/// reflected straight back to *itself* (the server never excludes the sender on
/// an echo channel), proving the honest round-trip without a second peer.
#[tokio::test]
async fn echo_test_reflects_voice_to_sender() {
    let bind_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let server = start_server(bind_ip).await;

        let mut client = connect_client(server.tcp_addr).await;
        assert!(matches!(client.recv().await, ServerMessage::Welcome { .. }));
        client
            .send(&ClientMessage::Hello {
                version: "test".into(),
                protocol: tc_shared::config::PROTOCOL_VERSION,
                pubkey: None,
            })
            .await;

        client.send(&ClientMessage::StartEchoTest).await;
        let (channel_id, token) = match client.recv().await {
            ServerMessage::EchoTestReady {
                channel_id,
                udp_token,
                voice_key,
            } => {
                assert!(channel_id.starts_with(tc_shared::config::ECHO_CHANNEL_PREFIX));
                assert_eq!(voice_key.len(), 32);
                (channel_id, udp_token)
            }
            other => panic!("expected EchoTestReady, got {:?}", other),
        };

        // Register UDP + drain the hello ACK.
        let udp = UdpSocket::bind((bind_ip, 0)).await.unwrap();
        udp.connect(server.udp_addr).await.unwrap();
        udp.send(&encode_udp_hello(token)).await.unwrap();
        let mut ack = [0u8; 64];
        tokio::time::timeout(std::time::Duration::from_secs(2), udp.recv(&mut ack))
            .await
            .expect("hello ACK timed out")
            .unwrap();

        // A voice packet on the echo channel must come straight back to us.
        let voice = VoicePacket {
            sequence: 1,
            channel_id: channel_id.clone(),
            opus_data: vec![0xCD; 80],
        };
        let encoded = voice.encode();
        udp.send(&encoded).await.unwrap();

        let mut buf = [0u8; 2048];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), udp.recv(&mut buf))
            .await
            .expect("echo reflect timed out")
            .unwrap();
        assert_eq!(
            &buf[..n],
            &encoded[..],
            "echoed packet must be byte-identical"
        );

        client.send(&ClientMessage::StopEchoTest).await;
        assert!(matches!(client.recv().await, ServerMessage::LeftChannel));
    })
    .await
    .expect("echo e2e test timed out after 10s");
}

async fn run_two_client_voice(bind_ip: std::net::IpAddr) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let server = start_server(bind_ip).await;

        // ── Connect client A ──
        let mut client_a = connect_client(server.tcp_addr).await;
        let msg = client_a.recv().await;
        assert!(matches!(msg, ServerMessage::Welcome { .. }));

        client_a
            .send(&ClientMessage::Hello {
                version: "test".into(),
                protocol: tc_shared::config::PROTOCOL_VERSION,
                pubkey: None,
            })
            .await;

        // ── Connect client B ──
        let mut client_b = connect_client(server.tcp_addr).await;
        let msg = client_b.recv().await;
        assert!(matches!(msg, ServerMessage::Welcome { .. }));

        client_b
            .send(&ClientMessage::Hello {
                version: "test".into(),
                protocol: tc_shared::config::PROTOCOL_VERSION,
                pubkey: None,
            })
            .await;

        // ── A creates channel ──
        client_a
            .send(&ClientMessage::CreateChannel { name: None })
            .await;
        let channel_id = match client_a.recv().await {
            ServerMessage::ChannelCreated { channel_id } => channel_id,
            other => panic!("expected ChannelCreated, got {:?}", other),
        };

        // ── A joins channel ──
        client_a
            .send(&ClientMessage::JoinChannel {
                channel_id: channel_id.clone(),
            })
            .await;
        let (token_a, _voice_key) = match client_a.recv().await {
            ServerMessage::JoinedChannel {
                udp_token,
                voice_key,
                ..
            } => (udp_token, voice_key),
            other => panic!("expected JoinedChannel, got {:?}", other),
        };

        // ── B joins same channel ──
        client_b
            .send(&ClientMessage::JoinChannel {
                channel_id: channel_id.clone(),
            })
            .await;
        let token_b = match client_b.recv().await {
            ServerMessage::JoinedChannel { udp_token, .. } => udp_token,
            other => panic!("expected JoinedChannel, got {:?}", other),
        };

        // A should receive PeerJoined for B
        let msg = client_a.recv().await;
        assert!(
            matches!(msg, ServerMessage::PeerJoined { .. }),
            "expected PeerJoined, got {:?}",
            msg
        );

        // ── Chat: A sends message, B receives it ──
        client_a
            .send(&ClientMessage::ChatMessage {
                text: "hello from A".into(),
            })
            .await;
        let msg = client_b.recv().await;
        match msg {
            ServerMessage::ChatMessage { from, text } => {
                assert_eq!(text, "hello from A");
                assert!(!from.is_empty());
            }
            other => panic!("expected ChatMessage, got {:?}", other),
        }

        // ── UDP: register both clients (same IP family as the server) ──
        let udp_a = UdpSocket::bind((bind_ip, 0)).await.unwrap();
        udp_a.connect(server.udp_addr).await.unwrap();

        let udp_b = UdpSocket::bind((bind_ip, 0)).await.unwrap();
        udp_b.connect(server.udp_addr).await.unwrap();

        // Send UDP hello for A
        let hello_a = encode_udp_hello(token_a);
        udp_a.send(&hello_a).await.unwrap();
        let mut ack_buf = [0u8; 64];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), udp_a.recv(&mut ack_buf))
            .await
            .expect("UDP hello ACK timed out for A")
            .unwrap();
        assert_eq!(n, hello_a.len(), "ACK should echo the hello packet");

        // Send UDP hello for B
        let hello_b = encode_udp_hello(token_b);
        udp_b.send(&hello_b).await.unwrap();
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), udp_b.recv(&mut ack_buf))
            .await
            .expect("UDP hello ACK timed out for B")
            .unwrap();
        assert_eq!(n, hello_b.len(), "ACK should echo the hello packet");

        // ── Voice: A sends voice packet, B receives it via relay ──
        let voice = VoicePacket {
            sequence: 1,
            channel_id: channel_id.clone(),
            opus_data: vec![0xAA; 100], // fake opus data
        };
        let encoded_voice = voice.encode();
        udp_a.send(&encoded_voice).await.unwrap();

        let mut voice_buf = [0u8; 2048];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            udp_b.recv(&mut voice_buf),
        )
        .await
        .expect("voice relay timed out")
        .unwrap();

        // Verify the relayed packet is identical
        assert_eq!(&voice_buf[..n], &encoded_voice[..]);

        // ── A leaves channel, B gets PeerLeft ──
        client_a.send(&ClientMessage::LeaveChannel).await;
        let msg = client_a.recv().await;
        assert!(matches!(msg, ServerMessage::LeftChannel));

        let msg = client_b.recv().await;
        assert!(
            matches!(msg, ServerMessage::PeerLeft { .. }),
            "expected PeerLeft, got {:?}",
            msg
        );
    })
    .await
    .expect("e2e test timed out after 10s");
}
