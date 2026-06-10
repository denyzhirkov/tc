//! Known-servers registry: persisted list of every server the user has
//! connected to, with the last channel they joined and a `favourite` flag.
//!
//! Backed by `UserSettings.servers`; we mirror it into AppCore so commands
//! don't have to re-read tc.toml on every change.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;
use tc_client::settings::ServerEntry;
use tokio::sync::Mutex;

use crate::state::AppCore;

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[derive(Serialize)]
pub struct ServerView {
    pub addr: String,
    pub label: Option<String>,
    pub last_channel: Option<String>,
    pub favourite: bool,
    pub last_used_unix: u64,
}

impl From<&ServerEntry> for ServerView {
    fn from(e: &ServerEntry) -> Self {
        Self {
            addr: e.addr.clone(),
            label: e.label.clone(),
            last_channel: e.last_channel.clone(),
            favourite: e.favourite,
            last_used_unix: e.last_used_unix,
        }
    }
}

#[tauri::command]
pub async fn list_servers(state: CoreState<'_>) -> Result<Vec<ServerView>, String> {
    let c = state.lock().await;
    Ok(c.servers.iter().map(ServerView::from).collect())
}

#[tauri::command]
pub async fn forget_server(state: CoreState<'_>, addr: String) -> Result<(), String> {
    let mut c = state.lock().await;
    c.servers.retain(|s| s.addr != addr);
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_server_favourite(
    state: CoreState<'_>,
    addr: String,
    favourite: bool,
) -> Result<(), String> {
    let mut c = state.lock().await;
    if let Some(e) = c.servers.iter_mut().find(|s| s.addr == addr) {
        e.favourite = favourite;
    }
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn label_server(
    state: CoreState<'_>,
    addr: String,
    label: Option<String>,
) -> Result<(), String> {
    let mut c = state.lock().await;
    if let Some(e) = c.servers.iter_mut().find(|s| s.addr == addr) {
        e.label = label.filter(|s| !s.trim().is_empty());
    }
    c.save();
    Ok(())
}

/// Insert-or-update entry on every successful connect. Bumps `last_used_unix`.
pub fn touch(servers: &mut Vec<ServerEntry>, addr: &str) {
    let now = unix_now();
    if let Some(e) = servers.iter_mut().find(|s| s.addr == addr) {
        e.last_used_unix = now;
        return;
    }
    servers.push(ServerEntry {
        addr: addr.to_string(),
        last_used_unix: now,
        ..Default::default()
    });
}

/// Remember the most recent channel joined on `addr`.
pub fn record_channel(servers: &mut [ServerEntry], addr: &str, channel: &str) {
    if let Some(e) = servers.iter_mut().find(|s| s.addr == addr) {
        e.last_channel = Some(channel.to_string());
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
