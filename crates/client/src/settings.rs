use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "tc.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_level: Option<u32>,
    /// Microphone gain (0–200, default 100 = 1.0×).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_gain: Option<u32>,
    /// Incoming audio volume (0–200, default 100 = 1.0×).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_vol: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// TOFU: trusted server certificate fingerprints (server_addr → "sha256:hex").
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trusted_servers: HashMap<String, String>,
}

fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

impl UserSettings {
    pub fn load() -> Self {
        let path = settings_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        match toml::to_string_pretty(self) {
            Ok(contents) => {
                if let Err(e) = std::fs::write(&path, contents) {
                    tracing::warn!("failed to save settings to {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                tracing::warn!("failed to serialize settings: {}", e);
            }
        }
    }
}
