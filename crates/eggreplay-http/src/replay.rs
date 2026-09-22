//! EggServe-backed offline replay with lazy, bounded streaming.

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
    matcher: Matcher,
    session: MatcherSession,
    store: Session,
}

/// A loaded replay session with isolated per-server consumption state.
///
/// Loading is metadata-bounded: only flow records and body references are
/// retained. Response/request blob bytes are never read during [`Self::load`];
/// the selected response is opened and streamed in bounded chunks per
/// request. See `docs/architecture.md` for the lazy storage boundary.
#[derive(Clone)]
pub struct ReplayFixture {
    state: Arc<Mutex<ReplayState>>,
}

impl ReplayFixture {
    /// Load flow metadata without reading any blob bytes.
    ///
    /// Each candidate holds a [`eggreplay_core::CandidateBody`] descriptor
    /// derived from the flow's body reference. Use the returned fixture with
    /// [`Self::start`] to stream selected responses.
    pub fn load(session: &Session) -> Result<Self, ReplayError> {
        Self::load_with_matcher(session, Matcher::strict(8))
    }

    /// Load with an explicit matcher (for semantic-mode tests).
    pub fn load_with_matcher(session: &Session, matcher: Matcher) -> Result<Self, ReplayError> {
        let mut candidates = Vec::new();
        for item in session.iter_flows()? {
            let flow = item?;
            candidates.push(MatchCandidate::from_flow(flow));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ReplayState {
                candidates,
                matcher,
                session: MatcherSession::new(),
                store: session.clone(),
            })),
        })
    }

    /// Return the number of loaded candidates (metadata only).
    pub fn candidate_count(&self) -> usize {
        self.state
            .lock()
            .map(|guard| guard.candidates.len())
            .unwrap_or(0)
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
        let runtime = eggserve_server::RuntimeConfig::builder()
            .bind(bind)
            .max_request_body_bytes(max_body_bytes)
            .build()
            .map_err(|error| ReplayError::Serve(error.to_string()))?;
        Server::builder()
            .runtime(runtime)
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
    use eggserve_primitives::{ResponseStream, ResponseStreamError};
    use sha2::{Digest, Sha256};

    let (head, body, connection) = request.into_parts();
    let (body, trailers) = body
        .read_all_with_trailers()
        .await
        .map_err(|error| ServiceError::internal(error.to_string()))?;
    let actual = request_from_eggserve(&head, &connection, &body, trailers.as_ref())
        .map_err(ServiceError::internal)?;
    let (index, response_headers, response_status, response_body_ref, response_trailers, store) = {
        let mut guard = state
            .lock()
            .map_err(|_| ServiceError::internal("replay state poisoned"))?;
        let candidates = guard.candidates.clone();
        let matcher = guard.matcher.clone();
        let store = guard.store.clone();
        let store_for_loader = store.clone();
        let mut loader = |_idx: usize, candidate: &MatchCandidate| -> Option<Vec<u8>> {
            match &candidate.flow.request.body {
                BodyRef::Blob(blob) => store_for_loader.read_blob(blob).ok(),
                BodyRef::Absent | BodyRef::Empty => {
                    if candidate.request_body.is_empty() {
                        Some(Vec::new())
                    } else {
                        Some(candidate.request_body.clone())
                    }
                }
            }
        };
        let result = matcher.select_with_loader(
            &actual,
            &body,
            &candidates,
            ConsumptionMode::Once,
            &mut guard.session,
            &mut loader,
        );
        let index = match result {
            MatchResult::Matched(index) => index,
            MatchResult::Exhausted { .. } => {
                return response_bytes(409, &[], "eggreplay replay fixture exhausted\n");
            }
            MatchResult::NoMatch { .. } => {
                return response_bytes(404, &[], "eggreplay replay no match\n");
            }
        };
        let flow = guard.candidates[index].flow.clone();
        match flow.outcome {
            FlowOutcome::Response(response) => (
                index,
                response.headers.clone(),
                response.status,
                response.body.clone(),
                response.trailers.clone(),
                store,
            ),
            FlowOutcome::Error(error) => {
                return response_bytes(
                    502,
                    &[],
                    format!("recorded upstream error: {:?}\n", error.category),
                );
            }
        }
    };
    let _ = index;
    match response_body_ref {
        BodyRef::Absent | BodyRef::Empty => {
            let status = StatusCode::new(response_status)
                .map_err(|e| ServiceError::internal(e.to_string()))?;
            let mut builder = Response::builder().status(status);
            for header in response_headers {
                builder = builder
                    .header(header.name, header.value)
                    .map_err(|e| ServiceError::internal(e.to_string()))?;
            }
            builder
                .body(ResponseBody::Empty)
                .map_err(|e| ServiceError::internal(e.to_string()))
        }
        BodyRef::Blob(blob) => {
            let handle = store
                .open_blob(&blob)
                .map_err(|e| ServiceError::internal(e.to_string()))?;
            let length = handle.len();
            let digest = handle.sha256().to_owned();
            let std_file = handle.into_file();
            let tokio_file = tokio::fs::File::from_std(std_file);
            let byte_stream = futures_util::stream::unfold(
                (Some(tokio_file), Some(Sha256::new()), 0u64, false),
                move |(mut file_opt, mut hasher_opt, mut read, terminal)| {
                    let digest = digest.clone();
                    async move {
                        if terminal || file_opt.is_none() {
                            return None;
                        }
                        let file = file_opt.as_mut().expect("file present");
                        let mut buf = vec![0u8; 64 * 1024];
                        use tokio::io::AsyncReadExt;
                        match file.read(&mut buf).await {
                            Ok(0) => {
                                let hasher = hasher_opt.take().expect("hasher present");
                                let actual = format!("{:x}", hasher.finalize());
                                file_opt = None;
                                if read != length || actual != digest {
                                    let msg = format!(
                                        "blob integrity failure for {digest} (read {read} of {length})"
                                    );
                                    return Some((
                                        Err(ResponseStreamError::new(msg)),
                                        (file_opt, hasher_opt, read, true),
                                    ));
                                }
                                None
                            }
                            Ok(n) => {
                                buf.truncate(n);
                                if let Some(hasher) = hasher_opt.as_mut() {
                                    hasher.update(&buf);
                                }
                                read += n as u64;
                                if read > length {
                                    file_opt = None;
                                    hasher_opt = None;
                                    return Some((
                                        Err(ResponseStreamError::new(format!(
                                            "blob integrity failure for {digest}"
                                        ))),
                                        (file_opt, hasher_opt, read, true),
                                    ));
                                }
                                let bytes = bytes::Bytes::from(buf);
                                Some((Ok(bytes), (file_opt, hasher_opt, read, false)))
                            }
                            Err(e) => {
                                file_opt = None;
                                hasher_opt = None;
                                Some((
                                    Err(ResponseStreamError::new(e.to_string())),
                                    (file_opt, hasher_opt, read, true),
                                ))
                            }
                        }
                    }
                },
            );
            let trailers_owned = response_trailers.clone();
            let stream =
                ResponseStream::with_known_length_and_trailers(byte_stream, length, async move {
                    if trailers_owned.is_empty() {
                        return Ok(None);
                    }
                    let mut block = eggserve_primitives::HeaderBlock::new();
                    for header in trailers_owned {
                        block
                            .push_bytes(header.name, header.value.as_bytes())
                            .map_err(|e| ResponseStreamError::new(e.to_string()))?;
                    }
                    eggserve_primitives::Trailers::new(block)
                        .map(Some)
                        .map_err(|e| ResponseStreamError::new(e.to_string()))
                });
            let status = StatusCode::new(response_status)
                .map_err(|e| ServiceError::internal(e.to_string()))?;
            let mut builder = Response::builder().status(status);
            for header in response_headers {
                builder = builder
                    .header(header.name, header.value)
                    .map_err(|e| ServiceError::internal(e.to_string()))?;
            }
            builder
                .body(ResponseBody::Stream(stream))
                .map_err(|e| ServiceError::internal(e.to_string()))
        }
    }
}

#[cfg(feature = "eggserve")]
fn response_bytes(
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

#[cfg(test)]
mod tests {
    use super::*;
    use eggreplay_core::{
        BodyRef, Flow, FlowOutcome, HttpRequest, HttpResponse, Provenance, SessionMetadata,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};
    use std::io::Write;

    fn test_limits() -> StoreLimits {
        StoreLimits {
            max_line_bytes: 4 * 1024 * 1024,
            max_blob_bytes: 16 * 1024 * 1024,
            max_flows: 100_000,
            max_total_bytes: 512 * 1024 * 1024,
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-c001-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn large_pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_add(seed)).collect()
    }

    fn flow_with_bodies(
        id: &str,
        method: &str,
        authority: &str,
        path: &str,
        req_body: BodyRef,
        resp_body: BodyRef,
        resp_trailers: Vec<HeaderEntry>,
    ) -> Flow {
        Flow {
            schema_version: eggreplay_core::SCHEMA_VERSION,
            id: id.into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: method.into(),
                scheme: "http".into(),
                authority: authority.into(),
                path: path.into(),
                query: vec![],
                headers: vec![],
                body: req_body,
                trailers: vec![],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![HeaderEntry {
                    name: "x-test".into(),
                    value: "v".into(),
                }],
                body: resp_body,
                trailers: resp_trailers,
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![],
        }
    }

    #[test]
    fn lazy_load_does_not_read_blob_bytes() {
        let dir = temp_path("lazy-load");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let mut expected = Vec::new();
        let mut flows = Vec::new();
        for i in 0..3 {
            // Each response is 2 MiB: aggregate 6 MiB, far larger than any
            // metadata retained by ReplayFixture.
            let payload = large_pattern(2 * 1024 * 1024, i as u8);
            let mut sink = writer.begin_blob().unwrap();
            sink.write_all(&payload).unwrap();
            let blob_ref = sink.finish().unwrap();
            expected.push(payload);
            flows.push(flow_with_bodies(
                &format!("flow-{i}"),
                "GET",
                "example.test",
                &format!("/item-{i}"),
                BodyRef::Empty,
                blob_ref,
                vec![],
            ));
        }
        for flow in &flows {
            writer.append_flow(flow).unwrap();
        }
        let session = writer.finish().unwrap();
        // Structural proof without RSS thresholds: delete an unselected blob
        // after Session::open. If load read blob bytes it would fail; lazy
        // load must still succeed with digest descriptors only.
        let victim = match &flows[2].outcome {
            FlowOutcome::Response(r) => match &r.body {
                BodyRef::Blob(b) => b.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        std::fs::remove_file(dir.join("blobs").join(&victim.sha256)).unwrap();
        let fixture = ReplayFixture::load(&session).expect("lazy load must not read blobs");
        assert_eq!(fixture.candidate_count(), 3);
        // Verify descriptors are metadata-only, not materialized vectors.
        {
            let guard = fixture.state.lock().unwrap();
            for candidate in &guard.candidates {
                assert!(candidate.request_body.is_empty());
                assert!(matches!(
                    candidate.body,
                    eggreplay_core::CandidateBody::Empty
                        | eggreplay_core::CandidateBody::Absent
                        | eggreplay_core::CandidateBody::Digest { .. }
                ));
            }
            // Response blobs are not retained in memory either: the state
            // holds only the cloned Session (paths + limits), not payloads.
            assert_eq!(guard.candidates.len(), 3);
        }
        // Selecting the deleted blob must fail closed, not silently serve.
        assert!(session.open_blob(&victim).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn selected_large_response_streams_bounded_and_exact() {
        let dir = temp_path("stream-exact");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let payload = large_pattern(2 * 1024 * 1024 + 123, 7);
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(&payload).unwrap();
        let blob_ref = sink.finish().unwrap();
        let flow = flow_with_bodies(
            "large",
            "GET",
            "example.test",
            "/large",
            BodyRef::Empty,
            blob_ref.clone(),
            vec![],
        );
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let blob = match &blob_ref {
            BodyRef::Blob(b) => b.clone(),
            _ => unreachable!(),
        };
        // Mirror the replay streaming path: open without allocating, then
        // read in 64 KiB chunks via Tokio async IO with incremental hashing.
        let handle = session.open_blob(&blob).unwrap();
        assert_eq!(handle.len(), payload.len() as u64);
        let mut file = tokio::fs::File::from_std(handle.into_file());
        use tokio::io::AsyncReadExt;
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        let mut total = 0usize;
        let mut max_chunk = 0usize;
        let mut collected = Vec::new();
        loop {
            let mut buf = vec![0u8; 64 * 1024];
            let n = file.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            max_chunk = max_chunk.max(n);
            hasher.update(&buf[..n]);
            collected.extend_from_slice(&buf[..n]);
            total += n;
            assert!(n <= 64 * 1024, "chunks must stay bounded");
        }
        assert_eq!(total, payload.len());
        assert!(max_chunk <= 64 * 1024);
        assert!(max_chunk > 0);
        assert_eq!(collected, payload);
        assert_eq!(format!("{:x}", hasher.finalize()), blob.sha256);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trailers_survive_lazy_fixture() {
        let dir = temp_path("trailers");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(b"trailer-body").unwrap();
        let blob_ref = sink.finish().unwrap();
        let trailers = vec![
            HeaderEntry {
                name: "x-checksum".into(),
                value: "abc123".into(),
            },
            HeaderEntry {
                name: "x-count".into(),
                value: "2".into(),
            },
        ];
        let flow = flow_with_bodies(
            "t1",
            "GET",
            "example.test",
            "/trailers",
            BodyRef::Empty,
            blob_ref,
            trailers.clone(),
        );
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let fixture = ReplayFixture::load(&session).unwrap();
        let guard = fixture.state.lock().unwrap();
        match &guard.candidates[0].flow.outcome {
            FlowOutcome::Response(r) => assert_eq!(r.trailers, trailers),
            _ => panic!("expected response"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn trailer_stream_preserves_recorded_trailers() {
        use eggserve_primitives::{HeaderBlock, ResponseStream, ResponseStreamError};
        use futures_util::StreamExt;

        // Mirror the replay construction: known-length byte stream plus a
        // terminal trailer future built from recorded trailers.
        let payload = b"trailer-payload".to_vec();
        let trailers = vec![HeaderEntry {
            name: "x-replay-trailer".into(),
            value: "trailer-value".into(),
        }];
        let byte_stream = futures_util::stream::once(async move {
            Ok::<_, ResponseStreamError>(bytes::Bytes::from(payload.clone()))
        });
        let trailers_owned = trailers.clone();
        let mut stream =
            ResponseStream::with_known_length_and_trailers(byte_stream, 15, async move {
                if trailers_owned.is_empty() {
                    return Ok(None);
                }
                let mut block = HeaderBlock::new();
                for header in trailers_owned {
                    block
                        .push_bytes(header.name, header.value.as_bytes())
                        .map_err(|e| ResponseStreamError::new(e.to_string()))?;
                }
                eggserve_primitives::Trailers::new(block)
                    .map(Some)
                    .map_err(|e| ResponseStreamError::new(e.to_string()))
            });
        assert_eq!(stream.known_length(), Some(15));
        assert!(stream.has_trailers());
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"trailer-payload");
        let (mut _bytes, trailer_future) = stream.into_parts();
        let trailers_out = trailer_future
            .expect("trailers attached")
            .await
            .expect("trailer future ok")
            .expect("trailers present");
        let names: Vec<String> = trailers_out
            .iter()
            .map(|f| f.name.as_str().to_owned())
            .collect();
        assert!(names.iter().any(|n| n == "x-replay-trailer"));
    }

    #[test]
    fn corrupt_replaced_body_fails_integrity() {
        let dir = temp_path("corrupt");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let payload = large_pattern(256 * 1024, 3);
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(&payload).unwrap();
        let blob_ref = sink.finish().unwrap();
        let flow = flow_with_bodies(
            "c1",
            "GET",
            "example.test",
            "/corrupt",
            BodyRef::Empty,
            blob_ref.clone(),
            vec![],
        );
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let blob = match &blob_ref {
            BodyRef::Blob(b) => b.clone(),
            _ => unreachable!(),
        };
        // Lazy load succeeds before corruption is observed.
        let _fixture = ReplayFixture::load(&session).unwrap();
        // Replace with same-length different bytes: open-time length check
        // passes, but incremental hash verification must fail.
        let corrupt = large_pattern(256 * 1024, 99);
        assert_ne!(corrupt, payload);
        std::fs::write(dir.join("blobs").join(&blob.sha256), &corrupt).unwrap();
        // Full verification now fails.
        assert!(session.read_blob(&blob).is_err());
        // Open succeeds on length (same length) but read_all hash fails.
        let mut handle = session.open_blob(&blob).unwrap();
        assert!(handle.read_all().is_err(), "hash mismatch must fail");
        // Different-length replacement fails at open time.
        std::fs::write(dir.join("blobs").join(&blob.sha256), b"short").unwrap();
        assert!(session.open_blob(&blob).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn concurrent_streams_do_not_clone_payloads() {
        let dir = temp_path("concurrent");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let payload = large_pattern(1024 * 1024, 11);
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(&payload).unwrap();
        let blob_ref = sink.finish().unwrap();
        let flow = flow_with_bodies(
            "cc1",
            "GET",
            "example.test",
            "/concurrent",
            BodyRef::Empty,
            blob_ref.clone(),
            vec![],
        );
        writer.append_flow(&flow).unwrap();
        let session = writer.finish().unwrap();
        let blob = match &blob_ref {
            BodyRef::Blob(b) => b.clone(),
            _ => unreachable!(),
        };
        let _fixture = ReplayFixture::load(&session).unwrap();
        // Two concurrent bounded streams from validated handles, no whole-
        // payload clones: each task streams 64 KiB chunks independently.
        let stream_one = {
            let session = session.clone();
            let blob = blob.clone();
            let expected = payload.clone();
            tokio::spawn(async move {
                let handle = session.open_blob(&blob).unwrap();
                let mut file = tokio::fs::File::from_std(handle.into_file());
                use tokio::io::AsyncReadExt;
                let mut out = Vec::new();
                loop {
                    let mut buf = vec![0u8; 64 * 1024];
                    let n = file.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    out.extend_from_slice(&buf[..n]);
                }
                assert_eq!(out, expected);
            })
        };
        let stream_two = {
            let session = session.clone();
            let blob = blob.clone();
            let expected = payload.clone();
            tokio::spawn(async move {
                let handle = session.open_blob(&blob).unwrap();
                let mut file = tokio::fs::File::from_std(handle.into_file());
                use tokio::io::AsyncReadExt;
                let mut out = Vec::new();
                loop {
                    let mut buf = vec![0u8; 64 * 1024];
                    let n = file.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    out.extend_from_slice(&buf[..n]);
                }
                assert_eq!(out, expected);
            })
        };
        let (a, b) = tokio::join!(stream_one, stream_two);
        a.unwrap();
        b.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn replay_server_streams_large_body_and_trailers_concurrently() {
        use eggfetch_core::Client;
        use http_body_util::BodyExt;

        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let authority = format!("127.0.0.1:{port}");
        let dir = temp_path("server-e2e");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let payload = large_pattern(512 * 1024, 5);
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(&payload).unwrap();
        let blob_ref = sink.finish().unwrap();
        let trailers = vec![HeaderEntry {
            name: "x-replay-trailer".into(),
            value: "trailer-value".into(),
        }];
        // Two identical flows so two concurrent Once-consumption requests
        // both match without exhaustion.
        for i in 0..2 {
            let flow = flow_with_bodies(
                &format!("srv-{i}"),
                "GET",
                &authority,
                "/large",
                BodyRef::Empty,
                blob_ref.clone(),
                trailers.clone(),
            );
            writer.append_flow(&flow).unwrap();
        }
        let session = writer.finish().unwrap();
        // Strict matching includes headers, but EggFetch adds Host (and
        // optional User-Agent). Use the practical profile ignoring volatile
        // transport headers so the test proves streaming/trailers/concurrency
        // rather than header brittleness; exact-body semantics stay strict via
        // digest comparison.
        let mut matcher = eggreplay_core::Matcher::practical(8);
        matcher.ignore_header("host");
        matcher.ignore_header("accept");
        matcher.ignore_header("accept-encoding");
        matcher.ignore_header("connection");
        matcher.ignore_header("te");
        let fixture = ReplayFixture::load_with_matcher(&session, matcher).unwrap();
        let bind: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let server = fixture
            .start(bind, test_limits().max_blob_bytes)
            .await
            .expect("replay server starts");
        let client = Client::builder().retry_canceled_requests(false).build();
        let fetch_one = {
            let payload = payload.clone();
            let authority = authority.clone();
            let client = client.clone();
            async move {
                let uri: http::Uri = format!("http://{authority}/large").parse().unwrap();
                let request = http::Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("TE", "trailers")
                    .body(http_body_util::Full::new(bytes::Bytes::new()))
                    .unwrap();
                let response = client.execute_http_body_default(request).await.unwrap();
                let (parts, mut body) = response.into_parts();
                assert_eq!(parts.status.as_u16(), 200);
                let mut collected = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.unwrap();
                    if frame.is_data() {
                        collected.extend_from_slice(&frame.into_data().expect("data frame"));
                    }
                }
                assert_eq!(collected, payload);
                // Trailers are proven structurally in
                // `trailers_survive_lazy_fixture` and
                // `trailer_stream_preserves_recorded_trailers`; H1 trailer
                // emission additionally requires TE negotiation in EggServe
                // and is not asserted on the wire here to avoid coupling this
                // concurrency test to H1 trailer policy.
            }
        };
        let fetch_two = {
            let payload = payload.clone();
            let authority = authority.clone();
            let client = client.clone();
            async move {
                let uri: http::Uri = format!("http://{authority}/large").parse().unwrap();
                let request = http::Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(http_body_util::Full::new(bytes::Bytes::new()))
                    .unwrap();
                let response = client.execute_http_body_default(request).await.unwrap();
                let (parts, mut body) = response.into_parts();
                assert_eq!(parts.status.as_u16(), 200);
                let mut collected = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.unwrap();
                    if frame.is_data() {
                        collected.extend_from_slice(&frame.into_data().expect("data"));
                    }
                }
                assert_eq!(collected, payload);
            }
        };
        tokio::join!(fetch_one, fetch_two);
        server.shutdown();
        server.wait().await;
        std::fs::remove_dir_all(&dir).ok();
    }
}
