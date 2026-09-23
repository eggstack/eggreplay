//! Transport-neutral semantic contracts for EggReplay.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod config;
pub mod error;
pub mod flow;
pub mod matching;
pub mod report;
pub mod scenario;
pub mod security;
pub mod stream;

pub use config::{
    Config, Limits, MatcherProfile, OutputFormat, RecordMode, RecordPolicy, RedactionProfile,
};
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
    ComparisonPolicy, DiffFinding, DiffKind, RegressionReport, ReportScheduler, TimingAssertion,
    compare_flows, compare_flows_with_policy, compare_flows_with_timing,
    compare_flows_with_timing_and_policy, compare_stream_events,
};
pub use scenario::{
    ExtractionFailureBehavior, JsonPointerReplacement, RULES_SCHEMA_VERSION,
    RenderedScenarioResponse, RequestPredicate, Scenario, ScenarioResponse, ScenarioRules,
    ScenarioRuntime, ScenarioStep, ScenarioTransition, VariableExtraction, VariableSource,
};
pub use security::{
    BODY_JSON_MARKER_PREFIX, DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
    RESPONSE_BODY_JSON_MARKER_PREFIX, RedactionConfig, apply_form_redaction, apply_json_redaction,
    push_body_markers, reconcile_headers_after_body_redaction, redact_flow, redact_json,
    redact_url,
};
pub use stream::{
    FlowStreamEvents, SseEvent, SseParseResult, StreamDirection, StreamEvent, StreamEventKind,
    StreamEvents, StreamTimingMode, compare_sse, parse_sse, timeline_order_offsets,
};

/// Legacy flow-schema constant retained for downstream source compatibility.
pub const SCHEMA_VERSION: u16 = FLOW_SCHEMA_VERSION;

/// Current flow record schema. Kept at v1 while session metadata evolves.
pub const FLOW_SCHEMA_VERSION: u16 = 1;

/// Original session manifest schema version.
pub const SESSION_SCHEMA_V1: u16 = 1;

/// Latest session manifest schema understood by this version.
pub const SESSION_SCHEMA_VERSION: u16 = 2;

/// Current JSON report schema version.
pub const REPORT_SCHEMA_VERSION: u16 = 1;

/// Tool version embedded in fixtures and reports.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
