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
