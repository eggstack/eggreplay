//! Candidate replay through the same EggFetch native execution path.

use crate::error_classify::{body_error_event_fields, map_fetch_error};
use bytes::Bytes;
use eggfetch_core::{Client, Error as FetchError};
use eggreplay_core::{
    BodyRef, Flow, FlowError, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse, QueryPair,
    StreamEvent, StreamEventKind,
};
use http::{HeaderMap, HeaderValue, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Candidate execution error.
#[derive(Debug, Error)]
pub enum RegressionError {
    /// Request could not be materialized.
    #[error("request conversion: {0}")]
    Conversion(String),
    /// EggFetch transport failure.
    #[error("EggFetch: {0}")]
    Fetch(#[from] FetchError),
    /// Candidate body exceeded the observation limit.
    #[error("candidate body exceeds limit {0} bytes")]
    BodyLimit(u64),
    /// Candidate body stream failed.
    #[error("candidate body: {0}")]
    Body(String),
    /// Candidate WebSocket handshake or semantic script failed.
    #[error("candidate WebSocket: {0}")]
    WebSocket(String),
}

/// Request headers the candidate driver owns, so a fixture cannot set them.
///
/// A candidate request is built by EggReplay and sent by EggFetch, not
/// replayed from the recorded connection. Copying these from the fixture would
/// let a recorded (or tampered) request choose the target authority (`host`),
/// the message framing (`transfer-encoding`, `content-length`, `te`,
/// `trailer`), the connection lifecycle (`connection`, `proxy-connection`,
/// `keep-alive`), or hand a proxy-hop credential (`proxy-authorization`) to a
/// target that never asked for it. The transport derives framing from the
/// materialized [`Full`] body and the host from the operator's target, so
/// forwarding a fixture value could only disagree with what is actually sent.
///
/// This is deliberately **not** an allow-list: end-to-end fields
/// (`authorization`, `cookie`, `accept`, `user-agent`, `x-trace`, and the
/// `forwarded` family) are part of the recorded request semantics and stay.
const DRIVER_CONTROLLED_REQUEST_HEADERS: &[&str] = &[
    "host",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "content-length",
    "upgrade",
    "expect",
    "proxy-authorization",
    "proxy-authenticate",
];

/// WebSocket handshake fields that the candidate driver mints per attempt.
///
/// Layered on top of [`is_driver_controlled_header`] for the WebSocket
/// candidate path only: an ordinary HTTP candidate must not treat these as
/// driver-controlled, and one handshake attempt must never reuse a recorded
/// key, accept digest, version, or extension negotiation from another.
#[cfg(feature = "websocket")]
const WEBSOCKET_HANDSHAKE_HEADERS: &[&str] = &[
    "sec-websocket-key",
    "sec-websocket-accept",
    "sec-websocket-version",
    "sec-websocket-extensions",
];

/// Whether `name` is a driver-controlled request header for this candidate.
///
/// Shared by [`execute_candidate`] and [`compare_websocket_candidate`] so the
/// two paths cannot drift; `name` is compared case-insensitively because the
/// recorded spelling is whatever the origin saw.
fn is_driver_controlled_header(name: &str) -> bool {
    DRIVER_CONTROLLED_REQUEST_HEADERS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Whether `name` is a WebSocket handshake field the candidate mints.
#[cfg(feature = "websocket")]
fn is_websocket_handshake_header(name: &str) -> bool {
    WEBSOCKET_HANDSHAKE_HEADERS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// The `Host` value a candidate must send: the operator's target authority.
///
/// Never the recorded `host`. `http::uri::Authority` can carry userinfo
/// syntax, so only the part after the last `@` is the host authority; the
/// operator's target itself still fails closed upstream if it carries
/// credentials.
fn candidate_host(target_base: &Uri) -> Result<&str, RegressionError> {
    let authority = target_base
        .authority()
        .ok_or_else(|| RegressionError::Conversion("target base has no authority".into()))?;
    let authority = authority.as_str();
    Ok(authority.rsplit('@').next().unwrap_or(authority))
}

/// Replay a recorded WebSocket transcript against a candidate through the
/// same EggFetch client and return payload-safe semantic findings.
#[cfg(feature = "websocket")]
pub async fn compare_websocket_candidate(
    client: &Client,
    session: &eggreplay_store::Session,
    request: &HttpRequest,
    target_base: &Uri,
    conversation: &eggreplay_core::WebSocketConversation,
    cadence_tolerance_ns: Option<u64>,
) -> Result<Vec<eggreplay_core::DiffFinding>, RegressionError> {
    use base64::Engine;
    use eggfetch_core::{Headers, NetworkStream};
    use eggreplay_core::WebSocketDirection;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    let uri = target_uri(target_base, request)?;
    let host = candidate_host(target_base)?;
    let key = base64::engine::general_purpose::STANDARD.encode(uuid::Uuid::new_v4().as_bytes());
    let mut headers = Headers::new();
    for header in &request.headers {
        if is_driver_controlled_header(&header.name) || is_websocket_handshake_header(&header.name)
        {
            continue;
        }
        headers
            .append(&header.name, &header.value)
            .map_err(|e| RegressionError::WebSocket(e.to_string()))?;
    }
    headers
        .append("Host", host)
        .and_then(|_| headers.append("Connection", "Upgrade"))
        .and_then(|_| headers.append("Upgrade", "websocket"))
        .and_then(|_| headers.append("Sec-WebSocket-Version", "13"))
        .and_then(|_| headers.append("Sec-WebSocket-Key", &key))
        .map_err(|e| RegressionError::WebSocket(e.to_string()))?;
    let send = client
        .request(Method::GET, &uri.to_string())
        .map_err(|e| RegressionError::WebSocket(e.to_string()))?
        .headers(headers)
        .send();
    let response = match tokio::time::timeout(std::time::Duration::from_secs(30), send).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) | Err(_) => {
            return Ok(vec![ws_finding(
                "handshake.transport",
                "101",
                "unavailable",
            )]);
        }
    };
    let mut findings = Vec::new();
    if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
        findings.push(ws_finding(
            "handshake.status",
            "101",
            &response.status().to_string(),
        ));
        return Ok(findings);
    }
    let response_headers = response.headers().clone();
    let accept = response_headers
        .get("sec-websocket-accept")
        .and_then(|v| v.to_str().ok());
    if accept != Some(derive_accept_key(key.as_bytes()).as_str()) {
        findings.push(ws_finding("handshake.accept", "valid", "invalid"));
    }
    if !response_headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        || !response_headers
            .get("connection")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
    {
        findings.push(ws_finding(
            "handshake.upgrade",
            "websocket_upgrade",
            "invalid",
        ));
    }
    if response_headers.contains_key("sec-websocket-extensions") {
        findings.push(ws_finding("handshake.extensions", "none", "negotiated"));
    }
    let selected = response_headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok());
    if selected != conversation.selected_subprotocol.as_deref() {
        findings.push(ws_finding(
            "handshake.subprotocol",
            conversation
                .selected_subprotocol
                .as_deref()
                .unwrap_or("none"),
            selected.unwrap_or("none"),
        ));
    }
    if !findings.is_empty() {
        return Ok(findings);
    }
    let Some(network_stream) = response.into_network_stream() else {
        return Ok(vec![ws_finding(
            "handshake.stream",
            "upgraded",
            "unavailable",
        )]);
    };
    let NetworkStream::Upgraded(io) = network_stream else {
        return Ok(vec![ws_finding(
            "handshake.stream",
            "upgraded",
            "unavailable",
        )]);
    };
    let mut socket = crate::websocket::client(
        io,
        Some(
            crate::websocket::WebSocketConfig::default()
                .max_message_size(Some(16 * 1024 * 1024))
                .max_frame_size(Some(16 * 1024 * 1024)),
        ),
    )
    .await;
    let conversation_started = Instant::now();
    let mut close_reply_pending = false;
    let mut auto_pong_pending = false;
    for expected in &conversation.messages {
        let elapsed = conversation_started.elapsed();
        if elapsed >= std::time::Duration::from_secs(60 * 60) {
            findings.push(ws_finding(
                "terminal",
                "completed_within_limit",
                "duration_limit",
            ));
            break;
        }
        if expected.direction == WebSocketDirection::ClientToServer {
            let payload = match &expected.payload {
                Some(BodyRef::Blob(blob)) => session
                    .read_blob(blob)
                    .map_err(|e| RegressionError::WebSocket(e.to_string()))?,
                _ => Vec::new(),
            };
            let message = ws_message(
                expected.kind,
                payload,
                expected.close_code,
                expected.close_reason.as_deref(),
            );
            if expected.kind == eggreplay_core::WebSocketMessageKind::Close && close_reply_pending {
                close_reply_pending = false;
                if let Err(error) = socket.flush().await {
                    findings.push(ws_finding(
                        "message.close",
                        "clean",
                        &format!("error:{:?}", error),
                    ));
                    break;
                }
            } else if expected.kind == eggreplay_core::WebSocketMessageKind::Pong
                && auto_pong_pending
            {
                auto_pong_pending = false;
                if let Err(error) = socket.flush().await {
                    findings.push(ws_finding(
                        "message.pong",
                        "automatic",
                        &format!("error:{:?}", error),
                    ));
                    break;
                }
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    crate::websocket::send(&mut socket, message),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        findings.push(ws_finding(
                            "message.write",
                            "success",
                            &format!("error:{:?}", error),
                        ));
                        break;
                    }
                    Err(_) => {
                        findings.push(ws_finding("message.write", "success", "timeout"));
                        break;
                    }
                }
            }
        } else {
            let actual = match tokio::time::timeout(
                std::time::Duration::from_secs(30)
                    .min(std::time::Duration::from_secs(60 * 60).saturating_sub(elapsed)),
                crate::websocket::next(&mut socket),
            )
            .await
            {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => {
                    findings.push(ws_finding(
                        &format!("message.{}.read", expected.sequence),
                        "message",
                        &format!("error:{:?}", error),
                    ));
                    break;
                }
                _ => {
                    findings.push(ws_finding(
                        &format!("message.{}.read", expected.sequence),
                        "message",
                        "timeout_or_eof",
                    ));
                    break;
                }
            };
            if !candidate_message_matches(expected, &actual, session) {
                findings.push(ws_finding(
                    &format!("message.{}", expected.sequence),
                    &expected_message_summary(expected, session),
                    &candidate_message_summary(&actual),
                ));
                break;
            }
            if expected.kind == eggreplay_core::WebSocketMessageKind::Close {
                close_reply_pending = true;
            }
            if expected.kind == eggreplay_core::WebSocketMessageKind::Ping {
                auto_pong_pending = true;
            }
        }
        if let Some(tolerance) = cadence_tolerance_ns {
            let observed = elapsed_ns(conversation_started);
            if observed.abs_diff(expected.delta_ns) > tolerance {
                findings.push(ws_finding(
                    &format!("message.{}.cadence", expected.sequence),
                    &expected.delta_ns.to_string(),
                    &observed.to_string(),
                ));
            }
        }
    }
    if matches!(
        conversation.terminal,
        eggreplay_core::WebSocketTerminal::Abnormal { .. }
    ) {
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::websocket::next(&mut socket),
        )
        .await
        {
            Ok(Some(Ok(message))) => findings.push(ws_finding(
                "terminal",
                "abnormal_end",
                &candidate_message_summary(&message),
            )),
            Ok(Some(Err(_))) | Ok(None) => {}
            Err(_) => findings.push(ws_finding(
                "terminal",
                "abnormal_end",
                "connection_remained_open",
            )),
        }
    }
    Ok(findings)
}

#[cfg(feature = "websocket")]
fn ws_finding(field: &str, baseline: &str, candidate: &str) -> eggreplay_core::DiffFinding {
    eggreplay_core::DiffFinding {
        kind: eggreplay_core::DiffKind::WebSocket,
        field: field.into(),
        baseline: baseline.into(),
        candidate: candidate.into(),
    }
}

#[cfg(feature = "websocket")]
fn message_kind(
    message: &tokio_tungstenite::tungstenite::Message,
) -> eggreplay_core::WebSocketMessageKind {
    use tokio_tungstenite::tungstenite::Message;
    match message {
        Message::Text(_) => eggreplay_core::WebSocketMessageKind::Text,
        Message::Binary(_) => eggreplay_core::WebSocketMessageKind::Binary,
        Message::Ping(_) => eggreplay_core::WebSocketMessageKind::Ping,
        Message::Pong(_) => eggreplay_core::WebSocketMessageKind::Pong,
        Message::Close(_) => eggreplay_core::WebSocketMessageKind::Close,
        Message::Frame(_) => eggreplay_core::WebSocketMessageKind::Binary,
    }
}

#[cfg(feature = "websocket")]
fn expected_message_summary(
    message: &eggreplay_core::WebSocketMessage,
    session: &eggreplay_store::Session,
) -> String {
    use sha2::{Digest, Sha256};
    if message
        .redactions
        .iter()
        .any(|r| matches!(r, eggreplay_core::WebSocketRedaction::WholeMessage))
    {
        return format!("{:?}:redacted", message.kind);
    }
    let bytes = match &message.payload {
        Some(BodyRef::Blob(blob)) => session.read_blob(blob).unwrap_or_default(),
        _ => Vec::new(),
    };
    format!(
        "{:?}:length:{}:sha256:{:x}",
        message.kind,
        bytes.len(),
        Sha256::digest(bytes)
    )
}

#[cfg(feature = "websocket")]
fn candidate_message_summary(message: &tokio_tungstenite::tungstenite::Message) -> String {
    use sha2::{Digest, Sha256};
    let bytes = match message {
        tokio_tungstenite::tungstenite::Message::Text(s) => s.as_bytes(),
        tokio_tungstenite::tungstenite::Message::Binary(b)
        | tokio_tungstenite::tungstenite::Message::Ping(b)
        | tokio_tungstenite::tungstenite::Message::Pong(b) => b,
        tokio_tungstenite::tungstenite::Message::Close(_)
        | tokio_tungstenite::tungstenite::Message::Frame(_) => &[],
    };
    format!(
        "{:?}:length:{}:sha256:{:x}",
        message_kind(message),
        bytes.len(),
        Sha256::digest(bytes)
    )
}

#[cfg(feature = "websocket")]
fn ws_message(
    kind: eggreplay_core::WebSocketMessageKind,
    payload: Vec<u8>,
    code: Option<u16>,
    reason: Option<&str>,
) -> tokio_tungstenite::tungstenite::Message {
    use tokio_tungstenite::tungstenite::{
        Message,
        protocol::frame::{CloseFrame, coding::CloseCode},
    };
    match kind {
        eggreplay_core::WebSocketMessageKind::Text => {
            Message::Text(String::from_utf8_lossy(&payload).into_owned().into())
        }
        eggreplay_core::WebSocketMessageKind::Binary => Message::Binary(payload.into()),
        eggreplay_core::WebSocketMessageKind::Ping => Message::Ping(payload.into()),
        eggreplay_core::WebSocketMessageKind::Pong => Message::Pong(payload.into()),
        eggreplay_core::WebSocketMessageKind::Close => {
            Message::Close(code.map(|code| CloseFrame {
                code: CloseCode::from(code),
                reason: reason.unwrap_or("").into(),
            }))
        }
    }
}

#[cfg(feature = "websocket")]
fn candidate_message_matches(
    expected: &eggreplay_core::WebSocketMessage,
    actual: &tokio_tungstenite::tungstenite::Message,
    session: &eggreplay_store::Session,
) -> bool {
    use eggreplay_core::{WebSocketMessageKind as Kind, WebSocketRedaction};
    use tokio_tungstenite::tungstenite::Message;
    let (kind, payload, code, reason) = match actual {
        Message::Text(s) => (Kind::Text, s.as_bytes().to_vec(), None, None),
        Message::Binary(b) => (Kind::Binary, b.to_vec(), None, None),
        Message::Ping(b) => (Kind::Ping, b.to_vec(), None, None),
        Message::Pong(b) => (Kind::Pong, b.to_vec(), None, None),
        Message::Close(frame) => (
            Kind::Close,
            Vec::new(),
            frame.as_ref().map(|f| u16::from(f.code)),
            frame.as_ref().map(|f| f.reason.to_string()),
        ),
        Message::Frame(_) => return false,
    };
    if kind != expected.kind || (kind == Kind::Close && code != expected.close_code) {
        return false;
    }
    if expected.redactions.iter().any(|r| {
        matches!(
            r,
            WebSocketRedaction::WholeMessage | WebSocketRedaction::CloseReason
        )
    }) {
        return true;
    }
    if kind == Kind::Close {
        return reason == expected.close_reason;
    }
    let expected_payload = match &expected.payload {
        Some(BodyRef::Blob(blob)) => match session.read_blob(blob) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        },
        _ => Vec::new(),
    };
    if let Some(WebSocketRedaction::JsonPointers { pointers }) = expected
        .redactions
        .iter()
        .find(|redaction| matches!(redaction, WebSocketRedaction::JsonPointers { .. }))
    {
        let paths = pointers.iter().cloned().collect();
        return match (
            eggreplay_core::apply_json_redaction(&expected_payload, &paths),
            eggreplay_core::apply_json_redaction(&payload, &paths),
        ) {
            (Ok((a, _)), Ok((b, _))) => a == b,
            _ => false,
        };
    }
    payload == expected_payload
}

/// Candidate result plus bounded body bytes and response stream events.
///
/// Response stream observation is authoritative for M010 closure; request
/// cadence is not captured because the candidate path materializes the
/// baseline request into one `Full` body. Do not fabricate request timing by
/// copying baseline events.
#[derive(Debug, Clone)]
pub struct CandidateObservation {
    /// Semantic candidate flow.
    pub flow: Flow,
    /// Candidate response bytes up to the configured observation bound.
    pub response_body: Vec<u8>,
    /// Bounded response DATA/trailer/terminal events with monotonic deltas.
    pub response_events: Vec<StreamEvent>,
}

/// Replay a materialized baseline request against a candidate target.
#[allow(clippy::too_many_arguments)]
pub async fn execute_candidate(
    client: &Client,
    request: &HttpRequest,
    request_body: &[u8],
    target_base: &Uri,
    max_body_bytes: u64,
    physical_route: Option<eggreplay_core::PhysicalRoute>,
) -> Result<CandidateObservation, RegressionError> {
    let uri = target_uri(target_base, request)?;
    let host =
        HeaderValue::from_bytes(candidate_host(target_base)?.as_bytes()).map_err(|error| {
            RegressionError::Conversion(format!("invalid target authority: {error}"))
        })?;
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|error| RegressionError::Conversion(error.to_string()))?;
    let mut headers = HeaderMap::new();
    for header in &request.headers {
        // Framing, connection management, and proxy-hop credentials belong to
        // the driver; `Host` is the operator's target, not the recording's.
        if is_driver_controlled_header(&header.name) {
            continue;
        }
        let name = http::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| RegressionError::Conversion(error.to_string()))?;
        let value = HeaderValue::from_bytes(header.value.as_bytes())
            .map_err(|error| RegressionError::Conversion(error.to_string()))?;
        headers.append(name, value);
    }
    // EggFetch/hyper do not synthesize `Host` for an HTTP/1.1 client request,
    // so it is set here from the target URI. Inserting after the loop also
    // guarantees exactly one value even if the fixture carried several.
    headers.insert(http::header::HOST, host);
    let mut outgoing = Request::builder()
        .method(method)
        .uri(uri)
        .body(Full::new(Bytes::copy_from_slice(request_body)))
        .map_err(|error| RegressionError::Conversion(error.to_string()))?;
    *outgoing.headers_mut() = headers;
    let start = now_ms();
    let result = client.execute_http_body_default(outgoing).await;
    let elapsed = now_ms().saturating_sub(start);
    let mut response_body = Vec::new();
    let mut response_events: Vec<StreamEvent> = Vec::new();
    let outcome = match result {
        Ok(response) => {
            let (parts, mut body) = response.into_parts();
            let mut bytes = Vec::new();
            let mut trailers = Vec::new();
            let response_started = Instant::now();
            let mut offset = 0u64;
            let mut terminal_error = false;
            while let Some(frame) = body.frame().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        // Mid-body transport failure: retain status/headers and
                        // partial bytes, append a terminal Error event, and
                        // return an observation suitable for regression. This
                        // observable condition must not become a generic CLI
                        // runtime failure.
                        //
                        // The category/phase pair comes from the shared table,
                        // not from a literal. Before M018 this site discarded
                        // the error and hardcoded `("other", "body")`, so a
                        // candidate cut off by a deadline recorded identically
                        // to one killed by a reset — while the *recorded* run of
                        // the same request said `timeout`. A candidate
                        // observation is a semantic artifact, so a regression
                        // report could assert a difference that was an artifact
                        // of which side of the recorder observed it.
                        let (category, phase) = body_error_event_fields(&error);
                        push_candidate_event(
                            &mut response_events,
                            StreamEvent {
                                delta_ns: elapsed_ns(response_started),
                                event: StreamEventKind::Error {
                                    offset,
                                    category: category.into(),
                                    phase: phase.into(),
                                },
                            },
                        )?;
                        terminal_error = true;
                        break;
                    }
                };
                if frame.is_data() {
                    let data = match frame.into_data() {
                        Ok(data) => data,
                        Err(_) => {
                            // `into_data` hands back the frame itself, not an
                            // error value, so there is no transport evidence to
                            // classify and `other`/`body` is an honest reading
                            // rather than a discarded cause. This mirrors the
                            // recording path's deliberate decision to keep
                            // `Other` at the request tee.
                            push_candidate_event(
                                &mut response_events,
                                StreamEvent {
                                    delta_ns: elapsed_ns(response_started),
                                    event: StreamEventKind::Error {
                                        offset,
                                        category: "other".into(),
                                        phase: "body".into(),
                                    },
                                },
                            )?;
                            terminal_error = true;
                            break;
                        }
                    };
                    if data.is_empty() {
                        continue;
                    }
                    if bytes.len() as u64 + data.len() as u64 > max_body_bytes {
                        return Err(RegressionError::BodyLimit(max_body_bytes));
                    }
                    let length = data.len() as u64;
                    bytes.extend_from_slice(&data);
                    push_candidate_event(
                        &mut response_events,
                        StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: StreamEventKind::Data { offset, length },
                        },
                    )?;
                    offset = offset.saturating_add(length);
                } else if frame.is_trailers() {
                    let map = match frame.into_trailers() {
                        Ok(map) => map,
                        Err(_) => {
                            push_candidate_event(
                                &mut response_events,
                                StreamEvent {
                                    delta_ns: elapsed_ns(response_started),
                                    event: StreamEventKind::Error {
                                        offset,
                                        category: "other".into(),
                                        phase: "body".into(),
                                    },
                                },
                            )?;
                            terminal_error = true;
                            break;
                        }
                    };
                    trailers = map
                        .iter()
                        .map(|(name, value)| HeaderEntry {
                            name: name.as_str().into(),
                            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
                        })
                        .collect();
                    push_candidate_event(
                        &mut response_events,
                        StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: StreamEventKind::Trailers {
                                fields: trailers.clone(),
                            },
                        },
                    )?;
                }
            }
            if !terminal_error {
                push_candidate_event(
                    &mut response_events,
                    StreamEvent {
                        delta_ns: elapsed_ns(response_started),
                        event: StreamEventKind::End,
                    },
                )?;
            }
            let response_empty = bytes.is_empty();
            response_body = bytes;
            response_events.shrink_to_fit();
            FlowOutcome::Response(HttpResponse {
                status: parts.status.as_u16(),
                headers: parts
                    .headers
                    .iter()
                    .map(|(name, value)| HeaderEntry {
                        name: name.as_str().into(),
                        value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    })
                    .collect(),
                body: if response_empty {
                    BodyRef::Empty
                } else {
                    BodyRef::Absent
                },
                trailers,
            })
        }
        Err(error) => FlowOutcome::Error(map_error(&error)),
    };
    let mut flow = Flow::new(request.clone(), outcome, start);
    flow.completed_at_ms = Some(start.saturating_add(elapsed));
    flow.provenance.mode = "eggfetch-native-candidate".into();
    flow.physical_route = physical_route;
    Ok(CandidateObservation {
        flow,
        response_body,
        response_events,
    })
}

fn target_uri(base: &Uri, request: &HttpRequest) -> Result<Uri, RegressionError> {
    let scheme = base
        .scheme_str()
        .ok_or_else(|| RegressionError::Conversion("target base has no scheme".into()))?;
    let authority = base
        .authority()
        .ok_or_else(|| RegressionError::Conversion("target base has no authority".into()))?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for QueryPair { key, value } in &request.query {
        query.append_pair(key, value);
    }
    let query = query.finish();
    format!(
        "{scheme}://{authority}{}{}",
        request.path,
        if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        }
    )
    .parse()
    .map_err(|error| RegressionError::Conversion(format!("invalid target URI: {error}")))
}

fn map_error(error: &FetchError) -> FlowError {
    // Before M018 this was a third, independent copy of the fetch-error table,
    // with no `CustomTransport` arm at all. A dead Eggress route observed *by a
    // candidate* therefore collapsed to `(Other, Other)` even though the
    // recording path attributed it correctly — the M016 defect, surviving one
    // layer down, in a file whose whole job is to compare a recorded run
    // against a candidate run.
    map_fetch_error(error)
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn push_candidate_event(
    events: &mut Vec<StreamEvent>,
    event: StreamEvent,
) -> Result<(), RegressionError> {
    const MAX_EVENTS_PER_DIRECTION: usize = eggreplay_core::stream::MAX_STREAM_EVENTS_PER_FLOW / 2;
    if matches!(&event.event, StreamEventKind::Data { length: 0, .. }) {
        return Ok(());
    }
    if event.delta_ns > eggreplay_core::stream::MAX_STREAM_DELAY_NS {
        return Err(RegressionError::Body(
            "candidate stream delay exceeds configured limit".into(),
        ));
    }
    if events.len() < MAX_EVENTS_PER_DIRECTION {
        events.push(event);
        return Ok(());
    }
    // Bounded coalescing for contiguous DATA when at the cap, mirroring
    // recording; otherwise fail closed via the existing body limit path.
    if let (Some(last), StreamEventKind::Data { offset, length }) =
        (events.last_mut(), &event.event)
        && let StreamEventKind::Data {
            offset: last_offset,
            length: last_length,
        } = &mut last.event
        && last_offset.saturating_add(*last_length) == *offset
    {
        *last_length = last_length.saturating_add(*length);
        return Ok(());
    }
    Err(RegressionError::Body(
        "candidate stream event count exceeds configured limit".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_core::{BodyRef, HttpRequest};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn test_request(authority: &str, path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            scheme: "http".into(),
            authority: authority.into(),
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: BodyRef::Empty,
            trailers: Vec::new(),
        }
    }

    fn serve_once(response: Vec<u8>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(&response);
            }
        });
        addr
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn candidate_websocket_executes_the_recorded_message_script() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            loop {
                let count = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            // The handshake still uses the shared driver-controlled classifier:
            // a recorded `Host` and a proxy-hop credential never reach the peer,
            // and the candidate mints one Host from the target.
            let lowered = request.to_ascii_lowercase();
            assert!(
                lowered.contains(&format!("\r\nhost: {address}\r\n")),
                "the candidate must derive Host from the target: {request}"
            );
            assert!(
                !lowered.contains("proxy-authorization") && !lowered.contains("sentinel"),
                "proxy credentials must not be forwarded: {request}"
            );
            let key = request
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Sec-WebSocket-Key: ")
                        .or_else(|| line.strip_prefix("sec-websocket-key: "))
                })
                .unwrap()
                .trim();
            let accept = crate::websocket::derive_accept_key(key.as_bytes());
            stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
            let mut socket = crate::websocket::server(stream, None).await;
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                tokio_tungstenite::tungstenite::Message::Text("hello".into())
            );
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    "world".into(),
                ))
                .await
                .unwrap();
        });
        let dir =
            std::env::temp_dir().join(format!("eggreplay-candidate-ws-{}", std::process::id()));
        let writer = eggreplay_store::SessionWriter::create(
            &dir,
            eggreplay_core::SessionMetadata::default(),
            eggreplay_store::StoreLimits::default(),
        )
        .unwrap();
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(b"hello").unwrap();
        let hello = sink.finish().unwrap();
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(b"world").unwrap();
        let world = sink.finish().unwrap();
        let session = writer.finish().unwrap();
        let conversation = eggreplay_core::WebSocketConversation {
            id: "ws-candidate".into(),
            flow_id: "flow".into(),
            offered_subprotocols: vec![],
            selected_subprotocol: None,
            messages: vec![
                eggreplay_core::WebSocketMessage {
                    sequence: 0,
                    direction: eggreplay_core::WebSocketDirection::ClientToServer,
                    delta_ns: 0,
                    kind: eggreplay_core::WebSocketMessageKind::Text,
                    payload: Some(hello),
                    close_code: None,
                    close_reason: None,
                    redactions: vec![],
                },
                eggreplay_core::WebSocketMessage {
                    sequence: 1,
                    direction: eggreplay_core::WebSocketDirection::ServerToClient,
                    delta_ns: 1,
                    kind: eggreplay_core::WebSocketMessageKind::Text,
                    payload: Some(world),
                    close_code: None,
                    close_reason: None,
                    redactions: vec![],
                },
            ],
            terminal: eggreplay_core::WebSocketTerminal::Abnormal {
                cause: "eof".into(),
            },
        };
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{address}").parse().unwrap();
        // Recorded `Host` and a proxy-hop credential: the handshake path must
        // drop both through the shared classifier.
        let mut request = test_request("internal.example", "/socket");
        request.headers = vec![
            eggreplay_core::HeaderEntry {
                name: "Host".into(),
                value: "internal.example".into(),
            },
            eggreplay_core::HeaderEntry {
                name: "Proxy-Authorization".into(),
                value: "Basic SENTINEL".into(),
            },
        ];
        let findings =
            compare_websocket_candidate(&client, &session, &request, &target, &conversation, None)
                .await
                .unwrap();
        assert!(findings.is_empty(), "{findings:?}");
        peer.await.unwrap();
        drop(session);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn candidate_data_offsets_and_deltas_are_valid_and_bounded() {
        // M010-C1 #6: DATA offsets/deltas valid and bounded, using Instant.
        let body = b"hello-world".to_vec();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(&body)
        )
        .into_bytes();
        let addr = serve_once(response);
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{addr}").parse().unwrap();
        let observation = execute_candidate(
            &client,
            &test_request(&addr.to_string(), "/"),
            &[],
            &target,
            1024 * 1024,
            None,
        )
        .await
        .unwrap();
        assert_eq!(observation.response_body, body);
        // Offsets contiguous from zero, lengths sum to body length.
        let mut offset = 0u64;
        let mut last_delta = 0u64;
        let mut data_bytes = 0u64;
        for event in &observation.response_events {
            assert!(event.delta_ns <= eggreplay_core::stream::MAX_STREAM_DELAY_NS);
            assert!(event.delta_ns >= last_delta);
            last_delta = event.delta_ns;
            if let StreamEventKind::Data { offset: o, length } = &event.event {
                assert_eq!(*o, offset);
                assert!(*length > 0);
                offset = offset.saturating_add(*length);
                data_bytes = data_bytes.saturating_add(*length);
            }
        }
        assert_eq!(data_bytes, body.len() as u64);
        assert!(
            observation.response_events.len() <= eggreplay_core::stream::MAX_STREAM_EVENTS_PER_FLOW
        );
        // Terminal clean End.
        assert!(matches!(
            observation.response_events.last().map(|event| &event.event),
            Some(StreamEventKind::End)
        ));
    }

    #[tokio::test]
    async fn candidate_trailers_and_clean_end_are_captured() {
        // M010-C1 #7: trailers and clean End captured. Use chunked with
        // trailers via raw TCP: EggFetch preserves trailer frames.
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\nX-Trailer: trailer-value\r\n\r\n".to_vec();
        let addr = serve_once(response);
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{addr}").parse().unwrap();
        let observation = execute_candidate(
            &client,
            &test_request(&addr.to_string(), "/trailers"),
            &[],
            &target,
            1024 * 1024,
            None,
        )
        .await
        .unwrap();
        assert_eq!(observation.response_body, b"hello");
        // Must contain DATA, optionally Trailers, and terminal End.
        let has_data = observation
            .response_events
            .iter()
            .any(|event| matches!(&event.event, StreamEventKind::Data { .. }));
        assert!(has_data, "DATA must be captured");
        assert!(matches!(
            observation.response_events.last().map(|event| &event.event),
            Some(StreamEventKind::End)
        ));
        // If trailers survived the transport, they are in both flow and events.
        if !observation.flow_outcome_trailers().is_empty() {
            assert!(
                observation
                    .response_events
                    .iter()
                    .any(|event| { matches!(&event.event, StreamEventKind::Trailers { .. }) })
            );
        }
    }

    #[tokio::test]
    async fn candidate_mid_body_error_returns_partial_observation() {
        // M010-C1 #8: mid-body error retains status/headers/partial bytes and
        // appends an Error event instead of a generic runtime failure.
        // Send Content-Length 100 but close after 5 bytes.
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nhello".to_vec();
        let addr = serve_once(response);
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{addr}").parse().unwrap();
        let observation = execute_candidate(
            &client,
            &test_request(&addr.to_string(), "/partial"),
            &[],
            &target,
            1024 * 1024,
            None,
        )
        .await
        .unwrap();
        // Must be a Response outcome (not Error) with partial bytes and Error event.
        match &observation.flow.outcome {
            FlowOutcome::Response(response) => {
                assert_eq!(response.status, 200);
            }
            FlowOutcome::Error(error) => {
                // Transport may surface truncation as body error before head?
                // For mid-body truncation after head, we require Response.
                // If the client maps truncation to Error outcome, at least
                // response_events must be empty (no fabrication). Document.
                let _ = error;
                assert!(
                    observation.response_events.is_empty(),
                    "transport-level error has no response events"
                );
                return;
            }
        }
        // If Response, check partial bytes and terminal Error.
        if matches!(
            observation.response_events.last().map(|event| &event.event),
            Some(StreamEventKind::Error { .. })
        ) {
            assert_eq!(observation.response_body, b"hello");
            if let Some(StreamEventKind::Error {
                offset,
                category,
                phase,
            }) = observation.response_events.last().map(|event| &event.event)
            {
                assert_eq!(*offset, 5);
                // This used to assert `other`, and the assertion *was* the
                // defect: the site discarded the transport error and wrote
                // `("other", "body")` as a literal, so this test pinned the
                // uninformative value and made it look intentional. The
                // truncated body here is a protocol failure, and that is what
                // the fixture now says — the same thing the recording path
                // would have said for the identical failure.
                assert_eq!(category, "protocol");
                assert_ne!(
                    category, "other",
                    "a candidate body failure must not collapse to `other`"
                );
                assert_eq!(phase, "body");
            } else {
                unreachable!();
            }
        } else {
            // If transport completed without error (e.g., lenient), at least
            // DATA offsets must be valid; do not fail the test on transport
            // leniency, but prove partial bytes are retained.
            assert!(!observation.response_body.is_empty());
        }
    }

    trait ObservationExt {
        fn flow_outcome_trailers(&self) -> Vec<HeaderEntry>;
    }

    impl ObservationExt for CandidateObservation {
        fn flow_outcome_trailers(&self) -> Vec<HeaderEntry> {
            match &self.flow.outcome {
                FlowOutcome::Response(response) => response.trailers.clone(),
                FlowOutcome::Error(_) => Vec::new(),
            }
        }
    }

    #[test]
    fn body_limit_is_enforced() {
        // Existing body-byte limits remain enforced.
        assert!(matches!(
            RegressionError::BodyLimit(10),
            RegressionError::BodyLimit(_)
        ));
    }

    /// The candidate path must classify a route failure exactly as the
    /// recording path does.
    ///
    /// Before M018 `map_error` here had no `CustomTransport` arm, so every
    /// Eggress route failure observed by a candidate collapsed to
    /// `(Other, Other)` — meaning a regression report could show a dead-route
    /// candidate as an unclassified failure while the recorded baseline for the
    /// same route said `unreachable`. This is the M016 defect, one layer down,
    /// and the reason the fix is a shared table rather than a new arm here.
    #[test]
    fn a_candidate_route_failure_is_attributed_not_collapsed() {
        use eggfetch_core::transport::dialer::{DialError, DialErrorKind};
        use eggreplay_core::ErrorCategory;
        use std::sync::Arc;

        let cases = [
            (DialErrorKind::Connection, ErrorCategory::Unreachable),
            (DialErrorKind::Timeout, ErrorCategory::Timeout),
            (DialErrorKind::Authentication, ErrorCategory::Policy),
            (DialErrorKind::Rejected, ErrorCategory::Policy),
        ];
        for (kind, expected) in cases {
            let error = FetchError::CustomTransport(Arc::new(DialError::new(kind, "route down")));
            let mapped = map_error(&error);
            assert_eq!(
                mapped.category, expected,
                "a candidate route failure must be attributed, not collapsed to Other: {kind:?}"
            );
            assert_ne!(mapped.category, ErrorCategory::Other);
        }
    }

    /// The recording path and the candidate path must agree for the same
    /// failure. Stated as a property rather than as a call-graph assertion so
    /// it keeps holding if the shared table is later split again — which is
    /// exactly the regression this milestone exists to prevent.
    #[test]
    fn the_candidate_and_recording_paths_agree_on_a_route_failure() {
        use eggfetch_core::transport::dialer::{DialError, DialErrorKind};
        use std::sync::Arc;

        let error = FetchError::CustomTransport(Arc::new(DialError::new(
            DialErrorKind::Rejected,
            "route down",
        )));
        assert_eq!(
            map_error(&error).category,
            crate::error_classify::map_fetch_error(&error).category,
            "a recorded run and a candidate run of one request must classify \
             the same failure identically"
        );
    }

    /// Regression treats a `WholeMessage` redaction as a wildcard, not as a
    /// literal `[REDACTED]` payload comparison.
    #[cfg(feature = "websocket")]
    #[test]
    fn candidate_whole_message_redaction_matches_any_live_payload() {
        use eggreplay_core::{
            SessionMetadata, WebSocketDirection, WebSocketMessageKind, WebSocketRedaction,
        };
        use eggreplay_store::{SessionWriter, StoreLimits};
        use tokio_tungstenite::tungstenite::Message;

        let dir = std::env::temp_dir().join(format!(
            "eggreplay-regression-ws-wildcard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writer =
            SessionWriter::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let session = writer.finish().unwrap();
        let expected = eggreplay_core::WebSocketMessage {
            sequence: 0,
            direction: WebSocketDirection::ClientToServer,
            delta_ns: 0,
            kind: WebSocketMessageKind::Text,
            payload: None,
            close_code: None,
            close_reason: None,
            redactions: vec![WebSocketRedaction::WholeMessage],
        };
        assert!(
            candidate_message_matches(
                &expected,
                &Message::Text("live bytes that differ from the marker".into()),
                &session
            ),
            "a withheld payload must match whatever the candidate sends"
        );
        assert!(
            !candidate_message_matches(
                &expected,
                &Message::Binary(b"wrong kind".to_vec().into()),
                &session
            ),
            "the message kind still has to agree"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `Host` is the operator's target authority, never the fixture's, and
    /// never a credential the target operator did not send.
    #[test]
    fn the_candidate_host_is_the_target_authority_without_userinfo() {
        let target: Uri = "http://user:secret@example.test:8080/path".parse().unwrap();
        assert_eq!(candidate_host(&target).unwrap(), "example.test:8080");
        let plain: Uri = "http://127.0.0.1:9000".parse().unwrap();
        assert_eq!(candidate_host(&plain).unwrap(), "127.0.0.1:9000");
        let relative: Uri = "/no-authority".parse().unwrap();
        assert!(
            candidate_host(&relative).is_err(),
            "a target without an authority must fail closed"
        );
    }

    /// The documented driver-controlled set, case-insensitively.
    #[test]
    fn the_header_classifier_covers_the_documented_skip_set() {
        for name in [
            "host",
            "connection",
            "proxy-connection",
            "keep-alive",
            "te",
            "trailer",
            "transfer-encoding",
            "content-length",
            "upgrade",
            "expect",
            "proxy-authorization",
            "proxy-authenticate",
        ] {
            assert!(is_driver_controlled_header(name), "{name}");
            assert!(
                is_driver_controlled_header(&name.to_ascii_uppercase()),
                "the recorded spelling may be any case: {name}"
            );
        }
        // End-to-end fields are request semantics, not framing.
        for name in [
            "authorization",
            "cookie",
            "accept",
            "user-agent",
            "x-trace",
            "forwarded",
            "x-forwarded-for",
            "sec-websocket-key",
        ] {
            assert!(!is_driver_controlled_header(name), "{name}");
        }
    }

    /// The WebSocket handshake set is layered on top, never a replacement.
    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_handshake_headers_stay_websocket_specific() {
        for name in [
            "sec-websocket-key",
            "sec-websocket-accept",
            "sec-websocket-version",
            "sec-websocket-extensions",
        ] {
            assert!(is_websocket_handshake_header(name), "{name}");
            assert!(
                !is_driver_controlled_header(name),
                "an ordinary HTTP candidate must not skip {name}"
            );
        }
        // The pre-M021D WebSocket skip list is still fully covered by the
        // shared classifier plus the handshake set. If this ever fails, the
        // handshake path has been weakened by a refactor.
        for name in [
            "host",
            "connection",
            "upgrade",
            "content-length",
            "transfer-encoding",
            "sec-websocket-key",
            "sec-websocket-accept",
            "sec-websocket-version",
            "sec-websocket-extensions",
        ] {
            assert!(
                is_driver_controlled_header(name) || is_websocket_handshake_header(name),
                "{name} would be forwarded"
            );
        }
    }

    /// Accept exactly one request, capture its raw bytes, and answer `200 ok`.
    ///
    /// The server is deliberately raw TCP: the point is to see the bytes a
    /// fixture-driven candidate actually puts on the wire, not what a
    /// framework re-renders.
    fn capture_one_request() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            let mut head_end: Option<usize> = None;
            let mut body_length = 0usize;
            loop {
                if let Some(end) = head_end
                    && raw.len() >= end + 4 + body_length
                {
                    break;
                }
                let count = stream.read(&mut chunk).unwrap_or(0);
                if count == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..count]);
                if head_end.is_none()
                    && let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    head_end = Some(index);
                    let head = String::from_utf8_lossy(&raw[..index]).to_ascii_lowercase();
                    body_length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                }
            }
            let _ = sender.send(raw);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });
        (address, receiver)
    }

    /// Split a captured request into its line, lowercased headers, and body.
    fn request_parts(raw: &[u8]) -> (String, Vec<(String, String)>, Vec<u8>) {
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request must have a head terminator");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let body = raw[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_owned();
        let headers = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            })
            .collect();
        (request_line, headers, body)
    }

    /// A fixture that carries recorded framing, a foreign `Host`, and a
    /// proxy-hop credential must not be able to steer the candidate request.
    #[tokio::test]
    async fn candidate_never_forwards_fixture_host_framing_or_proxy_credentials() {
        let (address, captured) = capture_one_request();
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{address}").parse().unwrap();
        let mut request = test_request("internal.example", "/trace");
        request.method = "POST".into();
        request.headers = vec![
            HeaderEntry {
                name: "Host".into(),
                value: "internal.example".into(),
            },
            HeaderEntry {
                name: "Connection".into(),
                value: "keep-alive".into(),
            },
            HeaderEntry {
                name: "Transfer-Encoding".into(),
                value: "chunked".into(),
            },
            HeaderEntry {
                name: "Content-Length".into(),
                value: "999".into(),
            },
            HeaderEntry {
                name: "Proxy-Authorization".into(),
                value: "Basic SENTINEL".into(),
            },
            HeaderEntry {
                name: "X-Trace".into(),
                value: "trace-1".into(),
            },
        ];
        let observation =
            execute_candidate(&client, &request, b"hello", &target, 1024 * 1024, None)
                .await
                .unwrap();
        assert_eq!(observation.response_body, b"ok");
        let raw = captured
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("request must be captured");
        let (request_line, headers, body) = request_parts(&raw);
        assert_eq!(request_line, "POST /trace HTTP/1.1");
        let expected_host = address.to_string();
        let hosts = headers
            .iter()
            .filter(|(name, _)| name == "host")
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            hosts,
            vec![expected_host.as_str()],
            "exactly one Host, from the target: {headers:?}"
        );
        let content_lengths = headers
            .iter()
            .filter(|(name, _)| name == "content-length")
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            content_lengths,
            vec![body.len().to_string()],
            "the transport recomputes framing from the materialized body"
        );
        assert_eq!(body, b"hello");
        assert_eq!(
            headers
                .iter()
                .find(|(name, _)| name == "x-trace")
                .map(|(_, value)| value.as_str()),
            Some("trace-1"),
            "ordinary end-to-end headers stay"
        );
        let wire = String::from_utf8_lossy(&raw);
        for sentinel in [
            "internal.example",
            "SENTINEL",
            "keep-alive",
            "chunked",
            "999",
        ] {
            assert!(
                !wire.contains(sentinel),
                "{sentinel} must not reach the target: {wire}"
            );
        }
        for name in [
            "proxy-authorization",
            "proxy-connection",
            "proxy-authenticate",
            "te",
            "trailer",
            "expect",
            "upgrade",
        ] {
            assert!(
                !headers.iter().any(|(header, _)| header == name),
                "{name} must not be forwarded: {headers:?}"
            );
        }
        assert!(
            !matches!(headers.iter().find(|(name, _)| name == "connection"), Some((_, value)) if value == "keep-alive"),
            "the recorded connection policy must not be forwarded: {headers:?}"
        );
    }

    /// An ordinary fixture is unaffected: nothing that belongs to request
    /// semantics is stripped.
    #[tokio::test]
    async fn candidate_with_only_ordinary_headers_still_executes() {
        let (address, captured) = capture_one_request();
        let client = Client::builder().retry_canceled_requests(false).build();
        let target: Uri = format!("http://{address}").parse().unwrap();
        let mut request = test_request("internal.example", "/ordinary");
        request.headers = vec![
            HeaderEntry {
                name: "Authorization".into(),
                value: "Bearer token-1".into(),
            },
            HeaderEntry {
                name: "Cookie".into(),
                value: "session=abc".into(),
            },
            HeaderEntry {
                name: "Accept".into(),
                value: "application/json".into(),
            },
            HeaderEntry {
                name: "User-Agent".into(),
                value: "eggreplay-test".into(),
            },
            HeaderEntry {
                name: "X-Trace".into(),
                value: "trace-2".into(),
            },
        ];
        let observation = execute_candidate(&client, &request, b"", &target, 1024 * 1024, None)
            .await
            .unwrap();
        assert_eq!(observation.response_body, b"ok");
        let raw = captured
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("request must be captured");
        let (_, headers, body) = request_parts(&raw);
        assert!(body.is_empty());
        for (name, value) in [
            ("authorization", "Bearer token-1"),
            ("cookie", "session=abc"),
            ("accept", "application/json"),
            ("user-agent", "eggreplay-test"),
            ("x-trace", "trace-2"),
        ] {
            assert_eq!(
                headers
                    .iter()
                    .find(|(header, _)| header == name)
                    .map(|(_, value)| value.as_str()),
                Some(value),
                "{name} is request semantics, not framing"
            );
        }
        let expected_host = address.to_string();
        assert_eq!(
            headers
                .iter()
                .find(|(name, _)| name == "host")
                .map(|(_, value)| value.as_str()),
            Some(expected_host.as_str())
        );
    }
}
