//! Transport-neutral semantic contracts for EggReplay.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod config;
pub mod error;
pub mod flow;

pub use config::{Config, Limits, MatcherProfile, OutputFormat, RedactionProfile};
pub use error::{ErrorCategory, ErrorPhase, FlowError};
pub use flow::{
    BlobRef, BodyRef, Flow, FlowId, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse,
    PhysicalRoute, Provenance, QueryPair, RedactionMarker, SessionMetadata, Trailers,
};

/// The current persisted schema version.
pub const SCHEMA_VERSION: u16 = 1;

/// Tool version embedded in fixtures and reports.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
