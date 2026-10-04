//! Stable error categories shared by all presentation layers.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The phase in which an interaction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPhase {
    /// Request could not be materialized or sent.
    Request,
    /// Name resolution or connection establishment failed.
    Connect,
    /// TLS negotiation or verification failed.
    Tls,
    /// Request or response headers failed.
    Headers,
    /// Request or response body streaming failed.
    Body,
    /// The operation exceeded its configured deadline.
    Timeout,
    /// The operation was cancelled.
    Cancelled,
    /// The failure came from fixture or policy validation.
    Policy,
    /// The failure was not safely classifiable.
    Other,
}

/// A stable semantic category for an unsuccessful flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// DNS resolution failed.
    Dns,
    /// The peer refused a connection.
    ConnectionRefused,
    /// The destination could not be reached.
    Unreachable,
    /// TLS certificate or handshake verification failed.
    Tls,
    /// A request or response was malformed.
    Protocol,
    /// A configured limit or policy rejected the operation.
    Policy,
    /// A deadline expired.
    Timeout,
    /// The operation was cancelled.
    Cancelled,
    /// No more precise category is proven.
    Other,
}

/// Persistable flow error with safe, bounded context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{category:?} during {phase:?}: {message}")]
pub struct FlowError {
    /// Stable category.
    pub category: ErrorCategory,
    /// Operation phase.
    pub phase: ErrorPhase,
    /// Sanitized diagnostic detail; credentials and bodies must not appear.
    pub message: String,
}

impl ErrorCategory {
    /// The stable snake_case wire name.
    ///
    /// Presentation layers that record a category as a bounded string — the
    /// `stream-events` extension, for one — need the same spelling the
    /// serializer produces, without going through JSON. Kept in step with
    /// `#[serde(rename_all = "snake_case")]` by the unit tests below.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::ConnectionRefused => "connection_refused",
            Self::Unreachable => "unreachable",
            Self::Tls => "tls",
            Self::Protocol => "protocol",
            Self::Policy => "policy",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Other => "other",
        }
    }
}

impl ErrorPhase {
    /// The stable snake_case wire name, matching
    /// `#[serde(rename_all = "snake_case")]`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Headers => "headers",
            Self::Body => "body",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Policy => "policy",
            Self::Other => "other",
        }
    }
}

impl FlowError {
    /// Construct a bounded semantic error.
    pub fn new(category: ErrorCategory, phase: ErrorPhase, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 512 {
            message.truncate(512);
        }
        Self {
            category,
            phase,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `as_str` exists so a string-recording surface can use the same spelling
    /// the serializer emits. If the two ever disagree, a recorded stream event
    /// would say something the JSON decoder would not read back — so the
    /// contract is pinned against serde rather than against a literal.
    #[test]
    fn as_str_matches_the_serialized_wire_name() {
        for category in [
            ErrorCategory::Dns,
            ErrorCategory::ConnectionRefused,
            ErrorCategory::Unreachable,
            ErrorCategory::Tls,
            ErrorCategory::Protocol,
            ErrorCategory::Policy,
            ErrorCategory::Timeout,
            ErrorCategory::Cancelled,
            ErrorCategory::Other,
        ] {
            assert_eq!(
                serde_json::to_string(&category).expect("serialize"),
                format!("\"{}\"", category.as_str()),
                "category wire name drifted from as_str"
            );
        }
        for phase in [
            ErrorPhase::Request,
            ErrorPhase::Connect,
            ErrorPhase::Tls,
            ErrorPhase::Headers,
            ErrorPhase::Body,
            ErrorPhase::Timeout,
            ErrorPhase::Cancelled,
            ErrorPhase::Policy,
            ErrorPhase::Other,
        ] {
            assert_eq!(
                serde_json::to_string(&phase).expect("serialize"),
                format!("\"{}\"", phase.as_str()),
                "phase wire name drifted from as_str"
            );
        }
    }

    /// The `stream-events` validator bounds these strings to 64 bytes. Every
    /// name must fit, or a classified terminal error would be rejected at
    /// finalization instead of recorded.
    #[test]
    fn every_wire_name_fits_the_stream_event_bound() {
        assert!(ErrorCategory::ConnectionRefused.as_str().len() <= 64);
        assert!(ErrorCategory::Unreachable.as_str().len() <= 64);
        assert!(ErrorPhase::Headers.as_str().len() <= 64);
    }
}
