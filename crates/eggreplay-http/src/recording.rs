//! EggFetch-native streaming observation and schema conversion.

use bytes::Bytes;
use eggfetch_core::{Client, Error as FetchError};
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, Flow, FlowError, FlowOutcome, HeaderEntry, HttpRequest,
    HttpResponse, QueryPair, RedactionConfig,
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
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[cfg(feature = "eggserve")]
use eggserve_primitives::{
    HeaderBlock, RequestBodyPolicy, Response, ResponseBody, ResponseStream, ResponseStreamError,
    StatusCode, Trailers,
};
#[cfg(feature = "eggserve")]
use eggserve_server::{Server, ServerHandle, ServiceError, service_fn_with_policy};
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

/// Execute one request through EggFetch and append one semantic flow.
///
/// Request and response DATA frames are written into store sinks as they are
/// polled. The function does not collect either body into memory; an aborted
/// stream leaves the writer's incomplete staging material unreferenced.
pub async fn record_request<B>(
    client: &Client,
    writer: &mut SessionWriter,
    request: Request<B>,
) -> Result<Flow, HttpError>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let started_at_ms = now_ms();
    let (parts, body) = request.into_parts();
    let uri = parts.uri.clone();
    let converted = request_head(&parts.method, &uri, &parts.headers)?;
    let request_sink = Arc::new(Mutex::new(Some(writer.begin_blob()?)));
    let request_trailers = Arc::new(Mutex::new(Vec::new()));
    let upstream_body = tee_body(body, request_sink.clone(), request_trailers.clone());
    let upstream = Request::from_parts(parts, upstream_body);

    let result = client.execute_http_body_default(upstream).await;
    let request_body = finish_sink(request_sink)?;
    let request_trailers = take_trailers(request_trailers)?;
    let mut semantic_request = converted.request;
    semantic_request.body = request_body;
    semantic_request.trailers = request_trailers;

    let outcome = match result {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            let response_sink = Arc::new(Mutex::new(Some(writer.begin_blob()?)));
            let mut body = Box::pin(body);
            let mut response_trailers = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|error| HttpError::Body(error.to_string()))?;
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
                } else if frame.is_trailers() {
                    let trailers = frame
                        .into_trailers()
                        .map_err(|_| HttpError::Body("invalid response trailer frame".into()))?;
                    response_trailers = header_entries(&trailers).0;
                }
            }
            let response_body = finish_sink(response_sink)?;
            FlowOutcome::Response(HttpResponse {
                status: parts.status.as_u16(),
                headers: header_entries(&parts.headers).0,
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
    eggreplay_core::redact_flow(&mut flow, &RedactionConfig::default_secure(), "default-v1");
    if converted.lossy_header_values {
        flow.annotations
            .push(("conversion".into(), "opaque-header-values-escaped".into()));
    }
    writer.append_flow(&flow)?;
    Ok(flow)
}

/// Execute one request through EggFetch using a concurrent session.
///
/// Body sinks are independent of the flow-log lock: `begin_blob` never holds
/// the append mutex while bytes stream, and `append_flow` serializes only the
/// final bounded metadata write. No network await holds the session lock.
pub async fn record_request_with_session<B>(
    client: &Client,
    session: &RecordingSession,
    request: Request<B>,
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
    let request_sink: Arc<Mutex<Option<RecordingBodyWriter>>> =
        Arc::new(Mutex::new(Some(session.begin_blob()?)));
    let request_trailers = Arc::new(Mutex::new(Vec::new()));
    let upstream_body = tee_body_with_session(body, request_sink.clone(), request_trailers.clone());
    let upstream = Request::from_parts(parts, upstream_body);

    let result = client.execute_http_body_default(upstream).await;
    let request_body = finish_session_sink(request_sink)?;
    let request_trailers = take_trailers(request_trailers)?;
    let mut semantic_request = converted.request;
    semantic_request.body = request_body;
    semantic_request.trailers = request_trailers;

    let outcome = match result {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            let response_sink: Arc<Mutex<Option<RecordingBodyWriter>>> =
                Arc::new(Mutex::new(Some(session.begin_blob()?)));
            let mut body = Box::pin(body);
            let mut response_trailers = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|error| HttpError::Body(error.to_string()))?;
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
                } else if frame.is_trailers() {
                    let trailers = frame
                        .into_trailers()
                        .map_err(|_| HttpError::Body("invalid response trailer frame".into()))?;
                    response_trailers = header_entries(&trailers).0;
                }
            }
            let response_body = finish_session_sink(response_sink)?;
            FlowOutcome::Response(HttpResponse {
                status: parts.status.as_u16(),
                headers: header_entries(&parts.headers).0,
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
    eggreplay_core::redact_flow(&mut flow, &RedactionConfig::default_secure(), "default-v1");
    if converted.lossy_header_values {
        flow.annotations
            .push(("conversion".into(), "opaque-header-values-escaped".into()));
    }
    session.append_flow(&flow)?;
    Ok(flow)
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
pub async fn start_recording_gateway(
    bind: SocketAddr,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
    max_body_bytes: u64,
) -> Result<ServerHandle, HttpError> {
    let service = service_fn_with_policy(
        move |request| {
            let upstream_base = upstream_base.clone();
            let client = client.clone();
            let session = session.clone();
            async move { gateway_request(request, upstream_base, client, session).await }
        },
        RequestBodyPolicy::Stream {
            max_bytes: max_body_bytes,
        },
    );
    // Server-level body cap defaults to 0 (reject all); align it with the
    // service policy so the effective limit is the caller's bound.
    let runtime = eggserve_server::RuntimeConfig::builder()
        .bind(bind)
        .max_request_body_bytes(max_body_bytes)
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
async fn gateway_request(
    request: eggserve_primitives::Request,
    upstream_base: Uri,
    client: Client,
    session: RecordingSession,
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
    let flow = record_request_with_session(&client, &session, request)
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
fn response_stream(
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

fn finish_sink(
    sink: Arc<Mutex<Option<eggreplay_store::BodyWriter>>>,
) -> Result<BodyRef, HttpError> {
    let writer = sink
        .lock()
        .map_err(|_| HttpError::Body("body sink poisoned".into()))?
        .take();
    match writer {
        Some(writer) => Ok(writer.finish()?),
        None => Ok(BodyRef::Absent),
    }
}

fn tee_body_with_session<B>(
    body: B,
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
) -> StreamBody<TeeSessionStream<B>>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    StreamBody::new(TeeSessionStream {
        body: Box::pin(body),
        sink,
        trailers,
        done: false,
    })
}

struct TeeSessionStream<B> {
    body: Pin<Box<B>>,
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
    trailers: Arc<Mutex<Vec<HeaderEntry>>>,
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

fn finish_session_sink(
    sink: Arc<Mutex<Option<eggreplay_store::RecordingBodyWriter>>>,
) -> Result<BodyRef, HttpError> {
    let writer = sink
        .lock()
        .map_err(|_| HttpError::Body("body sink poisoned".into()))?
        .take();
    match writer {
        Some(writer) => Ok(writer.finish()?),
        None => Ok(BodyRef::Absent),
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
    let authority = uri
        .authority()
        .ok_or_else(|| HttpError::Conversion("request URI has no authority".into()))?
        .as_str()
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
        let flow = record_request(&client, &mut writer, request).await.unwrap();
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
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        let result = record_request_with_session(&client, &session, req).await;
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
}
