//! Explicit configuration contracts used by later milestones.

use serde::{Deserialize, Serialize};

/// Output presentation format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Human-readable terminal output.
    #[default]
    Human,
    /// Versioned machine-readable JSON.
    Json,
    /// JUnit XML presentation.
    Junit,
}

/// Request/response and fixture resource limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum body bytes captured or materialized.
    pub max_body_bytes: u64,
    /// Maximum JSONL line size.
    pub max_line_bytes: u64,
    /// Maximum number of flows in a session.
    pub max_flows: usize,
    /// Maximum diagnostic candidates returned for one near miss.
    pub max_candidates: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 64 * 1024 * 1024,
            max_line_bytes: 4 * 1024 * 1024,
            max_flows: 100_000,
            max_candidates: 8,
        }
    }
}

/// Named redaction policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionProfile {
    /// Profile identifier persisted with a session.
    pub id: String,
    /// Header names treated as sensitive.
    pub sensitive_headers: Vec<String>,
    /// Query keys treated as sensitive.
    pub sensitive_query_keys: Vec<String>,
    /// JSON pointer paths treated as sensitive.
    pub sensitive_json_paths: Vec<String>,
}

impl Default for RedactionProfile {
    fn default() -> Self {
        Self {
            id: "default-v1".into(),
            sensitive_headers: [
                "authorization",
                "cookie",
                "set-cookie",
                "proxy-authorization",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            sensitive_query_keys: Vec::new(),
            sensitive_json_paths: Vec::new(),
        }
    }
}

/// Named matcher policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatcherProfile {
    /// Compare all selected request dimensions exactly.
    #[default]
    Strict,
    /// Ignore explicitly volatile headers and configured fields.
    Practical,
}

/// Top-level configuration skeleton.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Config {
    /// Resource bounds.
    #[serde(default)]
    pub limits: Limits,
    /// Redaction policy.
    #[serde(default)]
    pub redaction: RedactionProfile,
    /// Matcher profile.
    #[serde(default)]
    pub matcher: MatcherProfile,
    /// Default output format.
    #[serde(default)]
    pub output: OutputFormat,
}
