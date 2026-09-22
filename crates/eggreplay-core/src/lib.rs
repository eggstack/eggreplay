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
    BodyMatchMode, CandidateBody, ConsumptionMode, MatchCandidate, MatchDimension, MatchResult,
    Matcher, MatcherSession, NearMiss, NormalizedRequest,
};
pub use report::{
    DiffFinding, DiffKind, RegressionReport, ReportScheduler, TimingAssertion, compare_flows,
    compare_flows_with_timing,
};
pub use security::{
    BODY_JSON_MARKER_PREFIX, DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
    RESPONSE_BODY_JSON_MARKER_PREFIX, RedactionConfig, apply_form_redaction, apply_json_redaction,
    push_body_markers, reconcile_headers_after_body_redaction, redact_flow, redact_json,
    redact_url,
};

/// The current persisted schema version.
pub const SCHEMA_VERSION: u16 = 1;

/// Tool version embedded in fixtures and reports.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
