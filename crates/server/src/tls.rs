use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

const CERT_FILENAME: &str = "tc-cert.pem";
const KEY_FILENAME: &str = "tc-key.pem";

/// Returns the directory next to the current executable, falling back to cwd.
fn cert_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Load existing cert/key from PEM files, or generate a self-signed pair.
pub fn load_or_generate_tls_config() -> Result<Arc<ServerConfig>> {
    let dir = cert_dir();
    let cert_path = dir.join(CERT_FILENAME);
    let key_path = dir.join(KEY_FILENAME);

    let (cert_pem, key_pem) = if cert_path.exists() && key_path.exists() {
        tracing::info!("loading TLS cert from {}", cert_path.display());
        let cert_pem = std::fs::read(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key_pem = std::fs::read(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        (cert_pem, key_pem)
    } else {
        tracing::info!("generating self-signed TLS certificate");
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let cert = rcgen::generate_simple_self_signed(subject_alt_names)
            .context("generating self-signed certificate")?;

        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();

        std::fs::write(&cert_path, &cert_pem)
            .with_context(|| format!("writing {}", cert_path.display()))?;
        std::fs::write(&key_path, &key_pem)
            .with_context(|| format!("writing {}", key_path.display()))?;
        tracing::info!("saved TLS cert to {}", cert_path.display());

        (cert_pem.into_bytes(), key_pem.into_bytes())
    };

    // Parse PEM → DER
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()
        .context("parsing certificate PEM")?;

    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("parsing private key PEM")?
        .context("no private key found in PEM")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS ServerConfig")?;

    Ok(Arc::new(config))
}
