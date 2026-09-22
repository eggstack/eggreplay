//! Versioned semantic regression report authority.

use crate::{Flow, FlowOutcome, HeaderEntry, REPORT_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Scheduler recorded in a regression report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportScheduler {
    /// One baseline flow at a time.
    Sequential,
    /// Bounded capture-order concurrency.
    RecordedStartOrder,
}

/// Typed difference kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// Status code changed.
    Status,
    /// A selected response header changed.
    Header,
    /// A selected response trailer changed.
    Trailer,
    /// Body bytes or semantic JSON changed.
    Body,
    /// Success/error outcome changed.
    Outcome,
    /// An explicit timing threshold was exceeded.
    Timing,
}

/// One stable, machine-readable regression finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFinding {
    /// Finding category.
    pub kind: DiffKind,
    /// Stable field path.
    pub field: String,
    /// Redaction-safe baseline summary.
    pub baseline: String,
    /// Redaction-safe candidate summary.
    pub candidate: String,
}

/// An explicit timing assertion; no implicit timing comparisons are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingAssertion {
    /// Maximum candidate elapsed milliseconds.
    pub max_elapsed_ms: u64,
}

/// Versioned semantic comparison report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegressionReport {
    /// Report schema version.
    pub schema_version: u16,
    /// Scheduler used for candidate execution.
    pub scheduler: ReportScheduler,
    /// Baseline flow identifiers in stable order.
    pub baseline_flow_ids: Vec<String>,
    /// Findings in stable field/category order.
    pub findings: Vec<DiffFinding>,
}

impl RegressionReport {
    /// Return whether all comparisons passed.
    pub fn is_success(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Compare one baseline flow and one candidate observation.
pub fn compare_flows(
    baseline: &Flow,
    candidate: &Flow,
    baseline_body: &[u8],
    candidate_body: &[u8],
    scheduler: ReportScheduler,
) -> RegressionReport {
    compare_flows_with_timing(
        baseline,
        candidate,
        baseline_body,
        candidate_body,
        scheduler,
        None,
    )
}

/// Compare flows and optionally apply an explicit timing assertion.
pub fn compare_flows_with_timing(
    baseline: &Flow,
    candidate: &Flow,
    baseline_body: &[u8],
    candidate_body: &[u8],
    scheduler: ReportScheduler,
    timing: Option<TimingAssertion>,
) -> RegressionReport {
    let mut findings = Vec::new();
    match (&baseline.outcome, &candidate.outcome) {
        (FlowOutcome::Response(expected), FlowOutcome::Response(actual)) => {
            if expected.status != actual.status {
                findings.push(DiffFinding {
                    kind: DiffKind::Status,
                    field: "response.status".into(),
                    baseline: expected.status.to_string(),
                    candidate: actual.status.to_string(),
                });
            }
            compare_headers(
                &mut findings,
                "response.headers",
                &expected.headers,
                &actual.headers,
                DiffKind::Header,
            );
            compare_headers(
                &mut findings,
                "response.trailers",
                &expected.trailers,
                &actual.trailers,
                DiffKind::Trailer,
            );
            if sha256(baseline_body) != sha256(candidate_body)
                || baseline_body.len() != candidate_body.len()
            {
                findings.push(DiffFinding {
                    kind: DiffKind::Body,
                    field: "response.body".into(),
                    baseline: format!(
                        "sha256:{} length:{}",
                        sha256(baseline_body),
                        baseline_body.len()
                    ),
                    candidate: format!(
                        "sha256:{} length:{}",
                        sha256(candidate_body),
                        candidate_body.len()
                    ),
                });
            }
        }
        (FlowOutcome::Error(expected), FlowOutcome::Error(actual)) => {
            if expected.category != actual.category || expected.phase != actual.phase {
                findings.push(DiffFinding {
                    kind: DiffKind::Outcome,
                    field: "outcome.error".into(),
                    baseline: format!("{:?}/{:?}", expected.category, expected.phase),
                    candidate: format!("{:?}/{:?}", actual.category, actual.phase),
                });
            }
        }
        (expected, actual) => findings.push(DiffFinding {
            kind: DiffKind::Outcome,
            field: "outcome.kind".into(),
            baseline: outcome_name(expected).into(),
            candidate: outcome_name(actual).into(),
        }),
    }
    if let Some(assertion) = timing
        && let Some(end) = candidate.completed_at_ms
    {
        let elapsed = end.saturating_sub(candidate.started_at_ms);
        if elapsed > assertion.max_elapsed_ms {
            findings.push(DiffFinding {
                kind: DiffKind::Timing,
                field: "timing.elapsed_ms".into(),
                baseline: assertion.max_elapsed_ms.to_string(),
                candidate: elapsed.to_string(),
            });
        }
    }
    findings.sort_by(|left, right| {
        (format!("{:?}", left.kind), &left.field).cmp(&(format!("{:?}", right.kind), &right.field))
    });
    RegressionReport {
        schema_version: REPORT_SCHEMA_VERSION,
        scheduler,
        baseline_flow_ids: vec![baseline.id.clone()],
        findings,
    }
}

fn compare_headers(
    findings: &mut Vec<DiffFinding>,
    prefix: &str,
    baseline: &[HeaderEntry],
    candidate: &[HeaderEntry],
    kind: DiffKind,
) {
    let left = header_map(baseline);
    let right = header_map(candidate);
    let names = left
        .keys()
        .chain(right.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for name in names {
        let a = left.get(&name).cloned().unwrap_or_default();
        let b = right.get(&name).cloned().unwrap_or_default();
        if a != b {
            findings.push(DiffFinding {
                kind: kind.clone(),
                field: format!("{prefix}.{name}"),
                baseline: if a.is_empty() {
                    "<absent>".into()
                } else {
                    "<present>".into()
                },
                candidate: if b.is_empty() {
                    "<absent>".into()
                } else {
                    "<present>".into()
                },
            });
        }
    }
}

fn header_map(headers: &[HeaderEntry]) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    for header in headers {
        result
            .entry(header.name.to_ascii_lowercase())
            .or_insert_with(Vec::new)
            .push(header.value.clone());
    }
    result
}
fn outcome_name(outcome: &FlowOutcome) -> &'static str {
    match outcome {
        FlowOutcome::Response(_) => "response",
        FlowOutcome::Error(_) => "error",
    }
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
