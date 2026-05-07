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
    /// "open" | "vad" | "ptt" — capture-side voice mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_mode: Option<String>,
    /// Hotkey bindings (action → accelerator string, e.g. "CommandOrControl+Shift+M").
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hotkeys: HashMap<String, String>,
    /// OS notifications on peer-join / chat. None = default (true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications: Option<bool>,
    /// Launch on system login. None = default (false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
    /// Hide window to tray instead of closing on the close button.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_to_tray: Option<bool>,
    /// UI language code: "en" or "ru". None = default ("en").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// TOFU: trusted server certificate fingerprints (server_addr → "sha256:hex").
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub trusted_servers: HashMap<String, String>,
    /// Known servers (registry shown by /servers; updated on every connect).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerEntry>,
    /// Known DM peers (pubkey-hex → last name + last_seen).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dm_peers: Vec<DmPeer>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DmPeer {
    /// 32-byte pubkey, hex-encoded.
    pub pubkey_hex: String,
    /// Last display name we saw this peer use.
    pub name: String,
    pub last_seen_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerEntry {
    pub addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_channel: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub favourite: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_used_unix: u64,
}

fn is_false(b: &bool) -> bool { !*b }
fn is_zero(n: &u64) -> bool { *n == 0 }

pub fn config_dir() -> PathBuf {
    // XDG: ~/.config/tc/ (Linux/macOS) or %APPDATA%/tc/ (Windows).
    // If a legacy ~/.config/termicall/ directory exists and the new one does
    // not, contents are migrated on first access.
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs_next().join(".config")
            })
    };
    let new_dir = base.join("tc");
    let legacy_dir = base.join("termicall");
    migrate_legacy_dir(&legacy_dir, &new_dir);
    new_dir
}

/// One-time migration: copy every file from `~/.config/termicall/` into
/// `~/.config/tc/` if the new directory does not yet exist. The legacy
/// directory is left in place (read-only safety; user can delete manually).
fn migrate_legacy_dir(legacy: &PathBuf, new: &PathBuf) {
    if !legacy.exists() || new.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new) {
        tracing::warn!("could not create config dir {}: {}", new.display(), e);
        return;
    }
    match std::fs::read_dir(legacy) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let from = entry.path();
                if let Some(name) = from.file_name() {
                    let to = new.join(name);
                    if let Err(e) = std::fs::copy(&from, &to) {
                        tracing::warn!(
                            "migration: failed to copy {} → {}: {}",
                            from.display(),
                            to.display(),
                            e
                        );
                    }
                }
            }
            tracing::info!(
                "migrated config from {} to {}",
                legacy.display(),
                new.display()
            );
        }
        Err(e) => tracing::warn!("could not read legacy dir {}: {}", legacy.display(), e),
    }
}

fn dirs_next() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn settings_path() -> PathBuf {
    // Check legacy path first (next to binary) for migration
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(FILE_NAME)));
    if let Some(ref legacy_path) = legacy {
        if legacy_path.exists() {
            let new_dir = config_dir();
            let new_path = new_dir.join(FILE_NAME);
            if !new_path.exists() {
                // Migrate: copy legacy config to XDG path
                if std::fs::create_dir_all(&new_dir).is_ok() {
                    if std::fs::copy(legacy_path, &new_path).is_ok() {
                        let _ = std::fs::remove_file(legacy_path);
                        tracing::info!(
                            "migrated config from {} to {}",
                            legacy_path.display(),
                            new_path.display()
                        );
                        return new_path;
                    }
                }
            }
            // If new path already exists, prefer it (ignore legacy)
            if new_path.exists() {
                return new_path;
            }
            // Fallback: if migration failed, use legacy
            return legacy_path.clone();
        }
    }

    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join(FILE_NAME)
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
