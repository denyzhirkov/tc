//! tc:// URL scheme handler.
//!
//! Format: `tc://host[:port][/channel-id]`
//!   - `tc://192.168.1.10`              → connect to host on default TCP port
//!   - `tc://example.com:7100`          → connect to host:port
//!   - `tc://example.com/lobby`         → connect, then join `lobby`
//!   - `tc://example.com:7100/lobby`    → connect to host:port, then join
//!
//! Port defaults to `tc_shared::config::TCP_PORT` when omitted.

use tauri::{AppHandle, Emitter};
use url::Url;

#[derive(Debug, Clone)]
pub struct Invite {
    pub addr: String,
    pub channel: Option<String>,
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
    let channel = if trimmed.is_empty() { None } else { Some(trimmed) };

    Some(Invite { addr, channel })
}

/// Forward the URL to the frontend; the frontend dispatches connect+join.
pub fn forward(app: &AppHandle, raw: &str) {
    let Some(invite) = parse_invite(raw) else {
        tracing::warn!("ignoring malformed deep-link: {}", raw);
        return;
    };
    tracing::info!(addr = %invite.addr, channel = ?invite.channel, "deep-link invite");
    if let Err(e) = app.emit(
        "invite",
        serde_json::json!({
            "addr": invite.addr,
            "channel": invite.channel,
        }),
    ) {
        tracing::warn!("failed to emit invite event: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_only() {
        let i = parse_invite("tc://example.com").unwrap();
        assert_eq!(i.addr, format!("example.com:{}", tc_shared::config::TCP_PORT));
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
        assert_eq!(i.addr, format!("example.com:{}", tc_shared::config::TCP_PORT));
        assert_eq!(i.channel.as_deref(), Some("pub-room"));
    }

    #[test]
    fn rejects_other_scheme() {
        assert!(parse_invite("https://example.com/lobby").is_none());
    }
}
