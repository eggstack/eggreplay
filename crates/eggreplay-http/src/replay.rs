//! EggServe-backed offline replay with an intentionally strict temporary selector.

use eggreplay_core::{
    BodyRef, ConsumptionMode, FlowOutcome, HeaderEntry, HttpRequest, MatchCandidate, MatchResult,
    Matcher, MatcherSession, QueryPair,
};
use eggreplay_store::{Session, StoreError};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[cfg(feature = "eggserve")]
use eggserve_primitives::{HeaderBlock, RequestBodyPolicy, Response, ResponseBody, StatusCode};
#[cfg(feature = "eggserve")]
use eggserve_server::{Server, ServerHandle, ServiceError, service_fn_with_policy};

/// Replay failures.
#[derive(Debug, Error)]
pub enum ReplayError {
    /// Fixture loading or body verification failed.
    #[error("fixture: {0}")]
    Store(#[from] StoreError),
    /// Replay state was poisoned or invalid.
    #[error("replay state: {0}")]
    State(String),
    /// EggServe could not start or serve the runtime.
    #[cfg(feature = "eggserve")]
    #[error("EggServe: {0}")]
    Serve(String),
}

#[derive(Clone)]
struct ReplayState {
    candidates: Vec<MatchCandidate>,
    response_bodies: Vec<Vec<u8>>,
    matcher: Matcher,
    session: MatcherSession,
}

/// A loaded replay session with isolated per-server consumption state.
#[derive(Clone)]
pub struct ReplayFixture {
    state: Arc<Mutex<ReplayState>>,
}

impl ReplayFixture {
    /// Load all flow metadata and response/request bodies from a validated session.
    pub fn load(session: &Session) -> Result<Self, ReplayError> {
        let mut candidates = Vec::new();
        let mut response_bodies = Vec::new();
        for item in session.iter_flows()? {
            let flow = item?;
            let request_body = body_bytes(session, &flow.request.body)?;
            let response_body = match &flow.outcome {
                FlowOutcome::Response(response) => body_bytes(session, &response.body)?,
                FlowOutcome::Error(_) => Vec::new(),
            };
            response_bodies.push(response_body);
            candidates.push(MatchCandidate::new(flow, request_body));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ReplayState {
                candidates,
                response_bodies,
                matcher: Matcher::strict(8),
                session: MatcherSession::new(),
            })),
        })
    }

    /// Start an EggServe H1 replay server on the requested address.
    #[cfg(feature = "eggserve")]
    pub async fn start(
        self,
        bind: SocketAddr,
        max_body_bytes: u64,
    ) -> Result<ServerHandle, ReplayError> {
        let state = self.state.clone();
        let service = service_fn_with_policy(
            move |request| {
                let state = state.clone();
                async move { handle_request(state, request).await }
            },
            RequestBodyPolicy::Stream {
                max_bytes: max_body_bytes,
            },
        );
        Server::builder()
            .bind(bind)
            .build()
            .map_err(|error| ReplayError::Serve(error.to_string()))?
            .start_with_service(service)
            .await
            .map_err(|error| ReplayError::Serve(error.to_string()))
    }
}

#[cfg(feature = "eggserve")]
async fn handle_request(
    state: Arc<Mutex<ReplayState>>,
    request: eggserve_primitives::Request,
) -> Result<Response, ServiceError> {
    let (head, body, connection) = request.into_parts();
    let (body, trailers) = body
        .read_all_with_trailers()
        .await
        .map_err(|error| ServiceError::internal(error.to_string()))?;
    let actual = request_from_eggserve(&head, &connection, &body, trailers.as_ref())
        .map_err(ServiceError::internal)?;
    let mut guard = state
        .lock()
        .map_err(|_| ServiceError::internal("replay state poisoned"))?;
    let candidates = guard.candidates.clone();
    let matcher = guard.matcher.clone();
    let result = matcher.select(
        &actual,
        &body,
        &candidates,
        ConsumptionMode::Once,
        &mut guard.session,
    );
    let index = match result {
        MatchResult::Matched(index) => index,
        MatchResult::Exhausted { .. } => {
            return response(409, &[], "eggreplay replay fixture exhausted\n");
        }
        MatchResult::NoMatch { .. } => return response(404, &[], "eggreplay replay no match\n"),
    };
    match &guard.candidates[index].flow.outcome {
        FlowOutcome::Response(response_flow) => response(
            response_flow.status,
            &response_flow.headers,
            guard.response_bodies[index].clone(),
        ),
        FlowOutcome::Error(error) => response(
            502,
            &[],
            format!("recorded upstream error: {:?}\n", error.category),
        ),
    }
}

#[cfg(feature = "eggserve")]
fn response(
    status: u16,
    headers: &[HeaderEntry],
    body: impl Into<Vec<u8>>,
) -> Result<Response, ServiceError> {
    let status =
        StatusCode::new(status).map_err(|error| ServiceError::internal(error.to_string()))?;
    let mut builder = Response::builder().status(status);
    for header in headers {
        builder = builder
            .header(header.name.clone(), header.value.clone())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
    }
    builder
        .body(ResponseBody::Bytes(body.into()))
        .map_err(|error| ServiceError::internal(error.to_string()))
}

#[cfg(feature = "eggserve")]
fn request_from_eggserve(
    head: &eggserve_primitives::RequestHead,
    connection: &eggserve_primitives::ConnectionInfo,
    body: &[u8],
    trailers: Option<&eggserve_primitives::Trailers>,
) -> Result<HttpRequest, String> {
    let query = head.target().query().map_or_else(Vec::new, |value| {
        url::form_urlencoded::parse(value.as_bytes())
            .map(|(key, value)| QueryPair {
                key: key.into_owned(),
                value: value.into_owned(),
            })
            .collect()
    });
    let headers = eggserve_headers(head.headers());
    let trailers = trailers.map(eggserve_trailers).unwrap_or_default();
    Ok(HttpRequest {
        method: head.method().as_str().into(),
        scheme: connection.scheme.as_str().into(),
        authority: head
            .authority()
            .map(|value| value.as_str().to_owned())
            .or_else(|| {
                headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("host"))
                    .map(|header| header.value.clone())
            })
            .unwrap_or_default(),
        path: head.target().path().into(),
        query,
        headers,
        body: if body.is_empty() {
            BodyRef::Empty
        } else {
            BodyRef::Absent
        },
        trailers,
    })
}

#[cfg(feature = "eggserve")]
fn eggserve_headers(headers: &HeaderBlock) -> Vec<HeaderEntry> {
    headers
        .iter()
        .map(|field| HeaderEntry {
            name: field.name.as_str().to_ascii_lowercase(),
            value: String::from_utf8_lossy(field.value.as_bytes()).into_owned(),
        })
        .collect()
}

#[cfg(feature = "eggserve")]
fn eggserve_trailers(trailers: &eggserve_primitives::Trailers) -> Vec<HeaderEntry> {
    trailers
        .iter()
        .map(|field| HeaderEntry {
            name: field.name.as_str().to_ascii_lowercase(),
            value: String::from_utf8_lossy(field.value.as_bytes()).into_owned(),
        })
        .collect()
}

fn body_bytes(session: &Session, body: &BodyRef) -> Result<Vec<u8>, StoreError> {
    match body {
        BodyRef::Absent | BodyRef::Empty => Ok(Vec::new()),
        BodyRef::Blob(blob) => session.read_blob(blob),
    }
}
