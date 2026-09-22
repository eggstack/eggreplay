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

/// A candidate flow plus its materialized request body.
#[derive(Debug, Clone)]
pub struct MatchCandidate {
    /// Original flow record.
    pub flow: Flow,
    /// Request body bytes, loaded by the store layer.
    pub request_body: Vec<u8>,
}

impl MatchCandidate {
    /// Construct a candidate.
    pub fn new(flow: Flow, request_body: Vec<u8>) -> Self {
        Self { flow, request_body }
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
    pub fn select(
        &self,
        actual: &HttpRequest,
        actual_body: &[u8],
        candidates: &[MatchCandidate],
        mode: ConsumptionMode,
        session: &mut MatcherSession,
    ) -> MatchResult {
        let actual_normalized = self.normalize(actual);
        let mut near = Vec::new();
        let mut matching_consumed = false;
        for (index, candidate) in candidates.iter().enumerate() {
            let (mut dimensions, body_match) = self.differences(
                &actual_normalized,
                actual_body,
                &candidate.flow.request,
                &candidate.request_body,
            );
            if actual.method != candidate.flow.request.method {
                dimensions.push(MatchDimension::Method);
            }
            if dimensions.is_empty() && body_match {
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

    fn differences(
        &self,
        actual: &NormalizedRequest,
        body: &[u8],
        expected: &HttpRequest,
        expected_body: &[u8],
    ) -> (Vec<MatchDimension>, bool) {
        let expected = self.normalize(expected);
        let mut dimensions = Vec::new();
        if actual.scheme != expected.scheme || actual.authority != expected.authority {
            dimensions.push(MatchDimension::Authority);
        }
        if actual.path != expected.path {
            dimensions.push(MatchDimension::Path);
        }
        if actual.query != expected.query {
            dimensions.push(MatchDimension::Query);
        }
        if actual.headers != expected.headers {
            dimensions.push(MatchDimension::Headers);
        }
        if body_match(
            self.body_mode,
            body,
            expected_body,
            &self.ignored_json_paths,
        ) {
            (dimensions, true)
        } else {
            (dimensions, false)
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
}
