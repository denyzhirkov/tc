//! DM peer registry and history-fetch helper. The peer list is mirrored from
//! `AppCore.dm_peers` (persisted in tc.toml) and updated on every incoming DM.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;
use tc_client::settings::DmPeer;
use tokio::sync::Mutex;

use crate::history::{self, ChatLine};
use crate::state::AppCore;
use sha2::{Digest, Sha256};

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[derive(Serialize)]
pub struct DmPeerView {
    pub pubkey_hex: String,
    pub name: String,
    pub last_seen_unix: u64,
}

impl From<&DmPeer> for DmPeerView {
    fn from(p: &DmPeer) -> Self {
        Self {
            pubkey_hex: p.pubkey_hex.clone(),
            name: p.name.clone(),
            last_seen_unix: p.last_seen_unix,
        }
    }
}

/// Update the registry on every incoming DM. Inserts or refreshes the entry.
pub fn touch(peers: &mut Vec<DmPeer>, pubkey_hex: &str, name: &str) {
    let now = unix_now();
    if let Some(p) = peers.iter_mut().find(|p| p.pubkey_hex == pubkey_hex) {
        p.name = name.to_string();
        p.last_seen_unix = now;
        return;
    }
    peers.push(DmPeer {
        pubkey_hex: pubkey_hex.to_string(),
        name: name.to_string(),
        last_seen_unix: now,
    });
}

#[tauri::command]
pub async fn list_dm_peers(state: CoreState<'_>) -> Result<Vec<DmPeerView>, String> {
    let c = state.lock().await;
    let mut out: Vec<DmPeerView> = c.dm_peers.iter().map(DmPeerView::from).collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.last_seen_unix));
    Ok(out)
}

#[tauri::command]
pub async fn forget_dm_peer(state: CoreState<'_>, pubkey_hex: String) -> Result<(), String> {
    let mut c = state.lock().await;
    c.dm_peers.retain(|p| p.pubkey_hex != pubkey_hex);
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn get_dm_history(
    state: CoreState<'_>,
    pubkey_hex: String,
    limit: Option<usize>,
) -> Result<Vec<ChatLine>, String> {
    let server = state
        .lock()
        .await
        .server_addr
        .clone()
        .ok_or_else(|| "no server selected".to_string())?;
    history::get_history(server, format!("@dm:{}", pubkey_hex), limit).await
}

/// Send a DM and append the outgoing line to local history. Always succeeds in
/// the local store even if the network send fails (so the user sees what they sent).
#[tauri::command]
pub async fn send_dm_to(
    state: CoreState<'_>,
    pubkey_hex: String,
    text: String,
) -> Result<(), String> {
    use tc_shared::ClientMessage;

    let (conn, server, self_name) = {
        let c = state.lock().await;
        (
            c.conn.clone(),
            c.server_addr.clone(),
            c.name.clone().unwrap_or_else(|| "you".to_string()),
        )
    };
    let conn = conn.ok_or_else(|| "not connected".to_string())?;
    let server = server.ok_or_else(|| "no server".to_string())?;

    let to_pubkey = hex::decode(&pubkey_hex).map_err(|e| format!("bad pubkey hex: {}", e))?;
    conn.send(ClientMessage::DirectMessage {
        to_pubkey,
        text: text.clone(),
    })
    .map_err(|e| format!("send failed: {:#}", e))?;

    history::append(
        &server,
        &format!("@dm:{}", pubkey_hex),
        &ChatLine {
            ts_unix: history::unix_now(),
            kind: history::LineKind::Dm,
            from: self_name,
            text,
        },
    );
    Ok(())
}

/// Resolve a peer reference to a full pubkey-hex.
/// - 64-char hex → returned as-is (lowercased)
/// - 16-char hex → matched against known peers' fingerprints
/// Returns None if the input doesn't match either form or no known peer matches.
#[tauri::command]
pub async fn resolve_peer(state: CoreState<'_>, query: String) -> Result<Option<String>, String> {
    let q = query.trim().to_ascii_lowercase();
    if !q.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }
    if q.len() == 64 {
        return Ok(Some(q));
    }
    if q.len() == 16 {
        let c = state.lock().await;
        for p in &c.dm_peers {
            if fingerprint_of(&p.pubkey_hex) == q {
                return Ok(Some(p.pubkey_hex.clone()));
            }
        }
    }
    Ok(None)
}

fn fingerprint_of(pubkey_hex: &str) -> String {
    if let Ok(bytes) = hex::decode(pubkey_hex) {
        let hash = Sha256::digest(&bytes);
        return hash.iter().take(8).map(|b| format!("{:02x}", b)).collect();
    }
    String::new()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
