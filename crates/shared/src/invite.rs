//! tc:// invite link building.
//!
//! Format: `tc://host[:port][/channel-id]`; the port is omitted when it
//! equals the default [`TCP_PORT`]. Parsing lives in the desktop app
//! (`deeplink.rs`) — only the desktop registers the OS scheme handler.

use crate::config::TCP_PORT;

/// Build an invite URL from a `host` or `host:port` address and an optional
/// channel id.
pub fn invite_url(addr: &str, channel: Option<&str>) -> String {
    let default_suffix = format!(":{}", TCP_PORT);
    let host = addr.strip_suffix(default_suffix.as_str()).unwrap_or(addr);
    match channel {
        Some(c) if !c.is_empty() => format!("tc://{}/{}", host, c),
        _ => format!("tc://{}", host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omits_default_port() {
        assert_eq!(
            invite_url(&format!("example.com:{}", TCP_PORT), None),
            "tc://example.com"
        );
    }

    #[test]
    fn keeps_custom_port() {
        assert_eq!(
            invite_url("example.com:9000", None),
            "tc://example.com:9000"
        );
    }

    #[test]
    fn appends_channel() {
        assert_eq!(
            invite_url("example.com:9000", Some("lobby")),
            "tc://example.com:9000/lobby"
        );
    }

    #[test]
    fn host_only_with_channel() {
        assert_eq!(
            invite_url(&format!("10.0.0.5:{}", TCP_PORT), Some("lobby")),
            "tc://10.0.0.5/lobby"
        );
    }
}
