//! Optional Eggress listener-free route adapter for EggFetch.
//!
//! Only the narrow `pproxy-compat` grammar is enabled (see Cargo features):
//! `direct` or `OutboundConnector::from_pproxy_uri` expressions such as
//! `socks5://127.0.0.1:1080` or two-hop `socks5://...__http://...`. No
//! extended, SSH, QUIC, listener, or server surfaces are enabled. Malformed
//! chains fail closed with credential-redacted diagnostics and never fall
//! back to direct.

use eggfetch_core::{DialError, DialErrorKind, DialFuture, DialStream, DialTarget, Dialer};
use eggress_outbound::{OutboundConnectErrorKind, OutboundConnector};
use std::sync::Arc;

/// An EggFetch dialer that delegates physical TCP route establishment to Eggress.
#[derive(Clone)]
pub struct EggressDialer {
    connector: Arc<OutboundConnector>,
}

impl EggressDialer {
    /// Build a direct route adapter.
    pub fn direct() -> Self {
        Self {
            connector: Arc::new(OutboundConnector::direct()),
        }
    }

    /// Build an adapter around a caller-owned connector.
    pub fn new(connector: OutboundConnector) -> Self {
        Self {
            connector: Arc::new(connector),
        }
    }
}

impl Dialer for EggressDialer {
    fn dial(&self, target: DialTarget) -> DialFuture<'_> {
        let connector = self.connector.clone();
        Box::pin(async move {
            let stream = connector
                .connect_tcp_detailed(target.host(), target.port())
                .await
                .map_err(map_error)?;
            Ok(Box::new(stream.0) as DialStream)
        })
    }
}

fn map_error(error: eggress_outbound::OutboundConnectError) -> DialError {
    let kind = match error.kind() {
        OutboundConnectErrorKind::Timeout => DialErrorKind::Timeout,
        OutboundConnectErrorKind::Authentication => DialErrorKind::Authentication,
        OutboundConnectErrorKind::Policy => DialErrorKind::Rejected,
        OutboundConnectErrorKind::ConnectionRefused
        | OutboundConnectErrorKind::Dns
        | OutboundConnectErrorKind::NetworkUnreachable
        | OutboundConnectErrorKind::HostUnreachable
        | OutboundConnectErrorKind::Tls
        | OutboundConnectErrorKind::Protocol
        | OutboundConnectErrorKind::Other => DialErrorKind::Connection,
        _ => DialErrorKind::Other,
    };
    DialError::new(kind, format!("Eggress route {}", error.kind()))
}

/// Parse a CLI `--route` value.
///
/// `direct` (case-sensitive) selects the ordinary EggFetch path (returns
/// `None`, no dialer). Any other value is parsed via
/// `OutboundConnector::from_pproxy_uri`, whose errors already redact
/// credentials; we further scrub any `@`-userinfo that might appear in
/// wrapper messages. No fallback-to-direct is permitted: malformed input
/// returns `Err` before any network execution.
pub fn parse_route(route: &str) -> Result<Option<OutboundConnector>, String> {
    if route == "direct" {
        return Ok(None);
    }
    OutboundConnector::from_pproxy_uri(route)
        .map(Some)
        .map_err(|error| {
            let message = error.to_string();
            // Defense-in-depth: redact any userinfo that might appear despite
            // Eggress's own redaction.
            redact_route_credentials(&message)
        })
}

/// Redact `://user...@` credentials in a route expression or error string.
pub fn redact_route_credentials(input: &str) -> String {
    // Split two-hop `__` chains and redact each hop's userinfo.
    input
        .split("__")
        .map(|hop| {
            if let Some(scheme_end) = hop.find("://")
                && let Some(at) = hop.rfind('@')
                && at > scheme_end + 3
            {
                format!("{}://<redacted>@{}", &hop[..scheme_end], &hop[at + 1..])
            } else {
                hop.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("__")
}

/// Build a redaction-safe physical route description for flows.
pub fn physical_route_for(
    route: &str,
    connector: &Option<OutboundConnector>,
) -> eggreplay_core::PhysicalRoute {
    match connector {
        None => eggreplay_core::PhysicalRoute {
            kind: "direct".into(),
            description: Some("direct".into()),
        },
        Some(_) => eggreplay_core::PhysicalRoute {
            kind: "eggress".into(),
            description: Some(redact_route_credentials(route)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_hop_pproxy_construction_succeeds() {
        let connector =
            OutboundConnector::from_pproxy_uri("socks5://127.0.0.1:1080__http://127.0.0.1:8080")
                .expect("two-hop chain must construct");
        let _dialer = EggressDialer::new(connector);
    }

    #[test]
    fn malformed_route_fails_closed_without_credentials() {
        let sentinel = "C004-SENTINEL-PASS-abc123";
        let bad = format!("socks5://user:{sentinel}@127.0.0.1:1080__redir://127.0.0.1:1234");
        let result = parse_route(&bad);
        assert!(result.is_err(), "unsupported hop must fail, not fallback");
        let message = result.err().expect("must be err");
        assert!(
            !message.contains(sentinel),
            "credential must be redacted: {message}"
        );
        // No silent direct fallback.
        assert!(parse_route("bogus://not-a-route").is_err());
        assert!(parse_route("direct").unwrap().is_none());
    }

    #[test]
    fn route_redaction_removes_userinfo() {
        let redacted = redact_route_credentials(
            "socks5://alice:secret123@127.0.0.1:1080__http://127.0.0.1:8080",
        );
        assert!(!redacted.contains("secret123"));
        assert!(!redacted.contains("alice"));
        assert!(redacted.contains("<redacted>"));
    }
}
