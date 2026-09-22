//! Transport-neutral semantic contracts for EggReplay.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod config;
pub mod error;
pub mod flow;
pub mod matching;
pub mod report;
pub mod security;

pub use config::{Config, Limits, MatcherProfile, OutputFormat, RedactionProfile};
pub use error::{ErrorCategory, ErrorPhase, FlowError};
pub use flow::{
    BlobRef, BodyRef, Flow, FlowId, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse,
    PhysicalRoute, Provenance, QueryPair, RedactionMarker, SessionMetadata, Trailers,
};
pub use matching::{
    BodyMatchMode, ConsumptionMode, MatchCandidate, MatchDimension, MatchResult, Matcher,
    MatcherSession, NearMiss, NormalizedRequest,
};
pub use report::{DiffFinding, DiffKind, RegressionReport, ReportScheduler, compare_flows};
pub use security::{RedactionConfig, redact_flow, redact_json, redact_url};

/// The current persisted schema version.
pub const SCHEMA_VERSION: u16 = 1;

/// Tool version embedded in fixtures and reports.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
