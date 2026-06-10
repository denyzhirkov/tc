//! Local-only chat history per (server, channel).
//!
//! Storage: JSON-Lines under `<config>/history/<sha256(server|channel)>.jsonl`.
//! Append-only; reads tail the file and parse the last `limit` lines.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::state::AppCore;

type CoreState<'a> = tauri::State<'a, std::sync::Arc<Mutex<AppCore>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Chat,
    Dm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatLine {
    pub ts_unix: u64,
    pub kind: LineKind,
    pub from: String,
    pub text: String,
}

/// Append a single line. Errors are logged but never bubble — history loss is
/// always recoverable, but we don't want to drop UI events because of disk issues.
pub fn append(server: &str, channel: &str, line: &ChatLine) {
    if let Err(e) = append_inner(server, channel, line) {
        tracing::warn!("history append failed: {}", e);
    }
}

fn append_inner(server: &str, channel: &str, line: &ChatLine) -> std::io::Result<()> {
    let path = path_for(server, channel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let mut s = serde_json::to_string(line).map_err(std::io::Error::other)?;
    s.push('\n');
    f.write_all(s.as_bytes())?;
    Ok(())
}

#[tauri::command]
pub async fn get_history(
    server: String,
    channel: String,
    limit: Option<usize>,
) -> Result<Vec<ChatLine>, String> {
    let limit = limit.unwrap_or(50).min(500);
    let path = path_for(&server, &channel);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).map_err(|e| format!("open {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut lines: Vec<ChatLine> = Vec::new();
    for raw in reader.lines().map_while(Result::ok) {
        if raw.trim().is_empty() {
            continue;
        }
        if let Ok(l) = serde_json::from_str::<ChatLine>(&raw) {
            lines.push(l);
        }
    }
    let take = lines.len().saturating_sub(limit);
    Ok(lines.split_off(take))
}

#[tauri::command]
pub async fn clear_history(
    _state: CoreState<'_>,
    server: String,
    channel: String,
) -> Result<(), String> {
    let path = path_for(&server, &channel);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove {}: {}", path.display(), e))?;
    }
    Ok(())
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path_for(server: &str, channel: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(server.as_bytes());
    hasher.update(b"|");
    hasher.update(channel.as_bytes());
    let digest = hasher.finalize();
    let key: String = digest
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect();
    tc_client::settings::config_dir()
        .join("history")
        .join(format!("{}.jsonl", key))
}
