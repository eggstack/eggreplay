//! Schema-1 semantic HTTP flow and session values.

use crate::{ErrorCategory, ErrorPhase, FlowError, SCHEMA_VERSION, TOOL_VERSION};
use serde::{Deserialize, Serialize};

/// A stable flow identifier.
pub type FlowId = String;

/// One ordered header field. Duplicate names are intentional and preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEntry {
    /// Header name as observed; matching can canonicalize it separately.
    pub name: String,
    /// Header value as UTF-8 text. Opaque invalid fields are rejected by the
    /// HTTP adapter before reaching the semantic model.
    pub value: String,
}

/// One ordered query key/value pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPair {
    /// Query key.
    pub key: String,
    /// Query value; an empty value is distinct from an absent key.
    pub value: String,
}

/// Ordered multi-value headers or trailers.
pub type Trailers = Vec<HeaderEntry>;

/// A content-addressed body reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyRef {
    /// No body was present or permitted.
    Absent,
    /// A body was present and had zero bytes.
    Empty,
    /// Body bytes are stored below `blobs/`.
    Blob(BlobRef),
}

impl BodyRef {
    /// Return the referenced byte length, if known.
    pub const fn len(&self) -> Option<u64> {
        match self {
            Self::Absent => None,
            Self::Empty => Some(0),
            Self::Blob(blob) => Some(blob.length),
        }
    }

    /// Return whether this body is the explicit zero-byte variant.
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// A validated SHA-256 body reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// Lowercase hexadecimal SHA-256 digest.
    pub sha256: String,
    /// Exact body length in bytes.
    pub length: u64,
}

impl BlobRef {
    /// Construct a blob reference after validating its digest form.
    pub fn new(sha256: impl Into<String>, length: u64) -> Result<Self, FlowError> {
        let sha256 = sha256.into();
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(FlowError::new(
                ErrorCategory::Policy,
                ErrorPhase::Policy,
                "blob digest must be 64 hexadecimal characters",
            ));
        }
        let sha256 = sha256.to_ascii_lowercase();
        Ok(Self { sha256, length })
    }
}

/// A semantic HTTP request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    /// HTTP method token.
    pub method: String,
    /// Logical scheme, such as `http` or `https`.
    pub scheme: String,
    /// Logical authority, excluding userinfo.
    pub authority: String,
    /// Origin-form path.
    pub path: String,
    /// Ordered query values.
    #[serde(default)]
    pub query: Vec<QueryPair>,
    /// Ordered request headers.
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    /// Request body reference.
    pub body: BodyRef,
    /// Request trailers, if a terminal trailer block was observed.
    #[serde(default)]
    pub trailers: Trailers,
}

/// A semantic HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    /// Numeric HTTP status code.
    pub status: u16,
    /// Ordered response headers.
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    /// Response body reference.
    pub body: BodyRef,
    /// Response trailers, if a terminal trailer block was observed.
    #[serde(default)]
    pub trailers: Trailers,
}

/// The mutually exclusive result of a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlowOutcome {
    /// A response was received.
    Response(HttpResponse),
    /// No response was received and the transport produced a semantic error.
    Error(FlowError),
}

/// Physical route metadata kept separate from the logical origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalRoute {
    /// Route kind, for example `direct` or `eggress`.
    pub kind: String,
    /// Redaction-safe route description.
    pub description: Option<String>,
}

/// Capture provenance metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Acquisition mode.
    pub mode: String,
    /// Component that observed the transaction.
    pub observer: String,
}

/// Explicit redaction marker attached to a flow field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionMarker {
    /// Field path, for example `request.headers.authorization`.
    pub field: String,
    /// Policy identifier that caused the redaction.
    pub profile: String,
}

/// One recorded semantic transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    /// Schema version of this record.
    pub schema_version: u16,
    /// Stable flow identifier.
    pub id: FlowId,
    /// Request start time in Unix milliseconds.
    pub started_at_ms: u64,
    /// Request completion time in Unix milliseconds, when known.
    pub completed_at_ms: Option<u64>,
    /// Logical HTTP request.
    pub request: HttpRequest,
    /// Response or semantic error, never both.
    pub outcome: FlowOutcome,
    /// Optional physical route.
    pub physical_route: Option<PhysicalRoute>,
    /// Capture provenance.
    pub provenance: Provenance,
    /// Free-form bounded annotations.
    #[serde(default)]
    pub annotations: Vec<(String, String)>,
    /// Fields intentionally redacted from this flow.
    #[serde(default)]
    pub redactions: Vec<RedactionMarker>,
}

impl Flow {
    /// Construct a schema-1 flow with a generated identifier.
    pub fn new(request: HttpRequest, outcome: FlowOutcome, started_at_ms: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: format!("flow-{started_at_ms}-0"),
            started_at_ms,
            completed_at_ms: None,
            request,
            outcome,
            physical_route: None,
            provenance: Provenance {
                mode: "unknown".into(),
                observer: "eggreplay".into(),
            },
            annotations: Vec::new(),
            redactions: Vec::new(),
        }
    }

    /// Validate schema and cheap semantic invariants before persistence.
    pub fn validate(&self) -> Result<(), FlowError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(FlowError::new(
                ErrorCategory::Policy,
                ErrorPhase::Policy,
                "unsupported flow schema",
            ));
        }
        if self.id.is_empty()
            || self.id.len() > 256
            || self.id.contains('/')
            || self.id.contains('\\')
        {
            return Err(FlowError::new(
                ErrorCategory::Policy,
                ErrorPhase::Policy,
                "invalid flow identifier",
            ));
        }
        if self.request.method.is_empty()
            || self.request.path.is_empty()
            || !self.request.path.starts_with('/')
        {
            return Err(FlowError::new(
                ErrorCategory::Protocol,
                ErrorPhase::Request,
                "invalid request line fields",
            ));
        }
        if let Some(end) = self.completed_at_ms
            && end < self.started_at_ms
        {
            return Err(FlowError::new(
                ErrorCategory::Policy,
                ErrorPhase::Policy,
                "flow completion precedes start",
            ));
        }
        if let FlowOutcome::Response(response) = &self.outcome
            && !(100..=599).contains(&response.status)
        {
            return Err(FlowError::new(
                ErrorCategory::Protocol,
                ErrorPhase::Headers,
                "invalid response status",
            ));
        }
        Ok(())
    }
}

/// Session-level metadata stored in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// Schema version of the session.
    pub schema_version: u16,
    /// Tool version that wrote the session.
    pub tool_version: String,
    /// Stable session identifier.
    pub session_id: String,
    /// Capture mode.
    pub capture_mode: String,
    /// Logical source description, already redacted.
    pub source: Option<String>,
    /// Logical target description, already redacted.
    pub target: Option<String>,
    /// Matcher profile identifier.
    pub matcher_profile: String,
    /// Redaction profile identifier.
    pub redaction_profile: String,
}

impl Default for SessionMetadata {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tool_version: TOOL_VERSION.into(),
            session_id: "session-0".into(),
            capture_mode: "semantic".into(),
            source: None,
            target: None,
            matcher_profile: "strict".into(),
            redaction_profile: "default-v1".into(),
        }
    }
}
