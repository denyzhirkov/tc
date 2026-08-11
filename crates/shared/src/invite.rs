//! tc:// invite link building.
//!
//! Format: `tc://host:port[/channel-id]`. The port is **always** written, even
//! when it is the default: a link is something a person copies into a message,
//! and everything downstream of that — someone reading it, a chat client
//! linkifying it, a paste into `/server` — has no reason to know what our
//! default is. Only our own deep-link handler ever did.
//!
//! Links without a port still parse: `deeplink.rs` fills in [`TCP_PORT`], so
//! every link shared before this stays valid.

use crate::config::TCP_PORT;

/// Whether `addr` already carries a port.
///
/// The awkward case is a bare IPv6 literal: `::1` is all colons and no port,
/// while `[::1]:7100` has one. Bracketed forms are decided by what follows the
/// bracket; unbracketed ones by there being exactly one colon to be a
/// separator.
fn has_port(addr: &str) -> bool {
    match addr.rfind(']') {
        Some(close) => addr[close + 1..].starts_with(':'),
        None => addr.matches(':').count() == 1,
    }
}

/// Build an invite URL from a `host` or `host:port` address and an optional
/// channel id. A missing port is filled in with [`TCP_PORT`]; a bare IPv6
/// literal is bracketed on the way, because a URL cannot hold one otherwise.
pub fn invite_url(addr: &str, channel: Option<&str>) -> String {
    let authority = if has_port(addr) {
        addr.to_string()
    } else if addr.contains(':') && !addr.starts_with('[') {
        format!("[{}]:{}", addr, TCP_PORT)
    } else {
        format!("{}:{}", addr, TCP_PORT)
    };
    match channel {
        Some(c) if !c.is_empty() => format!("tc://{}/{}", authority, c),
        _ => format!("tc://{}", authority),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this file was rewritten for: the default port used to be
    /// stripped, so a link to a server on 7100 carried no port at all and
    /// anything but our own handler had to guess it.
    #[test]
    fn keeps_the_default_port() {
        assert_eq!(
            invite_url(&format!("example.com:{}", TCP_PORT), None),
            format!("tc://example.com:{}", TCP_PORT)
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
    fn fills_in_the_port_when_the_address_has_none() {
        assert_eq!(
            invite_url("example.com", None),
            format!("tc://example.com:{}", TCP_PORT)
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
            format!("tc://10.0.0.5:{}/lobby", TCP_PORT)
        );
    }

    #[test]
    fn brackets_a_bare_ipv6_literal() {
        assert_eq!(
            invite_url("::1", Some("lobby")),
            format!("tc://[::1]:{}/lobby", TCP_PORT)
        );
    }

    #[test]
    fn leaves_a_bracketed_ipv6_with_a_port_alone() {
        assert_eq!(invite_url("[::1]:9000", None), "tc://[::1]:9000");
    }

    #[test]
    fn adds_the_port_to_a_bracketed_ipv6_without_one() {
        assert_eq!(
            invite_url("[fe80::1]", None),
            format!("tc://[fe80::1]:{}", TCP_PORT)
        );
    }
}
