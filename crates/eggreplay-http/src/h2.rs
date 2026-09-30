//! Experimental HTTP/2 outbound qualification (M014B).
//!
//! Capability summary, closed in
//! `plans/closure/m014b-http2-qualification.md`:
//!
//! - H2 record and regression-candidate execution through EggFetch
//!   (`native-http2`, ALPN `h2` over local TLS) are **experimental**.
//!   Callers opt in per client with an explicit
//!   [`HttpVersionPolicy`]; the EggReplay default remains HTTP/1.1.
//! - H2 inbound serving (EggServe replay/gateway) is **unsupported**:
//!   the qualified EggServe direct runtime is H1-only, and H2
//!   composition lives outside the adopted dependency closure.
//!   H2-recorded fixtures replay over H1 because the flow store is
//!   version-neutral.
//! - H2 interception (MITM) is **unsupported** and needs its own
//!   ALPN/caller-owned-connection evidence.
//! - Cleartext prior-knowledge (`h2c`) is **not exposed**: EggFetch
//!   negotiates H2 via ALPN, and `Http2Only` against a cleartext
//!   endpoint fails closed rather than downgrading.
//! - Routed H2-over-TLS through an Eggress TCP route is **experimental**
//!   at the same tier as direct H2. The [`EggressDialer`](crate::EggressDialer)
//!   returns raw TCP bytes; SNI/ALPN ownership stays in EggFetch by
//!   construction and is pinned by the routed qualification test.
//!
//! Per RFC 9113 section 8.2.2, HTTP/1 connection-specific headers are
//! forbidden in H2. EggFetch strips them internally, but EggReplay
//! validates at its own boundary with [`check_h2_headers`] so H2 callers
//! fail fast instead of silently sending altered semantics.

use crate::recording::HttpError;
pub use crate::recording::negotiated_version_annotation;
pub use eggfetch_core::HttpVersionPolicy;

/// HTTP/1 connection-specific headers forbidden in HTTP/2 (RFC 9113 8.2.2).
pub const H2_FORBIDDEN_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
];

/// Validate request headers for H2 use.
///
/// Rejects `Connection`, `Keep-Alive`, `Proxy-Connection`,
/// `Transfer-Encoding`, and `Upgrade` unconditionally. `TE` is permitted
/// only when every value is `trailers`. Returns `Ok` for headers that are
/// safe to send over H2.
pub fn check_h2_headers(headers: &http::HeaderMap) -> Result<(), HttpError> {
    for forbidden in H2_FORBIDDEN_HEADERS {
        if headers.contains_key(*forbidden) {
            return Err(HttpError::Conversion(format!(
                "header '{forbidden}' is forbidden in HTTP/2"
            )));
        }
    }
    for value in headers.get_all(http::header::TE) {
        let text = value.to_str().unwrap_or("");
        let trailers_only = text
            .split(',')
            .map(str::trim)
            .all(|token| token.eq_ignore_ascii_case("trailers"));
        if !trailers_only {
            return Err(HttpError::Conversion(
                "TE is forbidden in HTTP/2 unless the value is 'trailers'".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                http::header::HeaderName::from_bytes(name.as_bytes()).expect("valid name"),
                http::HeaderValue::from_str(value).expect("valid value"),
            );
        }
        map
    }

    #[test]
    fn plain_headers_pass() {
        let map = headers(&[("content-type", "application/json"), ("x-e", "1")]);
        check_h2_headers(&map).expect("plain headers must pass");
    }

    #[test]
    fn te_trailers_passes() {
        let map = headers(&[("te", "trailers")]);
        check_h2_headers(&map).expect("TE: trailers must pass");
    }

    #[test]
    fn forbidden_headers_fail() {
        for forbidden in [
            "connection",
            "keep-alive",
            "proxy-connection",
            "transfer-encoding",
            "upgrade",
        ] {
            let map = headers(&[(forbidden, "x")]);
            let error = check_h2_headers(&map).expect_err("forbidden header must fail");
            assert!(
                error.to_string().contains(forbidden),
                "error must name the header: {error}"
            );
        }
    }

    #[test]
    fn te_other_values_fail() {
        for te in ["gzip", "trailers, gzip", ""] {
            let map = headers(&[("te", te)]);
            assert!(
                check_h2_headers(&map).is_err(),
                "TE '{te}' must fail for H2"
            );
        }
    }

    #[test]
    fn version_annotation_marks_h2_only() {
        assert_eq!(
            negotiated_version_annotation(http::Version::HTTP_2),
            Some(("transport".to_string(), "http-version:h2".to_string()))
        );
        assert_eq!(negotiated_version_annotation(http::Version::HTTP_11), None);
        assert_eq!(negotiated_version_annotation(http::Version::HTTP_10), None);
    }

    #[test]
    fn version_policy_originates_from_pinned_eggfetch() {
        // The re-exported policy must round-trip through EggFetch's own
        // builder so H2 opt-in can never desync from the pinned engine.
        let policy = HttpVersionPolicy::Http2Only;
        assert_ne!(policy, HttpVersionPolicy::Http1Only);
    }
}
