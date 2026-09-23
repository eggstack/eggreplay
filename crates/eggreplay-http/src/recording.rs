//! EggFetch-native streaming observation and schema conversion.

#[cfg(all(feature = "eggserve", feature = "websocket"))]
use base64::Engine;
use bytes::Bytes;
use eggfetch_core::{Client, Error as FetchError};
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, Flow, FlowError, FlowOutcome, HeaderEntry, HttpRequest,
    HttpResponse, QueryPair, RedactionConfig, WebSocketConversation, WebSocketDirection,
    WebSocketMessage, WebSocketMessageKind, WebSocketRedaction, WebSocketTerminal,
    apply_form_redaction, apply_json_redaction, push_body_markers,
    reconcile_headers_after_body_redaction,
};
use eggreplay_store::{RecordingSession, SessionWriter, StoreError};
use futures_util::Stream;
use http::{Request, Uri};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, StreamBody};
use std::io::Write;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[cfg(feature = "eggserve")]
use eggserve_primitives::{
    HeaderBlock, RequestBodyPolicy, Response, ResponseBody, ResponseStream, ResponseStreamError,
    StatusCode, Trailers,
};
#[cfg(feature = "eggserve")]
use eggserve_server::{
    Request as ServeRequest, Server, ServerHandle, Service, ServiceError, ServiceFuture,
};
#[cfg(feature = "eggserve")]
use std::net::SocketAddr;
#[cfg(feature = "eggserve")]
use tokio::io::AsyncReadExt;

#[derive(Debug)]
struct BodyError(String);

impl std::fmt::Display for BodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BodyError {}

/// Errors from conversion, delegated transport, or fixture publication.
#[derive(Debug, Error)]
pub enum HttpError {
    /// Request/response semantic conversion failed.
    #[error("HTTP conversion: {0}")]
    Conversion(String),
    /// EggFetch rejected or failed the native request.
    #[error("EggFetch: {0}")]
    Fetch(#[from] FetchError),
    /// A stream body failed while being observed.
    #[error("body stream: {0}")]
    Body(String),
    /// The fixture could not accept the observed flow.
    #[error("fixture: {0}")]
    Store(#[from] StoreError),
}

/// Request conversion result with explicit loss annotations.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// The semantic request head used by the flow.
    pub request: HttpRequest,
    /// Whether any HTTP header value required an opaque-safe representation.
    pub lossy_header_values: bool,
}

fn content_type(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        })
}

fn is_json_content_type(content_type: Option<&str>) -> bool {
    matches!(content_type, Some(value) if value == "application/json" || value.ends_with("+json"))
}

fn is_form_content_type(content_type: Option<&str>) -> bool {
    matches!(content_type, Some("application/x-www-form-urlencoded"))
}

fn transform_body(
    original: &[u8],
    content_type: Option<&str>,
    config: &RedactionConfig,
    profile_id: &str,
    is_response: bool,
) -> Result<(Vec<u8>, Vec<eggreplay_core::RedactionMarker>), HttpError> {
    if original.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if !config.json_paths.is_empty() && is_json_content_type(content_type) {
        let (redacted, _) =
            apply_json_redaction(original, &config.json_paths).map_err(HttpError::Body)?;
        let mut markers = Vec::new();
        push_body_markers(&mut markers, &config.json_paths, profile_id, is_response);
        return Ok((redacted, markers));
    }
    if !config.query_keys.is_empty() && is_form_content_type(content_type) {
        let (redacted, _) =
            apply_form_redaction(original, &config.query_keys).map_err(HttpError::Body)?;
        let markers = config
            .query_keys
            .iter()
            .map(|key| eggreplay_core::RedactionMarker {
                field: format!(
                    "{}body.form:{key}",
                    if is_response { "response." } else { "request." }
                ),
                profile: profile_id.into(),
            })
            .collect();
        return Ok((redacted, markers));
    }
    if !config.json_paths.is_empty() {
        return Err(HttpError::Body(
            "body redaction requested but media type unsupported; failing closed".into(),
        ));
    }
    Ok((original.to_vec(), Vec::new()))
}

/// Execute one request through EggFetch and append one semantic flow.
///
/// The redaction policy is explicit. Without structured body selectors, DATA
/// frames stream to staging without buffering; with JSON/form selectors,
/// staging bytes transform before blob publication (bounded by
/// `max_structured_bytes`, fail-closed). Raw bytes never become finalized
/// blobs. An aborted stream leaves no referenced blob.
#[allow(clippy::too_many_arguments)]
pub async fn record_request<B>(
    client: &Client,
    writer: &mut SessionWriter,
    request: Request<B>,
    redaction: &RedactionConfig,
    profile_id: &str,
    max_structured_bytes: u64,
    physical_route: Option<eggreplay_core::PhysicalRoute>,
) -> Result<Flow, HttpError>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let started_at_ms = now_ms();
    let (parts, body) = request.into_parts();
    let uri = parts.uri.clone();
    let converted = request_head(&parts.method, &uri, &parts.headers)?;
    let req_content_type = content_type(&parts.headers);
    let request_sink = Arc::new(Mutex::new(Some(writer.begin_blob()?)));
    let request_trailers = Arc::new(Mutex::new(Vec::new()));
    let upstream_body = tee_body(body, request_sink.clone(), request_trailers.clone());
    let upstream = Request::from_parts(parts, upstream_body);

    let result = client.execute_http_body_default(upstream).await;
    let (request_body, mut req_markers) = finish_sink_redacted(
        request_sink,
        writer,
        req_content_type.as_deref(),
        redaction,
        profile_id,
        max_structured_bytes,
        false,
    )?;
    let request_trailers = take_trailers(request_trailers)?;
    let mut semantic_request = converted.request;
    semantic_request.body = request_body;
    semantic_request.trailers = request_trailers;

    let mut response_events = Vec::new();
    let outcome = match result {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            let resp_content_type = content_type(&parts.headers);
            let response_sink = Arc::new(Mutex::new(Some(writer.begin_blob()?)));
            let mut body = Box::pin(body);
            let mut response_trailers = Vec::new();
            let response_started = Instant::now();
            let mut response_offset = 0u64;
            let mut response_failed = false;
            while let Some(frame) = body.frame().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        push_stream_event(
                            &mut response_events,
                            eggreplay_core::StreamEvent {
                                delta_ns: elapsed_ns(response_started),
                                event: eggreplay_core::StreamEventKind::Error {
                                    offset: response_offset,
                                    category: "other".into(),
                                    phase: "body".into(),
                                },
                            },
                        )?;
                        response_failed = true;
                        let _ = error;
                        break;
                    }
                };
                if frame.is_data() {
                    let data = frame
                        .into_data()
                        .map_err(|_| HttpError::Body("invalid response data frame".into()))?;
                    let mut sink = response_sink
                        .lock()
                        .map_err(|_| HttpError::Body("response sink poisoned".into()))?;
                    sink.as_mut()
                        .ok_or_else(|| HttpError::Body("response sink already closed".into()))?
                        .write_all(&data)
                        .map_err(|error| HttpError::Body(error.to_string()))?;
                    let length = data.len() as u64;
                    push_stream_event(
                        &mut response_events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: eggreplay_core::StreamEventKind::Data {
                                offset: response_offset,
                                length,
                            },
                        },
                    )?;
                    response_offset = response_offset.saturating_add(length);
                } else if frame.is_trailers() {
                    let trailers = frame
                        .into_trailers()
                        .map_err(|_| HttpError::Body("invalid response trailer frame".into()))?;
                    response_trailers = header_entries(&trailers).0;
                    push_stream_event(
                        &mut response_events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: eggreplay_core::StreamEventKind::Trailers {
                                fields: response_trailers.clone(),
                            },
                        },
                    )?;
                }
            }
            if !response_failed {
                push_stream_event(
                    &mut response_events,
                    eggreplay_core::StreamEvent {
                        delta_ns: elapsed_ns(response_started),
                        event: eggreplay_core::StreamEventKind::End,
                    },
                )?;
            }
            // Transform staged response before publication; raw never finalized.
            // Temporarily take sink for redaction check without finishing raw.
            let (response_body, mut resp_markers) = {
                let mut staged = response_sink
                    .lock()
                    .map_err(|_| HttpError::Body("response sink poisoned".into()))?
                    .take()
                    .ok_or_else(|| HttpError::Body("response sink already closed".into()))?;
                if staged.len() == 0 {
                    (staged.finish()?, Vec::new())
                } else if !redaction.json_paths.is_empty() || !redaction.query_keys.is_empty() {
                    let needs = (!redaction.json_paths.is_empty()
                        && is_json_content_type(resp_content_type.as_deref()))
                        || (!redaction.query_keys.is_empty()
                            && is_form_content_type(resp_content_type.as_deref()));
                    if !needs {
                        if !redaction.json_paths.is_empty() {
                            drop(staged);
                            return Err(HttpError::Body(
                                "response body redaction requested but media type unsupported; failing closed".into(),
                            ));
                        }
                        (staged.finish()?, Vec::new())
                    } else {
                        let raw = staged
                            .read_staging_bounded(max_structured_bytes)
                            .map_err(|error| HttpError::Body(error.to_string()))?;
                        drop(staged);
                        let (redacted, markers) = transform_body(
                            &raw,
                            resp_content_type.as_deref(),
                            redaction,
                            profile_id,
                            true,
                        )?;
                        let body_ref = if redacted.is_empty() {
                            BodyRef::Empty
                        } else {
                            let mut fresh = writer.begin_blob()?;
                            fresh
                                .write_all(&redacted)
                                .map_err(|error| HttpError::Body(error.to_string()))?;
                            fresh.finish()?
                        };
                        (body_ref, markers)
                    }
                } else {
                    (staged.finish()?, Vec::new())
                }
            };
            let mut resp_headers = header_entries(&parts.headers).0;
            let mut resp_notes = Vec::new();
            if !resp_markers.is_empty() {
                let (mut reconciled, mut notes) = reconcile_headers_after_body_redaction(
                    &mut resp_headers,
                    match &response_body {
                        BodyRef::Blob(blob) => blob.length,
                        _ => 0,
                    },
                    profile_id,
                );
                req_markers.append(&mut resp_markers);
                req_markers.append(&mut reconciled);
                resp_notes.append(&mut notes);
            }
            let outcome = FlowOutcome::Response(HttpResponse {
                status: parts.status.as_u16(),
                headers: resp_headers,
                body: response_body,
                trailers: response_trailers,
            });
            let _ = resp_notes;
            outcome
        }
        Err(error) => FlowOutcome::Error(map_fetch_error(&error)),
    };

    let mut flow = Flow::new(semantic_request, outcome, started_at_ms);
    flow.completed_at_ms = Some(now_ms());
    flow.provenance.mode = "eggfetch-native".into();
    flow.provenance.observer = "eggreplay-http".into();
    flow.physical_route = physical_route.clone();
    eggreplay_core::redact_flow(&mut flow, redaction, profile_id);
    flow.redactions.append(&mut req_markers);
    // Reconcile request framing if request body was redacted.
    if req_markers.iter().any(|marker| {
        marker.field.starts_with("request.body.json:")
            || marker.field.starts_with("request.body.form:")
    }) {
        let new_len = flow.request.body.len().unwrap_or(0);
        let (mut markers, mut notes) =
            reconcile_headers_after_body_redaction(&mut flow.request.headers, new_len, profile_id);
        flow.redactions.append(&mut markers);
        flow.annotations.append(&mut notes);
    }
    if converted.lossy_header_values {
        flow.annotations
            .push(("conversion".into(), "opaque-header-values-escaped".into()));
    }
    reconcile_stream_event_body_length(&mut response_events, &flow.outcome);
    writer.append_flow(&flow)?;
    writer.append_stream_events(eggreplay_core::FlowStreamEvents {
        flow_id: flow.id.clone(),
        start_offset_ns: monotonic_offset_ns(),
        request: Vec::new(),
        response: response_events,
    })?;
    Ok(flow)
}

/// Execute one request through EggFetch using a concurrent session.
///
/// Body sinks are independent of the flow-log lock: `begin_blob` never holds
/// the append mutex while bytes stream, and `append_flow` serializes only the
/// final bounded metadata write. No network await holds the session lock.
///
/// The redaction policy is explicit with the same before-publication guarantee
/// as `record_request`.
#[allow(clippy::too_many_arguments)]
pub async fn record_request_with_session<B>(
    client: &Client,
    session: &RecordingSession,
    request: Request<B>,
    redaction: &RedactionConfig,
    profile_id: &str,
    max_structured_bytes: u64,
    physical_route: Option<eggreplay_core::PhysicalRoute>,
) -> Result<Flow, HttpError>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    use eggreplay_store::RecordingBodyWriter;
    let started_at_ms = now_ms();
    let (parts, body) = request.into_parts();
    let uri = parts.uri.clone();
    let converted = request_head(&parts.method, &uri, &parts.headers)?;
    let req_content_type = content_type(&parts.headers);
    let request_sink: Arc<Mutex<Option<RecordingBodyWriter>>> =
        Arc::new(Mutex::new(Some(session.begin_blob()?)));
    let request_trailers = Arc::new(Mutex::new(Vec::new()));
    let request_events = Arc::new(Mutex::new(Vec::new()));
    let request_started = Instant::now();
    let capture_start_offset_ns = monotonic_offset_ns();
    let upstream_body = tee_body_with_session(
        body,
        request_sink.clone(),
        request_trailers.clone(),
        request_events.clone(),
        request_started,
    );
    let upstream = Request::from_parts(parts, upstream_body);

    let result = client.execute_http_body_default(upstream).await;
    let (request_body, mut all_markers) = finish_session_sink_redacted(
        request_sink,
        session,
        req_content_type.as_deref(),
        redaction,
        profile_id,
        max_structured_bytes,
        false,
    )?;
    let request_trailers = take_trailers(request_trailers)?;
    let request_events = take_stream_events(request_events)?;
    let mut semantic_request = converted.request;
    semantic_request.body = request_body;
    semantic_request.trailers = request_trailers;

    let mut response_events = Vec::new();
    let outcome = match result {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            let resp_content_type = content_type(&parts.headers);
            let response_sink: Arc<Mutex<Option<RecordingBodyWriter>>> =
                Arc::new(Mutex::new(Some(session.begin_blob()?)));
            let mut body = Box::pin(body);
            let mut response_trailers = Vec::new();
            let response_started = Instant::now();
            let mut response_offset = 0u64;
            let mut response_failed = false;
            while let Some(frame) = body.frame().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        push_stream_event(
                            &mut response_events,
                            eggreplay_core::StreamEvent {
                                delta_ns: elapsed_ns(response_started),
                                event: eggreplay_core::StreamEventKind::Error {
                                    offset: response_offset,
                                    category: "other".into(),
                                    phase: "body".into(),
                                },
                            },
                        )?;
                        let _ = error;
                        response_failed = true;
                        break;
                    }
                };
                if frame.is_data() {
                    let data = frame
                        .into_data()
                        .map_err(|_| HttpError::Body("invalid response data frame".into()))?;
                    let mut sink = response_sink
                        .lock()
                        .map_err(|_| HttpError::Body("response sink poisoned".into()))?;
                    sink.as_mut()
                        .ok_or_else(|| HttpError::Body("response sink already closed".into()))?
                        .write_all(&data)
                        .map_err(|error| HttpError::Body(error.to_string()))?;
                    let length = data.len() as u64;
                    push_stream_event(
                        &mut response_events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: eggreplay_core::StreamEventKind::Data {
                                offset: response_offset,
                                length,
                            },
                        },
                    )?;
                    response_offset = response_offset.saturating_add(length);
                } else if frame.is_trailers() {
                    let trailers = frame
                        .into_trailers()
                        .map_err(|_| HttpError::Body("invalid response trailer frame".into()))?;
                    response_trailers = header_entries(&trailers).0;
                    push_stream_event(
                        &mut response_events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(response_started),
                            event: eggreplay_core::StreamEventKind::Trailers {
                                fields: response_trailers.clone(),
                            },
                        },
                    )?;
                }
            }
            if !response_failed {
                push_stream_event(
                    &mut response_events,
                    eggreplay_core::StreamEvent {
                        delta_ns: elapsed_ns(response_started),
                        event: eggreplay_core::StreamEventKind::End,
                    },
                )?;
            }
            // Redact staged response before publication.
            let (response_body, mut resp_markers) = {
                let mut staged = response_sink
                    .lock()
                    .map_err(|_| HttpError::Body("response sink poisoned".into()))?
                    .take()
                    .ok_or_else(|| HttpError::Body("response sink already closed".into()))?;
                if staged.len() == 0 {
                    (staged.finish()?, Vec::new())
                } else if !redaction.json_paths.is_empty() || !redaction.query_keys.is_empty() {
                    let needs = (!redaction.json_paths.is_empty()
                        && is_json_content_type(resp_content_type.as_deref()))
                        || (!redaction.query_keys.is_empty()
                            && is_form_content_type(resp_content_type.as_deref()));
                    if !needs {
                        if !redaction.json_paths.is_empty() {
                            drop(staged);
                            return Err(HttpError::Body(
                                "response body redaction requested but media type unsupported; failing closed".into(),
                            ));
                        }
                        (staged.finish()?, Vec::new())
                    } else {
                        let raw = staged
                            .read_staging_bounded(max_structured_bytes)
                            .map_err(|error| HttpError::Body(error.to_string()))?;
                        drop(staged);
                        let (redacted, markers) = transform_body(
                            &raw,
                            resp_content_type.as_deref(),
                            redaction,
                            profile_id,
                            true,
                        )?;
                        let body_ref = if redacted.is_empty() {
                            BodyRef::Empty
                        } else {
                            let mut fresh = session.begin_blob()?;
                            fresh
                                .write_all(&redacted)
                                .map_err(|error| HttpError::Body(error.to_string()))?;
                            fresh.finish()?
                        };
                        (body_ref, markers)
                    }
                } else {
                    (staged.finish()?, Vec::new())
                }
            };
            let mut resp_headers = header_entries(&parts.headers).0;
            if !resp_markers.is_empty() {
                let (mut reconciled, notes) = reconcile_headers_after_body_redaction(
                    &mut resp_headers,
                    match &response_body {
                        BodyRef::Blob(blob) => blob.length,
                        _ => 0,
                    },
                    profile_id,
                );
                all_markers.append(&mut resp_markers);
                all_markers.append(&mut reconciled);
                let _ = notes;
            }
            FlowOutcome::Response(HttpResponse {
                status: parts.status.as_u16(),
                headers: resp_headers,
                body: response_body,
                trailers: response_trailers,
            })
        }
        Err(error) => FlowOutcome::Error(map_fetch_error(&error)),
    };

    let mut flow = Flow::new(semantic_request, outcome, started_at_ms);
    flow.completed_at_ms = Some(now_ms());
    flow.provenance.mode = "eggfetch-native".into();
    flow.provenance.observer = "eggreplay-http".into();
    flow.physical_route = physical_route.clone();
    eggreplay_core::redact_flow(&mut flow, redaction, profile_id);
    flow.redactions.append(&mut all_markers);
    let req_had_redaction = all_markers.iter().any(|marker| {
        marker.field.starts_with("request.body.json:")
            || marker.field.starts_with("request.body.form:")
    });
    // Note: all_markers moved above; recompute flag before move for request reconcile.
    // To avoid borrow issues, check via flow redactions for request markers.
    if flow.redactions.iter().any(|marker| {
        marker.field.starts_with("request.body.json:")
            || marker.field.starts_with("request.body.form:")
    }) {
        let _ = req_had_redaction;
        let new_len = flow.request.body.len().unwrap_or(0);
        let (mut markers, mut notes) =
            reconcile_headers_after_body_redaction(&mut flow.request.headers, new_len, profile_id);
        flow.redactions.append(&mut markers);
        flow.annotations.append(&mut notes);
    }
    if converted.lossy_header_values {
        flow.annotations
            .push(("conversion".into(), "opaque-header-values-escaped".into()));
    }
    reconcile_stream_event_body_length(&mut response_events, &flow.outcome);
    session.append_flow(&flow)?;
    session.append_stream_events(eggreplay_core::FlowStreamEvents {
        flow_id: flow.id.clone(),
        start_offset_ns: capture_start_offset_ns,
        request: request_events,
        response: response_events,
    })?;
    Ok(flow)
}

/// WebSocket-specific policies for the recording gateway.
#[derive(Debug, Clone, Copy)]
pub struct WebSocketRecordingOptions {
    /// Whether new WebSocket upgrades may be acquired. Defaults to false.
    pub enabled: bool,
    /// Replace all text message payloads with a safe replay marker.
    pub redact_text: bool,
    /// Replace all binary message payloads with a safe replay marker.
    pub redact_binary: bool,
    /// Maximum wall-clock lifetime of one conversation.
    pub max_duration: std::time::Duration,
    /// Maximum concurrent admitted WebSocket tunnels.
    pub max_active_tunnels: usize,
}

impl Default for WebSocketRecordingOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            redact_text: false,
            redact_binary: false,
            max_duration: std::time::Duration::from_secs(60 * 60),
            max_active_tunnels: 16,
        }
    }
}

#[cfg(feature = "eggserve")]
struct GatewayTunnelService<F> {
    handler: F,
    body_policy: RequestBodyPolicy,
}

#[cfg(feature = "eggserve")]
impl<F, Fut> Service for GatewayTunnelService<F>
where
    F: Fn(ServeRequest, Option<eggserve_server::tunnel::TunnelCapability>) -> Fut
        + Send
        + Sync
        + 'static,
    Fut: std::future::Future<Output = Result<Response, ServiceError>> + Send + 'static,
{
    fn request_body_policy(&self, head: &eggserve_primitives::RequestHead) -> RequestBodyPolicy {
        let upgrade = head.headers().iter().any(|field| {
            field.name.as_str().eq_ignore_ascii_case("upgrade")
                && field.value.as_bytes().eq_ignore_ascii_case(b"websocket")
        });
        if upgrade {
            RequestBodyPolicy::Reject
        } else {
            self.body_policy
        }
    }

    fn call(&self, request: ServeRequest) -> ServiceFuture<'_> {
        Box::pin((self.handler)(request, None))
    }

    fn call_with_tunnel(
        &self,
        request: ServeRequest,
        tunnel: Option<eggserve_server::tunnel::TunnelCapability>,
    ) -> ServiceFuture<'_> {
        Box::pin((self.handler)(request, tunnel))
    }
}

/// Start a bounded EggServe recording gateway.
///
/// The gateway uses EggServe for inbound lifecycle/framing and EggFetch for
/// outbound execution. The concurrent session is shared via cheap clones;
/// no Tokio mutex spans the awaited upstream transaction, so independent
/// requests stream request/response blobs concurrently. Only the final
/// bounded JSONL append holds the flow-log lock.
///
/// Shutdown policy: `ServerHandle::shutdown` stops admission, `wait` drains
/// in-flight gateway tasks, then the owner calls `RecordingSession::shutdown`
/// (idempotent) and `finish`, which fails if any body sink is still active
/// rather than racing finalization.
#[cfg(feature = "eggserve")]
#[allow(clippy::too_many_arguments)]
pub async fn start_recording_gateway(
    bind: SocketAddr,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
    max_body_bytes: u64,
    redaction: RedactionConfig,
    profile_id: String,
    max_structured_bytes: u64,
    physical_route: eggreplay_core::PhysicalRoute,
) -> Result<ServerHandle, HttpError> {
    start_recording_gateway_with_websockets(
        bind,
        upstream_base,
        client,
        session,
        max_body_bytes,
        redaction,
        profile_id,
        max_structured_bytes,
        physical_route,
        WebSocketRecordingOptions::default(),
    )
    .await
}

/// Start a recording gateway with explicit WebSocket admission/redaction
/// policy. WebSocket acquisition is disabled by default.
#[cfg(feature = "eggserve")]
#[allow(clippy::too_many_arguments)]
pub async fn start_recording_gateway_with_websockets(
    bind: SocketAddr,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
    max_body_bytes: u64,
    redaction: RedactionConfig,
    profile_id: String,
    max_structured_bytes: u64,
    physical_route: eggreplay_core::PhysicalRoute,
    websocket: WebSocketRecordingOptions,
) -> Result<ServerHandle, HttpError> {
    if websocket.enabled && (websocket.max_active_tunnels == 0 || websocket.max_duration.is_zero())
    {
        return Err(HttpError::Conversion(
            "WebSocket recording bounds must be positive".into(),
        ));
    }
    let handler =
        move |request: ServeRequest, tunnel: Option<eggserve_server::tunnel::TunnelCapability>| {
            let upstream_base = upstream_base.clone();
            let client = client.clone();
            let session = session.clone();
            let redaction = redaction.clone();
            let profile_id = profile_id.clone();
            let physical_route = physical_route.clone();
            async move {
                let websocket_intent = request.head().headers().iter().any(|field| {
                    field.name.as_str().eq_ignore_ascii_case("upgrade")
                        && std::str::from_utf8(field.value.as_bytes()).is_ok_and(|value| {
                            value
                                .split(',')
                                .any(|token| token.trim().eq_ignore_ascii_case("websocket"))
                        })
                });
                if let Some(tunnel) = tunnel {
                    #[cfg(feature = "websocket")]
                    {
                        if websocket.enabled {
                            return gateway_websocket_request(
                                request,
                                tunnel,
                                upstream_base,
                                client,
                                session,
                                redaction,
                                profile_id,
                                max_structured_bytes,
                                physical_route,
                                websocket,
                            )
                            .await;
                        }
                    }
                    drop(tunnel);
                    return Ok(denied_upgrade_response());
                }
                if websocket_intent {
                    return Err(ServiceError::rejected(
                        400,
                        "invalid WebSocket upgrade request",
                    ));
                }
                gateway_request(
                    request,
                    upstream_base,
                    client,
                    session,
                    redaction,
                    profile_id,
                    max_structured_bytes,
                    physical_route,
                )
                .await
            }
        };
    let service = GatewayTunnelService {
        handler,
        body_policy: RequestBodyPolicy::Stream {
            max_bytes: max_body_bytes,
        },
    };
    // Server-level body cap defaults to 0 (reject all); align it with the
    // service policy so the effective limit is the caller's bound.
    let builder = eggserve_server::RuntimeConfig::builder()
        .bind(bind)
        .max_request_body_bytes(max_body_bytes);
    let builder = if websocket.enabled {
        builder
            .max_active_tunnels(websocket.max_active_tunnels)
            .disable_connection_total_timeout()
    } else {
        builder
    };
    let runtime = builder
        .build()
        .map_err(|error| HttpError::Conversion(error.to_string()))?;
    Server::builder()
        .runtime(runtime)
        .build()
        .map_err(|error| HttpError::Conversion(error.to_string()))?
        .start_with_service(service)
        .await
        .map_err(|error| HttpError::Conversion(error.to_string()))
}

#[cfg(feature = "eggserve")]
#[allow(clippy::too_many_arguments)]
async fn gateway_request(
    request: eggserve_primitives::Request,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
    redaction: RedactionConfig,
    profile_id: String,
    max_structured_bytes: u64,
    physical_route: eggreplay_core::PhysicalRoute,
) -> Result<Response, ServiceError> {
    let (head, body, _connection) = request.into_parts();
    let mut headers = http::HeaderMap::new();
    for field in head.headers().iter() {
        if field.name.as_str().eq_ignore_ascii_case("host") {
            continue;
        }
        let name = http::header::HeaderName::from_bytes(field.name.as_str().as_bytes())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        let value = http::HeaderValue::from_bytes(field.value.as_bytes())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        headers.append(name, value);
    }
    let target = format!(
        "{}{}",
        head.target().path(),
        head.target()
            .query()
            .map_or(String::new(), |query| format!("?{query}"))
    );
    let authority = upstream_base
        .authority()
        .ok_or_else(|| ServiceError::internal("upstream URI has no authority"))?;
    let scheme = upstream_base
        .scheme_str()
        .ok_or_else(|| ServiceError::internal("upstream URI has no scheme"))?;
    let uri: Uri = format!("{scheme}://{authority}{target}")
        .parse()
        .map_err(|error| ServiceError::internal(format!("invalid upstream URI: {error}")))?;
    let body = StreamBody::new(eggserve_body_stream(body));
    let mut request = Request::builder()
        .method(head.method().as_str())
        .uri(uri)
        .body(body)
        .map_err(|error| ServiceError::internal(error.to_string()))?;
    *request.headers_mut() = headers;
    // No session lock spans the awaited upstream transaction: body sinks are
    // independent, only the final append briefly locks the flow log.
    // Redaction transforms happen before blob publication (fail-closed).
    let flow = record_request_with_session(
        &client,
        &session,
        request,
        &redaction,
        &profile_id,
        max_structured_bytes,
        Some(physical_route),
    )
    .await
    .map_err(|error| ServiceError::internal(error.to_string()))?;
    match flow.outcome {
        FlowOutcome::Response(response) => {
            let body_path = session
                .body_path(&response.body)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            let body_length = response.body.len().unwrap_or(0);
            let stream = response_stream(body_path, body_length, response.trailers.clone());
            let status = StatusCode::new(response.status)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            let mut builder = Response::builder().status(status);
            for header in response.headers {
                builder = builder
                    .header(header.name, header.value)
                    .map_err(|error| ServiceError::internal(error.to_string()))?;
            }
            builder
                .body(ResponseBody::Stream(stream))
                .map_err(|error| ServiceError::internal(error.to_string()))
        }
        FlowOutcome::Error(error) => Err(ServiceError::rejected(
            502,
            format!("upstream failure: {:?}", error.category),
        )),
    }
}

#[cfg(feature = "eggserve")]
fn denied_upgrade_response() -> Response {
    Response::builder()
        .status(StatusCode::new(403).expect("valid status"))
        .body(ResponseBody::Empty)
        .expect("valid denial response")
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
#[allow(clippy::too_many_arguments)]
async fn gateway_websocket_request(
    request: ServeRequest,
    tunnel: eggserve_server::tunnel::TunnelCapability,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
    redaction: RedactionConfig,
    profile_id: String,
    _max_structured_bytes: u64,
    physical_route: eggreplay_core::PhysicalRoute,
    options: WebSocketRecordingOptions,
) -> Result<Response, ServiceError> {
    use eggfetch_core::{Headers, NetworkStream};
    use eggreplay_core::{
        normalize_websocket_handshake_headers, validate_websocket_handshake_headers,
    };
    use eggserve_primitives::TunnelKind;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    let lifecycle = request.lifecycle_clone();
    let (head, _body, _connection) = request.into_parts();
    if head.version() != eggserve_primitives::HttpVersion::Http11
        || !head.method().is_get()
        || tunnel.request().kind() != TunnelKind::Http1Upgrade
        || tunnel
            .request()
            .protocol()
            .is_none_or(|protocol| !protocol.as_str().eq_ignore_ascii_case("websocket"))
    {
        return Err(ServiceError::rejected(
            400,
            "invalid WebSocket upgrade request",
        ));
    }

    let inbound_fields = head
        .headers()
        .iter()
        .map(|field| {
            Ok(HeaderEntry {
                name: field.name.as_str().to_owned(),
                value: std::str::from_utf8(field.value.as_bytes())
                    .map_err(|_| ServiceError::rejected(400, "invalid WebSocket header"))?
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>, ServiceError>>()?;
    let limits = eggreplay_core::WebSocketLimits::default();
    validate_websocket_handshake_headers(&inbound_fields, limits, false)
        .map_err(|_| ServiceError::rejected(400, "WebSocket handshake exceeds limits"))?;
    let key = single_header(&inbound_fields, "sec-websocket-key")
        .ok_or_else(|| ServiceError::rejected(400, "invalid WebSocket key"))?;
    if !eggreplay_http_valid_websocket_key(&key)
        || single_header(&inbound_fields, "sec-websocket-version").as_deref() != Some("13")
        || !header_has_token(&inbound_fields, "connection", "upgrade")
        || !header_has_token(&inbound_fields, "upgrade", "websocket")
    {
        return Err(ServiceError::rejected(400, "invalid WebSocket handshake"));
    }
    if single_header(&inbound_fields, "content-length")
        .is_some_and(|length| length.parse::<u64>() != Ok(0))
        || inbound_fields
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(ServiceError::rejected(
            400,
            "WebSocket upgrade cannot carry a request body",
        ));
    }

    let target = format!(
        "{}{}",
        head.target().path(),
        head.target()
            .query()
            .map_or(String::new(), |query| format!("?{query}"))
    );
    let authority = upstream_base
        .authority()
        .ok_or_else(|| ServiceError::internal("upstream URI has no authority"))?;
    let scheme = upstream_base
        .scheme_str()
        .ok_or_else(|| ServiceError::internal("upstream URI has no scheme"))?;
    let upstream_uri = format!("{scheme}://{authority}{target}");
    let upstream_key =
        base64::engine::general_purpose::STANDARD.encode(uuid::Uuid::new_v4().as_bytes());
    let mut outgoing = Headers::new();
    for field in &inbound_fields {
        let name = field.name.to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "host"
                | "connection"
                | "upgrade"
                | "content-length"
                | "transfer-encoding"
                | "sec-websocket-key"
                | "sec-websocket-accept"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
        ) {
            continue;
        }
        outgoing
            .append(&field.name, &field.value)
            .map_err(|_| ServiceError::rejected(400, "invalid WebSocket header"))?;
    }
    outgoing
        .append("Connection", "Upgrade")
        .and_then(|_| outgoing.append("Upgrade", "websocket"))
        .and_then(|_| outgoing.append("Sec-WebSocket-Version", "13"))
        .and_then(|_| outgoing.append("Sec-WebSocket-Key", &upstream_key))
        .map_err(|_| ServiceError::internal("failed to build WebSocket handshake"))?;

    let upstream_response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client
            .request(http::Method::GET, &upstream_uri)
            .map_err(|_| ServiceError::internal("failed to construct upstream request"))?
            .headers(outgoing)
            .send(),
    )
    .await
    .map_err(|_| ServiceError::rejected(504, "upstream WebSocket handshake timed out"))?
    .map_err(|_| ServiceError::rejected(502, "upstream WebSocket handshake failed"))?;
    if upstream_response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(ServiceError::rejected(
            502,
            "upstream rejected WebSocket handshake",
        ));
    }
    let upstream_headers = upstream_response.headers().clone();
    let upstream_header_entries = header_entries(&upstream_headers).0;
    validate_websocket_handshake_headers(&upstream_header_entries, limits, true)
        .map_err(|_| ServiceError::rejected(502, "unsupported upstream WebSocket handshake"))?;
    if !header_has_token(&upstream_header_entries, "connection", "upgrade")
        || !header_has_token(&upstream_header_entries, "upgrade", "websocket")
    {
        return Err(ServiceError::rejected(
            502,
            "invalid upstream WebSocket upgrade",
        ));
    }
    let upstream_accept = upstream_headers
        .get("sec-websocket-accept")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ServiceError::rejected(502, "invalid upstream WebSocket accept"))?;
    if upstream_accept != derive_accept_key(upstream_key.as_bytes()) {
        return Err(ServiceError::rejected(
            502,
            "invalid upstream WebSocket accept",
        ));
    }
    let offered_subprotocols = offered_subprotocols(&inbound_fields);
    let selected_subprotocol = match upstream_headers.get("sec-websocket-protocol") {
        Some(value) => {
            let value = value
                .to_str()
                .map_err(|_| ServiceError::rejected(502, "invalid upstream subprotocol"))?;
            if value.contains(',') || !offered_subprotocols.iter().any(|offered| offered == value) {
                return Err(ServiceError::rejected(
                    502,
                    "upstream selected an unoffered subprotocol",
                ));
            }
            Some(value.to_owned())
        }
        None => None,
    };
    let NetworkStream::Upgraded(upstream_stream) = upstream_response
        .into_network_stream()
        .ok_or_else(|| ServiceError::rejected(502, "upstream upgrade stream unavailable"))?
    else {
        return Err(ServiceError::rejected(
            502,
            "upstream upgrade stream unavailable",
        ));
    };

    let mut inbound_response_headers = HeaderBlock::new();
    inbound_response_headers
        .push_bytes(
            "sec-websocket-accept",
            derive_accept_key(key.as_bytes()).as_bytes(),
        )
        .map_err(|_| ServiceError::internal("failed to build WebSocket response"))?;
    if let Some(protocol) = &selected_subprotocol {
        inbound_response_headers
            .push_bytes("sec-websocket-protocol", protocol.as_bytes())
            .map_err(|_| ServiceError::internal("failed to build WebSocket response"))?;
    }
    let handler_session = session.clone();
    let handler_redaction = redaction.clone();
    let handler_profile = profile_id.clone();
    let handler_offered = offered_subprotocols.clone();
    let handler_selected = selected_subprotocol.clone();
    let handler_options = options;
    let flow_id = uuid::Uuid::new_v4().to_string();
    let conversation_flow_id = flow_id.clone();
    let accepted = tunnel
        .accept(
            inbound_response_headers,
            move |downstream_stream| async move {
                let downstream_config = websocket_codec_config();
                let mut downstream =
                    crate::websocket::server(downstream_stream, Some(downstream_config)).await;
                let mut upstream =
                    crate::websocket::client(upstream_stream, Some(websocket_codec_config())).await;
                let (messages, terminal) = relay_websocket(
                    &mut downstream,
                    &mut upstream,
                    &handler_session,
                    &handler_redaction,
                    &handler_profile,
                    handler_options,
                    lifecycle,
                )
                .await;
                let conversation = WebSocketConversation {
                    id: uuid::Uuid::new_v4().to_string(),
                    flow_id: conversation_flow_id,
                    offered_subprotocols: handler_offered,
                    selected_subprotocol: handler_selected,
                    messages,
                    terminal,
                };
                let _ = handler_session.append_websocket_conversation(conversation);
            },
        )
        .map_err(|_| ServiceError::internal("WebSocket tunnel could not be accepted"))?;

    let request_uri: Uri = upstream_uri
        .parse()
        .map_err(|_| ServiceError::internal("invalid upstream WebSocket URI"))?;
    let mut request_headers = http::HeaderMap::new();
    for header in normalize_websocket_handshake_headers(&inbound_fields, false) {
        let name = http::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| ServiceError::internal("invalid WebSocket header name"))?;
        let value = http::HeaderValue::from_bytes(header.value.as_bytes())
            .map_err(|_| ServiceError::internal("invalid WebSocket header value"))?;
        request_headers.append(name, value);
    }
    let recorded_request = request_head(&http::Method::GET, &request_uri, &request_headers)
        .map_err(|_| ServiceError::internal("failed to normalize WebSocket request"))?;
    let response_fields = normalize_websocket_handshake_headers(&upstream_header_entries, true);
    let response = HttpResponse {
        status: 101,
        headers: response_fields,
        body: BodyRef::Absent,
        trailers: Vec::new(),
    };
    let now = unix_millis();
    let mut flow = Flow {
        schema_version: eggreplay_core::SCHEMA_VERSION,
        id: flow_id,
        started_at_ms: now,
        completed_at_ms: Some(now),
        request: recorded_request.request,
        outcome: FlowOutcome::Response(response),
        physical_route: Some(physical_route),
        provenance: eggreplay_core::Provenance {
            mode: "eggfetch-native-websocket".into(),
            observer: "eggreplay-websocket-gateway".into(),
        },
        annotations: Vec::new(),
        redactions: Vec::new(),
    };
    eggreplay_core::redact_flow(&mut flow, &redaction, &profile_id);
    session
        .append_flow(&flow)
        .map_err(|_| ServiceError::internal("failed to stage WebSocket handshake"))?;
    Ok(accepted)
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn websocket_codec_config() -> crate::websocket::WebSocketConfig {
    crate::websocket::WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024))
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn websocket_error_cause(error: &crate::websocket::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};
    match error {
        Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => "reset",
        Error::Io(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => "eof",
        Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            "reset"
        }
        _ => "protocol-error",
    }
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
async fn relay_websocket<D, U>(
    downstream: &mut crate::websocket::WebSocketStream<D>,
    upstream: &mut crate::websocket::WebSocketStream<U>,
    session: &RecordingSession,
    redaction: &RedactionConfig,
    profile: &str,
    options: WebSocketRecordingOptions,
    lifecycle: eggserve_primitives::RequestLifecycle,
) -> (Vec<WebSocketMessage>, WebSocketTerminal)
where
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin,
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let started = Instant::now();
    let deadline = tokio::time::sleep(options.max_duration);
    tokio::pin!(deadline);
    let mut messages = Vec::new();
    let mut close_seen = [false, false];
    let mut downstream_open = true;
    let mut upstream_open = true;
    let mut total_payload_bytes = 0u64;
    let mut client_ping_pongs: Vec<Vec<u8>> = Vec::new();
    let mut server_ping_pongs: Vec<Vec<u8>> = Vec::new();
    let terminal = loop {
        let incoming = tokio::select! {
            _ = &mut deadline => break WebSocketTerminal::Abnormal { cause: "duration-limit".into() },
            _ = lifecycle.cancelled() => break WebSocketTerminal::Abnormal { cause: "shutdown".into() },
            value = downstream.next(), if downstream_open => (WebSocketDirection::ClientToServer, value),
            value = upstream.next(), if upstream_open => (WebSocketDirection::ServerToClient, value),
        };
        let (direction, value) = incoming;
        let message = match value {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                break WebSocketTerminal::Abnormal {
                    cause: websocket_error_cause(&error).into(),
                };
            }
            None => {
                match direction {
                    WebSocketDirection::ClientToServer => downstream_open = false,
                    WebSocketDirection::ServerToClient => upstream_open = false,
                }
                if close_seen[0] && close_seen[1] {
                    break WebSocketTerminal::CleanClose {
                        code: None,
                        reason: None,
                    };
                }
                if !close_seen[0] && !close_seen[1] {
                    break WebSocketTerminal::Abnormal {
                        cause: "eof".into(),
                    };
                }
                if !downstream_open && !upstream_open {
                    break WebSocketTerminal::Abnormal {
                        cause: "eof".into(),
                    };
                }
                continue;
            }
        };
        let message = message;
        let (kind, bytes, close_code, mut close_reason) = match &message {
            Message::Text(text) => (
                WebSocketMessageKind::Text,
                text.as_bytes().to_vec(),
                None,
                None,
            ),
            Message::Binary(data) => (WebSocketMessageKind::Binary, data.to_vec(), None, None),
            Message::Ping(data) => (WebSocketMessageKind::Ping, data.to_vec(), None, None),
            Message::Pong(data) => (WebSocketMessageKind::Pong, data.to_vec(), None, None),
            Message::Close(frame) => (
                WebSocketMessageKind::Close,
                Vec::new(),
                frame.as_ref().map(|f| u16::from(f.code)),
                frame
                    .as_ref()
                    .and_then(|f| (!f.reason.is_empty()).then(|| f.reason.to_string())),
            ),
            Message::Frame(_) => continue,
        };
        let mut suppress_forward = false;
        match (kind, direction) {
            (WebSocketMessageKind::Ping, WebSocketDirection::ClientToServer) => {
                client_ping_pongs.push(bytes.clone())
            }
            (WebSocketMessageKind::Ping, WebSocketDirection::ServerToClient) => {
                server_ping_pongs.push(bytes.clone())
            }
            (WebSocketMessageKind::Pong, WebSocketDirection::ServerToClient) => {
                if let Some(index) = client_ping_pongs
                    .iter()
                    .position(|payload| payload == &bytes)
                {
                    client_ping_pongs.remove(index);
                    suppress_forward = true;
                }
            }
            (WebSocketMessageKind::Pong, WebSocketDirection::ClientToServer) => {
                if let Some(index) = server_ping_pongs
                    .iter()
                    .position(|payload| payload == &bytes)
                {
                    server_ping_pongs.remove(index);
                    suppress_forward = true;
                }
            }
            _ => {}
        }
        let mut redacted = None;
        let mut payload_bytes = bytes;
        match kind {
            WebSocketMessageKind::Text if options.redact_text => {
                payload_bytes = b"[REDACTED]".to_vec();
                redacted = Some(WebSocketRedaction::WholeMessage);
            }
            WebSocketMessageKind::Text if !redaction.json_paths.is_empty() => {
                if let Ok((clean, _)) = apply_json_redaction(&payload_bytes, &redaction.json_paths)
                {
                    payload_bytes = clean;
                    redacted = Some(WebSocketRedaction::JsonPointers {
                        pointers: redaction.json_paths.iter().cloned().collect(),
                    });
                }
            }
            WebSocketMessageKind::Binary if options.redact_binary => {
                payload_bytes = b"[REDACTED]".to_vec();
                redacted = Some(WebSocketRedaction::WholeMessage);
            }
            _ => {}
        }
        if kind == WebSocketMessageKind::Close && options.redact_text && close_reason.is_some() {
            close_reason = Some("[REDACTED]".into());
            redacted = Some(WebSocketRedaction::CloseReason);
        }
        let payload = if payload_bytes.is_empty() {
            None
        } else {
            let Ok(mut writer) = session.begin_blob() else {
                break WebSocketTerminal::Abnormal {
                    cause: "storage-error".into(),
                };
            };
            if writer.write_all(&payload_bytes).is_err() {
                break WebSocketTerminal::Abnormal {
                    cause: "storage-error".into(),
                };
            }
            match writer.finish() {
                Ok(body) => Some(body),
                Err(_) => {
                    break WebSocketTerminal::Abnormal {
                        cause: "storage-error".into(),
                    };
                }
            }
        };
        let limits = eggreplay_core::WebSocketLimits::default();
        total_payload_bytes = total_payload_bytes.saturating_add(payload_bytes.len() as u64);
        if messages.len() >= limits.max_messages_per_conversation
            || total_payload_bytes > limits.max_total_payload_bytes
            || elapsed_ns(started) > limits.max_duration_ns
        {
            break WebSocketTerminal::Abnormal {
                cause: "capture-limit".into(),
            };
        }
        let seq = messages.len() as u64;
        messages.push(WebSocketMessage {
            sequence: seq,
            direction,
            delta_ns: elapsed_ns(started),
            kind,
            payload,
            close_code,
            close_reason: close_reason.clone(),
            redactions: redacted.into_iter().collect(),
        });
        if kind == WebSocketMessageKind::Close {
            let slot = match direction {
                WebSocketDirection::ClientToServer => 0,
                WebSocketDirection::ServerToClient => 1,
            };
            let duplicate = close_seen[slot] || close_seen[1 - slot];
            close_seen[slot] = true;
            if !duplicate {
                let sent = match direction {
                    WebSocketDirection::ClientToServer => upstream.send(message).await,
                    WebSocketDirection::ServerToClient => downstream.send(message).await,
                };
                if sent.is_err() {
                    break WebSocketTerminal::Abnormal {
                        cause: "write-error".into(),
                    };
                }
            }
            if close_seen[0] && close_seen[1] {
                break WebSocketTerminal::CleanClose {
                    code: close_code,
                    reason: close_reason,
                };
            }
            continue;
        }
        if suppress_forward {
            continue;
        }
        let sent = match direction {
            WebSocketDirection::ClientToServer => upstream.send(message).await,
            WebSocketDirection::ServerToClient => downstream.send(message).await,
        };
        if sent.is_err() {
            break WebSocketTerminal::Abnormal {
                cause: "write-error".into(),
            };
        }
    };
    let _ = profile;
    (messages, terminal)
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn eggreplay_http_valid_websocket_key(key: &str) -> bool {
    crate::websocket::valid_client_key(key)
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn single_header(headers: &[HeaderEntry], name: &str) -> Option<String> {
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name));
    let value = values.next()?.value.clone();
    values.next().is_none().then_some(value)
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn header_has_token(headers: &[HeaderEntry], name: &str, token: &str) -> bool {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .flat_map(|header| header.value.split(','))
        .any(|value| value.trim().eq_ignore_ascii_case(token))
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn offered_subprotocols(headers: &[HeaderEntry]) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("sec-websocket-protocol"))
        .flat_map(|header| header.value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(all(feature = "eggserve", feature = "websocket"))]
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(feature = "eggserve")]
fn eggserve_body_stream(
    body: eggserve_primitives::RequestBody,
) -> impl futures_util::Stream<Item = Result<Frame<Bytes>, BodyError>> + Send {
    futures_util::stream::unfold((body, false), |(mut body, trailers_emitted)| async move {
        if trailers_emitted {
            return None;
        }
        match body.next_chunk().await {
            Ok(Some(bytes)) => Some((Ok(Frame::data(bytes)), (body, false))),
            Ok(None) => {
                let next = match body.trailers().await {
                    Ok(Some(trailers)) => Some(Frame::trailers({
                        let mut map = http::HeaderMap::new();
                        for field in trailers.iter() {
                            if let (Ok(name), Ok(value)) = (
                                http::header::HeaderName::from_bytes(
                                    field.name.as_str().as_bytes(),
                                ),
                                http::HeaderValue::from_bytes(field.value.as_bytes()),
                            ) {
                                map.append(name, value);
                            }
                        }
                        map
                    })),
                    Ok(None) => None,
                    Err(error) => return Some((Err(BodyError(error.to_string())), (body, true))),
                };
                next.map(|frame| (Ok(frame), (body, true)))
            }
            Err(error) => Some((Err(BodyError(error.to_string())), (body, true))),
        }
    })
}

#[cfg(feature = "eggserve")]
fn file_stream(
    path: std::path::PathBuf,
) -> impl futures_util::Stream<Item = Result<Bytes, ResponseStreamError>> + Send {
    futures_util::stream::unfold((path, None), |(path, file)| async move {
        let mut file = match file {
            Some(file) => file,
            None => match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) => {
                    return Some((
                        Err(ResponseStreamError::new(error.to_string())),
                        (path, None),
                    ));
                }
            },
        };
        let mut buffer = vec![0u8; 64 * 1024];
        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(length) => {
                buffer.truncate(length);
                Some((Ok(Bytes::from(buffer)), (path, Some(file))))
            }
            Err(error) => Some((
                Err(ResponseStreamError::new(error.to_string())),
                (path, None),
            )),
        }
    })
}

#[cfg(feature = "eggserve")]
pub(crate) fn response_stream(
    path: Option<std::path::PathBuf>,
    length: u64,
    trailers: Vec<HeaderEntry>,
) -> ResponseStream {
    let bytes = match path {
        Some(path) => Box::pin(file_stream(path))
            as Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, ResponseStreamError>> + Send>>,
        None => Box::pin(futures_util::stream::empty())
            as Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, ResponseStreamError>> + Send>>,
    };
    ResponseStream::with_known_length_and_trailers(bytes, length, async move {
        if trailers.is_empty() {
            return Ok(None);
        }
        let mut block = HeaderBlock::new();
        for header in trailers {
            block
                .push_bytes(header.name, header.value.as_bytes())
                .map_err(|error| ResponseStreamError::new(error.to_string()))?;
        }
        Trailers::new(block)
            .map(Some)
            .map_err(|error| ResponseStreamError::new(error.to_string()))
    })
}

fn tee_body<B>(
    body: B,
    sink: Arc<Mutex<Option<eggreplay_store::BodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
) -> StreamBody<TeeStream<B>>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    StreamBody::new(TeeStream {
        body: Box::pin(body),
        sink,
        trailers,
        done: false,
    })
}

struct TeeStream<B> {
    body: Pin<Box<B>>,
    sink: Arc<Mutex<Option<eggreplay_store::BodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
    done: bool,
}

impl<B> Stream for TeeStream<B>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<Frame<Bytes>, BodyError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.body.as_mut().poll_frame(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.done = true;
                Poll::Ready(Some(Err(BodyError(error.to_string()))))
            }
            Poll::Ready(Some(Ok(frame))) => {
                if frame.is_data() {
                    let data = match frame.into_data() {
                        Ok(data) => data,
                        Err(_) => {
                            return Poll::Ready(Some(Err(BodyError("invalid data frame".into()))));
                        }
                    };
                    let result = this
                        .sink
                        .lock()
                        .map_err(|_| BodyError("request sink poisoned".into()))
                        .and_then(|mut guard| {
                            guard
                                .as_mut()
                                .ok_or_else(|| BodyError("request sink already closed".into()))
                                .and_then(|writer| {
                                    writer
                                        .write_all(&data)
                                        .map_err(|error| BodyError(error.to_string()))
                                })
                        });
                    Poll::Ready(Some(result.map(|()| Frame::data(data))))
                } else if frame.is_trailers() {
                    let header_map = match frame.into_trailers() {
                        Ok(headers) => headers,
                        Err(_) => {
                            return Poll::Ready(Some(Err(BodyError(
                                "invalid trailer frame".into(),
                            ))));
                        }
                    };
                    if let Ok(mut target) = this.trailers.lock() {
                        target.extend(header_entries(&header_map).0);
                    }
                    Poll::Ready(Some(Ok(Frame::trailers(header_map))))
                } else {
                    Poll::Ready(Some(Ok(frame)))
                }
            }
        }
    }
}

/// Finish a staged request/response body with pre-publication redaction.
///
/// Staging bytes transform before any finalized blob exists; raw staging is
/// dropped (Drop cleans) on transform or fail-closed paths, so sensitive
/// bytes never become addressable blobs.
fn finish_sink_redacted(
    sink: Arc<Mutex<Option<eggreplay_store::BodyWriter>>>,
    writer: &SessionWriter,
    content_type: Option<&str>,
    redaction: &RedactionConfig,
    profile_id: &str,
    max_structured_bytes: u64,
    is_response: bool,
) -> Result<(BodyRef, Vec<eggreplay_core::RedactionMarker>), HttpError> {
    let mut staged = sink
        .lock()
        .map_err(|_| HttpError::Body("body sink poisoned".into()))?
        .take()
        .ok_or_else(|| HttpError::Body("body sink already closed".into()))?;
    if staged.len() == 0 {
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let wants_body = !redaction.json_paths.is_empty() || !redaction.query_keys.is_empty();
    if !wants_body {
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let needs_transform = (!redaction.json_paths.is_empty() && is_json_content_type(content_type))
        || (!redaction.query_keys.is_empty() && is_form_content_type(content_type));
    if !needs_transform {
        // Fail closed when structured redaction was requested but this body
        // is neither JSON nor form (opaque). Drop staging without publishing.
        if !redaction.json_paths.is_empty() {
            drop(staged);
            return Err(HttpError::Body(
                "body redaction requested but media type unsupported; failing closed".into(),
            ));
        }
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let raw = staged
        .read_staging_bounded(max_structured_bytes)
        .map_err(|error| HttpError::Body(error.to_string()))?;
    // Abort raw staging (Drop cleans file) before publishing redacted.
    drop(staged);
    let (redacted, markers) =
        transform_body(&raw, content_type, redaction, profile_id, is_response)?;
    // Publish redacted bytes as the single finalized blob.
    if redacted.is_empty() {
        return Ok((BodyRef::Empty, markers));
    }
    let mut fresh = writer.begin_blob()?;
    fresh
        .write_all(&redacted)
        .map_err(|error| HttpError::Body(error.to_string()))?;
    let body_ref = fresh.finish()?;
    Ok((body_ref, markers))
}

fn finish_session_sink_redacted(
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
    session: &RecordingSession,
    content_type: Option<&str>,
    redaction: &RedactionConfig,
    profile_id: &str,
    max_structured_bytes: u64,
    is_response: bool,
) -> Result<(BodyRef, Vec<eggreplay_core::RedactionMarker>), HttpError> {
    let mut staged = sink
        .lock()
        .map_err(|_| HttpError::Body("body sink poisoned".into()))?
        .take()
        .ok_or_else(|| HttpError::Body("body sink already closed".into()))?;
    if staged.len() == 0 {
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let wants_body = !redaction.json_paths.is_empty() || !redaction.query_keys.is_empty();
    if !wants_body {
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let needs_transform = (!redaction.json_paths.is_empty() && is_json_content_type(content_type))
        || (!redaction.query_keys.is_empty() && is_form_content_type(content_type));
    if !needs_transform {
        if !redaction.json_paths.is_empty() {
            drop(staged);
            return Err(HttpError::Body(
                "body redaction requested but media type unsupported; failing closed".into(),
            ));
        }
        let body_ref = staged.finish()?;
        return Ok((body_ref, Vec::new()));
    }
    let raw = staged
        .read_staging_bounded(max_structured_bytes)
        .map_err(|error| HttpError::Body(error.to_string()))?;
    drop(staged);
    let (redacted, markers) =
        transform_body(&raw, content_type, redaction, profile_id, is_response)?;
    if redacted.is_empty() {
        return Ok((BodyRef::Empty, markers));
    }
    let mut fresh = session.begin_blob()?;
    fresh
        .write_all(&redacted)
        .map_err(|error| HttpError::Body(error.to_string()))?;
    let body_ref = fresh.finish()?;
    Ok((body_ref, markers))
}

fn tee_body_with_session<B>(
    body: B,
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
    events: Arc<Mutex<Vec<eggreplay_core::StreamEvent>>>,
    started: Instant,
) -> StreamBody<TeeSessionStream<B>>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    StreamBody::new(TeeSessionStream {
        body: Box::pin(body),
        sink,
        trailers,
        events,
        started,
        offset: 0,
        done: false,
    })
}

struct TeeSessionStream<B> {
    body: Pin<Box<B>>,
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
    events: Arc<Mutex<Vec<eggreplay_core::StreamEvent>>>,
    started: Instant,
    offset: u64,
    done: bool,
}

impl<B> Stream for TeeSessionStream<B>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<Frame<Bytes>, BodyError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.body.as_mut().poll_frame(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.done = true;
                if let Ok(mut events) = this.events.lock() {
                    let _ = push_stream_event_inner(
                        &mut events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(this.started),
                            event: eggreplay_core::StreamEventKind::End,
                        },
                    );
                }
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.done = true;
                if let Ok(mut events) = this.events.lock() {
                    let _ = push_stream_event_inner(
                        &mut events,
                        eggreplay_core::StreamEvent {
                            delta_ns: elapsed_ns(this.started),
                            event: eggreplay_core::StreamEventKind::Error {
                                offset: this.offset,
                                category: "other".into(),
                                phase: "body".into(),
                            },
                        },
                    );
                }
                Poll::Ready(Some(Err(BodyError(error.to_string()))))
            }
            Poll::Ready(Some(Ok(frame))) => {
                if frame.is_data() {
                    let data = match frame.into_data() {
                        Ok(data) => data,
                        Err(_) => {
                            return Poll::Ready(Some(Err(BodyError("invalid data frame".into()))));
                        }
                    };
                    let result = this
                        .sink
                        .lock()
                        .map_err(|_| BodyError("request sink poisoned".into()))
                        .and_then(|mut guard| {
                            guard
                                .as_mut()
                                .ok_or_else(|| BodyError("request sink already closed".into()))
                                .and_then(|writer| {
                                    writer
                                        .write_all(&data)
                                        .map_err(|error| BodyError(error.to_string()))
                                })
                        });
                    if result.is_ok() {
                        let length = data.len() as u64;
                        let recorded = this
                            .events
                            .lock()
                            .map_err(|_| BodyError("stream event registry poisoned".into()))
                            .and_then(|mut events| {
                                push_stream_event_inner(
                                    &mut events,
                                    eggreplay_core::StreamEvent {
                                        delta_ns: elapsed_ns(this.started),
                                        event: eggreplay_core::StreamEventKind::Data {
                                            offset: this.offset,
                                            length,
                                        },
                                    },
                                )
                                .map_err(BodyError)
                            });
                        if let Err(error) = recorded {
                            this.done = true;
                            return Poll::Ready(Some(Err(error)));
                        }
                        this.offset = this.offset.saturating_add(length);
                    }
                    Poll::Ready(Some(result.map(|()| Frame::data(data))))
                } else if frame.is_trailers() {
                    let header_map = match frame.into_trailers() {
                        Ok(headers) => headers,
                        Err(_) => {
                            return Poll::Ready(Some(Err(BodyError(
                                "invalid trailer frame".into(),
                            ))));
                        }
                    };
                    if let Ok(mut target) = this.trailers.lock() {
                        target.extend(header_entries(&header_map).0);
                    }
                    if let Ok(mut events) = this.events.lock() {
                        let fields = header_entries(&header_map).0;
                        if let Err(error) = push_stream_event_inner(
                            &mut events,
                            eggreplay_core::StreamEvent {
                                delta_ns: elapsed_ns(this.started),
                                event: eggreplay_core::StreamEventKind::Trailers { fields },
                            },
                        ) {
                            this.done = true;
                            return Poll::Ready(Some(Err(BodyError(error))));
                        }
                    }
                    Poll::Ready(Some(Ok(Frame::trailers(header_map))))
                } else {
                    Poll::Ready(Some(Ok(frame)))
                }
            }
        }
    }
}

fn take_trailers(trailers: Arc<Mutex<Vec<HeaderEntry>>>) -> Result<Vec<HeaderEntry>, HttpError> {
    Ok(std::mem::take(&mut *trailers.lock().map_err(|_| {
        HttpError::Body("trailer sink poisoned".into())
    })?))
}

fn request_head(
    method: &http::Method,
    uri: &Uri,
    headers: &http::HeaderMap,
) -> Result<RecordedRequest, HttpError> {
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| HttpError::Conversion("request URI has no scheme".into()))?;
    // Userinfo must never persist in logical authority.
    let raw_authority = uri
        .authority()
        .ok_or_else(|| HttpError::Conversion("request URI has no authority".into()))?
        .as_str();
    let authority = raw_authority
        .rsplit('@')
        .next()
        .unwrap_or(raw_authority)
        .to_owned();
    let path = uri.path().to_owned();
    let query = uri.query().map_or_else(Vec::new, |value| {
        url::form_urlencoded::parse(value.as_bytes())
            .map(|(key, value)| QueryPair {
                key: key.into_owned(),
                value: value.into_owned(),
            })
            .collect()
    });
    let (headers, lossy) = header_entries(headers);
    Ok(RecordedRequest {
        request: HttpRequest {
            method: method.as_str().into(),
            scheme: scheme.into(),
            authority,
            path,
            query,
            headers,
            body: BodyRef::Absent,
            trailers: Vec::new(),
        },
        lossy_header_values: lossy,
    })
}

fn header_entries(headers: &http::HeaderMap) -> (Vec<HeaderEntry>, bool) {
    let mut lossy = false;
    let entries = headers
        .iter()
        .map(|(name, value)| {
            let value = match value.to_str() {
                Ok(value) => value.to_owned(),
                Err(_) => {
                    lossy = true;
                    value
                        .as_bytes()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                }
            };
            HeaderEntry {
                name: name.as_str().to_ascii_lowercase(),
                value,
            }
        })
        .collect();
    (entries, lossy)
}

fn map_fetch_error(error: &FetchError) -> FlowError {
    let (category, phase) = match error {
        FetchError::Tls(_)
        | FetchError::CertificateVerification(_)
        | FetchError::HostnameVerification(_)
        | FetchError::TlsConfig(_)
        | FetchError::CaBundle(_) => (ErrorCategory::Tls, ErrorPhase::Tls),
        FetchError::Timeout { .. } | FetchError::TransportIoTimeout { .. } => {
            (ErrorCategory::Timeout, ErrorPhase::Timeout)
        }
        FetchError::Body(_)
        | FetchError::DecodedBodyTooLarge
        | FetchError::Decompression(_)
        | FetchError::DecompressionRatioExceeded => (ErrorCategory::Other, ErrorPhase::Body),
        FetchError::Protocol(_) | FetchError::Hyper(_) | FetchError::HyperClient(_) => {
            (ErrorCategory::Protocol, ErrorPhase::Headers)
        }
        FetchError::Connect(_) | FetchError::Io(_) | FetchError::Pool(_) => {
            (ErrorCategory::Unreachable, ErrorPhase::Connect)
        }
        _ => (ErrorCategory::Other, ErrorPhase::Other),
    };
    FlowError::new(category, phase, error.to_string())
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

fn monotonic_offset_ns() -> u64 {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

fn push_stream_event(
    events: &mut Vec<eggreplay_core::StreamEvent>,
    event: eggreplay_core::StreamEvent,
) -> Result<(), HttpError> {
    push_stream_event_inner(events, event).map_err(HttpError::Body)
}

fn push_stream_event_inner(
    events: &mut Vec<eggreplay_core::StreamEvent>,
    event: eggreplay_core::StreamEvent,
) -> Result<(), String> {
    const MAX_EVENTS_PER_DIRECTION: usize = eggreplay_core::stream::MAX_STREAM_EVENTS_PER_FLOW / 2;
    if matches!(
        &event.event,
        eggreplay_core::StreamEventKind::Data { length: 0, .. }
    ) {
        return Ok(());
    }
    if events.len() < MAX_EVENTS_PER_DIRECTION {
        events.push(event);
        return Ok(());
    }
    if let (Some(last), eggreplay_core::StreamEventKind::Data { offset, length }) =
        (events.last_mut(), &event.event)
        && let eggreplay_core::StreamEventKind::Data {
            offset: last_offset,
            length: last_length,
        } = &mut last.event
        && last_offset.saturating_add(*last_length) == *offset
    {
        *last_length = last_length.saturating_add(*length);
        return Ok(());
    }
    Err("stream event count exceeds configured limit".into())
}

fn take_stream_events(
    events: Arc<Mutex<Vec<eggreplay_core::StreamEvent>>>,
) -> Result<Vec<eggreplay_core::StreamEvent>, HttpError> {
    Ok(std::mem::take(&mut *events.lock().map_err(|_| {
        HttpError::Body("stream event registry poisoned".into())
    })?))
}

fn reconcile_stream_event_body_length(
    events: &mut Vec<eggreplay_core::StreamEvent>,
    outcome: &FlowOutcome,
) {
    let expected = match outcome {
        FlowOutcome::Response(response) => response.body.len().unwrap_or(0),
        FlowOutcome::Error(_) => return,
    };
    let actual = events.iter().fold(0u64, |total, event| {
        total.saturating_add(match &event.event {
            eggreplay_core::StreamEventKind::Data { length, .. } => *length,
            _ => 0,
        })
    });
    if actual == expected {
        return;
    }
    let first_data = events
        .iter()
        .position(|event| matches!(&event.event, eggreplay_core::StreamEventKind::Data { .. }));
    let first_delta = first_data.map_or(0, |index| events[index].delta_ns);
    events.retain(|event| !matches!(&event.event, eggreplay_core::StreamEventKind::Data { .. }));
    if expected > 0 {
        let insert_at = first_data.unwrap_or(events.len()).min(events.len());
        events.insert(
            insert_at,
            eggreplay_core::StreamEvent {
                delta_ns: first_delta,
                event: eggreplay_core::StreamEventKind::Data {
                    offset: 0,
                    length: expected,
                },
            },
        );
    }
    for event in events {
        if let eggreplay_core::StreamEventKind::Error { offset, .. } = &mut event.event {
            *offset = expected;
        }
    }
}

#[allow(dead_code)]
fn _size_hint<B: Body>(body: &B) -> SizeHint {
    body.size_hint()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_core::SessionMetadata;
    use eggreplay_store::{RecordingSession, StoreLimits};
    use http_body_util::Full;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(all(feature = "eggserve", feature = "websocket"))]
    #[tokio::test]
    async fn websocket_relay_records_bidirectional_messages_and_clean_close() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::duplex;
        use tokio_tungstenite::tungstenite::Message;

        let dir = std::env::temp_dir().join(format!("eggreplay-ws-relay-{}", now_ms()));
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let session_reader = session.clone();
        let (client_io, downstream_io) = duplex(4096);
        let (upstream_io, server_io) = duplex(4096);
        let mut client = crate::websocket::client(client_io, None).await;
        let mut downstream =
            crate::websocket::server(downstream_io, Some(websocket_codec_config())).await;
        let mut upstream =
            crate::websocket::client(upstream_io, Some(websocket_codec_config())).await;
        let mut server = crate::websocket::server(server_io, None).await;
        let lifecycle = eggserve_primitives::RequestLifecycle::from_shared(
            eggserve_primitives::RequestShared::new_active(),
        );
        let task = tokio::spawn(async move {
            relay_websocket(
                &mut downstream,
                &mut upstream,
                &session,
                &RedactionConfig::default_secure(),
                "test-v1",
                WebSocketRecordingOptions {
                    redact_text: true,
                    redact_binary: true,
                    ..WebSocketRecordingOptions::default()
                },
                lifecycle,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            client.send(Message::Ping(vec![1, 2].into())).await.unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap(),
                Message::Ping(vec![1, 2].into())
            );
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Pong(vec![1, 2].into())
            );
            server.send(Message::Ping(vec![3, 4].into())).await.unwrap();
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Ping(vec![3, 4].into())
            );
            assert_eq!(
                server.next().await.unwrap().unwrap(),
                Message::Pong(vec![3, 4].into())
            );
            client
                .send(Message::Binary(b"binary-secret".to_vec().into()))
                .await
                .unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap(),
                Message::Binary(b"binary-secret".to_vec().into())
            );
            server
                .send(Message::Binary(b"reply-secret".to_vec().into()))
                .await
                .unwrap();
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Binary(b"reply-secret".to_vec().into())
            );
            client
                .send(Message::Text("client-message".into()))
                .await
                .unwrap();
            assert_eq!(
                server.next().await.unwrap().unwrap(),
                Message::Text("client-message".into())
            );
            server
                .send(Message::Text("server-message".into()))
                .await
                .unwrap();
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Text("server-message".into())
            );
            client.send(Message::Close(None)).await.unwrap();
            assert!(matches!(
                server.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
            server.flush().await.unwrap();
            let (messages, terminal) = task.await.unwrap();
            assert_eq!(messages.len(), 10, "{messages:?} {terminal:?}");
            assert!(
                matches!(terminal, WebSocketTerminal::CleanClose { .. }),
                "{messages:?} {terminal:?}"
            );
            assert_eq!(messages[0].direction, WebSocketDirection::ClientToServer);
            assert_eq!(messages[1].direction, WebSocketDirection::ServerToClient);
            let recorded_text = messages
                .iter()
                .find(|message| {
                    message.kind == WebSocketMessageKind::Text
                        && message.direction == WebSocketDirection::ClientToServer
                })
                .unwrap();
            assert_eq!(
                recorded_text.redactions,
                vec![WebSocketRedaction::WholeMessage]
            );
            let stored = session_reader
                .read_body(recorded_text.payload.as_ref().unwrap())
                .unwrap();
            assert_eq!(stored, b"[REDACTED]");
            let recorded_binary = messages
                .iter()
                .find(|message| {
                    message.kind == WebSocketMessageKind::Binary
                        && message.direction == WebSocketDirection::ClientToServer
                })
                .unwrap();
            let stored_binary = session_reader
                .read_body(recorded_binary.payload.as_ref().unwrap())
                .unwrap();
            assert_eq!(stored_binary, b"[REDACTED]");
        })
        .await
        .expect("relay timed out");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(all(feature = "eggserve", feature = "websocket"))]
    #[tokio::test]
    async fn websocket_relay_records_server_initiated_clean_close() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::duplex;
        use tokio_tungstenite::tungstenite::Message;
        let dir = std::env::temp_dir().join(format!("eggreplay-ws-server-close-{}", now_ms()));
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let (client_io, downstream_io) = duplex(1024);
        let (upstream_io, server_io) = duplex(1024);
        let mut client = crate::websocket::client(client_io, None).await;
        let mut downstream =
            crate::websocket::server(downstream_io, Some(websocket_codec_config())).await;
        let mut upstream =
            crate::websocket::client(upstream_io, Some(websocket_codec_config())).await;
        let mut server = crate::websocket::server(server_io, None).await;
        let lifecycle = eggserve_primitives::RequestLifecycle::from_shared(
            eggserve_primitives::RequestShared::new_active(),
        );
        let task = tokio::spawn(async move {
            relay_websocket(
                &mut downstream,
                &mut upstream,
                &session,
                &RedactionConfig::default_secure(),
                "test-v1",
                WebSocketRecordingOptions::default(),
                lifecycle,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            server.send(Message::Close(None)).await.unwrap();
            assert!(matches!(
                client.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
            client.flush().await.unwrap();
            let (messages, terminal) = task.await.unwrap();
            assert_eq!(messages.len(), 2);
            assert!(matches!(terminal, WebSocketTerminal::CleanClose { .. }));
            assert_eq!(messages[0].direction, WebSocketDirection::ServerToClient);
            assert_eq!(messages[1].direction, WebSocketDirection::ClientToServer);
        })
        .await
        .expect("server close relay timed out");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(all(feature = "eggserve", feature = "websocket"))]
    #[tokio::test]
    async fn websocket_relay_marks_eof_abnormal() {
        use tokio::io::duplex;
        let dir = std::env::temp_dir().join(format!("eggreplay-ws-eof-{}", now_ms()));
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let (client_io, downstream_io) = duplex(1024);
        let (upstream_io, _server_io) = duplex(1024);
        let client = crate::websocket::client(client_io, None).await;
        let mut downstream =
            crate::websocket::server(downstream_io, Some(websocket_codec_config())).await;
        let mut upstream =
            crate::websocket::client(upstream_io, Some(websocket_codec_config())).await;
        let lifecycle = eggserve_primitives::RequestLifecycle::from_shared(
            eggserve_primitives::RequestShared::new_active(),
        );
        let task = tokio::spawn(async move {
            relay_websocket(
                &mut downstream,
                &mut upstream,
                &session,
                &RedactionConfig::default_secure(),
                "test-v1",
                WebSocketRecordingOptions::default(),
                lifecycle,
            )
            .await
        });
        drop(client);
        let (_, terminal) = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal,
            WebSocketTerminal::Abnormal {
                cause: "reset".into()
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(all(feature = "eggserve", feature = "websocket"))]
    #[tokio::test]
    async fn recording_gateway_captures_upgrade_and_leading_post_101_messages() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::Message;

        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut io, _) = upstream.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            loop {
                let count = io.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            let key = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("sec-websocket-key")
                        .then_some(value.trim())
                })
                .unwrap();
            let accept = crate::websocket::derive_accept_key(key.as_bytes());
            // The first unmasked server text frame deliberately shares the
            // write with 101 to verify EggFetch preserves post-upgrade bytes.
            let mut upgrade = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: chat\r\n\r\n").into_bytes();
            upgrade.extend_from_slice(&[0x81, 0x05, b'w', b'o', b'r', b'l', b'd']);
            io.write_all(&upgrade).await.unwrap();
            let mut socket = crate::websocket::server(io, Some(websocket_codec_config())).await;
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::Text("hello".into())
            );
            socket.send(Message::Text("again".into())).await.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
            socket.flush().await.unwrap();
        });
        let dir = std::env::temp_dir().join(format!("eggreplay-ws-gateway-{}", now_ms()));
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let upstream_uri: Uri = format!("http://{upstream_addr}").parse().unwrap();
        let server = start_recording_gateway_with_websockets(
            "127.0.0.1:0".parse().unwrap(),
            upstream_uri,
            client,
            session.clone(),
            StoreLimits::default().max_blob_bytes,
            RedactionConfig::default_secure(),
            "test-v1".into(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            eggreplay_core::PhysicalRoute {
                kind: "direct".into(),
                description: None,
            },
            WebSocketRecordingOptions {
                enabled: true,
                ..WebSocketRecordingOptions::default()
            },
        )
        .await
        .unwrap();
        let stream = tokio::net::TcpStream::connect(server.local_addr())
            .await
            .unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(format!("ws://{}/socket", server.local_addr()))
            .header("host", server.local_addr().to_string())
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-protocol", "chat")
            .body(())
            .unwrap();
        let (mut socket, response) = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            "chat"
        );
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("world".into())
        );
        socket.send(Message::Text("hello".into())).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("again".into())
        );
        socket.send(Message::Close(None)).await.unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
        socket.flush().await.unwrap();
        upstream_task.await.unwrap();
        server.shutdown();
        server.wait().await;
        session.shutdown();
        let fixture = session.finish().unwrap();
        let transcript: eggreplay_core::WebSocketTranscript = serde_json::from_slice(
            &fixture
                .read_extension("websocket-messages")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(fixture.manifest().flow_count, 1);
        assert_eq!(transcript.conversations.len(), 1);
        assert!(matches!(
            transcript.conversations[0].terminal,
            WebSocketTerminal::CleanClose { .. }
        ));
        assert_eq!(
            transcript.conversations[0].selected_subprotocol.as_deref(),
            Some("chat")
        );
        drop(fixture);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn conversion_preserves_repeated_headers_and_query_values() {
        let mut headers = http::HeaderMap::new();
        headers.append("x-test", http::HeaderValue::from_static("one"));
        headers.append("x-test", http::HeaderValue::from_static("two"));
        let uri: Uri = "http://example.test/path?a=1&a=2".parse().unwrap();
        let converted = request_head(&http::Method::POST, &uri, &headers).unwrap();
        assert_eq!(converted.request.query.len(), 2);
        assert_eq!(converted.request.headers.len(), 2);
    }

    #[tokio::test]
    async fn records_response_body_without_buffering_in_flow_model() {
        let destination = std::env::temp_dir().join(format!("eggreplay-recording-{}", now_ms()));
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .unwrap();
        let request = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1:1/test?q=x")
            .body(Full::new(Bytes::from_static(b"request")))
            .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let flow = record_request(
            &client,
            &mut writer,
            request,
            &RedactionConfig::default_secure(),
            "default-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(flow.outcome, FlowOutcome::Error(_)));
        let _ = writer.finish().unwrap();
        std::fs::remove_dir_all(destination).unwrap();
    }

    fn c002_temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-c002-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    /// Minimal delayed upstream: records start/end per connection, sleeps,
    /// then returns a fixed body. Supports GET and POST with Content-Length.
    async fn start_delayed_upstream(
        delay_ms: u64,
        response_body: Vec<u8>,
    ) -> (
        std::net::SocketAddr,
        Arc<std::sync::Mutex<Vec<(u64, u64)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let timings: Arc<std::sync::Mutex<Vec<(u64, u64)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let timings_clone = timings.clone();
        let body_clone = response_body.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let timings = timings_clone.clone();
                let body = body_clone.clone();
                tokio::spawn(async move {
                    let start = now_ms();
                    // Read headers until \r\n\r\n.
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let mut content_length = 0usize;
                    loop {
                        match socket.read(&mut tmp).await {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&tmp[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    // Parse Content-Length if present.
                                    let text = String::from_utf8_lossy(&buf);
                                    for line in text.lines() {
                                        if line.to_ascii_lowercase().starts_with("content-length:")
                                            && let Some(v) = line.split(':').nth(1)
                                        {
                                            content_length = v.trim().parse().unwrap_or(0);
                                        }
                                    }
                                    break;
                                }
                                if buf.len() > 64 * 1024 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    // Drain request body if any.
                    let header_end = buf
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4)
                        .unwrap_or(buf.len());
                    let mut already = buf.len().saturating_sub(header_end);
                    while already < content_length {
                        match socket.read(&mut tmp).await {
                            Ok(0) => break,
                            Ok(n) => already += n,
                            Err(_) => break,
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let end = now_ms();
                    timings.lock().unwrap().push((start, end));
                });
            }
        });
        (addr, timings, handle)
    }

    #[tokio::test]
    async fn gateway_overlapping_requests_are_concurrent() {
        let (upstream_addr, timings, upstream_handle) =
            start_delayed_upstream(300, b"upstream-ok".to_vec()).await;
        let dir = c002_temp("gateway-overlap");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let upstream_base: Uri = format!("http://{upstream_addr}").parse().unwrap();
        let gateway_bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = start_recording_gateway(
            gateway_bind,
            upstream_base,
            client.clone(),
            session.clone(),
            StoreLimits::default().max_blob_bytes,
            RedactionConfig::default_secure(),
            "default-v1".to_string(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            eggreplay_core::PhysicalRoute {
                kind: "direct".into(),
                description: Some("direct".into()),
            },
        )
        .await
        .unwrap();
        let gateway_addr = server.local_addr();
        let fetch = |path: &str| {
            let client = client.clone();
            let uri: Uri = format!("http://{gateway_addr}{path}").parse().unwrap();
            async move {
                let req = Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                let resp = client.execute_http_body_default(req).await.unwrap();
                let (parts, mut body) = resp.into_parts();
                assert_eq!(parts.status.as_u16(), 200);
                use http_body_util::BodyExt;
                let mut out = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.unwrap();
                    if frame.is_data() {
                        out.extend_from_slice(&frame.into_data().unwrap());
                    }
                }
                assert_eq!(out, b"upstream-ok");
            }
        };
        // Two overlapping gateway requests; second must start upstream before
        // first completes if the session lock does not serialize network awaits.
        let first = tokio::spawn(fetch("/one"));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let second = tokio::spawn(fetch("/two"));
        let (a, b) = tokio::join!(first, second);
        a.unwrap();
        b.unwrap();
        // Allow upstream timing writes to land.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let times = timings.lock().unwrap().clone();
        assert_eq!(times.len(), 2, "both upstreams must execute");
        let (s1, e1) = times[0];
        let (s2, e2) = times[1];
        let (first_start, first_end, second_start) =
            if s1 <= s2 { (s1, e1, s2) } else { (s2, e2, s1) };
        assert!(
            second_start < first_end,
            "second upstream must start before first completes ({first_start}-{first_end} vs {second_start})"
        );
        server.shutdown();
        server.wait().await;
        upstream_handle.abort();
        // Both flows recorded with unique IDs and correct count.
        assert_eq!(session.flow_count(), 2);
        session.shutdown();
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 2);
        let flows: Vec<_> = finalized
            .iter_flows()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let stream_events: eggreplay_core::StreamEvents =
            serde_json::from_slice(&finalized.read_extension("stream-events").unwrap().unwrap())
                .unwrap();
        assert_eq!(stream_events.flows.len(), 2);
        assert!(
            stream_events
                .flows
                .iter()
                .all(|events| flows.iter().any(|flow| flow.id == events.flow_id))
        );
        assert!(
            stream_events
                .flows
                .iter()
                .all(|events| !events.response.is_empty())
        );
        assert_ne!(flows[0].id, flows[1].id);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn gateway_concurrent_large_upload_download() {
        let big_response = vec![0xCDu8; 512 * 1024];
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(50, big_response.clone()).await;
        let dir = c002_temp("gateway-large");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let upstream_base: Uri = format!("http://{upstream_addr}").parse().unwrap();
        let server = start_recording_gateway(
            "127.0.0.1:0".parse().unwrap(),
            upstream_base,
            client.clone(),
            session.clone(),
            StoreLimits::default().max_blob_bytes,
            RedactionConfig::default_secure(),
            "default-v1".to_string(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            eggreplay_core::PhysicalRoute {
                kind: "direct".into(),
                description: Some("direct".into()),
            },
        )
        .await
        .unwrap();
        let gateway_addr = server.local_addr();
        let upload = vec![0xABu8; 256 * 1024];
        let fetch_large = |upload: Vec<u8>, expected: Vec<u8>| {
            let client = client.clone();
            async move {
                let uri: Uri = format!("http://{gateway_addr}/upload").parse().unwrap();
                let req = Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Full::new(Bytes::from(upload.clone())))
                    .unwrap();
                let resp = client.execute_http_body_default(req).await.unwrap();
                let (parts, mut body) = resp.into_parts();
                assert_eq!(parts.status.as_u16(), 200);
                use http_body_util::BodyExt;
                let mut out = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.unwrap();
                    if frame.is_data() {
                        out.extend_from_slice(&frame.into_data().unwrap());
                    }
                }
                assert_eq!(out, expected);
            }
        };
        let a = tokio::spawn(fetch_large(upload.clone(), big_response.clone()));
        let b = tokio::spawn(fetch_large(upload.clone(), big_response.clone()));
        let (ra, rb) = tokio::join!(a, b);
        ra.unwrap();
        rb.unwrap();
        server.shutdown();
        server.wait().await;
        upstream_handle.abort();
        assert_eq!(session.flow_count(), 2);
        session.shutdown();
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn cancellation_during_upload_leaves_no_flow_or_orphan() {
        // Use a delayed upstream so the record task hangs in the awaited
        // upstream transaction; aborting the task simulates Ctrl-C /
        // disconnect cancellation mid-upload/response.
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(2000, b"late".to_vec()).await;
        let dir = c002_temp("cancel-upload");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let upstream_base: Uri = format!("http://{upstream_addr}").parse().unwrap();
        let gateway = start_recording_gateway(
            "127.0.0.1:0".parse().unwrap(),
            upstream_base,
            client.clone(),
            session.clone(),
            StoreLimits::default().max_blob_bytes,
            RedactionConfig::default_secure(),
            "default-v1".to_string(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            eggreplay_core::PhysicalRoute {
                kind: "direct".into(),
                description: Some("direct".into()),
            },
        )
        .await
        .unwrap();
        let gateway_addr = gateway.local_addr();
        // Start a gateway request that will hang upstream, then abort the
        // client task mid-transaction.
        let client_clone = client.clone();
        let task = tokio::spawn(async move {
            let uri: Uri = format!("http://{gateway_addr}/hang").parse().unwrap();
            let req = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Full::new(Bytes::new()))
                .unwrap();
            let _ = client_clone.execute_http_body_default(req).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        task.abort();
        let _ = task.await;
        // Allow any in-flight gateway task to settle, then drain via shutdown.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        gateway.shutdown();
        gateway.wait().await;
        upstream_handle.abort();
        // Cancelled transactions must not leave a manifest reference; the
        // session may contain 0 or 1 flows depending on abort timing, but
        // any referenced blobs must be complete (no orphan referenced blobs).
        // For determinism, assert active sinks drained and finish succeeds.
        session.shutdown();
        for _ in 0..50 {
            if session.active_blobs() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(session.active_blobs(), 0);
        let finalized = session.finish().unwrap();
        // If a flow was appended before abort, its blobs must verify.
        for item in finalized.iter_flows().unwrap() {
            let flow = item.unwrap();
            flow.validate().unwrap();
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn failed_transaction_leaves_no_orphan_reference() {
        // Oversized body fails closed; no flow appended, no orphan blobs.
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(10, b"ok".to_vec()).await;
        let dir = c002_temp("failed-txn");
        let limits = StoreLimits {
            max_line_bytes: 4 * 1024 * 1024,
            max_blob_bytes: 1024,
            max_flows: 100,
            max_total_bytes: 512 * 1024 * 1024,
        };
        let session = RecordingSession::create(&dir, SessionMetadata::default(), limits).unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let big = vec![0u8; 8 * 1024];
        let uri: Uri = format!("http://{upstream_addr}/big").parse().unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .body(Full::new(Bytes::from(big)))
            .unwrap();
        let result = record_request_with_session(
            &client,
            &session,
            req,
            &RedactionConfig::default_secure(),
            "default-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            None,
        )
        .await;
        // Oversized request bodies fail the upstream body stream, which is
        // recorded as an Error flow without storing oversized bytes
        // (fail-closed, no orphan blobs).
        match result {
            Ok(flow) => {
                assert!(
                    matches!(flow.outcome, FlowOutcome::Error(_)),
                    "oversized must record Error outcome"
                );
                assert_eq!(session.flow_count(), 1);
            }
            Err(_) => {
                assert_eq!(session.flow_count(), 0);
            }
        }
        assert_eq!(session.active_blobs(), 0);
        upstream_handle.abort();
        session.shutdown();
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().blob_count, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shutdown_finalization_policy() {
        let dir = c002_temp("shutdown-policy");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        // Active transaction blocks finish.
        let held = session.begin_blob().unwrap();
        assert!(session.clone().finish().is_err());
        drop(held);
        assert_eq!(session.active_blobs(), 0);
        // Normal finish after drain succeeds.
        session.shutdown();
        let finalized = session.finish().unwrap();
        assert_eq!(finalized.manifest().flow_count, 0);
        std::fs::remove_dir_all(dir).ok();
        let _ = AtomicU64::new(0);
        let _ = Ordering::SeqCst;
    }

    fn c003_temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-c003-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    fn assert_no_sentinel_in_dir(dir: &std::path::Path, sentinel: &str) {
        let bytes = sentinel.as_bytes();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap();
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    stack.push(entry_path);
                } else if entry_path.is_file() {
                    let content = std::fs::read(&entry_path).unwrap();
                    assert!(
                        content.windows(bytes.len()).all(|window| window != bytes),
                        "sentinel leaked in {}",
                        entry_path.display()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn default_sensitive_headers_never_persisted() {
        let sentinel = "C003-SENTINEL-HEADER-9f8a7b6c5d";
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(10, b"ok".to_vec()).await;
        let dir = c003_temp("default-headers");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let uri: Uri = format!("http://{upstream_addr}/h").parse().unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Authorization", format!("Bearer {sentinel}"))
            .header("Cookie", format!("session={sentinel}"))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let flow = record_request_with_session(
            &client,
            &session,
            req,
            &RedactionConfig::default_secure(),
            "default-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            None,
        )
        .await
        .unwrap();
        // Markers present, values replaced.
        assert!(
            flow.redactions
                .iter()
                .any(|marker| marker.field.to_ascii_lowercase().contains("authorization"))
        );
        for header in &flow.request.headers {
            assert!(!header.value.contains(sentinel));
        }
        session.shutdown();
        let finalized = session.finish().unwrap();
        upstream_handle.abort();
        // Scan entire fixture directory.
        assert_no_sentinel_in_dir(&dir, sentinel);
        // Flow JSON itself must not contain sentinel.
        for item in finalized.iter_flows().unwrap() {
            let flow = item.unwrap();
            let json = serde_json::to_string(&flow).unwrap();
            assert!(!json.contains(sentinel));
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn configured_query_and_json_redacted_before_publication() {
        let sentinel_q = "C003-SENTINEL-QUERY-1a2b3c4d";
        let sentinel_j = "C003-SENTINEL-JSON-5e6f7a8b";
        let sentinel_r = "C003-SENTINEL-RESP-9c8d7e6f";
        let resp_body = format!(r#"{{"data":"ok","secret":"{sentinel_r}"}}"#).into_bytes();
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(10, resp_body).await;
        let dir = c003_temp("query-json");
        let session =
            RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let mut config = RedactionConfig::default_secure();
        config.query_keys.insert("api_key".into());
        config.json_paths.insert("/secret".into());
        let req_json = format!(r#"{{"user":"alice","secret":"{sentinel_j}"}}"#);
        let uri: Uri = format!("http://{upstream_addr}/submit?api_key={sentinel_q}&other=1")
            .parse()
            .unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(req_json)))
            .unwrap();
        let flow = record_request_with_session(
            &client,
            &session,
            req,
            &config,
            "custom-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            None,
        )
        .await
        .unwrap();
        // Query redacted, JSON markers present.
        for pair in &flow.request.query {
            if pair.key == "api_key" {
                assert_eq!(pair.value, "<redacted>");
            }
        }
        assert!(
            flow.redactions
                .iter()
                .any(|marker| marker.field.contains("api_key"))
        );
        assert!(
            flow.redactions
                .iter()
                .any(|marker| marker.field.contains("/secret"))
        );
        // Framing: Content-Length must equal redacted length, not stale.
        if let FlowOutcome::Response(response) = &flow.outcome
            && let Some(cl) = response
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("content-length"))
        {
            let declared: u64 = cl.value.parse().unwrap();
            assert_eq!(declared, response.body.len().unwrap_or(0));
        }
        session.shutdown();
        let finalized = session.finish().unwrap();
        upstream_handle.abort();
        for sentinel in [sentinel_q, sentinel_j, sentinel_r] {
            assert_no_sentinel_in_dir(&dir, sentinel);
        }
        for item in finalized.iter_flows().unwrap() {
            let flow = item.unwrap();
            let json = serde_json::to_string(&flow).unwrap();
            for sentinel in [sentinel_q, sentinel_j, sentinel_r] {
                assert!(!json.contains(sentinel));
            }
            // Markers must not contain secret values, only field paths.
            let markers = serde_json::to_string(&flow.redactions).unwrap();
            for sentinel in [sentinel_q, sentinel_j, sentinel_r] {
                assert!(!markers.contains(sentinel));
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn malformed_and_oversized_structured_body_fails_closed() {
        let sentinel = "C003-SENTINEL-MALFORMED-xyz123";
        let (upstream_addr, _timings, upstream_handle) =
            start_delayed_upstream(10, b"ok".to_vec()).await;
        let client = Client::builder().retry_canceled_requests(false).build();
        let mut config = RedactionConfig::default_secure();
        config.json_paths.insert("/secret".into());
        // Malformed JSON with redaction requested must fail, no flow/blob.
        {
            let dir = c003_temp("malformed");
            let session =
                RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                    .unwrap();
            let bad = format!(r#"{{invalid json "{sentinel}"}}"#);
            let uri: Uri = format!("http://{upstream_addr}/bad").parse().unwrap();
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(bad)))
                .unwrap();
            let result = record_request_with_session(
                &client,
                &session,
                req,
                &config,
                "custom-v1",
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                None,
            )
            .await;
            assert!(result.is_err(), "malformed must fail closed");
            let err = format!("{}", result.unwrap_err());
            assert!(!err.contains(sentinel), "logs must not print secrets");
            assert_eq!(session.flow_count(), 0);
            session.shutdown();
            let finalized = session.finish().unwrap();
            assert_eq!(finalized.manifest().flow_count, 0);
            assert_no_sentinel_in_dir(&dir, sentinel);
            std::fs::remove_dir_all(dir).ok();
        }
        // Oversized with small structured limit must fail closed.
        {
            let dir = c003_temp("oversized");
            let session =
                RecordingSession::create(&dir, SessionMetadata::default(), StoreLimits::default())
                    .unwrap();
            let big_json = format!(r#"{{"secret":"{sentinel}-{}"}}"#, "x".repeat(8 * 1024));
            let uri: Uri = format!("http://{upstream_addr}/big").parse().unwrap();
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(big_json)))
                .unwrap();
            let result = record_request_with_session(
                &client,
                &session,
                req,
                &config,
                "custom-v1",
                1024,
                None,
            )
            .await;
            assert!(result.is_err(), "oversized must fail closed");
            assert_eq!(session.flow_count(), 0);
            session.shutdown();
            let finalized = session.finish().unwrap();
            assert_eq!(finalized.manifest().flow_count, 0);
            assert_no_sentinel_in_dir(&dir, sentinel);
            std::fs::remove_dir_all(dir).ok();
        }
        upstream_handle.abort();
    }

    #[test]
    fn userinfo_never_reaches_persisted_authority() {
        let uri: Uri = "http://user:C003-SENTINEL-USERINFO@example.test/path"
            .parse()
            .unwrap();
        // Note: http::Uri may reject userinfo; construct via request_head with
        // a URI containing '@' in authority to prove stripping. Use a synthetic
        // authority string via http::Uri parsing of userinfo form if allowed,
        // otherwise directly assert redact_url strips.
        let redacted = eggreplay_core::redact_url(
            "http://user:C003-SENTINEL-USERINFO@example.test/path?x=1#frag",
        );
        assert!(!redacted.contains("C003-SENTINEL-USERINFO"));
        assert!(!redacted.contains("user"));
        let converted = request_head(&http::Method::GET, &uri, &http::HeaderMap::new());
        if let Ok(recorded) = converted {
            assert!(!recorded.request.authority.contains('@'));
            assert!(!recorded.request.authority.contains("user"));
        }
    }
}
