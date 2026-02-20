use std::sync::Arc;

use bytes::{Buf, BytesMut};
use tokio::io::AsyncReadExt;

use tc_shared::{
    extract_frames, try_decode_frame, write_tcp_frame, ClientMessage, ServerMessage,
};

/// Accept-all TLS certificate verifier for testing.
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

/// Read exactly one ServerMessage from a TLS stream.
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

#[tokio::test]
async fn tls_handshake_and_protocol_exchange() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        // Install crypto provider (idempotent)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Generate self-signed cert
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = certified.cert.der().clone();
        let key_der = certified.key_pair.serialize_der();

        // Server TLS config
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_der],
                rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        // Bind to random port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn minimal server: accept one connection, handle protocol
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(tls);

            // Send Welcome
            write_tcp_frame(
                &mut writer,
                &ServerMessage::Welcome {
                    version: "test".into(),
                    protocol: tc_shared::config::PROTOCOL_VERSION,
                },
            )
            .await
            .unwrap();

            // Read and respond to client messages
            let mut buf = [0u8; 4096];
            let mut pending = BytesMut::new();
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n]);
                let frames = extract_frames(&mut pending).unwrap();
                for frame in frames {
                    let msg: ClientMessage = bincode::deserialize(&frame).unwrap();
                    match msg {
                        ClientMessage::Hello { .. } => {}
                        ClientMessage::CreateChannel { .. } => {
                            write_tcp_frame(
                                &mut writer,
                                &ServerMessage::ChannelCreated {
                                    channel_id: "test1".into(),
                                },
                            )
                            .await
                            .unwrap();
                        }
                        ClientMessage::JoinChannel { channel_id } => {
                            write_tcp_frame(
                                &mut writer,
                                &ServerMessage::JoinedChannel {
                                    channel_id,
                                    participants: vec!["user-1".into()],
                                    udp_token: 12345,
                                    voice_key: vec![0u8; 32],
                                },
                            )
                            .await
                            .unwrap();
                        }
                        _ => {}
                    }
                }
            }
        });

        // Client: connect with TLS
        let client_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TestVerifier))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let domain = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let tls = connector.connect(domain.to_owned(), stream).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(tls);
        let mut pending = BytesMut::new();

        // 1. Receive Welcome
        let msg = read_server_msg(&mut reader, &mut pending).await;
        assert!(
            matches!(msg, ServerMessage::Welcome { .. }),
            "expected Welcome, got {:?}",
            msg
        );

        // 2. Send Hello
        write_tcp_frame(
            &mut writer,
            &ClientMessage::Hello {
                version: "test".into(),
                protocol: tc_shared::config::PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();

        // 3. Send CreateChannel
        write_tcp_frame(&mut writer, &ClientMessage::CreateChannel { name: None })
            .await
            .unwrap();

        // 4. Receive ChannelCreated
        let msg = read_server_msg(&mut reader, &mut pending).await;
        let channel_id = match msg {
            ServerMessage::ChannelCreated { channel_id } => channel_id,
            other => panic!("expected ChannelCreated, got {:?}", other),
        };

        // 5. Send JoinChannel
        write_tcp_frame(
            &mut writer,
            &ClientMessage::JoinChannel {
                channel_id: channel_id.clone(),
            },
        )
        .await
        .unwrap();

        // 6. Receive JoinedChannel
        let msg = read_server_msg(&mut reader, &mut pending).await;
        match msg {
            ServerMessage::JoinedChannel {
                channel_id: joined_id,
                participants,
                udp_token,
                voice_key,
            } => {
                assert_eq!(joined_id, channel_id);
                assert!(!participants.is_empty());
                assert_ne!(udp_token, 0);
                assert_eq!(voice_key.len(), 32);
            }
            other => panic!("expected JoinedChannel, got {:?}", other),
        }

        // Cleanup: drop client so server loop exits
        drop(writer);
        drop(reader);
        let _ = server.await;
    })
    .await
    .expect("test timed out after 5s");
}
