//! tc:// URL scheme handler.
//!
//! Format: `tc://host[:port][/channel-id]`
//!   - `tc://192.168.1.10`              → connect to host on default TCP port
//!   - `tc://example.com:7100`          → connect to host:port
//!   - `tc://example.com/lobby`         → connect, then join `lobby`
//!   - `tc://example.com:7100/lobby`    → connect to host:port, then join
//!
//! Port defaults to `tc_shared::config::TCP_PORT` when omitted.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use url::Url;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Invite {
    pub addr: String,
    pub channel: Option<String>,
}

/// Gate between the OS deep-link handler and the webview. On a cold start
/// (app launched by clicking a tc:// link) `on_open_url` fires before the
/// frontend has subscribed, so an emitted event would be lost — buffer the
/// invite until the frontend reports ready via `take_pending_invite`.
#[derive(Default)]
pub struct Gate(Mutex<GateInner>);

#[derive(Default)]
struct GateInner {
    frontend_ready: bool,
    pending: Option<Invite>,
}

/// Flips the gate to "ready" and drains the invite buffered before that,
/// if any. Called once by the frontend right after it subscribes to events.
#[tauri::command]
pub fn take_pending_invite(gate: tauri::State<'_, Gate>) -> Option<Invite> {
    let mut g = gate.0.lock().expect("deeplink gate poisoned");
    g.frontend_ready = true;
    g.pending.take()
}

pub fn parse_invite(raw: &str) -> Option<Invite> {
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "tc" {
        return None;
    }
    let host = url.host_str()?.to_string();
    let port = url.port().unwrap_or(tc_shared::config::TCP_PORT);
    let addr = format!("{}:{}", host, port);

    // Path looks like "/lobby" — strip the leading slash and any trailing one.
    let trimmed = url.path().trim_matches('/').to_string();
    let channel = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    };

    Some(Invite { addr, channel })
}

/// Forward the URL to the frontend; the frontend dispatches connect+join.
/// Until the frontend is ready the invite is buffered in the [`Gate`].
pub fn forward(app: &AppHandle, raw: &str) {
    let Some(invite) = parse_invite(raw) else {
        tracing::warn!("ignoring malformed deep-link: {}", raw);
        return;
    };
    tracing::info!(addr = %invite.addr, channel = ?invite.channel, "deep-link invite");
    {
        let gate = app.state::<Gate>();
        let mut g = gate.0.lock().expect("deeplink gate poisoned");
        if !g.frontend_ready {
            g.pending = Some(invite);
            return;
        }
    }
    if let Err(e) = app.emit("invite", invite) {
        tracing::warn!("failed to emit invite event: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_only() {
        let i = parse_invite("tc://example.com").unwrap();
        assert_eq!(
            i.addr,
            format!("example.com:{}", tc_shared::config::TCP_PORT)
        );
        assert_eq!(i.channel, None);
    }

    #[test]
    fn parses_host_port() {
        let i = parse_invite("tc://example.com:9000").unwrap();
        assert_eq!(i.addr, "example.com:9000");
    }

    #[test]
    fn parses_host_port_channel() {
        let i = parse_invite("tc://example.com:9000/lobby").unwrap();
        assert_eq!(i.addr, "example.com:9000");
        assert_eq!(i.channel.as_deref(), Some("lobby"));
    }

    #[test]
    fn parses_host_channel() {
        let i = parse_invite("tc://example.com/pub-room").unwrap();
        assert_eq!(
            i.addr,
            format!("example.com:{}", tc_shared::config::TCP_PORT)
        );
        assert_eq!(i.channel.as_deref(), Some("pub-room"));
    }

    #[test]
    fn rejects_other_scheme() {
        assert!(parse_invite("https://example.com/lobby").is_none());
    }
}
