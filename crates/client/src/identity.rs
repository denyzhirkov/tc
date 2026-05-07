//! Local Ed25519 identity persisted to the user's config directory.
//!
//! On first launch a new keypair is generated and saved to `<config>/identity.key`
//! as the raw 32-byte secret. The public key is what other peers see; the secret
//! never leaves the device.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

const FILE_NAME: &str = "identity.key";

/// A device-local identity backed by an Ed25519 keypair.
#[derive(Debug, Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Load the identity from disk, or generate and persist a new one if absent.
    pub fn load_or_generate() -> Result<Self> {
        let path = identity_path();
        if path.exists() {
            return Self::load_from(&path);
        }
        let id = Self::generate();
        id.save_to(&path).with_context(|| format!("write {}", path.display()))?;
        tracing::info!(path = %path.display(), "generated new identity");
        Ok(id)
    }

    fn generate() -> Self {
        let signing = SigningKey::generate(&mut OsRng);
        Self { signing }
    }

    fn load_from(path: &PathBuf) -> Result<Self> {
        let mut file = fs::File::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        let mut buf = [0u8; 32];
        file.read_exact(&mut buf)
            .with_context(|| format!("identity file must be exactly 32 bytes ({})", path.display()))?;
        Ok(Self { signing: SigningKey::from_bytes(&buf) })
    }

    fn save_to(&self, path: &PathBuf) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
        }
        let mut file = fs::File::create(path)?;
        file.write_all(&self.signing.to_bytes())?;
        Self::set_owner_only(&file)?;
        Ok(())
    }

    #[cfg(unix)]
    fn set_owner_only(file: &fs::File) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o600);
        file.set_permissions(perms)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn set_owner_only(_file: &fs::File) -> Result<()> {
        Ok(())
    }

    /// Raw 32-byte public key.
    pub fn pubkey(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Short hex fingerprint (first 8 bytes of sha256 of pubkey, lowercase hex).
    pub fn fingerprint(&self) -> String {
        let hash = Sha256::digest(self.pubkey());
        hash.iter().take(8).map(|b| format!("{:02x}", b)).collect()
    }
}

fn identity_path() -> PathBuf {
    super::settings::config_dir().join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_16_hex_chars() {
        let id = Identity::generate();
        let fp = id.fingerprint();
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn pubkey_is_32_bytes() {
        let id = Identity::generate();
        assert_eq!(id.pubkey().len(), 32);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let id = Identity::generate();
        id.save_to(&path.to_path_buf()).unwrap();
        let loaded = Identity::load_from(&path.to_path_buf()).unwrap();
        assert_eq!(id.pubkey(), loaded.pubkey());
    }
}
