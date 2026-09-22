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
