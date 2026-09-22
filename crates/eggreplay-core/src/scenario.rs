//! Bounded, deterministic authored replay scenarios.

use crate::{HeaderEntry, HttpRequest, RedactionConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SCENARIOS: usize = 128;
const MAX_STATES: usize = 128;
const MAX_TRANSITIONS: usize = 1024;
const MAX_VARIABLES: usize = 64;
const MAX_NAME_BYTES: usize = 128;
const MAX_TEMPLATE_BYTES: usize = 16 * 1024;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_JSON_BYTES: usize = 1024 * 1024;

/// Current schema for the `rules` session extension.
pub const RULES_SCHEMA_VERSION: u16 = 1;

/// A deterministic scenario rules document stored in the `rules` extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioRules {
    /// Rules schema version.
    pub schema_version: u16,
    /// Named scenarios, each isolated by replay-server instance.
    pub scenarios: Vec<Scenario>,
}

/// One independently selected finite state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// Stable scenario identifier.
    pub id: String,
    /// State entered when the scenario is selected.
    pub initial_state: String,
    /// Bounded list of valid state names.
    pub states: Vec<String>,
    /// Ordered transition list; the first matching transition wins.
    pub transitions: Vec<ScenarioTransition>,
}

/// One ordered state transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioTransition {
    /// Source state.
    pub from: String,
    /// Conjunctive request predicates; an empty list matches every request.
    #[serde(default)]
    pub when: Vec<RequestPredicate>,
    /// Values to extract before rendering the response.
    #[serde(default)]
    pub extract: Vec<VariableExtraction>,
    /// Action when an explicitly selected extraction cannot produce a value.
    #[serde(default)]
    pub extraction_failure: ExtractionFailureBehavior,
    /// Response produced when the transition matches.
    pub response: ScenarioResponse,
    /// Destination state, explicit even when it is the same as `from`.
    pub next_state: String,
}

/// Configured outcome when an extraction is missing or invalid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionFailureBehavior {
    /// Reject the request and preserve scenario state.
    #[default]
    Abort,
    /// Treat this transition as nonmatching and preserve scenario state.
    Skip,
}

/// Exact, duplicate-preserving request predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestPredicate {
    /// Exact method match.
    Method {
        /// Method string.
        value: String,
    },
    /// Exact path match (query remains a separate predicate).
    Path {
        /// Exact path string.
        value: String,
    },
    /// Match at least one occurrence of a query pair.
    Query {
        /// Exact query key.
        key: String,
        /// Exact query value.
        value: String,
    },
    /// Match at least one occurrence of a header without collapsing duplicates.
    Header {
        /// Header field name, compared case-insensitively.
        name: String,
        /// Exact header value.
        value: String,
    },
}

/// Source for an explicitly named deterministic variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VariableSource {
    /// A slash-separated request path segment, zero-based.
    PathSegment {
        /// Zero-based nonempty segment index.
        index: usize,
    },
    /// A query key; the first exact key occurrence is used.
    Query {
        /// Exact query key.
        key: String,
    },
    /// A header name; the first case-insensitive occurrence is used.
    Header {
        /// Header field name.
        name: String,
    },
    /// A JSON Pointer evaluated against a bounded JSON request body.
    JsonPointer {
        /// RFC 6901 pointer, or the empty string for the root value.
        pointer: String,
    },
    /// A value extracted by an earlier transition in this scenario.
    PriorVariable {
        /// Previously extracted variable name.
        name: String,
    },
}

/// One explicit extraction operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariableExtraction {
    /// Bounded variable name.
    pub name: String,
    /// Extraction source.
    pub source: VariableSource,
}

/// A bounded response authored in fixture data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioResponse {
    /// Response status.
    pub status: u16,
    /// Ordered headers; duplicate fields are preserved.
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    /// UTF-8 body template with `{{name}}` substitutions.
    #[serde(default)]
    pub body_template: String,
    /// Bounded JSON Pointer replacements applied after rendering the body.
    #[serde(default)]
    pub json_pointer_replacements: Vec<JsonPointerReplacement>,
}

/// One parsed JSON value replacement using a rendered UTF-8 string value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonPointerReplacement {
    /// RFC 6901 pointer to an existing value.
    pub pointer: String,
    /// Template rendered and stored as a JSON string value.
    pub value_template: String,
}

/// A fully rendered response projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedScenarioResponse {
    /// Response status.
    pub status: u16,
    /// Ordered response headers.
    pub headers: Vec<HeaderEntry>,
    /// Rendered body bytes.
    pub body: Vec<u8>,
}

/// A selected transition outcome, retaining its next state for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioStep {
    /// Scenario identifier.
    pub scenario_id: String,
    /// State before transition.
    pub previous_state: String,
    /// State after transition.
    pub next_state: String,
    /// Rendered response.
    pub response: RenderedScenarioResponse,
}

/// Per-server scenario state; create one for each replay-server instance.
#[derive(Debug, Clone)]
pub struct ScenarioRuntime {
    scenario: Scenario,
    state: String,
    variables: BTreeMap<String, String>,
    redaction: RedactionConfig,
}

impl ScenarioRules {
    /// Validate ordering, state references, uniqueness, and resource bounds.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RULES_SCHEMA_VERSION {
            return Err(format!("unsupported rules schema {}", self.schema_version));
        }
        if self.scenarios.len() > MAX_SCENARIOS {
            return Err("scenario count exceeds configured limit".into());
        }
        let mut ids = BTreeSet::new();
        for scenario in &self.scenarios {
            validate_name(&scenario.id, "scenario id")?;
            if !ids.insert(&scenario.id) {
                return Err("duplicate scenario id".into());
            }
            if scenario.states.is_empty() || scenario.states.len() > MAX_STATES {
                return Err("scenario state count exceeds configured bounds".into());
            }
            let mut states = BTreeSet::new();
            for state in &scenario.states {
                validate_name(state, "state name")?;
                if !states.insert(state) {
                    return Err("duplicate scenario state".into());
                }
            }
            if !states.contains(&scenario.initial_state) {
                return Err("initial state is not declared".into());
            }
            if scenario.transitions.len() > MAX_TRANSITIONS {
                return Err("scenario transition count exceeds configured limit".into());
            }
            for transition in &scenario.transitions {
                if !states.contains(&transition.from) || !states.contains(&transition.next_state) {
                    return Err("transition references an undeclared state".into());
                }
                validate_transition(transition)?;
            }
        }
        Ok(())
    }

    /// Select a named scenario and create isolated initial state.
    pub fn runtime(&self, scenario_id: &str) -> Result<ScenarioRuntime, String> {
        self.runtime_with_redaction(scenario_id, RedactionConfig::default_secure())
    }

    /// Select a named scenario with the effective policy that blocks
    /// extraction from fields marked sensitive.
    pub fn runtime_with_redaction(
        &self,
        scenario_id: &str,
        redaction: RedactionConfig,
    ) -> Result<ScenarioRuntime, String> {
        self.validate()?;
        let scenario = self
            .scenarios
            .iter()
            .find(|scenario| scenario.id == scenario_id)
            .cloned()
            .ok_or_else(|| format!("scenario {scenario_id:?} was not found"))?;
        Ok(ScenarioRuntime {
            state: scenario.initial_state.clone(),
            scenario,
            variables: BTreeMap::new(),
            redaction,
        })
    }
}

impl ScenarioRuntime {
    /// Current state name.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// Apply the first matching transition or return `None` when none matches.
    /// JSON extraction is bounded by `MAX_JSON_BYTES`; no host state is read.
    pub fn advance(
        &mut self,
        request: &HttpRequest,
        body: &[u8],
    ) -> Result<Option<ScenarioStep>, String> {
        let Some(transition) = self
            .scenario
            .transitions
            .iter()
            .find(|transition| {
                transition.from == self.state
                    && transition
                        .when
                        .iter()
                        .all(|predicate| predicate.matches(request))
            })
            .cloned()
        else {
            return Ok(None);
        };
        if transition.extract.len() + self.variables.len() > MAX_VARIABLES {
            return Err("scenario variable count exceeds configured limit".into());
        }
        let json_body = if transition
            .extract
            .iter()
            .any(|item| matches!(item.source, VariableSource::JsonPointer { .. }))
        {
            if body.len() > MAX_JSON_BYTES {
                return Err("JSON extraction body exceeds configured limit".into());
            }
            Some(
                serde_json::from_slice::<serde_json::Value>(body)
                    .map_err(|_| "JSON extraction body is invalid".to_owned())?,
            )
        } else {
            None
        };
        let mut next_variables = self.variables.clone();
        for extraction in &transition.extract {
            let value = match extract_value(
                extraction,
                request,
                json_body.as_ref(),
                &next_variables,
                &self.redaction,
            ) {
                Ok(value) => value,
                Err(error) => match transition.extraction_failure {
                    ExtractionFailureBehavior::Abort => return Err(error),
                    ExtractionFailureBehavior::Skip => return Ok(None),
                },
            };
            if value.len() > MAX_VALUE_BYTES {
                return Err("scenario variable value exceeds configured limit".into());
            }
            next_variables.insert(extraction.name.clone(), value);
        }
        let mut body = render(&transition.response.body_template, &next_variables)?;
        if !transition.response.json_pointer_replacements.is_empty() {
            let mut value: serde_json::Value = serde_json::from_str(&body)
                .map_err(|_| "scenario JSON transform body is invalid JSON".to_owned())?;
            for replacement in &transition.response.json_pointer_replacements {
                let target = value
                    .pointer_mut(&replacement.pointer)
                    .ok_or_else(|| "scenario JSON Pointer target was not found".to_owned())?;
                *target = serde_json::Value::String(render(
                    &replacement.value_template,
                    &next_variables,
                )?);
            }
            body = serde_json::to_string(&value)
                .map_err(|_| "scenario JSON transform serialization failed".to_owned())?;
            if body.len() > MAX_TEMPLATE_BYTES {
                return Err("rendered scenario JSON exceeds configured limit".into());
            }
        }
        if !(100..=599).contains(&transition.response.status) {
            return Err("scenario response status is invalid".into());
        }
        let headers = transition
            .response
            .headers
            .iter()
            .map(|header| {
                Ok(HeaderEntry {
                    name: header.name.clone(),
                    value: render(&header.value, &next_variables)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let previous_state = self.state.clone();
        self.state.clone_from(&transition.next_state);
        self.variables = next_variables;
        Ok(Some(ScenarioStep {
            scenario_id: self.scenario.id.clone(),
            previous_state,
            next_state: self.state.clone(),
            response: RenderedScenarioResponse {
                status: transition.response.status,
                headers,
                body: body.into_bytes(),
            },
        }))
    }
}

impl RequestPredicate {
    fn matches(&self, request: &HttpRequest) -> bool {
        match self {
            Self::Method { value } => request.method == *value,
            Self::Path { value } => request.path == *value,
            Self::Query { key, value } => request
                .query
                .iter()
                .any(|pair| pair.key == *key && pair.value == *value),
            Self::Header { name, value } => request
                .headers
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(name) && field.value == *value),
        }
    }
}

fn validate_transition(transition: &ScenarioTransition) -> Result<(), String> {
    if transition.extract.len() > MAX_VARIABLES {
        return Err("transition variable count exceeds configured limit".into());
    }
    if transition.response.body_template.len() > MAX_TEMPLATE_BYTES {
        return Err("scenario template exceeds configured limit".into());
    }
    if transition.response.json_pointer_replacements.len() > 32 {
        return Err("scenario JSON transform count exceeds configured limit".into());
    }
    for replacement in &transition.response.json_pointer_replacements {
        if replacement.pointer.len() > MAX_NAME_BYTES
            || (!replacement.pointer.is_empty() && !replacement.pointer.starts_with('/'))
            || replacement.value_template.len() > MAX_TEMPLATE_BYTES
        {
            return Err("invalid or oversized scenario JSON transform".into());
        }
    }
    if !(100..=599).contains(&transition.response.status) {
        return Err("scenario response status is invalid".into());
    }
    for header in &transition.response.headers {
        if header.name.len() > 256 || header.value.len() > MAX_VALUE_BYTES {
            return Err("scenario response header exceeds configured limit".into());
        }
        if header.name.is_empty()
            || !header.name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
            || header
                .value
                .bytes()
                .any(|byte| byte == b'\r' || byte == b'\n' || (byte < b' ' && byte != b'\t'))
        {
            return Err("scenario response header is malformed".into());
        }
    }
    let mut names = BTreeSet::new();
    for extraction in &transition.extract {
        validate_name(&extraction.name, "variable name")?;
        if !names.insert(&extraction.name) {
            return Err("duplicate variable extraction name".into());
        }
        match &extraction.source {
            VariableSource::Query { key } | VariableSource::Header { name: key } => {
                validate_name(key, "extraction key")?;
            }
            VariableSource::JsonPointer { pointer } => {
                if pointer.len() > MAX_NAME_BYTES
                    || (!pointer.is_empty() && !pointer.starts_with('/'))
                {
                    return Err("invalid JSON Pointer".into());
                }
            }
            VariableSource::PriorVariable { name } => validate_name(name, "prior variable")?,
            VariableSource::PathSegment { index } if *index > 1024 => {
                return Err("path segment index exceeds configured limit".into());
            }
            VariableSource::PathSegment { .. } => {}
        }
    }
    for predicate in &transition.when {
        let check = |name: &str, value: &str| {
            if value.len() > MAX_VALUE_BYTES {
                Err(format!(
                    "scenario {name} predicate exceeds configured limit"
                ))
            } else {
                Ok(())
            }
        };
        match predicate {
            RequestPredicate::Method { value } => check("method", value)?,
            RequestPredicate::Path { value } => check("path", value)?,
            RequestPredicate::Query { key, value } => {
                check("query key", key)?;
                check("query value", value)?;
            }
            RequestPredicate::Header { name, value } => {
                check("header name", name)?;
                check("header value", value)?;
            }
        }
    }
    Ok(())
}

fn validate_name(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_NAME_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("invalid or oversized {label}"));
    }
    Ok(())
}

fn extract_value(
    extraction: &VariableExtraction,
    request: &HttpRequest,
    json_body: Option<&serde_json::Value>,
    variables: &BTreeMap<String, String>,
    redaction: &RedactionConfig,
) -> Result<String, String> {
    match &extraction.source {
        VariableSource::Query { key } if redaction.query_keys.contains(key) => {
            return Err("scenario extraction source is redacted".into());
        }
        VariableSource::Header { name }
            if redaction.headers.contains(&name.to_ascii_lowercase()) =>
        {
            return Err("scenario extraction source is redacted".into());
        }
        VariableSource::JsonPointer { pointer } if redaction.json_paths.contains(pointer) => {
            return Err("scenario extraction source is redacted".into());
        }
        _ => {}
    }
    let value = match &extraction.source {
        VariableSource::PathSegment { index } => request
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .nth(*index)
            .map(str::to_owned),
        VariableSource::Query { key } => request
            .query
            .iter()
            .find(|pair| pair.key == *key)
            .map(|pair| pair.value.clone()),
        VariableSource::Header { name } => request
            .headers
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .map(|field| field.value.clone()),
        VariableSource::JsonPointer { pointer } => json_body
            .and_then(|value| value.pointer(pointer))
            .map(|value| match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            }),
        VariableSource::PriorVariable { name } => variables.get(name).cloned(),
    };
    value.ok_or_else(|| {
        format!(
            "scenario variable {:?} could not be extracted",
            extraction.name
        )
    })
}

/// Render only explicit `{{name}}` substitutions. Values are inserted as UTF-8
/// text without implicit HTML/JSON escaping; JSON bodies should use authored
/// values compatible with their declared content type.
fn render(template: &str, variables: &BTreeMap<String, String>) -> Result<String, String> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err("scenario template exceeds configured limit".into());
    }
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find("}}")
            .ok_or_else(|| "unterminated scenario variable".to_owned())?;
        let name = &rest[..end];
        validate_name(name, "template variable")?;
        let value = variables
            .get(name)
            .ok_or_else(|| format!("scenario variable {name:?} is missing"))?;
        output.push_str(value);
        rest = &rest[end + 2..];
        if output.len() > MAX_TEMPLATE_BYTES {
            return Err("rendered scenario template exceeds configured limit".into());
        }
    }
    output.push_str(rest);
    if output.len() > MAX_TEMPLATE_BYTES {
        return Err("rendered scenario template exceeds configured limit".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyRef, QueryPair};

    fn rules() -> ScenarioRules {
        ScenarioRules {
            schema_version: 1,
            scenarios: vec![Scenario {
                id: "cart".into(),
                initial_state: "empty".into(),
                states: vec!["empty".into(), "added".into()],
                transitions: vec![ScenarioTransition {
                    from: "empty".into(),
                    when: vec![RequestPredicate::Path {
                        value: "/cart/abc".into(),
                    }],
                    extract: vec![VariableExtraction {
                        name: "product".into(),
                        source: VariableSource::PathSegment { index: 1 },
                    }],
                    extraction_failure: ExtractionFailureBehavior::Abort,
                    response: ScenarioResponse {
                        status: 201,
                        headers: vec![HeaderEntry {
                            name: "x-product".into(),
                            value: "{{product}}".into(),
                        }],
                        body_template: "added {{product}}".into(),
                        json_pointer_replacements: vec![],
                    },
                    next_state: "added".into(),
                }],
            }],
        }
    }

    fn request() -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            scheme: "http".into(),
            authority: "example.test".into(),
            path: "/cart/abc".into(),
            query: vec![QueryPair {
                key: "token".into(),
                value: "x".into(),
            }],
            headers: vec![],
            body: BodyRef::Empty,
            trailers: vec![],
        }
    }

    #[test]
    fn transitions_are_ordered_deterministic_and_isolated() {
        let rules = rules();
        let mut first = rules.runtime("cart").unwrap();
        let mut second = rules.runtime("cart").unwrap();
        let step = first.advance(&request(), b"").unwrap().unwrap();
        assert_eq!(step.response.body, b"added abc");
        assert_eq!(step.response.headers[0].value, "abc");
        assert_eq!(first.state(), "added");
        assert_eq!(second.state(), "empty");
        assert!(second.advance(&request(), b"").unwrap().is_some());
    }

    #[test]
    fn missing_variables_and_invalid_state_references_fail_closed() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].extract.clear();
        let mut runtime = rule.runtime("cart").unwrap();
        assert!(runtime.advance(&request(), b"").is_err());
        rule.scenarios[0].transitions[0].next_state = "undeclared".into();
        assert!(rule.validate().is_err());
    }

    #[test]
    fn configured_extraction_skip_leaves_scenario_state_unchanged() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].extraction_failure = ExtractionFailureBehavior::Skip;
        rule.scenarios[0].transitions[0].extract[0].source =
            VariableSource::PathSegment { index: 4 };
        let mut runtime = rule.runtime("cart").unwrap();
        assert!(runtime.advance(&request(), b"").unwrap().is_none());
        assert_eq!(runtime.state(), "empty");
    }

    #[test]
    fn query_header_and_json_extraction_are_supported() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].extract = vec![
            VariableExtraction {
                name: "q".into(),
                source: VariableSource::Query {
                    key: "token".into(),
                },
            },
            VariableExtraction {
                name: "field".into(),
                source: VariableSource::JsonPointer {
                    pointer: "/id".into(),
                },
            },
            VariableExtraction {
                name: "prior".into(),
                source: VariableSource::PriorVariable { name: "q".into() },
            },
            VariableExtraction {
                name: "header".into(),
                source: VariableSource::Header {
                    name: "x-id".into(),
                },
            },
        ];
        rule.scenarios[0].transitions[0].response.body_template =
            "{{field}}-{{prior}}-{{header}}".into();
        rule.scenarios[0].transitions[0].response.headers[0].value = "{{header}}".into();
        let mut request = request();
        request.headers.push(HeaderEntry {
            name: "x-id".into(),
            value: "header".into(),
        });
        let mut runtime = rule.runtime("cart").unwrap();
        assert_eq!(
            runtime
                .advance(&request, br#"{"id":42}"#)
                .unwrap()
                .unwrap()
                .response
                .body,
            b"42-x-header"
        );
    }

    #[test]
    fn json_pointer_replacement_uses_parsed_values() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].response.body_template = r#"{"id":0}"#.into();
        rule.scenarios[0].transitions[0]
            .response
            .json_pointer_replacements = vec![JsonPointerReplacement {
            pointer: "/id".into(),
            value_template: "{{product}}".into(),
        }];
        let mut runtime = rule.runtime("cart").unwrap();
        let output = runtime.advance(&request(), b"").unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.response.body).unwrap()["id"],
            "abc"
        );
    }

    #[test]
    fn validation_enforces_template_and_transform_bounds() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].response.body_template =
            "x".repeat(MAX_TEMPLATE_BYTES + 1);
        assert!(rule.validate().is_err());
        rule.scenarios[0].transitions[0].response.body_template = "{}".into();
        rule.scenarios[0].transitions[0]
            .response
            .json_pointer_replacements = vec![JsonPointerReplacement {
            pointer: "not-a-pointer".into(),
            value_template: "x".into(),
        }];
        assert!(rule.validate().is_err());
    }

    #[test]
    fn scenario_never_extracts_default_redacted_headers() {
        let mut rule = rules();
        rule.scenarios[0].transitions[0].extract[0].source = VariableSource::Header {
            name: "authorization".into(),
        };
        let mut request = request();
        request.headers.push(HeaderEntry {
            name: "authorization".into(),
            value: "sensitive-value".into(),
        });
        let mut runtime = rule.runtime("cart").unwrap();
        let error = runtime.advance(&request, b"").unwrap_err();
        assert!(error.contains("redacted"));
        assert!(!error.contains("sensitive-value"));
        assert_eq!(runtime.state(), "empty");
    }
}
