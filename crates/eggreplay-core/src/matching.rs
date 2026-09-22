//! Deterministic request normalization, matching, consumption, and diagnostics.

use crate::{Flow, HeaderEntry, HttpRequest, MatcherProfile, QueryPair, RedactionMarker};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Body comparison policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyMatchMode {
    /// Compare exact bytes.
    ExactBytes,
    /// Compare exact UTF-8 text.
    ExactText,
    /// Compare parsed JSON after removing configured JSON pointers.
    SemanticJson,
}

/// Repeated-flow consumption behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumptionMode {
    /// A fixture flow can be consumed once.
    Once,
    /// After the first match, reuse the final matching flow.
    RepeatLast,
    /// Matching never consumes a flow.
    Unlimited,
}

/// A normalized request projection used by diagnostics and matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRequest {
    /// Lowercase scheme.
    pub scheme: String,
    /// Lowercase authority without a trailing default port.
    pub authority: String,
    /// Canonical path.
    pub path: String,
    /// Canonical ordered query pairs.
    pub query: Vec<QueryPair>,
    /// Header values grouped by lowercase name.
    pub headers: BTreeMap<String, Vec<String>>,
}

/// Semantic request-body descriptor for a match candidate.
///
/// `eggreplay-core` stays filesystem-free: digest metadata comes from the
/// flow's [`crate::BodyRef`] without opening any blob. `Inline` exists for
/// tests and for the narrowed body a semantic mode explicitly requests via a
/// loader; fixture-wide loads must use [`CandidateBody::from_body_ref`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateBody {
    /// No body was present or permitted.
    Absent,
    /// A zero-byte body was present.
    Empty,
    /// Content-addressed stored bytes described without materialization.
    Digest {
        /// Lowercase hex SHA-256.
        sha256: String,
        /// Exact byte length.
        length: u64,
    },
    /// Explicitly materialized bytes for a narrowed candidate.
    Inline(Vec<u8>),
}

impl CandidateBody {
    /// Build a metadata-bounded descriptor from a flow body reference.
    ///
    /// Never reads blob bytes. `Absent`/`Empty` stay zero-allocation;
    /// `Blob` becomes `Digest` metadata.
    pub fn from_body_ref(body: &crate::BodyRef) -> Self {
        match body {
            crate::BodyRef::Absent => Self::Absent,
            crate::BodyRef::Empty => Self::Empty,
            crate::BodyRef::Blob(blob) => Self::Digest {
                sha256: blob.sha256.clone(),
                length: blob.length,
            },
        }
    }

    /// Return the declared length when known without I/O.
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> Option<u64> {
        match self {
            Self::Absent => None,
            Self::Empty => Some(0),
            Self::Digest { length, .. } => Some(*length),
            Self::Inline(bytes) => Some(bytes.len() as u64),
        }
    }
}

/// A candidate flow plus its request-body descriptor.
#[derive(Debug, Clone)]
pub struct MatchCandidate {
    /// Original flow record.
    pub flow: Flow,
    /// Request body descriptor; fixture loads must not materialize blobs.
    pub body: CandidateBody,
    /// Legacy materialized request bytes, kept for API compatibility.
    ///
    /// Prefer [`MatchCandidate::body`]. This field mirrors `Inline` bytes
    /// when constructed via [`MatchCandidate::new`]; it is empty for lazy
    /// descriptors.
    pub request_body: Vec<u8>,
}

impl MatchCandidate {
    /// Construct a candidate with explicitly materialized bytes.
    ///
    /// Maps to [`CandidateBody::Inline`]; tests and narrowed semantic-mode
    /// loaders use this. Fixture-wide replay loads must use
    /// [`MatchCandidate::from_flow`] instead.
    pub fn new(flow: Flow, request_body: Vec<u8>) -> Self {
        let body = CandidateBody::Inline(request_body.clone());
        Self {
            flow,
            body,
            request_body,
        }
    }

    /// Construct a lazy candidate without reading any blob bytes.
    ///
    /// The descriptor is derived from `flow.request.body` metadata only.
    pub fn from_flow(flow: Flow) -> Self {
        let body = CandidateBody::from_body_ref(&flow.request.body);
        Self {
            flow,
            body,
            request_body: Vec::new(),
        }
    }

    /// Return the descriptor used for matching.
    pub fn candidate_body(&self) -> &CandidateBody {
        &self.body
    }
}

/// A dimension contributing to a near miss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchDimension {
    /// Method differs.
    Method,
    /// Scheme or authority differs.
    Authority,
    /// Path differs.
    Path,
    /// Query values differ.
    Query,
    /// Selected headers differ.
    Headers,
    /// Body differs.
    Body,
}

/// A bounded mismatch explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NearMiss {
    /// Candidate index in deterministic fixture order.
    pub candidate_index: usize,
    /// Number of differing dimensions.
    pub cost: u8,
    /// Explicit mismatch dimensions.
    pub dimensions: Vec<MatchDimension>,
    /// Redaction-safe summary only.
    pub summary: String,
}

/// The result of one selection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    /// A candidate index matched.
    Matched(usize),
    /// No candidate matched, with bounded diagnostics.
    NoMatch {
        /// Bounded closest candidates.
        near_misses: Vec<NearMiss>,
    },
    /// Matching candidates existed but all were consumed.
    Exhausted {
        /// Bounded closest candidates, including consumed exact candidates.
        near_misses: Vec<NearMiss>,
    },
}

/// A matcher with explicit profile and body policy.
#[derive(Debug, Clone)]
pub struct Matcher {
    profile: MatcherProfile,
    body_mode: BodyMatchMode,
    ignored_headers: BTreeSet<String>,
    ignored_json_paths: BTreeSet<String>,
    max_near_misses: usize,
}

impl Matcher {
    /// Construct the strict profile.
    pub fn strict(max_near_misses: usize) -> Self {
        Self::new(
            MatcherProfile::Strict,
            BodyMatchMode::ExactBytes,
            max_near_misses,
        )
    }

    /// Construct the practical profile with explicit volatile-header policy.
    pub fn practical(max_near_misses: usize) -> Self {
        let mut matcher = Self::new(
            MatcherProfile::Practical,
            BodyMatchMode::ExactBytes,
            max_near_misses,
        );
        matcher.ignored_headers.extend(
            ["date", "user-agent", "x-request-id"]
                .into_iter()
                .map(str::to_owned),
        );
        matcher
    }

    /// Construct a matcher with explicit body and diagnostic policies.
    pub fn new(profile: MatcherProfile, body_mode: BodyMatchMode, max_near_misses: usize) -> Self {
        Self {
            profile,
            body_mode,
            ignored_headers: BTreeSet::new(),
            ignored_json_paths: BTreeSet::new(),
            max_near_misses,
        }
    }

    /// Add a case-insensitive header to the ignore set.
    pub fn ignore_header(&mut self, name: impl Into<String>) {
        self.ignored_headers
            .insert(name.into().to_ascii_lowercase());
    }

    /// Add a JSON pointer to remove before semantic comparison.
    pub fn ignore_json_path(&mut self, path: impl Into<String>) {
        self.ignored_json_paths.insert(path.into());
    }

    /// Return the configured profile.
    pub const fn profile(&self) -> MatcherProfile {
        self.profile
    }

    /// Normalize a request without changing the recorded value.
    pub fn normalize(&self, request: &HttpRequest) -> NormalizedRequest {
        let authority = request.authority.to_ascii_lowercase();
        NormalizedRequest {
            scheme: request.scheme.to_ascii_lowercase(),
            authority,
            path: if request.path.is_empty() {
                "/".into()
            } else {
                request.path.clone()
            },
            query: request.query.clone(),
            headers: normalize_headers(&request.headers, &self.ignored_headers),
        }
    }

    /// Select one candidate in stable input order.
    ///
    /// Lazy descriptors (`Digest`) are compared via length + SHA-256 for
    /// `ExactBytes`/`ExactText` without materialization. `SemanticJson`
    /// candidates backed by `Digest` require a loader and are treated as a
    /// body mismatch when no loader is supplied; use
    /// [`Self::select_with_loader`] for the narrowing-load path.
    pub fn select(
        &self,
        actual: &HttpRequest,
        actual_body: &[u8],
        candidates: &[MatchCandidate],
        mode: ConsumptionMode,
        session: &mut MatcherSession,
    ) -> MatchResult {
        let mut no_loader = |_: usize, _: &MatchCandidate| -> Option<Vec<u8>> { None };
        self.select_with_loader(
            actual,
            actual_body,
            candidates,
            mode,
            session,
            &mut no_loader,
        )
    }

    /// Select with an explicit lazy body loader for higher-cost modes.
    ///
    /// The loader is invoked at most once per otherwise-matching candidate
    /// that needs materialization (currently only `SemanticJson` with a
    /// `Digest` descriptor). Candidates that already differ on method,
    /// authority, path, query, or headers never trigger a load, so
    /// fixture-wide request bodies stay metadata-bounded. The HTTP/store
    /// adapter owns the loader and its bound; core stays filesystem-free.
    pub fn select_with_loader(
        &self,
        actual: &HttpRequest,
        actual_body: &[u8],
        candidates: &[MatchCandidate],
        mode: ConsumptionMode,
        session: &mut MatcherSession,
        loader: &mut dyn FnMut(usize, &MatchCandidate) -> Option<Vec<u8>>,
    ) -> MatchResult {
        use sha2::{Digest as ShaDigest, Sha256};
        let actual_normalized = self.normalize(actual);
        let mut hasher = Sha256::new();
        hasher.update(actual_body);
        let actual_digest = format!("{:x}", hasher.finalize());
        let actual_len = actual_body.len() as u64;
        let mut near = Vec::new();
        let mut matching_consumed = false;
        for (index, candidate) in candidates.iter().enumerate() {
            let expected_normalized = self.normalize(&candidate.flow.request);
            let mut dimensions = Vec::new();
            if actual_normalized.scheme != expected_normalized.scheme
                || actual_normalized.authority != expected_normalized.authority
            {
                dimensions.push(MatchDimension::Authority);
            }
            if actual_normalized.path != expected_normalized.path {
                dimensions.push(MatchDimension::Path);
            }
            if actual_normalized.query != expected_normalized.query {
                dimensions.push(MatchDimension::Query);
            }
            if actual_normalized.headers != expected_normalized.headers {
                dimensions.push(MatchDimension::Headers);
            }
            if actual.method != candidate.flow.request.method {
                dimensions.push(MatchDimension::Method);
            }
            let narrowed = dimensions.is_empty();
            let body_match = self.body_matches_lazy(
                actual_body,
                &actual_digest,
                actual_len,
                &candidate.body,
                &candidate.request_body,
                narrowed,
                index,
                candidate,
                loader,
            );
            if narrowed && body_match {
                if session.is_available(index, mode) {
                    session.consume(index, mode);
                    return MatchResult::Matched(index);
                }
                matching_consumed = true;
            } else {
                let mut all = dimensions;
                if !body_match {
                    all.push(MatchDimension::Body);
                }
                if !all.is_empty() && near.len() < self.max_near_misses {
                    near.push(NearMiss {
                        candidate_index: index,
                        cost: all.len().min(u8::MAX as usize) as u8,
                        dimensions: all,
                        summary: "candidate differs in explicit request dimensions".into(),
                    });
                }
            }
        }
        near.sort_by_key(|item| (item.cost, item.candidate_index));
        if matching_consumed {
            MatchResult::Exhausted { near_misses: near }
        } else {
            MatchResult::NoMatch { near_misses: near }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn body_matches_lazy(
        &self,
        actual_body: &[u8],
        actual_digest: &str,
        actual_len: u64,
        descriptor: &CandidateBody,
        legacy_inline: &[u8],
        narrowed: bool,
        index: usize,
        candidate: &MatchCandidate,
        loader: &mut dyn FnMut(usize, &MatchCandidate) -> Option<Vec<u8>>,
    ) -> bool {
        match descriptor {
            CandidateBody::Absent | CandidateBody::Empty => actual_body.is_empty(),
            CandidateBody::Digest { sha256, length } => match self.body_mode {
                BodyMatchMode::ExactBytes => actual_len == *length && actual_digest == sha256,
                BodyMatchMode::ExactText => {
                    actual_len == *length
                        && actual_digest == sha256
                        && std::str::from_utf8(actual_body).is_ok()
                }
                BodyMatchMode::SemanticJson => {
                    if !narrowed {
                        return false;
                    }
                    match loader(index, candidate) {
                        Some(expected) => body_match(
                            self.body_mode,
                            actual_body,
                            &expected,
                            &self.ignored_json_paths,
                        ),
                        None => false,
                    }
                }
            },
            CandidateBody::Inline(expected) => {
                let bytes = if expected.is_empty() && !legacy_inline.is_empty() {
                    legacy_inline
                } else if !expected.is_empty() {
                    expected
                } else {
                    legacy_inline
                };
                // `new(flow, bytes)` stores the same bytes in both places;
                // `from_flow` leaves both empty for Absent/Empty which is
                // handled above via the descriptor, so Inline always uses the
                // non-empty side when they disagree.
                body_match(self.body_mode, actual_body, bytes, &self.ignored_json_paths)
            }
        }
    }
}

/// Per-replay-session consumption state; it is never persisted into fixtures.
#[derive(Debug, Clone, Default)]
pub struct MatcherSession {
    consumed: BTreeSet<usize>,
    last: Option<usize>,
}

impl MatcherSession {
    /// Create empty session state.
    pub fn new() -> Self {
        Self::default()
    }
    fn is_available(&self, index: usize, mode: ConsumptionMode) -> bool {
        match mode {
            ConsumptionMode::Once => !self.consumed.contains(&index),
            ConsumptionMode::RepeatLast => match self.last {
                Some(last) => last == index,
                None => !self.consumed.contains(&index),
            },
            ConsumptionMode::Unlimited => true,
        }
    }
    fn consume(&mut self, index: usize, mode: ConsumptionMode) {
        if !matches!(mode, ConsumptionMode::Unlimited) {
            self.consumed.insert(index);
        }
        if matches!(mode, ConsumptionMode::RepeatLast) {
            self.last = Some(index);
        }
    }
}

fn normalize_headers(
    headers: &[HeaderEntry],
    ignored: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut normalized = BTreeMap::new();
    for header in headers {
        let name = header.name.to_ascii_lowercase();
        if !ignored.contains(&name) {
            normalized
                .entry(name)
                .or_insert_with(Vec::new)
                .push(header.value.clone());
        }
    }
    normalized
}

fn body_match(
    mode: BodyMatchMode,
    actual: &[u8],
    expected: &[u8],
    ignored_paths: &BTreeSet<String>,
) -> bool {
    match mode {
        BodyMatchMode::ExactBytes => actual == expected,
        BodyMatchMode::ExactText => actual == expected && std::str::from_utf8(actual).is_ok(),
        BodyMatchMode::SemanticJson => {
            let (Ok(mut actual), Ok(mut expected)) = (
                serde_json::from_slice::<Value>(actual),
                serde_json::from_slice::<Value>(expected),
            ) else {
                return false;
            };
            for path in ignored_paths {
                remove_json_pointer(&mut actual, path);
                remove_json_pointer(&mut expected, path);
            }
            actual == expected
        }
    }
}

fn remove_json_pointer(value: &mut Value, pointer: &str) {
    if pointer.is_empty() {
        return;
    }
    let Some((parent, token)) = pointer.rsplit_once('/') else {
        return;
    };
    let Some(parent) = value.pointer_mut(parent) else {
        return;
    };
    match parent {
        Value::Object(map) => {
            map.remove(token);
        }
        Value::Array(items) => {
            if let Ok(index) = token.parse::<usize>()
                && index < items.len()
            {
                items.remove(index);
            }
        }
        _ => {}
    }
}

/// Keep the import visible in public API docs and make redaction-aware callers
/// able to inspect markers without coupling matching to storage.
pub fn has_redaction_marker(markers: &[RedactionMarker], field: &str) -> bool {
    markers.iter().any(|marker| marker.field == field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyRef, FlowOutcome, HttpResponse, Provenance, SCHEMA_VERSION};

    fn candidate(method: &str, path: &str, body: &[u8]) -> MatchCandidate {
        MatchCandidate::new(
            Flow {
                schema_version: SCHEMA_VERSION,
                id: path.into(),
                started_at_ms: 1,
                completed_at_ms: Some(2),
                request: HttpRequest {
                    method: method.into(),
                    scheme: "http".into(),
                    authority: "example.test".into(),
                    path: path.into(),
                    query: vec![QueryPair {
                        key: "a".into(),
                        value: "1".into(),
                    }],
                    headers: vec![
                        HeaderEntry {
                            name: "x-test".into(),
                            value: "v".into(),
                        },
                        HeaderEntry {
                            name: "x-test".into(),
                            value: "v2".into(),
                        },
                    ],
                    body: if body.is_empty() {
                        BodyRef::Empty
                    } else {
                        BodyRef::Absent
                    },
                    trailers: vec![],
                },
                outcome: FlowOutcome::Response(HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: BodyRef::Empty,
                    trailers: vec![],
                }),
                physical_route: None,
                provenance: Provenance {
                    mode: "test".into(),
                    observer: "test".into(),
                },
                annotations: vec![],
                redactions: vec![],
            },
            body.to_vec(),
        )
    }

    #[test]
    fn strict_matching_preserves_duplicate_header_values_and_consumes_once() {
        let candidate = candidate("GET", "/one", b"payload");
        let request = candidate.flow.request.clone();
        let mut session = MatcherSession::new();
        let matcher = Matcher::strict(4);
        assert_eq!(
            matcher.select(
                &request,
                b"payload",
                std::slice::from_ref(&candidate),
                ConsumptionMode::Once,
                &mut session
            ),
            MatchResult::Matched(0)
        );
        assert!(matches!(
            matcher.select(
                &request,
                b"payload",
                std::slice::from_ref(&candidate),
                ConsumptionMode::Once,
                &mut session
            ),
            MatchResult::Exhausted { .. }
        ));
    }

    #[test]
    fn semantic_json_ignores_configured_pointer_and_near_miss_is_bounded() {
        let mut matcher = Matcher::new(MatcherProfile::Practical, BodyMatchMode::SemanticJson, 1);
        matcher.ignore_json_path("/volatile");
        let candidate = candidate("POST", "/json", br#"{"a":1,"volatile":"old"}"#);
        let mut actual = candidate.flow.request.clone();
        actual.method = "POST".into();
        assert_eq!(
            matcher.select(
                &actual,
                br#"{"volatile":"new","a":1}"#,
                std::slice::from_ref(&candidate),
                ConsumptionMode::Unlimited,
                &mut MatcherSession::new()
            ),
            MatchResult::Matched(0)
        );
        actual.path = "/different".into();
        let result = matcher.select(
            &actual,
            b"wrong",
            std::slice::from_ref(&candidate),
            ConsumptionMode::Unlimited,
            &mut MatcherSession::new(),
        );
        assert!(matches!(result, MatchResult::NoMatch { near_misses } if near_misses.len() == 1));
    }

    fn digest_candidate(method: &str, path: &str, body: &[u8]) -> MatchCandidate {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(body);
        let digest = format!("{:x}", hasher.finalize());
        let blob = crate::BlobRef::new(digest, body.len() as u64).unwrap();
        let mut flow = candidate(method, path, b"").flow.clone();
        flow.request.body = BodyRef::Blob(blob);
        MatchCandidate::from_flow(flow)
    }

    #[test]
    fn exact_matching_uses_digest_length_without_materialization() {
        let payload = b"exact-payload-123";
        let cand = digest_candidate("POST", "/digest", payload);
        assert!(matches!(cand.body, CandidateBody::Digest { .. }));
        let request = cand.flow.request.clone();
        let matcher = Matcher::strict(4);
        let mut loader_calls = 0;
        let mut loader = |_: usize, _: &MatchCandidate| -> Option<Vec<u8>> {
            loader_calls += 1;
            None
        };
        assert_eq!(
            matcher.select_with_loader(
                &request,
                payload,
                std::slice::from_ref(&cand),
                ConsumptionMode::Unlimited,
                &mut MatcherSession::new(),
                &mut loader
            ),
            MatchResult::Matched(0)
        );
        assert_eq!(loader_calls, 0, "exact bytes must not materialize");
        // Length mismatch must not match even with identical prefix.
        assert!(matches!(
            matcher.select(
                &request,
                b"exact-payload-12",
                std::slice::from_ref(&cand),
                ConsumptionMode::Unlimited,
                &mut MatcherSession::new()
            ),
            MatchResult::NoMatch { .. }
        ));
        // Absent/empty stay zero-allocation and match only empty actual.
        let empty_flow = candidate("GET", "/empty", b"").flow.clone();
        let empty_candidate = MatchCandidate::from_flow(empty_flow);
        assert!(matches!(empty_candidate.body, CandidateBody::Empty));
        let req = empty_candidate.flow.request.clone();
        assert_eq!(
            matcher.select(
                &req,
                b"",
                std::slice::from_ref(&empty_candidate),
                ConsumptionMode::Unlimited,
                &mut MatcherSession::new()
            ),
            MatchResult::Matched(0)
        );
    }

    #[test]
    fn semantic_json_materializes_only_narrowed_candidates() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut matcher = Matcher::new(MatcherProfile::Practical, BodyMatchMode::SemanticJson, 8);
        matcher.ignore_json_path("/volatile");
        // Two candidates: first differs on path (never narrowed), second
        // matches on all non-body dimensions (narrowed).
        let mut far_body = br#"{"a":1,"volatile":"old"}"#.to_vec();
        let mut near_body = br#"{"a":1,"volatile":"old"}"#.to_vec();
        let _ = (&mut far_body, &mut near_body);
        let far = {
            let mut flow = candidate("POST", "/far", b"").flow.clone();
            flow.request.body = {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(br#"{"a":1,"volatile":"old"}"#);
                BodyRef::Blob(crate::BlobRef::new(format!("{:x}", h.finalize()), 24).unwrap())
            };
            MatchCandidate::from_flow(flow)
        };
        let near = {
            // Build a Digest descriptor with correct length for the JSON.
            let raw = br#"{"a":1,"volatile":"old"}"#;
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(raw);
            let mut flow = candidate("POST", "/json", b"").flow.clone();
            flow.request.body = BodyRef::Blob(
                crate::BlobRef::new(format!("{:x}", h.finalize()), raw.len() as u64).unwrap(),
            );
            MatchCandidate::from_flow(flow)
        };
        let candidates = vec![far, near];
        let mut actual = candidates[1].flow.request.clone();
        actual.method = "POST".into();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let expected_bytes = br#"{"a":1,"volatile":"old"}"#.to_vec();
        let mut loader = move |index: usize, _: &MatchCandidate| -> Option<Vec<u8>> {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            assert_eq!(index, 1, "only the narrowed candidate may load");
            Some(expected_bytes.clone())
        };
        let result = matcher.select_with_loader(
            &actual,
            br#"{"a":1,"volatile":"new"}"#,
            &candidates,
            ConsumptionMode::Unlimited,
            &mut MatcherSession::new(),
            &mut loader,
        );
        assert_eq!(result, MatchResult::Matched(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
