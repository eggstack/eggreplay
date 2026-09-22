//! Versioned semantic regression report authority.

use crate::{
    Flow, FlowOutcome, FlowStreamEvents, HeaderEntry, REPORT_SCHEMA_VERSION, StreamEventKind,
};
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
    /// Concurrent candidate execution scheduled by monotonic fixture offsets.
    Timeline,
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
    /// SSE semantic events differ or parsing failed.
    Sse,
    /// Ordered stream events differ.
    StreamEvent,
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
            if is_sse(&expected.headers) && is_sse(&actual.headers) {
                let baseline_sse = crate::parse_sse(baseline_body, false);
                let candidate_sse = crate::parse_sse(candidate_body, false);
                let finding = match (&baseline_sse.error, &candidate_sse.error) {
                    (Some(_), _) => Some(("baseline SSE is malformed", "candidate SSE inspected")),
                    (_, Some(_)) => Some(("baseline SSE parsed", "candidate SSE is malformed")),
                    (None, None)
                        if !crate::compare_sse(
                            &baseline_sse.events,
                            &candidate_sse.events,
                            &[],
                        ) =>
                    {
                        Some(("SSE event sequence", "SSE event sequence"))
                    }
                    _ => None,
                };
                if let Some((baseline, candidate)) = finding {
                    findings.push(DiffFinding {
                        kind: DiffKind::Sse,
                        field: "response.sse".into(),
                        baseline: baseline.into(),
                        candidate: candidate.into(),
                    });
                }
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

/// Compare opt-in stream semantics and, optionally, cadence tolerance.
/// Absolute capture timestamps are never compared.
pub fn compare_stream_events(
    baseline: &FlowStreamEvents,
    candidate: &FlowStreamEvents,
    cadence_tolerance_ns: Option<u64>,
) -> Vec<DiffFinding> {
    let mut findings = Vec::new();
    for (direction, left, right) in [
        ("request", &baseline.request, &candidate.request),
        ("response", &baseline.response, &candidate.response),
    ] {
        let shape_differs = left.len() != right.len()
            || left
                .iter()
                .zip(right)
                .any(|(a, b)| !same_stream_event(&a.event, &b.event));
        if shape_differs {
            findings.push(DiffFinding {
                kind: DiffKind::StreamEvent,
                field: format!("stream.{direction}.events"),
                baseline: format!("{} ordered events", left.len()),
                candidate: format!("{} ordered events", right.len()),
            });
        }
        if let Some(tolerance) = cadence_tolerance_ns {
            for (index, (a, b)) in left.iter().zip(right).enumerate() {
                let baseline_gap = a.delta_ns.saturating_sub(
                    index
                        .checked_sub(1)
                        .map_or(0, |previous| left[previous].delta_ns),
                );
                let candidate_gap = b.delta_ns.saturating_sub(
                    index
                        .checked_sub(1)
                        .map_or(0, |previous| right[previous].delta_ns),
                );
                if baseline_gap.abs_diff(candidate_gap) > tolerance {
                    findings.push(DiffFinding {
                        kind: DiffKind::Timing,
                        field: format!("stream.{direction}.cadence[{index}]"),
                        baseline: baseline_gap.to_string(),
                        candidate: candidate_gap.to_string(),
                    });
                }
            }
        }
    }
    findings.sort_by(|left, right| left.field.cmp(&right.field));
    findings
}

fn same_stream_event(left: &StreamEventKind, right: &StreamEventKind) -> bool {
    match (left, right) {
        (
            StreamEventKind::Data {
                offset: left_offset,
                length: left_length,
            },
            StreamEventKind::Data {
                offset: right_offset,
                length: right_length,
            },
        ) => left_offset == right_offset && left_length == right_length,
        (
            StreamEventKind::Trailers { fields: left },
            StreamEventKind::Trailers { fields: right },
        ) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(a, b)| a.name.eq_ignore_ascii_case(&b.name))
        }
        (StreamEventKind::End, StreamEventKind::End) => true,
        (
            StreamEventKind::Error {
                offset: left_offset,
                category: left_category,
                phase: left_phase,
            },
            StreamEventKind::Error {
                offset: right_offset,
                category: right_category,
                phase: right_phase,
            },
        ) => {
            left_offset == right_offset
                && left_category == right_category
                && left_phase == right_phase
        }
        _ => false,
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

fn is_sse(headers: &[HeaderEntry]) -> bool {
    headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("content-type")
            && header
                .value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyRef, HeaderEntry, HttpRequest, HttpResponse};

    #[test]
    fn stream_event_comparison_reports_shape_and_optional_cadence_only() {
        let make = |second_delta, length| FlowStreamEvents {
            flow_id: "flow".into(),
            start_offset_ns: 123,
            request: Vec::new(),
            response: vec![
                crate::StreamEvent {
                    delta_ns: 0,
                    event: StreamEventKind::Data {
                        offset: 0,
                        length: 1,
                    },
                },
                crate::StreamEvent {
                    delta_ns: second_delta,
                    event: StreamEventKind::Data { offset: 1, length },
                },
                crate::StreamEvent {
                    delta_ns: second_delta + 1,
                    event: StreamEventKind::End,
                },
            ],
        };
        let baseline = make(100, 1);
        let same_shape = make(1000, 1);
        assert!(compare_stream_events(&baseline, &same_shape, None).is_empty());
        let timing = compare_stream_events(&baseline, &same_shape, Some(10));
        assert!(
            timing
                .iter()
                .any(|finding| finding.kind == DiffKind::Timing)
        );
        let shape = compare_stream_events(&baseline, &make(100, 2), None);
        assert!(
            shape
                .iter()
                .any(|finding| finding.kind == DiffKind::StreamEvent)
        );
    }

    #[test]
    fn sse_semantic_difference_is_reported_alongside_raw_body_diff() {
        let request = HttpRequest {
            method: "GET".into(),
            scheme: "https".into(),
            authority: "example.test".into(),
            path: "/events".into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: BodyRef::Absent,
            trailers: Vec::new(),
        };
        let response = HttpResponse {
            status: 200,
            headers: vec![HeaderEntry {
                name: "content-type".into(),
                value: "text/event-stream".into(),
            }],
            body: BodyRef::Absent,
            trailers: Vec::new(),
        };
        let mut baseline = Flow::new(request.clone(), FlowOutcome::Response(response.clone()), 1);
        let candidate = Flow::new(request, FlowOutcome::Response(response), 1);
        let report = compare_flows(
            &baseline,
            &candidate,
            b"data: old\n\n",
            b"data: new\n\n",
            ReportScheduler::Sequential,
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == DiffKind::Body)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == DiffKind::Sse)
        );

        baseline.outcome = FlowOutcome::Response(HttpResponse {
            headers: vec![HeaderEntry {
                name: "content-type".into(),
                value: "application/json".into(),
            }],
            ..match baseline.outcome {
                FlowOutcome::Response(response) => response,
                _ => unreachable!(),
            }
        });
        let opaque = compare_flows(
            &baseline,
            &candidate,
            b"data: old\n\n",
            b"data: new\n\n",
            ReportScheduler::Sequential,
        );
        assert!(
            !opaque
                .findings
                .iter()
                .any(|finding| finding.kind == DiffKind::Sse)
        );
    }
}
