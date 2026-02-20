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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_all_none() {
        let s = UserSettings::default();
        assert!(s.server.is_none());
        assert!(s.input_device.is_none());
        assert!(s.output_device.is_none());
        assert!(s.vad_level.is_none());
        assert!(s.input_gain.is_none());
        assert!(s.output_vol.is_none());
        assert!(s.name.is_none());
        assert!(s.trusted_servers.is_empty());
    }

    #[test]
    fn settings_toml_roundtrip() {
        let mut s = UserSettings::default();
        s.server = Some("example.com:7100".into());
        s.name = Some("alice".into());
        s.vad_level = Some(15);
        s.input_gain = Some(120);
        s.output_vol = Some(80);
        s.trusted_servers.insert("srv".into(), "sha256:abc".into());

        let toml_str = toml::to_string_pretty(&s).unwrap();
        let back: UserSettings = toml::from_str(&toml_str).unwrap();

        assert_eq!(back.server.as_deref(), Some("example.com:7100"));
        assert_eq!(back.name.as_deref(), Some("alice"));
        assert_eq!(back.vad_level, Some(15));
        assert_eq!(back.input_gain, Some(120));
        assert_eq!(back.output_vol, Some(80));
        assert_eq!(back.trusted_servers["srv"], "sha256:abc");
    }

    #[test]
    fn settings_deserialize_empty_toml() {
        let back: UserSettings = toml::from_str("").unwrap();
        assert!(back.server.is_none());
        assert!(back.trusted_servers.is_empty());
    }

    #[test]
    fn settings_skip_none_fields_on_serialize() {
        let s = UserSettings::default();
        let toml_str = toml::to_string_pretty(&s).unwrap();
        assert!(!toml_str.contains("server"));
        assert!(!toml_str.contains("trusted_servers"));
    }

    #[test]
    fn settings_deserialize_ignores_unknown_fields() {
        let toml_str = r#"
            server = "test:7100"
            unknown_field = "ignored"
        "#;
        let s: UserSettings = toml::from_str(toml_str).unwrap();
        assert_eq!(s.server.as_deref(), Some("test:7100"));
    }
}
