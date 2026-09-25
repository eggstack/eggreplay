//! Proxy-only and hop-by-hop header filtering.
//!
//! One tested helper removes proxy framing before a request reaches the
//! upstream. Credential-bearing fields (`Proxy-Authorization`) are stripped
//! and never forwarded, persisted, or logged: only header *names* appear in
//! diagnostics.

/// Outcome of [`filter_proxy_headers`].
#[derive(Debug, Clone)]
pub struct ProxyFilterOutcome {
    /// Headers safe to forward upstream (duplicates and order preserved).
    pub forwarded: http::HeaderMap,
    /// Lowercased names that were removed (deduplicated, values never kept).
    pub removed: Vec<String>,
    /// Whether an `Upgrade` request was present (unsupported in M013B plain
    /// proxy; the caller must reject it).
    pub upgrade_requested: bool,
    /// Whether a `Proxy-Authorization` field was seen and stripped.
    pub had_proxy_authorization: bool,
}

/// Proxy-only headers stripped unconditionally.
const PROXY_ONLY: &[&str] = &[
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
    "keep-alive",
];

/// Remove proxy-only and hop-by-hop framing from explicit-proxy headers.
///
/// Policy:
/// - `Proxy-Connection`, `Proxy-Authorization`, `Proxy-Authenticate`, and
///   `Keep-Alive` are always stripped. `Proxy-Authorization` values never
///   appear in the outcome.
/// - `Connection` itself is stripped and every header it nominates
///   (comma-separated tokens) is stripped.
/// - `Upgrade` is stripped and reported via `upgrade_requested` so the
///   caller can reject upgrades it does not support.
/// - `TE` is forwarded only when its sole token is `trailers`; otherwise it
///   is stripped (M013B does not negotiate extended transfer codings).
/// - `Transfer-Encoding` and `Trailer` are end-to-end framing and are
///   preserved unless nominated by `Connection`.
/// - All other headers (including duplicates) pass through untouched.
#[must_use]
pub fn filter_proxy_headers(headers: http::HeaderMap) -> ProxyFilterOutcome {
    let mut removed: Vec<String> = Vec::new();
    let mut upgrade_requested = false;
    let mut had_proxy_authorization = false;

    // Collect `Connection`-nominated headers before removing `Connection`.
    let mut nominated: Vec<String> = Vec::new();
    for value in headers.get_all(http::header::CONNECTION) {
        let text = value.to_str().unwrap_or("");
        for token in text.split(',') {
            let token = token.trim().to_ascii_lowercase();
            if token.is_empty() {
                continue;
            }
            if token == "upgrade" {
                upgrade_requested = true;
            }
            if !nominated.contains(&token) {
                nominated.push(token);
            }
        }
    }

    // Whether a present `TE` header carries only the `trailers` token.
    let te_values: Vec<String> = headers
        .get_all(http::header::TE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_owned)
        .collect();
    let te_tokens: Vec<String> = te_values
        .iter()
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .collect();
    let te_forward = !te_tokens.is_empty()
        && te_tokens
            .iter()
            .all(|token| token.eq_ignore_ascii_case("trailers"));

    let mut forwarded = http::HeaderMap::with_capacity(headers.len());
    // `HeaderMap` iteration yields `Some(name)` for the first value of a key
    // and `None` for continuation (duplicate) values. Continuations inherit
    // the forwarding decision of their key so duplicates survive filtering.
    let mut current: Option<(http::header::HeaderName, bool)> = None;
    for (name_option, value) in headers {
        if let Some(name) = name_option {
            let lower = name.as_str().to_ascii_lowercase();
            let forward = classify(&lower, te_forward, &nominated);
            if !forward {
                apply_side_effects(
                    &lower,
                    &mut removed,
                    &mut upgrade_requested,
                    &mut had_proxy_authorization,
                );
            }
            current = Some((name, forward));
        }
        if let Some((name, true)) = current.as_ref() {
            forwarded.append(name.clone(), value);
        }
    }

    ProxyFilterOutcome {
        forwarded,
        removed,
        upgrade_requested,
        had_proxy_authorization,
    }
}

/// Decide whether a lowercased header name may be forwarded upstream.
fn classify(lower: &str, te_forward: bool, nominated: &[String]) -> bool {
    if lower == "connection" {
        return false;
    }
    if PROXY_ONLY.contains(&lower) {
        return false;
    }
    if lower == "upgrade" {
        return false;
    }
    if nominated.iter().any(|token| token == lower) {
        return false;
    }
    if lower == "te" {
        return te_forward;
    }
    true
}

/// Record names and flags for a stripped header (names only, never values).
fn apply_side_effects(
    lower: &str,
    removed: &mut Vec<String>,
    upgrade_requested: &mut bool,
    had_proxy_authorization: &mut bool,
) {
    if lower == "upgrade" {
        *upgrade_requested = true;
    }
    if lower == "proxy-authorization" {
        *had_proxy_authorization = true;
    }
    push_removed(removed, lower);
}

/// Record a removed name once (names only; values are never retained).
fn push_removed(removed: &mut Vec<String>, name: &str) {
    if !removed.iter().any(|existing| existing == name) {
        removed.push(name.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn strips_proxy_framing_and_keeps_end_to_end() {
        let outcome = filter_proxy_headers(map(&[
            ("proxy-connection", "keep-alive"),
            ("proxy-authorization", "Basic c2VjcmV0"),
            ("proxy-authenticate", "Basic realm=x"),
            ("keep-alive", "timeout=5"),
            ("connection", "keep-alive, X-Custom-Hop"),
            ("x-custom-hop", "remove-me"),
            ("x-dup", "one"),
            ("x-dup", "two"),
            ("transfer-encoding", "chunked"),
            ("trailer", "x-trailer"),
            ("content-type", "text/plain"),
        ]));
        assert!(!outcome.forwarded.contains_key("proxy-authorization"));
        assert!(!outcome.forwarded.contains_key("proxy-connection"));
        assert!(!outcome.forwarded.contains_key("proxy-authenticate"));
        assert!(!outcome.forwarded.contains_key("keep-alive"));
        assert!(!outcome.forwarded.contains_key("connection"));
        assert!(!outcome.forwarded.contains_key("x-custom-hop"));
        assert!(outcome.forwarded.contains_key("transfer-encoding"));
        assert!(outcome.forwarded.contains_key("trailer"));
        assert!(outcome.forwarded.contains_key("content-type"));
        let dup: Vec<_> = outcome.forwarded.get_all("x-dup").iter().collect();
        assert_eq!(dup.len(), 2);
        assert!(outcome.had_proxy_authorization);
        assert!(!outcome.upgrade_requested);
        for name in &outcome.removed {
            assert_eq!(*name, name.to_ascii_lowercase());
        }
    }

    #[test]
    fn proxy_authorization_value_never_survives() {
        let sentinel = "SENTINEL-PROXY-AUTH-9f27";
        let outcome =
            filter_proxy_headers(map(&[("proxy-authorization", sentinel), ("x-other", "ok")]));
        let debug = format!("{:?}", outcome.forwarded);
        assert!(!debug.contains(sentinel));
        assert!(!format!("{:?}", outcome.removed).contains(sentinel));
        assert!(outcome.had_proxy_authorization);
    }

    #[test]
    fn upgrade_is_reported_and_stripped() {
        let outcome =
            filter_proxy_headers(map(&[("upgrade", "websocket"), ("connection", "upgrade")]));
        assert!(outcome.upgrade_requested);
        assert!(!outcome.forwarded.contains_key("upgrade"));
    }

    #[test]
    fn te_trailers_passes_but_codings_are_stripped() {
        let keep = filter_proxy_headers(map(&[("te", "trailers")]));
        assert!(keep.forwarded.contains_key("te"));
        let strip = filter_proxy_headers(map(&[("te", "gzip, trailers")]));
        assert!(!strip.forwarded.contains_key("te"));
    }
}
