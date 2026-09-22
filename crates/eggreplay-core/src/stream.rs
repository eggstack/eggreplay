//! Bounded, payload-free stream timelines and derived SSE semantics.

use crate::HeaderEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Current stream-event extension schema.
pub const STREAM_EVENTS_SCHEMA_VERSION: u16 = 1;
/// Maximum event records for one flow, across request and response directions.
pub const MAX_STREAM_EVENTS_PER_FLOW: usize = 4096;
/// Maximum event records in one session extension.
pub const MAX_STREAM_EVENTS_PER_SESSION: usize = 1_000_000;
/// Maximum serialized stream-event extension size.
pub const MAX_STREAM_EVENTS_BYTES: usize = 16 * 1024 * 1024;
/// Maximum captured/replayed event delay.
pub const MAX_STREAM_DELAY_NS: u64 = 60_000_000_000;
/// Maximum total replay sleep for one flow.
pub const MAX_STREAM_TOTAL_DELAY_NS: u64 = 300_000_000_000;
/// Maximum SSE line bytes.
pub const MAX_SSE_LINE_BYTES: usize = 64 * 1024;
/// Maximum parsed SSE events.
pub const MAX_SSE_EVENTS: usize = 100_000;
/// Maximum source body bytes parsed into an SSE-derived view.
pub const MAX_SSE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Body direction for a semantic stream event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamDirection {
    /// Client request body.
    Request,
    /// Server response body.
    Response,
}

/// One semantic frame or terminal condition; DATA bytes remain in body blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEventKind {
    /// A delivered DATA boundary.
    Data {
        /// Byte offset before this frame.
        offset: u64,
        /// Number of bytes in this frame.
        length: u64,
    },
    /// A trailers frame.
    Trailers {
        /// Trailer fields observed at this point.
        fields: Vec<HeaderEntry>,
    },
    /// Clean end of stream.
    End,
    /// Semantic mid-body failure at the delivered byte offset.
    Error {
        /// Number of body bytes successfully delivered.
        offset: u64,
        /// Stable semantic error category.
        category: String,
        /// Stable semantic error phase.
        phase: String,
    },
}

/// One bounded event with a capture-local monotonic delta from stream start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Nanoseconds since the corresponding request/response body started.
    pub delta_ns: u64,
    /// Event meaning and body offset.
    pub event: StreamEventKind,
}

/// Timeline metadata associated with one flow; it contains no DATA payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStreamEvents {
    /// Stable flow identifier.
    pub flow_id: String,
    /// Capture-local monotonic start offset for optional timeline scheduling.
    pub start_offset_ns: u64,
    /// Request body events in observation order.
    #[serde(default)]
    pub request: Vec<StreamEvent>,
    /// Response body events in observation order.
    #[serde(default)]
    pub response: Vec<StreamEvent>,
}

/// Versioned stream-event extension payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEvents {
    /// Extension schema version.
    pub schema_version: u16,
    /// Timelines in fixture order.
    pub flows: Vec<FlowStreamEvents>,
}

/// Order fixture indices by capture-local start offset with fixture-order ties.
pub fn timeline_order_offsets(
    offsets_ns: &[u64],
    max_concurrency: usize,
) -> Result<Vec<usize>, String> {
    if max_concurrency == 0 || max_concurrency > 1024 {
        return Err("maximum timeline concurrency must be within 1..=1024".into());
    }
    if offsets_ns
        .iter()
        .any(|offset| *offset > MAX_STREAM_TOTAL_DELAY_NS)
    {
        return Err("timeline start offset exceeds configured limit".into());
    }
    let mut indices = (0..offsets_ns.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| (offsets_ns[*index], *index));
    Ok(indices)
}

impl Default for StreamEvents {
    fn default() -> Self {
        Self {
            schema_version: STREAM_EVENTS_SCHEMA_VERSION,
            flows: Vec::new(),
        }
    }
}

impl StreamEvents {
    /// Validate schema and configured bounds before accepting or replaying.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STREAM_EVENTS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported stream-events schema {}",
                self.schema_version
            ));
        }
        if self.flows.len() > MAX_STREAM_EVENTS_PER_SESSION {
            return Err("stream event flow count exceeds configured limit".into());
        }
        let mut total = 0usize;
        let mut ids = BTreeSet::new();
        for flow in &self.flows {
            if flow.flow_id.is_empty() || flow.flow_id.len() > 256 || !ids.insert(&flow.flow_id) {
                return Err("stream event flow id is empty, oversized, or duplicated".into());
            }
            let count = flow.request.len().saturating_add(flow.response.len());
            if count > MAX_STREAM_EVENTS_PER_FLOW {
                return Err("stream event count for flow exceeds configured limit".into());
            }
            total = total.saturating_add(count);
            if total > MAX_STREAM_EVENTS_PER_SESSION {
                return Err("aggregate stream event count exceeds configured limit".into());
            }
            for events in [&flow.request, &flow.response] {
                let mut last_delta = 0;
                let mut last_offset = 0;
                let mut terminal = false;
                let mut trailers_seen = false;
                for event in events {
                    if terminal
                        || event.delta_ns < last_delta
                        || event.delta_ns > MAX_STREAM_DELAY_NS
                    {
                        return Err("stream event order or delay is invalid".into());
                    }
                    last_delta = event.delta_ns;
                    match &event.event {
                        StreamEventKind::Data { offset, length } => {
                            if trailers_seen || *offset != last_offset || *length == 0 {
                                return Err(
                                    "stream DATA offsets must be contiguous and nonempty".into()
                                );
                            }
                            last_offset = offset.saturating_add(*length);
                        }
                        StreamEventKind::Error {
                            offset,
                            category,
                            phase,
                        } => {
                            if *offset != last_offset || category.len() > 64 || phase.len() > 64 {
                                return Err("stream terminal error is invalid".into());
                            }
                            terminal = true;
                        }
                        StreamEventKind::Trailers { fields } => {
                            if trailers_seen
                                || fields.len() > 256
                                || fields
                                    .iter()
                                    .any(|field| field.name.len() > 256 || field.value.len() > 8192)
                            {
                                return Err("stream trailers exceed configured limit".into());
                            }
                            trailers_seen = true;
                        }
                        StreamEventKind::End => terminal = true,
                    }
                }
            }
        }
        Ok(())
    }
}

/// Delay policy used by explicit timed replay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StreamTimingMode {
    /// Do not delay frames; compatibility default.
    Immediate,
    /// Replay recorded relative delays unchanged.
    Recorded,
    /// Scale relative delays by a finite factor in `0.01..=100.0`.
    Scaled(f64),
}

impl StreamTimingMode {
    /// Parse `immediate`, `recorded`, or `scaled:<factor>`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "immediate" => Ok(Self::Immediate),
            "recorded" => Ok(Self::Recorded),
            value => {
                let factor = value
                    .strip_prefix("scaled:")
                    .ok_or_else(|| {
                        "timing mode must be immediate, recorded, or scaled:<factor>".to_owned()
                    })?
                    .parse::<f64>()
                    .map_err(|_| "invalid timing scale".to_owned())?;
                if !factor.is_finite() || !(0.01..=100.0).contains(&factor) {
                    return Err("timing scale must be finite and within 0.01..=100".into());
                }
                Ok(Self::Scaled(factor))
            }
        }
    }

    /// Scale an event delta, enforcing per-delay and total-flow limits.
    pub fn delay_ns(self, delta_ns: u64, accumulated_ns: u64) -> Result<u64, String> {
        if delta_ns > MAX_STREAM_DELAY_NS {
            return Err("recorded stream delay exceeds configured limit".into());
        }
        let scaled = match self {
            Self::Immediate => 0,
            Self::Recorded => delta_ns,
            Self::Scaled(factor) => (delta_ns as f64 * factor).ceil() as u64,
        };
        if accumulated_ns.saturating_add(scaled) > MAX_STREAM_TOTAL_DELAY_NS {
            return Err("total replay delay exceeds configured limit".into());
        }
        Ok(scaled)
    }
}

/// One parsed, derived Server-Sent Event. Comments are opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseEvent {
    /// Concatenated `data` lines, joined with LF.
    pub data: String,
    /// Optional event type.
    pub event: Option<String>,
    /// Optional last-event identifier.
    pub id: Option<String>,
    /// Optional reconnect delay in milliseconds.
    pub retry: Option<u64>,
    /// Comment lines when requested by the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
}

/// Bounded parse result; malformed input leaves the original bytes untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SseParseResult {
    /// Parsed events in source order.
    pub events: Vec<SseEvent>,
    /// Bounded syntax diagnostic, if parsing encountered malformed input.
    pub error: Option<String>,
}

/// Parse an SSE body into a bounded derived view.
pub fn parse_sse(body: &[u8], include_comments: bool) -> SseParseResult {
    if body.len() > MAX_SSE_BODY_BYTES {
        return SseParseResult {
            events: Vec::new(),
            error: Some("SSE body exceeds configured byte limit".into()),
        };
    }
    let text = match std::str::from_utf8(body) {
        Ok(text) => text,
        Err(_) => {
            return SseParseResult {
                events: Vec::new(),
                error: Some("SSE body is not valid UTF-8".into()),
            };
        }
    };
    let mut result = SseParseResult {
        events: Vec::new(),
        error: None,
    };
    let mut data = Vec::new();
    let mut event = None;
    let mut id = None;
    let mut retry = None;
    let mut comments = Vec::new();
    let mut line_bytes = 0usize;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        line_bytes = line.len();
        if line_bytes > MAX_SSE_LINE_BYTES {
            result.error = Some("SSE line exceeds configured byte limit".into());
            break;
        }
        if line.is_empty() {
            if !data.is_empty()
                || event.is_some()
                || id.is_some()
                || retry.is_some()
                || !comments.is_empty()
            {
                if result.events.len() >= MAX_SSE_EVENTS {
                    result.error = Some("SSE event count exceeds configured limit".into());
                    break;
                }
                data.pop();
                result.events.push(SseEvent {
                    data: data.concat(),
                    event: event.take(),
                    id: id.take(),
                    retry: retry.take(),
                    comments: std::mem::take(&mut comments),
                });
                data.clear();
            }
            continue;
        }
        if let Some(comment) = line.strip_prefix(':') {
            if include_comments {
                comments.push(comment.strip_prefix(' ').unwrap_or(comment).to_owned());
            }
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                data.push(value);
                data.push("\n");
            }
            "event" => event = Some(value.to_owned()),
            "id" => {
                if value.contains('\0') {
                    result.error = Some("invalid SSE field value".into());
                } else {
                    id = Some(value.to_owned());
                }
            }
            "retry" => {
                if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
                    match value.parse() {
                        Ok(value) => retry = Some(value),
                        Err(_) => result.error = Some("invalid SSE field value".into()),
                    }
                } else {
                    result.error = Some("invalid SSE field value".into());
                }
            }
            _ => (),
        }
    }
    if result.error.is_none()
        && (!data.is_empty()
            || event.is_some()
            || id.is_some()
            || retry.is_some()
            || !comments.is_empty())
    {
        if result.events.len() >= MAX_SSE_EVENTS {
            result.error = Some("SSE event count exceeds configured limit".into());
        } else {
            data.pop();
            result.events.push(SseEvent {
                data: data.concat(),
                event,
                id,
                retry,
                comments,
            });
        }
    }
    let _ = line_bytes;
    result
}

/// Compare ordered SSE semantics while explicitly ignoring selected fields.
pub fn compare_sse(expected: &[SseEvent], actual: &[SseEvent], ignored: &[String]) -> bool {
    let ignored = ignored.iter().map(String::as_str).collect::<BTreeSet<_>>();
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(left, right)| {
            (ignored.contains("data") || left.data == right.data)
                && (ignored.contains("event") || left.event == right.event)
                && (ignored.contains("id") || left.id == right.id)
                && (ignored.contains("retry") || left.retry == right.retry)
                && (ignored.contains("comments") || left.comments == right.comments)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_mode_bounds_and_scaling_are_explicit() {
        assert_eq!(
            StreamTimingMode::parse("immediate")
                .unwrap()
                .delay_ns(50, 0)
                .unwrap(),
            0
        );
        assert_eq!(
            StreamTimingMode::parse("scaled:2")
                .unwrap()
                .delay_ns(50, 0)
                .unwrap(),
            100
        );
        assert!(StreamTimingMode::parse("scaled:NaN").is_err());
        assert!(StreamTimingMode::parse("scaled:1000").is_err());
    }

    #[test]
    fn parses_multiline_sse_and_compares_ignored_fields() {
        let parsed = parse_sse(
            b": heartbeat\nid: 7\nevent: update\ndata: first\ndata: second\n\n",
            true,
        );
        assert_eq!(parsed.error, None);
        assert_eq!(parsed.events[0].data, "first\nsecond");
        assert_eq!(parsed.events[0].comments, ["heartbeat"]);
        let changed = parse_sse(
            b"id: 8\nevent: update\ndata: first\ndata: second\n\n",
            false,
        );
        assert!(!compare_sse(&parsed.events, &changed.events, &[]));
        assert!(compare_sse(
            &parsed.events,
            &changed.events,
            &["id".into(), "comments".into()]
        ));
    }

    #[test]
    fn malformed_and_oversized_sse_is_reported() {
        assert!(parse_sse(&[0xff], false).error.is_some());
        assert!(
            parse_sse(
                format!("data: {}\n\n", "x".repeat(MAX_SSE_LINE_BYTES + 1)).as_bytes(),
                false
            )
            .error
            .is_some()
        );
    }

    #[test]
    fn stream_event_schema_rejects_discontinuities_and_large_delays() {
        let events = StreamEvents {
            schema_version: STREAM_EVENTS_SCHEMA_VERSION,
            flows: vec![FlowStreamEvents {
                flow_id: "f1".into(),
                start_offset_ns: 0,
                request: vec![StreamEvent {
                    delta_ns: MAX_STREAM_DELAY_NS + 1,
                    event: StreamEventKind::End,
                }],
                response: vec![],
            }],
        };
        assert!(events.validate().is_err());
    }

    #[test]
    fn timeline_offsets_are_stable_and_concurrency_bounded() {
        assert_eq!(timeline_order_offsets(&[20, 10, 10], 2).unwrap(), [1, 2, 0]);
        assert!(timeline_order_offsets(&[0], 0).is_err());
        assert!(timeline_order_offsets(&[0], 1025).is_err());
    }

    #[test]
    fn trailers_must_follow_data_and_precede_clean_end() {
        let mut events = StreamEvents {
            schema_version: STREAM_EVENTS_SCHEMA_VERSION,
            flows: vec![FlowStreamEvents {
                flow_id: "flow".into(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: vec![
                    StreamEvent {
                        delta_ns: 0,
                        event: StreamEventKind::Data {
                            offset: 0,
                            length: 1,
                        },
                    },
                    StreamEvent {
                        delta_ns: 1,
                        event: StreamEventKind::Trailers { fields: Vec::new() },
                    },
                    StreamEvent {
                        delta_ns: 2,
                        event: StreamEventKind::End,
                    },
                ],
            }],
        };
        assert!(events.validate().is_ok());
        events.flows[0].response.insert(
            2,
            StreamEvent {
                delta_ns: 2,
                event: StreamEventKind::Data {
                    offset: 1,
                    length: 1,
                },
            },
        );
        assert!(events.validate().is_err());
    }
}
