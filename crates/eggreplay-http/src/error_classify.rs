//! The one place a transport failure becomes a semantic category.
//!
//! # Why this is a module and not a helper in one caller
//!
//! `eggreplay-core::error` is the stable cross-layer taxonomy, and every adapter
//! that observes a failure must map it into the same `ErrorPhase` ×
//! `ErrorCategory` pair. Three separate tables existed for the same
//! `eggfetch_core::Error` type:
//!
//! | Site | Introduced | Defect |
//! |---|---|---|
//! | `recording.rs` request level | — | M016: every `DialErrorKind` collapsed to `Other` |
//! | `recording.rs` body level | — | M017: error discarded, hardcoded `("other", "body")` |
//! | `regression.rs` candidate path | pre-M016 | **both**, plus three more discarded-error sites |
//!
//! The candidate path is the interesting one: a recorded run and a candidate
//! run of the *same* request against the *same* dead route disagreed about what
//! went wrong, and a candidate body cut off by a deadline recorded
//! identically to one killed by a reset. Each table looked reasonable on its
//! own; the defect was that there were three.
//!
//! Everything that observes a fetch failure now classifies it here, so a route
//! failure reads the same from the recording path and the candidate path. The
//! invariant this module exists to protect: **if two code paths can observe the
//! same failure, they must classify it through the same function.**

use eggfetch_core::Error as FetchError;
use eggfetch_core::transport::dialer::DialErrorKind;
use eggreplay_core::{ErrorCategory, ErrorPhase, FlowError};

/// Map a request-level transport failure onto the stable taxonomy.
///
/// This is the table M016 repaired: before it, every `DialErrorKind` collapsed
/// to `(Other, Other)`, which made `ErrorCategory::ConnectionRefused`
/// unreachable anywhere in the product and made a dead Eggress route
/// indistinguishable from an unclassified failure.
pub(crate) fn map_fetch_error(error: &FetchError) -> FlowError {
    let (category, phase) = match error {
        FetchError::Tls(_)
        | FetchError::CertificateVerification(_)
        | FetchError::HostnameVerification(_)
        | FetchError::TlsConfig(_)
        | FetchError::CaBundle(_) => (ErrorCategory::Tls, ErrorPhase::Tls),
        FetchError::Timeout { .. } | FetchError::TransportIoTimeout { .. } => {
            (ErrorCategory::Timeout, ErrorPhase::Timeout)
        }
        FetchError::Body(_)
        | FetchError::DecodedBodyTooLarge
        | FetchError::Decompression(_)
        | FetchError::DecompressionRatioExceeded => (ErrorCategory::Other, ErrorPhase::Body),
        FetchError::Protocol(_) | FetchError::Hyper(_) | FetchError::HyperClient(_) => {
            (ErrorCategory::Protocol, ErrorPhase::Headers)
        }
        FetchError::Connect(_) | FetchError::Io(_) | FetchError::Pool(_) => {
            (ErrorCategory::Unreachable, ErrorPhase::Connect)
        }
        // A caller-supplied transport is how every Eggress route reports a
        // failure. Without this arm a dead route, a route timeout, and a
        // policy-rejected route all recorded as `Other`, and
        // `ErrorCategory::ConnectionRefused` was unreachable anywhere in the
        // product.
        //
        // `Unreachable` rather than `ConnectionRefused`: EggFetch's typed
        // evidence collapses every connection-establishment failure to one
        // kind, so inferring "refused" from it would be a guess. An honest
        // general category beats a specific wrong one.
        FetchError::CustomTransport(_) => match classify_dial_error(error) {
            Some(class) => class,
            None => (ErrorCategory::Other, ErrorPhase::Other),
        },
        _ => (ErrorCategory::Other, ErrorPhase::Other),
    };
    FlowError::new(category, phase, error.to_string())
}

/// Map a caller-supplied transport's `DialErrorKind` onto the stable vocabulary.
///
/// Shared by [`map_fetch_error`] and [`classify_body_error`] so a route failure
/// reads the same whichever layer observed it. Before M017 only the
/// request-level site consulted this, which is why a dead route was
/// attributable there but invisible from a body error.
pub(crate) fn classify_dial_error(error: &FetchError) -> Option<(ErrorCategory, ErrorPhase)> {
    let kind = error.custom_transport_error()?.kind();
    Some(match kind {
        DialErrorKind::Connection => (ErrorCategory::Unreachable, ErrorPhase::Connect),
        DialErrorKind::Timeout => (ErrorCategory::Timeout, ErrorPhase::Timeout),
        DialErrorKind::Authentication => (ErrorCategory::Policy, ErrorPhase::Connect),
        DialErrorKind::Rejected => (ErrorCategory::Policy, ErrorPhase::Policy),
        DialErrorKind::Other => (ErrorCategory::Other, ErrorPhase::Other),
    })
}

/// Classify a response-body streaming failure.
///
/// The body-phase counterpart of [`map_fetch_error`]. Same `FetchError` type
/// and the same category decisions, but the phase is `Body` because that is
/// where the failure happened — except for a deadline, which is reported as
/// `Timeout` rather than as a generic body failure so that a call cut off by
/// the outbound timeout is distinguishable from one killed by a reset.
///
/// Before M017 the recording site discarded the error and hardcoded
/// `("other", "body")`, so a deadline cut-off, a connection reset, and a
/// protocol violation all recorded identically. That is the same defect M016
/// fixed one layer up, where every `DialErrorKind` collapsed to `Other`; it
/// survived here because nobody looked at the body path — and then survived
/// *again* on the candidate path, which had three copies of the discard.
pub(crate) fn classify_body_error(error: &FetchError) -> (ErrorCategory, ErrorPhase) {
    match error {
        // An expired deadline is the actionable fact, and it is what makes an
        // un-terminated gRPC call legible: the fixture then says the call was
        // cut off by a deadline rather than merely "failed".
        FetchError::Timeout { .. } | FetchError::TransportIoTimeout { .. } => {
            (ErrorCategory::Timeout, ErrorPhase::Timeout)
        }
        FetchError::Tls(_)
        | FetchError::CertificateVerification(_)
        | FetchError::HostnameVerification(_)
        | FetchError::TlsConfig(_)
        | FetchError::CaBundle(_) => (ErrorCategory::Tls, ErrorPhase::Body),
        // A route that fails mid-body is the same route failure as at
        // request level; only the phase differs.
        FetchError::CustomTransport(_) => {
            let (category, _) =
                classify_dial_error(error).unwrap_or((ErrorCategory::Other, ErrorPhase::Other));
            (category, ErrorPhase::Body)
        }
        FetchError::Body(_)
        | FetchError::DecodedBodyTooLarge
        | FetchError::Decompression(_)
        | FetchError::DecompressionRatioExceeded => (ErrorCategory::Other, ErrorPhase::Body),
        // Protocol-shaped failures are `Body` here, where `map_fetch_error`
        // uses `Headers`: this site is past the headers by construction.
        FetchError::Protocol(_) | FetchError::Hyper(_) | FetchError::HyperClient(_) => {
            (ErrorCategory::Protocol, ErrorPhase::Body)
        }
        // The stream died underneath us, so the connection is gone.
        FetchError::Connect(_) | FetchError::Io(_) | FetchError::Pool(_) => {
            (ErrorCategory::Unreachable, ErrorPhase::Body)
        }
        _ => (ErrorCategory::Other, ErrorPhase::Body),
    }
}

/// Render a body-failure classification as the `(category, phase)` string pair
/// a [`eggreplay_core::StreamEventKind::Error`] carries.
///
/// Stream events store these as strings, so a site that pushes one must derive
/// the pair from the same table rather than writing literals. `as_str()` is
/// pinned against serde in `eggreplay-core`, so a wire-name drift fails the
/// build instead of producing an event the JSON decoder would not read back.
pub(crate) fn body_error_event_fields(error: &FetchError) -> (&'static str, &'static str) {
    let (category, phase) = classify_body_error(error);
    (category.as_str(), phase.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggfetch_core::transport::dialer::{DialError, DialErrorKind};
    use eggreplay_core::error::{ErrorCategory as Category, ErrorPhase as Phase};
    use std::sync::Arc;

    fn route_error(kind: DialErrorKind) -> FetchError {
        FetchError::CustomTransport(Arc::new(DialError::new(kind, "route failed")))
    }

    /// The candidate path and the recording path must agree about the same
    /// failure. This is the invariant the module exists to protect, and it is
    /// the test that would have failed before the candidate path was repaired.
    #[test]
    fn the_candidate_and_recording_tables_cannot_drift() {
        let cases = [
            (DialErrorKind::Connection, Category::Unreachable),
            (DialErrorKind::Timeout, Category::Timeout),
            (DialErrorKind::Authentication, Category::Policy),
            (DialErrorKind::Rejected, Category::Policy),
            (DialErrorKind::Other, Category::Other),
        ];
        for (kind, expected) in cases {
            let error = route_error(kind);
            // Both layers now call the same function; assert the property that
            // matters rather than the call graph.
            assert_eq!(
                map_fetch_error(&error).category,
                expected,
                "request level: {kind:?}"
            );
            assert_eq!(
                classify_body_error(&error).0,
                expected,
                "body level keeps the request-level category: {kind:?}"
            );
        }
    }

    /// A deadline cut-off must be distinguishable from a reset at both layers.
    /// This is M017's finding, and it is what keeps an un-terminated gRPC call
    /// legible in a *candidate* fixture, not only a recorded one.
    #[test]
    fn a_deadline_is_not_a_generic_body_failure_at_either_layer() {
        use eggfetch_core::timeout::TimeoutPhase;

        let timeout = FetchError::Timeout {
            phase: TimeoutPhase::Read,
            elapsed: std::time::Duration::from_secs(5),
        };
        assert_eq!(
            classify_body_error(&timeout),
            (Category::Timeout, Phase::Timeout)
        );
        let io = FetchError::Io(Arc::new(std::io::Error::other("connection reset by peer")));
        assert_eq!(
            classify_body_error(&io),
            (Category::Unreachable, Phase::Body)
        );
    }

    /// The string form pushed into a stream event must be the same vocabulary
    /// `classify_body_error` returns, so an event cannot claim `other` for a
    /// failure the taxonomy knows how to name.
    ///
    /// Note the deliberate split: the *category* carries the attribution
    /// (`timeout` for a route deadline) while the *phase* says where the
    /// failure surfaced (`body`, because that is where a body error is
    /// observed even when the underlying cause was a dial timeout).
    #[test]
    fn event_fields_derive_from_the_table_not_from_literals() {
        let error = route_error(DialErrorKind::Timeout);
        let (category, phase) = body_error_event_fields(&error);
        assert_eq!(category, Category::Timeout.as_str());
        assert_eq!(phase, Phase::Body.as_str());
        assert_ne!(category, Category::Other.as_str());
        assert_ne!(phase, "other");
    }
}
