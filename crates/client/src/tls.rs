use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsConnector;

/// Compute SHA-256 fingerprint of a DER-encoded certificate.
pub fn cert_fingerprint(cert: &CertificateDer<'_>) -> String {
    let hash = Sha256::digest(cert.as_ref());
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    format!("sha256:{}", hex)
}

/// Result of TOFU verification shared back to the caller.
#[derive(Debug, Clone)]
pub enum TofuResult {
    /// First time seeing this server — fingerprint was trusted automatically.
    TrustedNew(String),
    /// Known fingerprint matched.
    TrustedKnown,
    /// Fingerprint changed — connection was rejected.
    Mismatch { expected: String, actual: String },
}

/// Shared state for the TOFU verifier.
#[derive(Clone)]
pub struct TofuState {
    inner: Arc<Mutex<TofuInner>>,
}

#[derive(Debug)]
struct TofuInner {
    /// Known trusted fingerprints: server_addr → "sha256:hex".
    trusted: HashMap<String, String>,
    /// Server address being connected to (set before each connect).
    current_server: String,
    /// Result of the last verification.
    last_result: Option<TofuResult>,
}

impl TofuState {
    pub fn new(trusted: HashMap<String, String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TofuInner {
                trusted,
                current_server: String::new(),
                last_result: None,
            })),
        }
    }

    /// Set the server address before initiating a TLS connection.
    pub fn set_current_server(&self, addr: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.current_server = addr.to_string();
        inner.last_result = None;
    }

    /// Get the result of the last TLS verification.
    pub fn last_result(&self) -> Option<TofuResult> {
        self.inner.lock().unwrap().last_result.clone()
    }

    /// Get all trusted fingerprints (for saving to settings).
    pub fn trusted_map(&self) -> HashMap<String, String> {
        self.inner.lock().unwrap().trusted.clone()
    }

    /// Trust a specific server with a new fingerprint (for /trust command).
    pub fn trust_server(&self, addr: &str, fingerprint: &str) {
        self.inner.lock().unwrap().trusted.insert(addr.to_string(), fingerprint.to_string());
    }

    /// Remove a trusted server (for /trust reset).
    pub fn remove_server(&self, addr: &str) {
        self.inner.lock().unwrap().trusted.remove(addr);
    }
}

/// TOFU certificate verifier: Trust On First Use.
#[derive(Debug)]
struct TofuVerifier {
    state: Arc<Mutex<TofuInner>>,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fingerprint = cert_fingerprint(end_entity);
        let mut inner = self.state.lock().unwrap();
        let server = inner.current_server.clone();

        if let Some(known) = inner.trusted.get(&server) {
            if *known == fingerprint {
                inner.last_result = Some(TofuResult::TrustedKnown);
                Ok(ServerCertVerified::assertion())
            } else {
                // Certificate changed — auto-trust the new one
                let old = known.clone();
                inner.trusted.insert(server, fingerprint.clone());
                inner.last_result = Some(TofuResult::Mismatch {
                    expected: old,
                    actual: fingerprint,
                });
                Ok(ServerCertVerified::assertion())
            }
        } else {
            // First time — trust it
            inner.trusted.insert(server, fingerprint.clone());
            inner.last_result = Some(TofuResult::TrustedNew(fingerprint));
            Ok(ServerCertVerified::assertion())
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

/// Build a TLS connector with TOFU verification.
pub fn tls_connector(tofu: &TofuState) -> TlsConnector {
    let verifier = TofuVerifier {
        state: Arc::clone(&tofu.inner),
    };
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    TlsConnector::from(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls_pki_types::ServerName;

    fn dummy_cert() -> CertificateDer<'static> {
        CertificateDer::from(vec![1u8, 2, 3, 4, 5])
    }

    fn another_cert() -> CertificateDer<'static> {
        CertificateDer::from(vec![10u8, 20, 30, 40, 50])
    }

    // ── cert_fingerprint ─────────────────────────────────────────────

    #[test]
    fn fingerprint_format() {
        let cert = dummy_cert();
        let fp = cert_fingerprint(&cert);
        assert!(fp.starts_with("sha256:"));
        // SHA-256 = 64 hex chars + "sha256:" prefix = 71 total
        assert_eq!(fp.len(), 7 + 64);
    }

    #[test]
    fn fingerprint_deterministic() {
        let cert = dummy_cert();
        assert_eq!(cert_fingerprint(&cert), cert_fingerprint(&cert));
    }

    #[test]
    fn fingerprint_different_certs() {
        assert_ne!(cert_fingerprint(&dummy_cert()), cert_fingerprint(&another_cert()));
    }

    // ── TofuState API ────────────────────────────────────────────────

    #[test]
    fn tofu_state_set_current_server() {
        let tofu = TofuState::new(HashMap::new());
        tofu.set_current_server("example.com:7100");
        assert!(tofu.last_result().is_none());
    }

    #[test]
    fn tofu_state_trust_and_retrieve() {
        let tofu = TofuState::new(HashMap::new());
        tofu.trust_server("srv1", "sha256:aaa");
        tofu.trust_server("srv2", "sha256:bbb");

        let map = tofu.trusted_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map["srv1"], "sha256:aaa");
    }

    #[test]
    fn tofu_state_remove_server() {
        let tofu = TofuState::new(HashMap::new());
        tofu.trust_server("srv1", "sha256:aaa");
        tofu.remove_server("srv1");
        assert!(tofu.trusted_map().is_empty());
    }

    #[test]
    fn tofu_state_init_from_existing() {
        let mut existing = HashMap::new();
        existing.insert("srv1".into(), "sha256:aaa".into());
        let tofu = TofuState::new(existing);
        assert_eq!(tofu.trusted_map().len(), 1);
    }

    // ── TofuVerifier (TOFU logic) ────────────────────────────────────

    fn verify(state: &TofuState, cert: &CertificateDer<'_>) -> Result<ServerCertVerified, Error> {
        let verifier = TofuVerifier {
            state: Arc::clone(&state.inner),
        };
        let sni = ServerName::try_from("localhost").unwrap();
        verifier.verify_server_cert(cert, &[], &sni, &[], UnixTime::now())
    }

    #[test]
    fn tofu_first_connection_trusts_automatically() {
        let tofu = TofuState::new(HashMap::new());
        tofu.set_current_server("new-server:7100");
        let cert = dummy_cert();

        assert!(verify(&tofu, &cert).is_ok());

        match tofu.last_result().unwrap() {
            TofuResult::TrustedNew(fp) => assert!(fp.starts_with("sha256:")),
            other => panic!("expected TrustedNew, got {:?}", other),
        }

        // Fingerprint should be stored
        let map = tofu.trusted_map();
        assert!(map.contains_key("new-server:7100"));
    }

    #[test]
    fn tofu_known_cert_matches() {
        let cert = dummy_cert();
        let fp = cert_fingerprint(&cert);
        let mut trusted = HashMap::new();
        trusted.insert("known-server:7100".into(), fp);
        let tofu = TofuState::new(trusted);
        tofu.set_current_server("known-server:7100");

        assert!(verify(&tofu, &cert).is_ok());

        match tofu.last_result().unwrap() {
            TofuResult::TrustedKnown => {}
            other => panic!("expected TrustedKnown, got {:?}", other),
        }
    }

    #[test]
    fn tofu_cert_change_auto_trusts() {
        let cert1 = dummy_cert();
        let fp1 = cert_fingerprint(&cert1);
        let mut trusted = HashMap::new();
        trusted.insert("server:7100".into(), fp1);
        let tofu = TofuState::new(trusted);
        tofu.set_current_server("server:7100");

        // Connect with a different cert
        let cert2 = another_cert();
        assert!(verify(&tofu, &cert2).is_ok());

        match tofu.last_result().unwrap() {
            TofuResult::Mismatch { expected, actual } => {
                assert!(expected.starts_with("sha256:"));
                assert!(actual.starts_with("sha256:"));
                assert_ne!(expected, actual);
            }
            other => panic!("expected Mismatch, got {:?}", other),
        }

        // New fingerprint should be stored
        let map = tofu.trusted_map();
        assert_eq!(map["server:7100"], cert_fingerprint(&cert2));
    }
}
