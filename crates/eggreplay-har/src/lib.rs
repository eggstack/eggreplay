//! Lossy HAR interchange and explicit fixture migration for `EggReplay`.
//!
//! HAR is never canonical. Import maps HAR 1.2 entries into semantic flows
//! with a structured loss report; export projects semantic flows into HAR
//! with an explicit `_eggreplay` provenance/loss section. Both directions
//! are lossy by design and never imply round-trip losslessness.
//!
//! Transport ownership is unchanged: this crate performs no network I/O and
//! adds no HTTP stack. It converts already-observed HAR JSON into
//! [`eggreplay_core`] flows (via [`eggreplay_store`] writers) and projects
//! opened sessions back into HAR JSON. Secrets in imported HAR pass through
//! the caller's [`eggreplay_core::RedactionConfig`] before any blob is
//! published.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use base64::Engine as _;
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, Flow, FlowError, FlowOutcome, HeaderEntry, HttpRequest,
    HttpResponse, Provenance, QueryPair, RedactionConfig, RedactionMarker, apply_form_redaction,
    apply_json_redaction, push_body_markers, reconcile_headers_after_body_redaction,
};
use eggreplay_store::{Session, SessionWriter, StoreError};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::Path;

/// Supported HAR `log.version` values.
pub const HAR_VERSION_1_2: &str = "1.2";
/// Maximum accepted HAR document bytes (32 MiB).
pub const MAX_HAR_FILE_BYTES: usize = 32 * 1024 * 1024;
/// Maximum HAR entries accepted per import (matches store flow ceiling).
pub const MAX_HAR_ENTRIES: usize = 100_000;
/// Schema version for the optional `interop-provenance` extension written on import.
pub const INTEROP_PROVENANCE_SCHEMA_VERSION: u16 = 1;
/// HAR interchange is always lossy; this marker is embedded in every export.
pub const HAR_LOSSY_NOTICE: &str =
    "Lossy HAR export from EggReplay; not round-trip lossless. See log._eggreplay.losses.";

/// Current session schema understood by migration.
pub const CURRENT_SESSION_SCHEMA: u16 = eggreplay_core::SESSION_SCHEMA_VERSION;

/// Known session extensions and their current schema versions.
///
/// Unknown required extensions block migration unless a registered migrator
/// owns them (see [`registered_migrators`]).
pub const KNOWN_EXTENSIONS: &[(&str, u16)] = &[
    ("rules", eggreplay_core::RULES_SCHEMA_VERSION),
    (
        "stream-events",
        eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
    ),
    (
        "websocket-messages",
        eggreplay_core::WEBSOCKET_SCHEMA_VERSION,
    ),
    ("interop-provenance", INTEROP_PROVENANCE_SCHEMA_VERSION),
];

/// Structured loss action for one HAR field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LossAction {
    /// Value preserved into the semantic model.
    Preserved,
    /// Value preserved with an explicit annotation about representation change.
    Annotated,
    /// Value omitted because HAR cannot represent it; recorded in the report.
    Omitted,
    /// Value replaced by the redaction policy before publication.
    Redacted,
    /// Entry rejected entirely (unsupported/ambiguous with no safe mapping).
    Rejected,
}

/// One structured loss entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarLoss {
    /// Zero-based HAR entry index, when per-entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_index: Option<usize>,
    /// Flow identifier, when already assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
    /// Field path, for example `request.cookies[2]` or `response.trailers`.
    pub field: String,
    /// Human-readable reason (bounded, secret-free).
    pub reason: String,
    /// What happened to the value.
    pub action: LossAction,
}

impl HarLoss {
    /// Construct a global (non-entry) loss.
    pub fn global(field: impl Into<String>, reason: impl Into<String>, action: LossAction) -> Self {
        Self {
            entry_index: None,
            flow_id: None,
            field: field.into(),
            reason: reason.into(),
            action,
        }
    }

    /// Construct a per-entry loss.
    pub fn entry(
        index: usize,
        field: impl Into<String>,
        reason: impl Into<String>,
        action: LossAction,
    ) -> Self {
        Self {
            entry_index: Some(index),
            flow_id: None,
            field: field.into(),
            reason: reason.into(),
            action,
        }
    }
}

/// Report produced by [`import_har_to_writer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Number of flows published.
    pub flows: usize,
    /// Structured per-field losses.
    pub losses: Vec<HarLoss>,
    /// Number of HAR entries skipped as failed/ambiguous (mapped to typed errors still counts as flow).
    pub entries: usize,
}

/// Report produced by [`export_session_to_har`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReport {
    /// Number of HAR entries emitted.
    pub entries: usize,
    /// Structured per-field losses.
    pub losses: Vec<HarLoss>,
}

/// HAR interchange failures.
#[derive(Debug, thiserror::Error)]
pub enum HarError {
    /// HAR JSON was malformed or exceeded a bound.
    #[error("invalid HAR: {0}")]
    Invalid(String),
    /// One HAR entry could not be safely mapped.
    #[error("unsupported HAR entry {index}: {reason}")]
    UnsupportedEntry {
        /// Entry index.
        index: usize,
        /// Reason.
        reason: String,
    },
    /// Fixture publication failed.
    #[error("fixture: {0}")]
    Store(#[from] StoreError),
    /// Body redaction failed closed.
    #[error("redaction failed closed: {0}")]
    Redaction(String),
}

/// Migration failures with explicit blocking reasons.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// Source fixture could not be opened.
    #[error("fixture: {0}")]
    Store(#[from] StoreError),
    /// Migration blocked by unknown required extensions or future schemas.
    #[error("migration blocked: {0}")]
    Blocked(String),
    /// Filesystem replacement failed.
    #[error("runtime: {0}")]
    Runtime(String),
}

// ---------------------------------------------------------------------------
// HAR 1.2 wire format (tolerant subset)
// ---------------------------------------------------------------------------

/// Top-level HAR document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarDocument {
    log: HarLog,
}

/// HAR `log` object. Unknown fields are ignored (and reported as loss when
/// they carry semantic content such as `pages`/`browser`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarLog {
    version: String,
    #[serde(default)]
    creator: Option<HarCreator>,
    #[serde(default)]
    browser: Option<serde_json::Value>,
    #[serde(default)]
    pages: Option<Vec<serde_json::Value>>,
    entries: Vec<HarEntry>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarCreator {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarEntry {
    #[serde(default)]
    pageref: Option<String>,
    #[serde(rename = "startedDateTime")]
    #[serde(default)]
    started_date_time: Option<String>,
    #[serde(default)]
    time: Option<serde_json::Value>,
    request: HarRequest,
    response: HarResponse,
    #[serde(default)]
    cache: Option<serde_json::Value>,
    timings: Option<HarTimings>,
    #[serde(default, rename = "serverIPAddress")]
    server_ip_address: Option<String>,
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarRequest {
    method: String,
    url: String,
    #[serde(default, rename = "httpVersion")]
    http_version: Option<String>,
    #[serde(default)]
    cookies: Vec<HarCookie>,
    #[serde(default)]
    headers: Vec<HarNameValue>,
    #[serde(default, rename = "queryString")]
    query_string: Vec<HarNameValue>,
    #[serde(default, rename = "postData")]
    post_data: Option<HarPostData>,
    #[serde(default, rename = "headersSize")]
    headers_size: Option<serde_json::Value>,
    #[serde(default, rename = "bodySize")]
    body_size: Option<serde_json::Value>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarResponse {
    status: serde_json::Value,
    #[serde(default, rename = "statusText")]
    status_text: Option<String>,
    #[serde(default, rename = "httpVersion")]
    http_version: Option<String>,
    #[serde(default)]
    cookies: Vec<HarCookie>,
    #[serde(default)]
    headers: Vec<HarNameValue>,
    #[serde(default)]
    content: Option<HarContent>,
    #[serde(default, rename = "redirectURL")]
    redirect_url: Option<String>,
    #[serde(default, rename = "headersSize")]
    headers_size: Option<serde_json::Value>,
    #[serde(default, rename = "bodySize")]
    body_size: Option<serde_json::Value>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarContent {
    #[serde(default)]
    size: Option<i64>,
    #[serde(default)]
    compression: Option<serde_json::Value>,
    #[serde(default, rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarCookie {
    name: String,
    value: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    expires: Option<String>,
    #[serde(default, rename = "httpOnly")]
    http_only: Option<bool>,
    #[serde(default)]
    secure: Option<bool>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarNameValue {
    name: String,
    value: String,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarPostData {
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    params: Vec<HarPostParam>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarPostParam {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "fileName")]
    file_name: Option<String>,
    #[serde(default, rename = "contentType")]
    content_type: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarTimings {
    #[serde(default)]
    blocked: Option<serde_json::Value>,
    #[serde(default)]
    dns: Option<serde_json::Value>,
    #[serde(default)]
    connect: Option<serde_json::Value>,
    #[serde(default)]
    send: Option<serde_json::Value>,
    #[serde(default)]
    wait: Option<serde_json::Value>,
    #[serde(default)]
    receive: Option<serde_json::Value>,
    #[serde(default)]
    ssl: Option<serde_json::Value>,
    #[serde(default)]
    comment: Option<String>,
}

/// Optional `interop-provenance` extension payload written on HAR import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteropProvenance {
    /// Extension schema version.
    pub schema_version: u16,
    /// Tool version that performed the import.
    pub tool_version: String,
    /// Provenance source marker (`har-import`).
    pub source: String,
    /// Upstream HAR creator, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub har_creator: Option<String>,
    /// Number of HAR entries consumed.
    pub entries: usize,
    /// Number of flows published.
    pub flows: usize,
    /// Structured losses (bounded by extension caps via count limit).
    #[serde(default)]
    pub losses: Vec<HarLoss>,
}

// ---------------------------------------------------------------------------
// Migration registry
// ---------------------------------------------------------------------------

/// Pending flow plus staged body bytes awaiting transactional publication.
type PendingFlow = (Flow, Vec<u8>, Vec<u8>, Vec<RedactionMarker>);

/// Return the registered extension migrators as `(name, current_schema)`.
///
/// Only identity migration (current-to-current) is implemented; older
/// registered schemas would list their migrator here. Unknown required
/// extensions are never silently ignored.
pub fn registered_migrators() -> Vec<(String, u16)> {
    KNOWN_EXTENSIONS
        .iter()
        .map(|(name, version)| ((*name).to_owned(), *version))
        .collect()
}

/// Return blocking reasons for migrating `manifest`'s extensions.
///
/// An empty vector means migration may proceed via [`migrate_session`].
pub fn migration_blockers(extensions: &[eggreplay_store::ExtensionDescriptor]) -> Vec<String> {
    let mut blockers = Vec::new();
    for extension in extensions {
        match KNOWN_EXTENSIONS
            .iter()
            .find(|(name, _)| *name == extension.name)
        {
            None => {
                if extension.required_for_replay {
                    blockers.push(format!(
                        "unknown required extension {} (schema {}) has no registered migrator",
                        extension.name, extension.schema_version
                    ));
                }
            }
            Some((_, current)) => {
                if extension.schema_version != *current {
                    blockers.push(format!(
                        "extension {} schema {} is not the current schema {current} and has no registered migrator",
                        extension.name, extension.schema_version
                    ));
                }
            }
        }
    }
    blockers
}

/// Migrate `source` into `destination` at `target_schema`.
///
/// The source is never mutated. The destination must not exist. Extension
/// blockers from [`migration_blockers`] fail closed before any copy.
///
/// # Errors
///
/// Returns [`MigrationError::Blocked`] for unsupported target schemas or
/// unregistered required extensions, and [`MigrationError::Store`] when the
/// transactional copy fails.
pub fn migrate_session(
    source: &Session,
    destination: &Path,
    target_schema: u16,
) -> Result<Session, MigrationError> {
    if !(eggreplay_core::SESSION_SCHEMA_V1..=CURRENT_SESSION_SCHEMA).contains(&target_schema) {
        return Err(MigrationError::Blocked(format!(
            "unsupported target session schema {target_schema}"
        )));
    }
    let blockers = migration_blockers(&source.manifest().extensions);
    if !blockers.is_empty() {
        return Err(MigrationError::Blocked(blockers.join("; ")));
    }
    // `copy_to` streams and revalidates every blob/flow/extension and
    // publishes transactionally; downgrades with extensions fail closed
    // inside the store.
    source
        .copy_to(destination, target_schema)
        .map_err(MigrationError::Store)
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

fn is_json_media_type(value: Option<&str>) -> bool {
    matches!(value, Some(media) if media == "application/json" || media.ends_with("+json"))
}

fn is_form_media_type(value: Option<&str>) -> bool {
    matches!(value, Some("application/x-www-form-urlencoded"))
}

fn base_media_type(value: Option<&str>) -> Option<String> {
    value.map(|raw| {
        raw.split(';')
            .next()
            .unwrap_or(raw)
            .trim()
            .to_ascii_lowercase()
    })
}

fn decode_har_body_text(
    text: Option<&str>,
    encoding: Option<&str>,
    max_bytes: u64,
) -> Result<Vec<u8>, HarError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    match encoding.map(str::to_ascii_lowercase).as_deref() {
        None | Some("") => {
            let bytes = text.as_bytes().to_vec();
            if bytes.len() as u64 > max_bytes {
                return Err(HarError::Invalid(
                    "HAR body exceeds configured limit".into(),
                ));
            }
            Ok(bytes)
        }
        Some("base64") => {
            let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            // Bound the encoded length before decoding (4/3 expansion).
            if cleaned.len() as u64 > max_bytes.saturating_mul(4).saturating_add(4) {
                return Err(HarError::Invalid(
                    "HAR body exceeds configured limit".into(),
                ));
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(cleaned.as_bytes())
                .map_err(|_| HarError::Invalid("HAR body is not valid base64".into()))?;
            if decoded.len() as u64 > max_bytes {
                return Err(HarError::Invalid(
                    "HAR body exceeds configured limit".into(),
                ));
            }
            Ok(decoded)
        }
        Some(other) => Err(HarError::Invalid(format!(
            "unsupported HAR body encoding {other}"
        ))),
    }
}

fn parse_har_time_ms(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Option<f64>, HarError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(number) = value.as_f64() {
        return Ok(Some(number));
    }
    if let Some(text) = value.as_str() {
        // Some exporters emit timings as strings; accept numeric strings.
        let parsed: f64 = text
            .parse()
            .map_err(|_| HarError::Invalid(format!("HAR {field} is not numeric")))?;
        return Ok(Some(parsed));
    }
    Err(HarError::Invalid(format!("HAR {field} is not numeric")))
}

fn started_ms(started: Option<&str>) -> Result<u64, HarError> {
    let Some(raw) = started else {
        return Err(HarError::Invalid(
            "HAR entry is missing startedDateTime".into(),
        ));
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|_| HarError::Invalid("HAR startedDateTime is not RFC 3339".into()))?;
    let millis = parsed.timestamp_millis();
    u64::try_from(millis.max(0))
        .map_err(|_| HarError::Invalid("HAR startedDateTime is out of range".into()))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn total_time_ms(time: Option<&serde_json::Value>) -> Result<u64, HarError> {
    let Some(value) = parse_har_time_ms(time, "entry.time")? else {
        return Ok(0);
    };
    if value.is_nan() || value.is_infinite() || value < 0.0 {
        return Err(HarError::Invalid(
            "HAR entry.time is not a valid duration".into(),
        ));
    }
    // Clamped to 0..=3_600_000 so the float-to-int cast is lossless in range.
    Ok(value.round().clamp(0.0, 3_600_000.0) as u64)
}

fn timing_number(value: Option<&serde_json::Value>) -> Option<f64> {
    let value = value?;
    if let Some(number) = value.as_f64() {
        // HAR uses -1 for "not applicable".
        if number < 0.0 {
            return None;
        }
        return Some(number);
    }
    value
        .as_str()
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|number| *number >= 0.0)
}

fn status_code(status: &serde_json::Value) -> Result<Option<u16>, HarError> {
    if let Some(number) = status.as_u64() {
        if number == 0 {
            return Ok(None);
        }
        if !(100..=599).contains(&number) {
            return Err(HarError::Invalid(format!(
                "HAR response status {number} is outside 100..=599"
            )));
        }
        let code = u16::try_from(number)
            .map_err(|_| HarError::Invalid("HAR response status is out of range".into()))?;
        return Ok(Some(code));
    }
    if let Some(text) = status.as_str() {
        let trimmed = text.trim();
        if trimmed == "0" || trimmed.is_empty() {
            return Ok(None);
        }
        let parsed: u64 = trimmed
            .parse()
            .map_err(|_| HarError::Invalid("HAR response status is not numeric".into()))?;
        if parsed == 0 {
            return Ok(None);
        }
        if !(100..=599).contains(&parsed) {
            return Err(HarError::Invalid(format!(
                "HAR response status {parsed} is outside 100..=599"
            )));
        }
        let code = u16::try_from(parsed)
            .map_err(|_| HarError::Invalid("HAR response status is out of range".into()))?;
        return Ok(Some(code));
    }
    Err(HarError::Invalid(
        "HAR response status is not numeric".into(),
    ))
}

fn header_entries(values: &[HarNameValue], limit: usize) -> Result<Vec<HeaderEntry>, HarError> {
    if values.len() > limit {
        return Err(HarError::Invalid("HAR header count exceeds limit".into()));
    }
    let mut out = Vec::with_capacity(values.len());
    for item in values {
        if item.name.is_empty() || item.name.len() > 256 || item.value.len() > 8192 {
            return Err(HarError::Invalid(
                "HAR header name/value exceeds limit".into(),
            ));
        }
        // Preserve observed case and duplicates; matching canonicalizes separately.
        out.push(HeaderEntry {
            name: item.name.clone(),
            value: item.value.clone(),
        });
    }
    Ok(out)
}

fn split_url(url: &str, index: usize) -> Result<(String, String, String, String), HarError> {
    let parsed: url::Url = url.parse().map_err(|_| HarError::UnsupportedEntry {
        index,
        reason: "URL is not a valid absolute HTTP(S) URL".into(),
    })?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(HarError::UnsupportedEntry {
            index,
            reason: format!("URL scheme {} is not http/https", parsed.scheme()),
        });
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| HarError::UnsupportedEntry {
            index,
            reason: "URL has no host".into(),
        })?;
    let authority = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let path = {
        let raw = parsed.path();
        if raw.is_empty() {
            "/".to_owned()
        } else {
            raw.to_owned()
        }
    };
    if !path.starts_with('/') {
        return Err(HarError::UnsupportedEntry {
            index,
            reason: "URL path is not origin-form".into(),
        });
    }
    Ok((
        parsed.scheme().to_owned(),
        authority,
        path,
        parsed.as_str().to_owned(),
    ))
}

/// Apply the selected redaction policy to buffered body bytes.
///
/// Mirrors the recording gateway: JSON/form bodies transform when their media
/// type matches; any other non-empty body with structured selectors fails
/// closed rather than publishing unredacted bytes.
#[allow(clippy::too_many_arguments)]
fn redact_import_body(
    original: Vec<u8>,
    media_type: Option<&str>,
    config: &RedactionConfig,
    profile_id: &str,
    is_response: bool,
    losses: &mut Vec<HarLoss>,
    index: usize,
    side: &str,
    max_structured_bytes: u64,
) -> Result<(Vec<u8>, Vec<RedactionMarker>), HarError> {
    if original.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if config.wants_body_redaction()
        && u64::try_from(original.len()).unwrap_or(u64::MAX) > max_structured_bytes
    {
        return Err(HarError::Redaction(format!(
            "entry {index} {side} body exceeds structured redaction bound; failing closed"
        )));
    }
    let base = base_media_type(media_type);
    if !config.json_paths.is_empty() && is_json_media_type(base.as_deref()) {
        let (redacted, _) =
            apply_json_redaction(&original, &config.json_paths).map_err(HarError::Redaction)?;
        let mut markers = Vec::new();
        push_body_markers(&mut markers, &config.json_paths, profile_id, is_response);
        losses.push(HarLoss {
            entry_index: Some(index),
            flow_id: None,
            field: format!("{side}.body.json"),
            reason: "JSON body paths redacted per selected profile".into(),
            action: LossAction::Redacted,
        });
        return Ok((redacted, markers));
    }
    if !config.query_keys.is_empty() && is_form_media_type(base.as_deref()) {
        let (redacted, _) =
            apply_form_redaction(&original, &config.query_keys).map_err(HarError::Redaction)?;
        let markers = config
            .query_keys
            .iter()
            .map(|key| RedactionMarker {
                field: format!("{side}.body.form:{key}"),
                profile: profile_id.into(),
            })
            .collect();
        losses.push(HarLoss {
            entry_index: Some(index),
            flow_id: None,
            field: format!("{side}.body.form"),
            reason: "form body keys redacted per selected profile".into(),
            action: LossAction::Redacted,
        });
        return Ok((redacted, markers));
    }
    if !config.json_paths.is_empty() {
        return Err(HarError::Redaction(format!(
            "entry {index} {side} body redaction requested but media type is not JSON; failing closed"
        )));
    }
    Ok((original, Vec::new()))
}

fn publish_body(writer: &mut SessionWriter, bytes: &[u8]) -> Result<BodyRef, HarError> {
    if bytes.is_empty() {
        return Ok(BodyRef::Empty);
    }
    let mut sink = writer.begin_blob().map_err(HarError::Store)?;
    sink.write_all(bytes)
        .map_err(|error| HarError::Invalid(format!("body write failed: {error}")))?;
    sink.finish().map_err(HarError::Store)
}

/// Import one HAR document into `writer`.
///
/// The writer must be freshly created; this function appends one flow per
/// convertible entry, applies `redaction` before any blob publication, and
/// returns a structured loss report. Unsupported entries fail the whole
/// import (no partial fixture) so callers never publish a silently degraded
/// subset.
///
/// # Errors
///
/// Returns [`HarError::Invalid`] for malformed or over-limit HAR,
/// [`HarError::UnsupportedEntry`] for entries with no safe mapping,
/// [`HarError::Redaction`] when the policy fails closed, and
/// [`HarError::Store`] when fixture publication fails.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub fn import_har_to_writer(
    writer: &mut SessionWriter,
    har_bytes: &[u8],
    redaction: &RedactionConfig,
    profile_id: &str,
    max_structured_bytes: u64,
    max_blob_bytes: u64,
) -> Result<ImportReport, HarError> {
    if har_bytes.len() > MAX_HAR_FILE_BYTES {
        return Err(HarError::Invalid("HAR document exceeds size limit".into()));
    }
    let document: HarDocument =
        serde_json::from_slice(har_bytes).map_err(|error| HarError::Invalid(error.to_string()))?;
    if document.log.version != HAR_VERSION_1_2 {
        return Err(HarError::Invalid(format!(
            "unsupported HAR version {} (only 1.2 is accepted)",
            document.log.version
        )));
    }
    if document.log.entries.len() > MAX_HAR_ENTRIES {
        return Err(HarError::Invalid("HAR entry count exceeds limit".into()));
    }

    let mut losses: Vec<HarLoss> = Vec::new();
    if document.log.browser.is_some() {
        losses.push(HarLoss::global(
            "log.browser",
            "HAR browser object has no EggReplay representation; omitted",
            LossAction::Omitted,
        ));
    }
    if document
        .log
        .pages
        .as_ref()
        .is_some_and(|pages| !pages.is_empty())
    {
        losses.push(HarLoss::global(
            "log.pages",
            "HAR pages have no EggReplay representation; entry timing uses startedDateTime/time only",
            LossAction::Omitted,
        ));
    }
    if document
        .log
        .creator
        .as_ref()
        .is_some_and(|creator| !creator.name.is_empty() && creator.name != "eggreplay")
    {
        losses.push(HarLoss::global(
            "log.creator",
            "upstream HAR creator preserved in interop-provenance only",
            LossAction::Annotated,
        ));
    }

    let mut pending: Vec<PendingFlow> = Vec::new();

    for (index, entry) in document.log.entries.iter().enumerate() {
        // --- Request URL ---
        if entry.request.method.is_empty() || entry.request.method.len() > 32 {
            return Err(HarError::UnsupportedEntry {
                index,
                reason: "request method is empty or exceeds limit".into(),
            });
        }
        let (scheme, authority, path, _) = split_url(&entry.request.url, index)?;
        // Strip userinfo: the url crate already excludes it from host, but
        // detect `user@` in the raw string to record the redaction.
        if entry.request.url.contains('@')
            && let Ok(parsed) = entry.request.url.parse::<url::Url>()
            && (!parsed.username().is_empty() || parsed.password().is_some())
        {
            losses.push(HarLoss::entry(
                index,
                "request.url.userinfo",
                "URL userinfo stripped; never persisted",
                LossAction::Redacted,
            ));
        }

        // --- Query: queryString array is authoritative for ordering; the URL
        // query component must agree or the divergence is recorded. ---
        let mut query = Vec::with_capacity(entry.request.query_string.len());
        if entry.request.query_string.len() > 1024 {
            return Err(HarError::UnsupportedEntry {
                index,
                reason: "query parameter count exceeds limit".into(),
            });
        }
        for param in &entry.request.query_string {
            if param.name.len() > 1024 || param.value.len() > 8192 {
                return Err(HarError::UnsupportedEntry {
                    index,
                    reason: "query parameter exceeds limit".into(),
                });
            }
            query.push(QueryPair {
                key: param.name.clone(),
                value: param.value.clone(),
            });
        }
        // Cross-check against the URL query component (decoded pairs).
        if let Ok(parsed) = entry.request.url.parse::<url::Url>() {
            let url_pairs: Vec<(String, String)> = parsed
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let array_pairs: Vec<(String, String)> = query
                .iter()
                .map(|pair| (pair.key.clone(), pair.value.clone()))
                .collect();
            if url_pairs != array_pairs {
                // Empty queryString with a non-empty URL query is the common
                // exporter shortcut; adopt the URL pairs rather than dropping
                // the query silently.
                if entry.request.query_string.is_empty() && !url_pairs.is_empty() {
                    query = url_pairs
                        .into_iter()
                        .map(|(key, value)| QueryPair { key, value })
                        .collect();
                    losses.push(HarLoss::entry(
                        index,
                        "request.queryString",
                        "empty queryString adopted from URL query component",
                        LossAction::Annotated,
                    ));
                } else {
                    losses.push(HarLoss::entry(
                        index,
                        "request.queryString",
                        "queryString array and URL query component disagree; array is authoritative",
                        LossAction::Annotated,
                    ));
                }
            }
        }

        // --- Headers: the headers array is authoritative. The cookies array
        // is a derived view; mismatches are recorded, never merged. ---
        let request_headers = header_entries(&entry.request.headers, 256)?;
        if !entry.request.cookies.is_empty() {
            let cookie_headers: Vec<&HeaderEntry> = request_headers
                .iter()
                .filter(|header| {
                    header.name.eq_ignore_ascii_case("cookie")
                        || header.name.eq_ignore_ascii_case("cookie2")
                })
                .collect();
            let cookie_text = cookie_headers
                .iter()
                .map(|header| header.value.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            let mut missing = 0usize;
            for cookie in &entry.request.cookies {
                // Cookie names/values are percent-encoded in headers; check
                // for a plain substring as a best-effort consistency signal.
                if !cookie_text.contains(&cookie.name) {
                    missing += 1;
                }
            }
            if missing > 0 {
                losses.push(HarLoss::entry(
                    index,
                    "request.cookies",
                    format!(
                        "{missing} cookie(s) have no corresponding Cookie header; headers array is authoritative"
                    ),
                    LossAction::Annotated,
                ));
            } else {
                losses.push(HarLoss::entry(
                    index,
                    "request.cookies",
                    "cookies array is a derived view of Cookie headers; headers array is authoritative",
                    LossAction::Annotated,
                ));
            }
        }
        if entry
            .request
            .http_version
            .as_deref()
            .is_some_and(|version| version != "http/1.1" && version != "HTTP/1.1")
        {
            losses.push(HarLoss::entry(
                index,
                "request.httpVersion",
                format!(
                    "HTTP version {} has no semantic representation; treated as version-agnostic",
                    entry.request.http_version.as_deref().unwrap_or("")
                ),
                LossAction::Annotated,
            ));
        }
        if entry.request.comment.is_some() {
            losses.push(HarLoss::entry(
                index,
                "request.comment",
                "HAR comment preserved as flow annotation",
                LossAction::Annotated,
            ));
        }

        // --- Request body ---
        let request_media = entry.request.post_data.as_ref().map(|post| {
            let raw = post.mime_type.split(';').next().unwrap_or(&post.mime_type);
            raw.trim().to_ascii_lowercase()
        });
        // postData.params without text: form-encoded params without a text
        // body (some exporters split them). Re-encode deterministically.
        let mut request_raw: Vec<u8> = Vec::new();
        let mut request_had_body_source = false;
        if let Some(post) = &entry.request.post_data {
            if let Some(text) = post.text.as_deref() {
                request_had_body_source = true;
                request_raw =
                    decode_har_body_text(Some(text), post.encoding.as_deref(), max_blob_bytes)?;
            } else if !post.params.is_empty() {
                request_had_body_source = true;
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                for param in &post.params {
                    if param.file_name.is_some() {
                        return Err(HarError::UnsupportedEntry {
                            index,
                            reason:
                                "multipart/file upload postData has no semantic body representation"
                                    .into(),
                        });
                    }
                    serializer.append_pair(&param.name, param.value.as_deref().unwrap_or(""));
                }
                request_raw = serializer.finish().into_bytes();
                losses.push(HarLoss::entry(
                    index,
                    "request.postData.params",
                    "form params without text re-encoded as application/x-www-form-urlencoded",
                    LossAction::Annotated,
                ));
            } else {
                // postData present but empty: explicit zero-byte body.
                request_had_body_source = true;
                request_raw = Vec::new();
            }
            if !post.params.is_empty() && post.text.is_some() {
                losses.push(HarLoss::entry(
                    index,
                    "request.postData.params",
                    "postData params duplicate postData.text; text is authoritative",
                    LossAction::Annotated,
                ));
            }
        }
        let (request_bytes, mut request_markers) = redact_import_body(
            request_raw,
            request_media.as_deref(),
            redaction,
            profile_id,
            false,
            &mut losses,
            index,
            "request",
            max_structured_bytes,
        )?;
        // Bound structured redaction buffering explicitly.
        if u64::try_from(request_bytes.len()).unwrap_or(u64::MAX) > max_blob_bytes {
            return Err(HarError::Invalid("HAR request body exceeds limit".into()));
        }
        if !request_had_body_source && !request_bytes.is_empty() {
            return Err(HarError::Invalid(
                "request body state is inconsistent".into(),
            ));
        }

        // --- Response ---
        let status = status_code(&entry.response.status)?;
        let response_headers = header_entries(&entry.response.headers, 256)?;
        if !entry.response.cookies.is_empty() {
            losses.push(HarLoss::entry(
                index,
                "response.cookies",
                "cookies array is a derived view of Set-Cookie headers; headers array is authoritative",
                LossAction::Annotated,
            ));
        }
        if entry
            .response
            .http_version
            .as_deref()
            .is_some_and(|version| version != "http/1.1" && version != "HTTP/1.1")
        {
            losses.push(HarLoss::entry(
                index,
                "response.httpVersion",
                "HTTP version has no semantic representation; treated as version-agnostic",
                LossAction::Annotated,
            ));
        }
        if entry
            .response
            .redirect_url
            .as_ref()
            .is_some_and(|url| !url.is_empty())
        {
            losses.push(HarLoss::entry(
                index,
                "response.redirectURL",
                "redirectURL duplicates the Location header; header is authoritative",
                LossAction::Annotated,
            ));
        }
        if entry.response.comment.is_some() {
            losses.push(HarLoss::entry(
                index,
                "response.comment",
                "HAR comment preserved as flow annotation",
                LossAction::Annotated,
            ));
        }
        let response_media = entry
            .response
            .content
            .as_ref()
            .and_then(|content| content.mime_type.as_deref())
            .map(|raw| {
                raw.split(';')
                    .next()
                    .unwrap_or(raw)
                    .trim()
                    .to_ascii_lowercase()
            });
        let mut response_raw: Vec<u8> = Vec::new();
        if let Some(content) = &entry.response.content {
            if content.compression.is_some() {
                losses.push(HarLoss::entry(
                    index,
                    "response.content.compression",
                    "compression savings are not represented; decoded body bytes are authoritative",
                    LossAction::Annotated,
                ));
            }
            if let Some(size) = content.size
                && size < 0
            {
                losses.push(HarLoss::entry(
                    index,
                    "response.content.size",
                    "negative content size treated as unknown; decoded bytes are authoritative",
                    LossAction::Annotated,
                ));
            }
            response_raw = decode_har_body_text(
                content.text.as_deref(),
                content.encoding.as_deref(),
                max_blob_bytes,
            )?;
            if content.text.is_none() && content.size.unwrap_or(0) != 0 {
                losses.push(HarLoss::entry(
                    index,
                    "response.content.text",
                    "missing content text with nonzero size treated as empty body",
                    LossAction::Annotated,
                ));
            }
        }
        let (response_bytes, mut response_markers) = redact_import_body(
            response_raw,
            response_media.as_deref(),
            redaction,
            profile_id,
            true,
            &mut losses,
            index,
            "response",
            max_structured_bytes,
        )?;

        // --- Timing: entry.time is the total duration; the breakdown collapses. ---
        let started_at_ms = started_ms(entry.started_date_time.as_deref())?;
        let duration_ms = total_time_ms(entry.time.as_ref())?;
        let completed_at_ms = started_at_ms.saturating_add(duration_ms);
        if let Some(timings) = &entry.timings {
            let mut applicable = 0u32;
            for (name, value) in [
                ("blocked", timings.blocked.as_ref()),
                ("dns", timings.dns.as_ref()),
                ("connect", timings.connect.as_ref()),
                ("send", timings.send.as_ref()),
                ("wait", timings.wait.as_ref()),
                ("receive", timings.receive.as_ref()),
                ("ssl", timings.ssl.as_ref()),
            ] {
                if timing_number(value).is_some() {
                    applicable += 1;
                }
                let _ = name;
            }
            if applicable > 0 {
                losses.push(HarLoss::entry(
                    index,
                    "timings",
                    "HAR timing breakdown collapsed to total duration; per-phase delays not represented",
                    LossAction::Annotated,
                ));
            }
        }
        if entry.server_ip_address.is_some() || entry.connection.is_some() {
            losses.push(HarLoss::entry(
                index,
                "serverIPAddress/connection",
                "physical transport identity has no semantic representation; omitted",
                LossAction::Omitted,
            ));
        }
        if entry.cache.is_some() {
            losses.push(HarLoss::entry(
                index,
                "cache",
                "HAR cache state has no semantic representation; omitted",
                LossAction::Omitted,
            ));
        }
        if entry.pageref.is_some() {
            losses.push(HarLoss::entry(
                index,
                "pageref",
                "HAR page reference has no semantic representation; omitted",
                LossAction::Omitted,
            ));
        }

        // --- Assemble flow (bodies published later, transactionally) ---
        let mut annotations: Vec<(String, String)> = Vec::new();
        if let Some(comment) = entry.request.comment.as_deref().filter(|c| !c.is_empty()) {
            annotations.push(("har.request.comment".into(), truncate_bounded(comment, 512)));
        }
        if let Some(comment) = entry.response.comment.as_deref().filter(|c| !c.is_empty()) {
            annotations.push((
                "har.response.comment".into(),
                truncate_bounded(comment, 512),
            ));
        }
        annotations.push(("har.entry.time_ms".into(), duration_ms.to_string()));
        if let Some(creator) = document.log.creator.as_ref()
            && !creator.name.is_empty()
        {
            annotations.push((
                "har.creator".into(),
                truncate_bounded(&format!("{}/{}", creator.name, creator.version), 256),
            ));
        }

        let flow_id = format!("har-import-{index:06}-{}", unique_suffix());
        // Response trailers cannot be represented in HAR: any imported flow
        // starts with none (export later reports the reverse).
        let outcome = if let Some(code) = status {
            FlowOutcome::Response(HttpResponse {
                status: code,
                headers: response_headers.clone(),
                body: BodyRef::Empty,
                trailers: Vec::new(),
            })
        } else {
            losses.push(HarLoss::entry(
                index,
                "response.status",
                "HAR status 0 mapped to a typed transport error; no response is recorded",
                LossAction::Annotated,
            ));
            FlowOutcome::Error(FlowError::new(
                ErrorCategory::Other,
                ErrorPhase::Other,
                "HAR-recorded request with no response (status 0)",
            ))
        };
        let mut flow = Flow {
            schema_version: eggreplay_core::FLOW_SCHEMA_VERSION,
            id: flow_id,
            started_at_ms,
            completed_at_ms: Some(completed_at_ms),
            request: HttpRequest {
                method: entry.request.method.clone(),
                scheme,
                authority,
                path,
                query,
                headers: request_headers,
                body: BodyRef::Empty,
                trailers: Vec::new(),
            },
            outcome,
            physical_route: None,
            provenance: Provenance {
                mode: "har-import".into(),
                observer: "eggreplay-har".into(),
            },
            annotations,
            redactions: Vec::new(),
        };
        // Header/query redaction after structural assembly (mirrors gateway).
        eggreplay_core::redact_flow(&mut flow, redaction, profile_id);
        let header_redactions = flow.redactions.len();
        if header_redactions > 0 {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "request/response.headers,query".into(),
                reason: format!(
                    "{header_redactions} header/query field(s) redacted per selected profile"
                ),
                action: LossAction::Redacted,
            });
        }
        flow.redactions.append(&mut request_markers);
        // Response markers attach only for Response outcomes.
        if let FlowOutcome::Response(_) = &flow.outcome {
            flow.redactions.append(&mut response_markers);
        } else if !response_markers.is_empty() {
            // Error outcomes carry no response body; redaction markers for an
            // absent body are meaningless, so record the omission.
            losses.push(HarLoss::entry(
                index,
                "response.body.redaction",
                "response body redaction markers omitted for typed-error outcome",
                LossAction::Omitted,
            ));
        }
        // Reconcile representation headers when a body transform occurred.
        // Request headers always reconcile; response headers only for real
        // responses (error outcomes carry no response).
        if flow
            .redactions
            .iter()
            .any(|marker| marker.field.contains("body.json") || marker.field.contains("body.form"))
        {
            let (mut req_markers, req_notes) = reconcile_headers_after_body_redaction(
                &mut flow.request.headers,
                u64::try_from(request_bytes.len()).unwrap_or(u64::MAX),
                profile_id,
            );
            flow.redactions.append(&mut req_markers);
            flow.annotations.extend(req_notes);
            if let FlowOutcome::Response(response) = &mut flow.outcome {
                let (mut resp_markers, resp_notes) = reconcile_headers_after_body_redaction(
                    &mut response.headers,
                    u64::try_from(response_bytes.len()).unwrap_or(u64::MAX),
                    profile_id,
                );
                flow.redactions.append(&mut resp_markers);
                flow.annotations.extend(resp_notes);
            }
        }
        let _ = response_headers;
        flow.validate()
            .map_err(|error| HarError::UnsupportedEntry {
                index,
                reason: error.to_string(),
            })?;
        pending.push((flow, request_bytes, response_bytes, Vec::new()));
    }

    // Publish bodies + flows transactionally. Any failure aborts before
    // `finish`; the caller owns the writer and must drop it without finishing
    // on error so no partial fixture is published.
    let mut count = 0usize;
    for (mut flow, request_bytes, response_bytes, _) in pending {
        let request_ref = publish_body(writer, &request_bytes)?;
        flow.request.body = request_ref;
        if let FlowOutcome::Response(response) = &mut flow.outcome {
            let response_ref = publish_body(writer, &response_bytes)?;
            response.body = response_ref;
        }
        // Attach flow ids to earlier per-entry losses that lacked them.
        writer.append_flow(&flow).map_err(HarError::Store)?;
        count += 1;
    }

    // Record import provenance as an optional extension (never required, so
    // older readers ignore it without changing replay semantics).
    let creator = document
        .log
        .creator
        .as_ref()
        .map(|creator| format!("{}/{}", creator.name, creator.version));
    let provenance = InteropProvenance {
        schema_version: INTEROP_PROVENANCE_SCHEMA_VERSION,
        tool_version: eggreplay_core::TOOL_VERSION.into(),
        source: "har-import".into(),
        har_creator: creator,
        entries: document.log.entries.len(),
        flows: count,
        losses: losses.iter().take(1024).cloned().collect(),
    };
    let provenance_bytes = serde_json::to_vec(&provenance)
        .map_err(|error| HarError::Invalid(format!("provenance encode failed: {error}")))?;
    writer
        .write_extension(
            "interop-provenance",
            INTEROP_PROVENANCE_SCHEMA_VERSION,
            "interop-provenance.json",
            false,
            &provenance_bytes,
        )
        .map_err(HarError::Store)?;

    Ok(ImportReport {
        flows: count,
        entries: document.log.entries.len(),
        losses,
    })
}

fn truncate_bounded(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_owned();
    }
    let mut out = input[..max].to_owned();
    out.push('…');
    out
}

fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), nanos % 1_000_000_000)
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn escape_json_text(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn har_headers(headers: &[HeaderEntry]) -> Vec<serde_json::Value> {
    headers
        .iter()
        .map(|header| serde_json::json!({"name": header.name, "value": header.value}))
        .collect()
}

fn har_cookies_from_headers(headers: &[HeaderEntry], response: bool) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for header in headers {
        let is_cookie = if response {
            header.name.eq_ignore_ascii_case("set-cookie")
        } else {
            header.name.eq_ignore_ascii_case("cookie")
                || header.name.eq_ignore_ascii_case("cookie2")
        };
        if !is_cookie {
            continue;
        }
        // Derived view only: split on ';' and take the first pair as name=value.
        let first = header.value.split(';').next().unwrap_or("").trim();
        let (name, value) = first.split_once('=').unwrap_or((first, ""));
        if name.is_empty() {
            continue;
        }
        out.push(serde_json::json!({"name": name.trim(), "value": value.trim()}));
        if out.len() >= 256 {
            break;
        }
    }
    out
}

fn har_query(pairs: &[QueryPair]) -> Vec<serde_json::Value> {
    pairs
        .iter()
        .map(|pair| serde_json::json!({"name": pair.key, "value": pair.value}))
        .collect()
}

fn read_body_bytes(session: &Session, body: &BodyRef) -> Result<Vec<u8>, HarError> {
    match body {
        BodyRef::Absent | BodyRef::Empty => Ok(Vec::new()),
        BodyRef::Blob(blob) => session.read_blob(blob).map_err(HarError::Store),
    }
}

fn content_type_of(headers: &[HeaderEntry]) -> Option<String> {
    headers.iter().find_map(|header| {
        if header.name.eq_ignore_ascii_case("content-type") {
            Some(
                header
                    .value
                    .split(';')
                    .next()
                    .unwrap_or(&header.value)
                    .trim()
                    .to_owned(),
            )
        } else {
            None
        }
    })
}

/// Export `session` into a HAR 1.2 document with an explicit `_eggreplay`
/// provenance/loss section.
///
/// Every loss class from the plan is covered: trailers, typed errors,
/// `rules` scenario extensions, `stream-events`, `WebSockets`, redaction
/// markers, physical routes, HTTP-version collapse, and timing synthesis.
/// The returned JSON always carries `log.comment` with [`HAR_LOSSY_NOTICE`].
///
/// # Errors
///
/// Returns [`HarError::Store`] when fixture bodies or extensions cannot be
/// read.
#[allow(clippy::too_many_lines)]
pub fn export_session_to_har(
    session: &Session,
) -> Result<(serde_json::Value, ExportReport), HarError> {
    let mut losses: Vec<HarLoss> = Vec::new();
    let manifest = session.manifest();

    // Session-level losses.
    if manifest
        .extensions
        .iter()
        .any(|extension| extension.name == "rules")
    {
        losses.push(HarLoss::global(
            "extensions.rules",
            "authored scenario rules have no HAR representation; omitted",
            LossAction::Omitted,
        ));
    }
    if manifest
        .extensions
        .iter()
        .any(|extension| extension.name == "stream-events")
    {
        losses.push(HarLoss::global(
            "extensions.stream-events",
            "stream event cadence/trailers/mid-body errors collapsed to total duration; metadata omitted",
            LossAction::Omitted,
        ));
    }
    if manifest
        .extensions
        .iter()
        .any(|extension| extension.name == "websocket-messages")
    {
        losses.push(HarLoss::global(
            "extensions.websocket-messages",
            "WebSocket conversations have no HAR representation; 101 flows exported as handshakes only",
            LossAction::Omitted,
        ));
    }
    if manifest
        .extensions
        .iter()
        .any(|extension| extension.name == "interop-provenance")
    {
        losses.push(HarLoss::global(
            "extensions.interop-provenance",
            "import provenance is EggReplay-internal; summarized in log._eggreplay",
            LossAction::Annotated,
        ));
    }
    for extension in &manifest.extensions {
        if !KNOWN_EXTENSIONS
            .iter()
            .any(|(name, _)| *name == extension.name)
            && !extension.required_for_replay
        {
            losses.push(HarLoss::global(
                format!("extensions.{}", extension.name),
                "unknown optional extension omitted from HAR",
                LossAction::Omitted,
            ));
        }
    }

    let flows: Vec<Flow> = session
        .iter_flows()
        .map_err(HarError::Store)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(HarError::Store)?;

    let mut entries = Vec::with_capacity(flows.len());
    for (index, flow) in flows.iter().enumerate() {
        // Reconstruct URL from semantic parts (query pairs percent-encoded).
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for pair in &flow.request.query {
            serializer.append_pair(&pair.key, &pair.value);
        }
        let query_string = serializer.finish();
        let url = if query_string.is_empty() {
            format!(
                "{}://{}{}",
                flow.request.scheme, flow.request.authority, flow.request.path
            )
        } else {
            format!(
                "{}://{}{}?{query_string}",
                flow.request.scheme, flow.request.authority, flow.request.path
            )
        };

        // Request body: UTF-8 as text, binary as base64.
        let request_bytes = read_body_bytes(session, &flow.request.body)?;
        let request_media = content_type_of(&flow.request.headers);
        let (request_text, request_encoding) = if request_bytes.is_empty() {
            (None, None)
        } else if let Some(text) = escape_json_text(&request_bytes) {
            (Some(text), None)
        } else {
            (
                Some(base64::engine::general_purpose::STANDARD.encode(&request_bytes)),
                Some("base64".to_owned()),
            )
        };
        if request_encoding.is_some() {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "request.postData.encoding".into(),
                reason: "binary request body base64-encoded for HAR".into(),
                action: LossAction::Annotated,
            });
        }
        let request_post_data = request_text.map(|text| {
            serde_json::json!({
                "mimeType": request_media.clone().unwrap_or_else(|| "application/octet-stream".into()),
                "text": text,
                "encoding": request_encoding.clone().unwrap_or_default(),
            })
        });

        if !flow.request.trailers.is_empty() {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "request.trailers".into(),
                reason: "request trailers have no HAR representation; omitted".into(),
                action: LossAction::Omitted,
            });
        }

        let started_millis = i64::try_from(flow.started_at_ms).unwrap_or(0);
        let started =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(started_millis).map_or_else(
                || "1970-01-01T00:00:00.000Z".into(),
                |time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
        let duration_ms = flow
            .completed_at_ms
            .map_or(0, |end| end.saturating_sub(flow.started_at_ms));

        let request_entry = serde_json::json!({
            "method": flow.request.method,
            "url": url,
            "httpVersion": "http/1.1",
            "cookies": har_cookies_from_headers(&flow.request.headers, false),
            "headers": har_headers(&flow.request.headers),
            "queryString": har_query(&flow.request.query),
            "headersSize": -1,
            "bodySize": request_bytes.len() as u64,
            "comment": "EggReplay semantic request; HTTP version assumed http/1.1 (lossy)",
        });
        let mut request_entry = request_entry;
        if let Some(post_data) = request_post_data {
            request_entry["postData"] = post_data;
        }

        let response_entry = match &flow.outcome {
            FlowOutcome::Response(response) => {
                let response_bytes = read_body_bytes(session, &response.body)?;
                let response_media = content_type_of(&response.headers);
                let (text, encoding, is_binary) = if response_bytes.is_empty() {
                    (None, None, false)
                } else if let Some(text) = escape_json_text(&response_bytes) {
                    (Some(text), None, false)
                } else {
                    (
                        Some(base64::engine::general_purpose::STANDARD.encode(&response_bytes)),
                        Some("base64".to_owned()),
                        true,
                    )
                };
                if is_binary {
                    losses.push(HarLoss {
                        entry_index: Some(index),
                        flow_id: Some(flow.id.clone()),
                        reason: "binary response body base64-encoded for HAR".into(),
                        field: "response.content.encoding".into(),
                        action: LossAction::Annotated,
                    });
                }
                if !response.trailers.is_empty() {
                    losses.push(HarLoss {
                        entry_index: Some(index),
                        flow_id: Some(flow.id.clone()),
                        field: "response.trailers".into(),
                        reason: "response trailers have no HAR representation; omitted".into(),
                        action: LossAction::Omitted,
                    });
                }
                let mut content = serde_json::json!({
                    "size": response_bytes.len() as u64,
                    "mimeType": response_media.unwrap_or_else(|| "application/octet-stream".into()),
                });
                if let Some(text) = text {
                    content["text"] = serde_json::Value::String(text);
                }
                if let Some(encoding) = encoding {
                    content["encoding"] = serde_json::Value::String(encoding);
                }
                serde_json::json!({
                    "status": response.status,
                    "statusText": "",
                    "httpVersion": "http/1.1",
                    "cookies": har_cookies_from_headers(&response.headers, true),
                    "headers": har_headers(&response.headers),
                    "content": content,
                    "redirectURL": "",
                    "headersSize": -1,
                    "bodySize": response_bytes.len() as u64,
                    "comment": "EggReplay semantic response; HTTP version assumed http/1.1 (lossy)",
                })
            }
            FlowOutcome::Error(error) => {
                losses.push(HarLoss {
                    entry_index: Some(index),
                    flow_id: Some(flow.id.clone()),
                    field: "response".into(),
                    reason: format!(
                        "typed transport error {:?}/{:?} has no HAR response; exported as status 0",
                        error.category, error.phase
                    ),
                    action: LossAction::Annotated,
                });
                serde_json::json!({
                    "status": 0,
                    "statusText": format!("{:?}/{:?}", error.category, error.phase),
                    "httpVersion": "http/1.1",
                    "cookies": [],
                    "headers": [],
                    "content": {"size": 0, "mimeType": "application/octet-stream"},
                    "redirectURL": "",
                    "headersSize": -1,
                    "bodySize": 0,
                    "comment": "EggReplay typed error; see entry._eggreplay (lossy)",
                })
            }
        };

        if !flow.redactions.is_empty() {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "redactions".into(),
                reason: format!(
                    "{} redacted field(s) remain <redacted>; marker provenance in _eggreplay only",
                    flow.redactions.len()
                ),
                action: LossAction::Annotated,
            });
        }
        if flow.physical_route.is_some() {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "physical_route".into(),
                reason: "physical route has no HAR representation; omitted".into(),
                action: LossAction::Omitted,
            });
        }
        if flow.request.headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("upgrade")
                && header.value.to_ascii_lowercase().contains("websocket")
        }) || matches!(&flow.outcome, FlowOutcome::Response(response) if response.status == 101)
        {
            losses.push(HarLoss {
                entry_index: Some(index),
                flow_id: Some(flow.id.clone()),
                field: "websocket".into(),
                reason: "WebSocket handshake exported without messages; conversation omitted"
                    .into(),
                action: LossAction::Omitted,
            });
        }

        let per_entry_eggreplay = serde_json::json!({
            "flow_id": flow.id,
            "redactions": flow.redactions,
            "annotations": flow.annotations,
            "provenance": flow.provenance,
        });
        entries.push(serde_json::json!({
            "startedDateTime": started,
            "time": duration_ms,
            "request": request_entry,
            "response": response_entry,
            "cache": {},
            "timings": {"send": 0, "wait": duration_ms, "receive": 0},
            "comment": "Lossy EggReplay projection; timings are synthetic totals",
            "_eggreplay": per_entry_eggreplay,
        }));
    }

    // Global synthetic-timing + version-collapse losses (once, not per entry).
    if !flows.is_empty() {
        losses.push(HarLoss::global(
            "timings",
            "HAR timings are synthetic totals (send=0, wait=duration, receive=0); recorded cadence not preserved",
            LossAction::Annotated,
        ));
        losses.push(HarLoss::global(
            "httpVersion",
            "EggReplay flows are version-agnostic; HAR httpVersion is assumed http/1.1",
            LossAction::Annotated,
        ));
    }

    let count = flows.len();
    let report = ExportReport {
        entries: count,
        losses: losses.clone(),
    };
    let har = serde_json::json!({
        "log": {
            "version": HAR_VERSION_1_2,
            "creator": {"name": "eggreplay", "version": eggreplay_core::TOOL_VERSION},
            "entries": entries,
            "comment": HAR_LOSSY_NOTICE,
            "_eggreplay": {
                "tool_version": eggreplay_core::TOOL_VERSION,
                "session_schema": manifest.metadata.schema_version,
                "session_id": manifest.metadata.session_id,
                "export_note": HAR_LOSSY_NOTICE,
                "losses": losses,
            }
        }
    });
    Ok((har, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_store::StoreLimits;
    use std::collections::BTreeSet;

    fn secure_redaction() -> (RedactionConfig, String) {
        (RedactionConfig::default_secure(), "default-v1".into())
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-har-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    const MINIMAL_HAR: &str = r#"{
      "log": {
        "version": "1.2",
        "creator": {"name": "test", "version": "0"},
        "entries": [
          {
            "startedDateTime": "2026-01-01T00:00:00.000Z",
            "time": 12,
            "request": {
              "method": "GET",
              "url": "http://example.test/items?b=2&a=1",
              "httpVersion": "http/1.1",
              "cookies": [],
              "headers": [{"name": "Accept", "value": "text/plain"}],
              "queryString": [{"name": "b", "value": "2"}, {"name": "a", "value": "1"}]
            },
            "response": {
              "status": 200,
              "statusText": "OK",
              "httpVersion": "http/1.1",
              "cookies": [],
              "headers": [{"name": "Content-Type", "value": "text/plain"}],
              "content": {"size": 5, "mimeType": "text/plain", "text": "hello"}
            },
            "cache": {},
            "timings": {"send": 1, "wait": 10, "receive": 1}
          }
        ]
      }
    }"#;

    #[test]
    fn import_preserves_method_url_query_headers_status_body_timing() {
        let dir = temp_dir("import-minimal");
        let fixture = dir.join("imported.eggr");
        let (redaction, profile) = secure_redaction();
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer,
            MINIMAL_HAR.as_bytes(),
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 1);
        let session = writer.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(flows.len(), 1);
        let flow = &flows[0];
        assert_eq!(flow.request.method, "GET");
        assert_eq!(flow.request.scheme, "http");
        assert_eq!(flow.request.authority, "example.test");
        assert_eq!(flow.request.path, "/items");
        assert_eq!(
            flow.request.query,
            vec![
                QueryPair {
                    key: "b".into(),
                    value: "2".into()
                },
                QueryPair {
                    key: "a".into(),
                    value: "1".into()
                },
            ]
        );
        assert_eq!(
            flow.request.headers,
            vec![HeaderEntry {
                name: "Accept".into(),
                value: "text/plain".into()
            }]
        );
        match &flow.outcome {
            FlowOutcome::Response(response) => {
                assert_eq!(response.status, 200);
                let body = session
                    .read_blob(match &response.body {
                        BodyRef::Blob(blob) => blob,
                        other => panic!("expected blob, got {other:?}"),
                    })
                    .unwrap();
                assert_eq!(body, b"hello");
            }
            FlowOutcome::Error(_) => panic!("expected response"),
        }
        assert_eq!(flow.started_at_ms, 1_767_225_600_000);
        assert_eq!(flow.completed_at_ms, Some(1_767_225_600_012));
        assert_eq!(flow.provenance.mode, "har-import");
        // Provenance extension is optional and present.
        assert!(
            session
                .read_extension("interop-provenance")
                .unwrap()
                .is_some()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_preserves_duplicate_headers_and_reports_cookie_views() {
        let har = r#"{
          "log": {"version": "1.2", "creator": {"name": "t", "version": "0"}, "entries": [{
            "startedDateTime": "2026-01-01T00:00:00.000Z", "time": 1,
            "request": {
              "method": "GET", "url": "http://example.test/",
              "headers": [
                {"name": "X-Dup", "value": "one"},
                {"name": "X-Dup", "value": "two"},
                {"name": "Cookie", "value": "a=1; b=2"}
              ],
              "cookies": [
                {"name": "a", "value": "1"},
                {"name": "ghost", "value": "9"}
              ],
              "queryString": []
            },
            "response": {"status": 200, "headers": [], "content": {"size": 0}},
            "cache": {}, "timings": {"send": 0, "wait": 1, "receive": 0}
          }]}
        }"#;
        let dir = temp_dir("import-dup");
        let fixture = dir.join("dup.eggr");
        let (redaction, profile) = secure_redaction();
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer,
            har.as_bytes(),
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 1);
        // Duplicate headers preserved (not collapsed), cookie divergence annotated.
        assert!(
            report
                .losses
                .iter()
                .any(|loss| loss.field == "request.cookies"),
            "cookie view divergence must be reported: {:?}",
            report.losses
        );
        let session = writer.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let dupes: Vec<_> = flows[0]
            .request
            .headers
            .iter()
            .filter(|header| header.name == "X-Dup")
            .collect();
        assert_eq!(dupes.len(), 2);
        assert_eq!(dupes[0].value, "one");
        assert_eq!(dupes[1].value, "two");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_applies_redaction_before_publication() {
        let har = r#"{
          "log": {"version": "1.2", "creator": {"name": "t", "version": "0"}, "entries": [{
            "startedDateTime": "2026-01-01T00:00:00.000Z", "time": 1,
            "request": {
              "method": "GET", "url": "http://example.test/?token=SECRET",
              "headers": [{"name": "Authorization", "value": "Bearer SECRET"}],
              "queryString": [{"name": "token", "value": "SECRET"}]
            },
            "response": {"status": 200, "headers": [], "content": {"size": 0}},
            "cache": {}, "timings": {"send": 0, "wait": 1, "receive": 0}
          }]}
        }"#;
        let dir = temp_dir("import-redact");
        let fixture = dir.join("redacted.eggr");
        let mut config = RedactionConfig::default_secure();
        config.query_keys.insert("token".into());
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        import_har_to_writer(
            &mut writer,
            har.as_bytes(),
            &config,
            "default-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        let session = writer.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !flows[0]
                .request
                .headers
                .iter()
                .any(|header| header.value.contains("SECRET"))
        );
        assert!(
            !flows[0]
                .request
                .query
                .iter()
                .any(|pair| pair.value.contains("SECRET"))
        );
        assert!(!flows[0].redactions.is_empty());
        // Stored blobs + JSONL must not contain the secret either.
        let tree = std::fs::read_dir(&fixture).unwrap();
        for entry in tree.flatten() {
            let path = entry.path();
            if path.is_file() {
                let bytes = std::fs::read(&path).unwrap_or_default();
                assert!(
                    !bytes.windows(6).any(|w| w == b"SECRET"),
                    "secret leaked in {}",
                    path.display()
                );
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_rejects_unsupported_version_and_maps_status_zero_to_error() {
        let dir = temp_dir("import-unsupported");
        let (redaction, profile) = secure_redaction();
        let bad_version = r#"{"log": {"version": "9.9", "entries": []}}"#;
        let fixture = dir.join("bad.eggr");
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        assert!(
            import_har_to_writer(
                &mut writer,
                bad_version.as_bytes(),
                &redaction,
                &profile,
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                StoreLimits::default().max_blob_bytes,
            )
            .is_err()
        );
        // Drop without finish so no partial fixture is published.
        drop(writer);
        assert!(!fixture.exists() || Session::open(&fixture, StoreLimits::default()).is_err());

        let failed = r#"{
          "log": {"version": "1.2", "entries": [{
            "startedDateTime": "2026-01-01T00:00:00.000Z", "time": 1,
            "request": {"method": "GET", "url": "http://example.test/", "headers": [], "queryString": []},
            "response": {"status": 0, "headers": [], "content": {"size": 0}},
            "cache": {}, "timings": {}
          }]}
        }"#;
        let fixture2 = dir.join("failed.eggr");
        let mut writer2 = SessionWriter::create(
            &fixture2,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer2,
            failed.as_bytes(),
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 1);
        let session = writer2.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(matches!(flows[0].outcome, FlowOutcome::Error(_)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn export_reports_trailers_errors_streams_websockets_redactions_routes() {
        let dir = temp_dir("export-loss");
        let fixture = dir.join("loss.eggr");
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        // Flow with trailers + redaction marker + physical route.
        let flow = Flow {
            schema_version: eggreplay_core::FLOW_SCHEMA_VERSION,
            id: "loss-flow".into(),
            started_at_ms: 10,
            completed_at_ms: Some(12),
            request: HttpRequest {
                method: "GET".into(),
                scheme: "http".into(),
                authority: "example.test".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: BodyRef::Empty,
                trailers: vec![HeaderEntry {
                    name: "x-trailer".into(),
                    value: "1".into(),
                }],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![],
                body: BodyRef::Empty,
                trailers: vec![HeaderEntry {
                    name: "x-res-trailer".into(),
                    value: "2".into(),
                }],
            }),
            physical_route: Some(eggreplay_core::PhysicalRoute {
                kind: "eggress".into(),
                description: Some("eggress".into()),
            }),
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![RedactionMarker {
                field: "request.headers.authorization".into(),
                profile: "default-v1".into(),
            }],
        };
        writer.append_flow(&flow).unwrap();
        // Error flow.
        let mut error_flow = flow.clone();
        error_flow.id = "error-flow".into();
        error_flow.request.trailers = vec![];
        error_flow.outcome = FlowOutcome::Error(FlowError::new(
            ErrorCategory::Timeout,
            ErrorPhase::Connect,
            "timed out",
        ));
        error_flow.physical_route = None;
        error_flow.redactions = vec![];
        if let FlowOutcome::Response(response) = &mut error_flow.outcome {
            let _ = response;
        }
        // Clear response trailers for the error flow (no response).
        writer.append_flow(&error_flow).unwrap();
        // Stream-events extension to trigger the global loss.
        writer
            .append_stream_events(eggreplay_core::FlowStreamEvents {
                flow_id: "loss-flow".into(),
                start_offset_ns: 0,
                request: vec![],
                response: vec![eggreplay_core::StreamEvent {
                    delta_ns: 0,
                    event: eggreplay_core::StreamEventKind::End,
                }],
            })
            .unwrap();
        let session = writer.finish().unwrap();
        let (har, report) = export_session_to_har(&session).unwrap();
        assert_eq!(report.entries, 2);
        let fields: BTreeSet<String> = report
            .losses
            .iter()
            .map(|loss| loss.field.clone())
            .collect();
        for expected in [
            "request.trailers",
            "response.trailers",
            "response",
            "extensions.stream-events",
            "redactions",
            "physical_route",
            "timings",
            "httpVersion",
        ] {
            assert!(
                fields.contains(expected),
                "missing loss {expected} in {fields:?}"
            );
        }
        assert_eq!(
            har["log"]["comment"],
            serde_json::Value::String(HAR_LOSSY_NOTICE.into())
        );
        assert!(har["log"]["_eggreplay"]["losses"].as_array().unwrap().len() >= 8);
        // Status-0 projection for the typed error.
        assert_eq!(har["log"]["entries"][1]["response"]["status"], 0);
        let _ = std::io::sink();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn migration_blocks_unknown_required_and_is_idempotent() {
        let dir = temp_dir("migrate-unit");
        let source_path = dir.join("source.eggr");
        let writer = SessionWriter::create(
            &source_path,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let source = writer.finish().unwrap();
        assert_eq!(source.manifest().metadata.schema_version, 1);
        // Schema-1 -> current upgrades the session schema.
        let upgraded_path = dir.join("upgraded.eggr");
        let upgraded = migrate_session(&source, &upgraded_path, CURRENT_SESSION_SCHEMA).unwrap();
        assert_eq!(
            upgraded.manifest().metadata.schema_version,
            CURRENT_SESSION_SCHEMA
        );
        // Current -> current is idempotent.
        let dest = dir.join("copy.eggr");
        let copied = migrate_session(&upgraded, &dest, CURRENT_SESSION_SCHEMA).unwrap();
        assert_eq!(copied.manifest().metadata, upgraded.manifest().metadata);
        assert_eq!(copied.manifest().extensions, upgraded.manifest().extensions);

        // Unknown required extension blocks migration.
        let blocked_path = dir.join("blocked.eggr");
        let mut blocked_writer = SessionWriter::create(
            &blocked_path,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        blocked_writer
            .write_extension("future-required", 1, "future.json", true, b"{}")
            .unwrap();
        // Session::open would already reject this fixture, so construct the
        // blocker check directly from the descriptor.
        let descriptors = vec![eggreplay_store::ExtensionDescriptor {
            name: "future-required".into(),
            schema_version: 1,
            path: "future.json".into(),
            required_for_replay: true,
        }];
        let blockers = migration_blockers(&descriptors);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("future-required"));
        // Cleanup staging dirs left by unfinished writers is best-effort; the
        // finished sessions remove explicitly.
        std::fs::remove_dir_all(dir).ok();
        let _ = blocked_writer;
    }

    fn corpus_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/corpus")
            .join(name)
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn golden_har_corpus_imports_with_expected_losses() {
        let (redaction, profile) = secure_redaction();
        // Minimal: 2 flows, no errors.
        let minimal = std::fs::read(corpus_path("minimal.har")).unwrap();
        let dir = temp_dir("corpus-minimal");
        let fixture = dir.join("minimal.eggr");
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer,
            &minimal,
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 2);
        assert_eq!(report.entries, 2);
        let session = writer.finish().unwrap();
        let (har, export_report) = export_session_to_har(&session).unwrap();
        assert_eq!(export_report.entries, 2);
        assert_eq!(har["log"]["entries"].as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(dir).ok();

        // Duplicates: 1 flow, cookie divergence reported, duplicates preserved.
        let duplicates = std::fs::read(corpus_path("duplicates.har")).unwrap();
        let dir = temp_dir("corpus-duplicates");
        let fixture = dir.join("dup.eggr");
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer,
            &duplicates,
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 1);
        assert!(
            report
                .losses
                .iter()
                .any(|loss| loss.field == "request.cookies")
        );
        let session = writer.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            flows[0]
                .request
                .headers
                .iter()
                .filter(|header| header.name == "X-Dup")
                .count(),
            2
        );
        std::fs::remove_dir_all(dir).ok();

        // Binary + error: 2 entries, one blob body (base64), one typed error.
        let binary = std::fs::read(corpus_path("binary-and-error.har")).unwrap();
        let dir = temp_dir("corpus-binary");
        let fixture = dir.join("binary.eggr");
        let mut writer = SessionWriter::create(
            &fixture,
            eggreplay_core::SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let report = import_har_to_writer(
            &mut writer,
            &binary,
            &redaction,
            &profile,
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            StoreLimits::default().max_blob_bytes,
        )
        .unwrap();
        assert_eq!(report.flows, 2);
        assert!(
            report
                .losses
                .iter()
                .any(|loss| loss.field == "request.url.userinfo")
        );
        assert!(
            report
                .losses
                .iter()
                .any(|loss| loss.field == "request.httpVersion")
        );
        let session = writer.finish().unwrap();
        let flows: Vec<Flow> = session
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(matches!(flows[1].outcome, FlowOutcome::Error(_)));
        // Binary request body round-trips through base64.
        let request_body = session
            .read_blob(match &flows[0].request.body {
                BodyRef::Blob(blob) => blob,
                other => panic!("expected blob, got {other:?}"),
            })
            .unwrap();
        assert_eq!(request_body, vec![0, 1, 2, 3, 4]);
        std::fs::remove_dir_all(dir).ok();
    }
}
