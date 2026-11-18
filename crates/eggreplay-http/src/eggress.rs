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

/// Marker replacing a route's userinfo.
const ROUTE_CREDENTIAL_MARKER: &str = "<redacted>";
/// Whole-value fallback when a route expression cannot be redacted safely.
const ROUTE_REDACTION_FALLBACK: &str = "<redacted-route>";

/// Redact `scheme://userinfo@` credentials in a route expression or error
/// string.
///
/// The authority span of every `scheme://` is redacted **before** any hop
/// splitting. `__` is the two-hop separator and a legal password character, so
/// the previous split-then-`@` scan could leave the fragment after `__`
/// visible (`socks5://user:pa__ss@host` leaked `ss@host`). The scanner is
/// span-based and works only on ASCII-delimited byte offsets:
///
/// * each `://` opens a span;
/// * the span ends at the first `/`, `?`, `#`, or the next `://`;
/// * within the span the **last** `@` is the userinfo delimiter, because
///   userinfo may contain `@` (and `%40` is opaque either way);
/// * everything between `://` and that `@` becomes `<redacted>`, preserving
///   the `@`, the host/port, and every `__` outside the span.
///
/// A span with `://` but no `@` is unchanged. A `://` with no scheme in front
/// of it means the input is not a route expression this scanner can reason
/// about, so the whole value becomes [`ROUTE_REDACTION_FALLBACK`]: an opaque
/// placeholder is safer than a partially redacted string that might still
/// carry a secret. The same fallback covers the one ambiguous in-span case: a
/// span that ends at another `://`, looks like `host:port`, and carries no
/// `__` hop separator is a `://` inside unescaped userinfo, not a chain, so
/// the part before it cannot be redacted reliably either.
pub fn redact_route_credentials(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    // Everything before `cursor` has been copied to `output` verbatim.
    let mut cursor = 0;
    // The next `://` search starts here; always past the previous span start.
    let mut search_from = 0;
    while let Some(offset) = input[search_from..].find("://") {
        let separator = search_from + offset;
        if !has_route_scheme(input, separator) {
            return ROUTE_REDACTION_FALLBACK.to_owned();
        }
        let authority_start = separator + 3;
        let authority_end = route_authority_end(input, authority_start);
        let authority = &input[authority_start..authority_end];
        match authority.rfind('@') {
            Some(delimiter) => {
                output.push_str(&input[cursor..authority_start]);
                output.push_str(ROUTE_CREDENTIAL_MARKER);
                output.push_str(&authority[delimiter..]);
                cursor = authority_end;
            }
            None => {
                let ends_at_next_hop = input[authority_end..].starts_with("://");
                if ends_at_next_hop && authority.contains(':') && !authority.contains("__") {
                    return ROUTE_REDACTION_FALLBACK.to_owned();
                }
            }
        }
        // `authority_end >= authority_start > separator`, so this always makes
        // progress and cannot loop on an empty authority.
        search_from = authority_end;
    }
    output.push_str(&input[cursor..]);
    output
}

/// Whether `separator` (the index of `://`) is preceded by a URI scheme.
///
/// The scheme must be a non-empty run of RFC 3986 scheme characters starting
/// with an ASCII letter. A bare `://` is not a route expression, and treating
/// it as one would mean guessing where its authority begins.
fn has_route_scheme(input: &str, separator: usize) -> bool {
    let bytes = input.as_bytes();
    let mut start = separator;
    while start > 0 && is_scheme_byte(bytes[start - 1]) {
        start -= 1;
    }
    start < separator && bytes[start].is_ascii_alphabetic()
}

const fn is_scheme_byte(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'-' | b'.')
}

/// The end of the authority span that starts at `authority_start`.
///
/// The span stops before the first path, query, or fragment delimiter, and
/// before a following `://` so a two-hop chain yields one span per hop. All
/// delimiters are ASCII, so the returned index is a `char` boundary.
fn route_authority_end(input: &str, authority_start: usize) -> usize {
    let rest = &input[authority_start..];
    let mut end = input.len();
    if let Some(offset) = rest.find(['/', '?', '#']) {
        end = authority_start + offset;
    }
    if let Some(offset) = rest.find("://")
        && authority_start + offset < end
    {
        end = authority_start + offset;
    }
    end
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
        assert_eq!(
            redacted, "socks5://<redacted>@127.0.0.1:1080__http://127.0.0.1:8080",
            "the host, port, and hop separator must survive"
        );
    }

    #[test]
    fn password_containing_the_hop_separator_is_fully_redacted() {
        // `__` is both the two-hop separator and a legal password character.
        // Splitting first exposed the fragment after it.
        let redacted = redact_route_credentials("socks5://user:pa__ss@host:1080");
        assert_eq!(redacted, "socks5://<redacted>@host:1080");
        assert!(!redacted.contains("pa"));
        assert!(!redacted.contains("ss@"));
    }

    #[test]
    fn the_last_at_is_the_userinfo_delimiter() {
        // Userinfo may itself contain `@`; the delimiter is the last one.
        let redacted = redact_route_credentials("http://user:p@ss@host:8080");
        assert_eq!(redacted, "http://<redacted>@host:8080");
        assert!(!redacted.contains("p@ss"));
        assert!(!redacted.contains("@ss"));
    }

    #[test]
    fn both_hops_are_redacted_and_the_separator_survives() {
        let redacted = redact_route_credentials("socks5://u1:p1@h1__http://u2:p2@h2");
        assert_eq!(redacted, "socks5://<redacted>@h1__http://<redacted>@h2");
        for fragment in ["u1", "p1", "u2", "p2", ":p1@", ":p2@"] {
            assert!(!redacted.contains(fragment), "{fragment} in {redacted}");
        }
    }

    #[test]
    fn credentials_in_only_one_hop_still_redact_that_hop() {
        let first = redact_route_credentials("socks5://u1:p1@h1__http://h2:8080");
        assert_eq!(first, "socks5://<redacted>@h1__http://h2:8080");
        let second = redact_route_credentials("socks5://h1:1080__http://u2:p2@h2");
        assert_eq!(second, "socks5://h1:1080__http://<redacted>@h2");
    }

    #[test]
    fn percent_encoded_at_is_inside_the_redacted_span() {
        let redacted = redact_route_credentials("socks5://user%40name:pa%40ss@host:1080");
        assert_eq!(redacted, "socks5://<redacted>@host:1080");
        assert!(!redacted.contains("user%40name"));
        assert!(!redacted.contains("pa%40ss"));
    }

    #[test]
    fn path_query_and_fragment_end_the_authority_span() {
        for (route, expected) in [
            (
                "http://user:pw@host:8080/path",
                "http://<redacted>@host:8080/path",
            ),
            (
                "http://user:pw@host:8080?q=1",
                "http://<redacted>@host:8080?q=1",
            ),
            (
                "http://user:pw@host:8080#frag",
                "http://<redacted>@host:8080#frag",
            ),
        ] {
            assert_eq!(redact_route_credentials(route), expected, "{route}");
        }
    }

    #[test]
    fn routes_without_credentials_are_unchanged() {
        for route in [
            "direct",
            "socks5://127.0.0.1:1080",
            "socks5://127.0.0.1:1080__http://127.0.0.1:8080",
            "",
        ] {
            assert_eq!(redact_route_credentials(route), route, "{route}");
        }
    }

    #[test]
    fn malformed_routes_do_not_panic_and_do_not_echo_secrets() {
        // A bare authority delimiter with empty userinfo is not a secret but
        // must still redact rather than panic.
        assert_eq!(
            redact_route_credentials("scheme://@"),
            "scheme://<redacted>@"
        );
        // No scheme at all: the whole value fails closed.
        assert_eq!(redact_route_credentials("://"), "<redacted-route>");
        // Malformed *and* credential-bearing: nothing may leak.
        let malformed = "://user:SENTINEL-PASS@host:1080";
        let redacted = redact_route_credentials(malformed);
        assert_eq!(redacted, "<redacted-route>");
        assert!(!redacted.contains("SENTINEL"));
        // Empty is not a secret and has nothing to scan.
        assert_eq!(redact_route_credentials(""), "");
    }

    #[test]
    fn a_literal_delimiter_inside_userinfo_fails_closed() {
        // `pa://ss` is not a hop: there is no `__` separator, and the span
        // before it looks like host:port. The part before the delimiter cannot
        // be redacted reliably, so the value goes opaque instead of echoing
        // `user:pa`.
        let leaked = "socks5://user:pa://ss@host:1080";
        let redacted = redact_route_credentials(leaked);
        assert_eq!(redacted, "<redacted-route>");
        assert!(!redacted.contains("pa"), "{redacted}");
        assert!(!redacted.contains("ss@host"), "{redacted}");
        // A real two-hop chain is still a chain, ports and all.
        assert_eq!(
            redact_route_credentials("socks5://h1:1080__http://u2:p2@h2:8080"),
            "socks5://h1:1080__http://<redacted>@h2:8080"
        );
    }

    #[test]
    fn redaction_never_turns_an_invalid_route_into_a_valid_one() {
        // `parse_route` is the product path: it parses first and only redacts
        // the error, so a malformed chain can never fall back to direct.
        let sentinel = "M021D-ROUTE-SENTINEL";
        let invalid = format!("redir://user:{sentinel}@127.0.0.1:1080");
        assert!(
            OutboundConnector::from_pproxy_uri(&invalid).is_err(),
            "Eggress must reject the unsupported scheme"
        );
        let error = match parse_route(&invalid) {
            Ok(_) => panic!("an unsupported scheme must fail closed"),
            Err(error) => error,
        };
        assert!(!error.contains(sentinel), "leaked credential: {error}");
        assert!(
            parse_route(&invalid).is_err(),
            "redaction must not fabricate a usable route"
        );
    }
}
