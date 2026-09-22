//! Explicit configuration contracts used by later milestones.

use serde::{Deserialize, Serialize};

/// Explicit replay/upstream recording policy. It is independent of flow
/// consumption (`Once`, `RepeatLast`, or `Unlimited`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordMode {
    /// Replay only; a miss is offline and never reaches the network.
    Sealed,
    /// Record only when creating a new fixture; an existing fixture is sealed.
    Once,
    /// Replay first and append newly observed misses.
    AppendNew,
    /// Always execute upstream and atomically replace the fixture at shutdown.
    ReRecord,
}

/// Effective decision for a requested [`RecordMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordPolicy {
    /// Effective record mode after considering fixture existence.
    pub mode: RecordMode,
    /// Whether this process may make upstream requests.
    pub upstream_enabled: bool,
}

impl RecordMode {
    /// Resolve and validate the network policy. Network-capable modes require
    /// an explicit upstream; `once` seals an already-existing fixture.
    pub fn resolve(
        self,
        fixture_exists: bool,
        upstream_configured: bool,
    ) -> Result<RecordPolicy, String> {
        let existing_once = self == Self::Once && fixture_exists;
        let mode = if existing_once { Self::Sealed } else { self };
        let requires_upstream = matches!(mode, Self::Once | Self::AppendNew | Self::ReRecord);
        if requires_upstream && !upstream_configured {
            return Err(format!("record mode {mode} requires an explicit upstream"));
        }
        if !requires_upstream && upstream_configured && !existing_once {
            return Err("sealed mode does not accept an upstream".into());
        }
        Ok(RecordPolicy {
            mode,
            upstream_enabled: requires_upstream,
        })
    }
}

impl std::fmt::Display for RecordMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Sealed => "sealed",
            Self::Once => "once",
            Self::AppendNew => "append-new",
            Self::ReRecord => "re-record",
        })
    }
}

#[cfg(test)]
mod record_mode_tests {
    use super::*;

    #[test]
    fn network_access_is_explicit_and_once_seals_existing_fixture() {
        assert!(RecordMode::Sealed.resolve(false, true).is_err());
        assert!(RecordMode::AppendNew.resolve(false, false).is_err());
        let once = RecordMode::Once.resolve(false, true).unwrap();
        assert!(once.upstream_enabled);
        let existing = RecordMode::Once.resolve(true, true).unwrap();
        assert_eq!(existing.mode, RecordMode::Sealed);
        assert!(!existing.upstream_enabled);
    }
}

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
