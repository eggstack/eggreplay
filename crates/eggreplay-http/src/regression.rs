//! Candidate replay through the same EggFetch native execution path.

use bytes::Bytes;
use eggfetch_core::{Client, Error as FetchError};
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, Flow, FlowError, FlowOutcome, HeaderEntry, HttpRequest,
    HttpResponse, QueryPair,
};
use http::{HeaderMap, HeaderValue, Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use std::time::{SystemTime, UNIX_EPOCH};
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
}

/// Candidate result plus bounded body bytes for semantic diffing.
#[derive(Debug, Clone)]
pub struct CandidateObservation {
    /// Semantic candidate flow.
    pub flow: Flow,
    /// Candidate response bytes up to the configured observation bound.
    pub response_body: Vec<u8>,
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
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|error| RegressionError::Conversion(error.to_string()))?;
    let mut headers = HeaderMap::new();
    for header in &request.headers {
        let name = http::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| RegressionError::Conversion(error.to_string()))?;
        let value = HeaderValue::from_bytes(header.value.as_bytes())
            .map_err(|error| RegressionError::Conversion(error.to_string()))?;
        headers.append(name, value);
    }
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
    let outcome = match result {
        Ok(response) => {
            let (parts, mut body) = response.into_parts();
            let mut bytes = Vec::new();
            let mut trailers = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|error| RegressionError::Body(error.to_string()))?;
                if frame.is_data() {
                    let data = frame
                        .into_data()
                        .map_err(|_| RegressionError::Body("invalid data frame".into()))?;
                    if bytes.len() as u64 + data.len() as u64 > max_body_bytes {
                        return Err(RegressionError::BodyLimit(max_body_bytes));
                    }
                    bytes.extend_from_slice(&data);
                } else if frame.is_trailers() {
                    let map = frame
                        .into_trailers()
                        .map_err(|_| RegressionError::Body("invalid trailer frame".into()))?;
                    trailers = map
                        .iter()
                        .map(|(name, value)| HeaderEntry {
                            name: name.as_str().into(),
                            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
                        })
                        .collect();
                }
            }
            let response_empty = bytes.is_empty();
            response_body = bytes;
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
    match error {
        FetchError::Timeout { .. } => FlowError::new(
            ErrorCategory::Timeout,
            ErrorPhase::Timeout,
            error.to_string(),
        ),
        FetchError::Tls(_)
        | FetchError::CertificateVerification(_)
        | FetchError::HostnameVerification(_) => {
            FlowError::new(ErrorCategory::Tls, ErrorPhase::Tls, error.to_string())
        }
        FetchError::Connect(_) | FetchError::Io(_) => FlowError::new(
            ErrorCategory::Unreachable,
            ErrorPhase::Connect,
            error.to_string(),
        ),
        FetchError::Body(_) => {
            FlowError::new(ErrorCategory::Other, ErrorPhase::Body, error.to_string())
        }
        _ => FlowError::new(ErrorCategory::Other, ErrorPhase::Other, error.to_string()),
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}
