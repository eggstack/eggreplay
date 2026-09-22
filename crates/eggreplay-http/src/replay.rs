//! EggServe-backed offline replay with lazy, bounded streaming.

use eggreplay_core::{
    BodyRef, ConsumptionMode, FlowOutcome, HeaderEntry, HttpRequest, MatchCandidate, MatchResult,
    Matcher, MatcherSession, QueryPair, ScenarioRules, ScenarioRuntime, StreamEvents,
    StreamTimingMode,
};
use eggreplay_store::{Session, StoreError};
use std::collections::HashMap;
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
    candidate_in_append: Vec<bool>,
    matcher: Matcher,
    session: MatcherSession,
    store: Session,
    scenario: Option<ScenarioRuntime>,
    stream_events: HashMap<String, eggreplay_core::FlowStreamEvents>,
    timing_mode: StreamTimingMode,
}

#[derive(Clone)]
enum TimedStreamStep {
    Data { delay_ns: u64, length: u64 },
    Error { delay_ns: u64, message: String },
}

#[cfg(feature = "eggserve")]
#[derive(Clone)]
struct AppendContext {
    upstream_base: http::Uri,
    client: eggfetch_core::Client,
    recording: eggreplay_store::RecordingSession,
    redaction: eggreplay_core::RedactionConfig,
    profile_id: String,
    max_structured_bytes: u64,
    physical_route: eggreplay_core::PhysicalRoute,
    miss_locks: Arc<MissLocks>,
}

#[cfg(feature = "eggserve")]
#[derive(Default)]
struct MissLocks {
    locks: Mutex<std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}

#[cfg(feature = "eggserve")]
impl MissLocks {
    fn lock_for(&self, key: String) -> Result<Arc<tokio::sync::Mutex<()>>, ServiceError> {
        let mut locks = self
            .locks
            .lock()
            .map_err(|_| ServiceError::internal("miss lock registry poisoned"))?;
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
            return Ok(lock);
        }
        if locks.len() >= 2048 {
            return Err(ServiceError::rejected(503, "too many concurrent miss keys"));
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }
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
        Self::load_inner(session, matcher, None, StreamTimingMode::Immediate)
    }

    /// Load with an explicit relative body-timing replay mode.
    pub fn load_with_timing(
        session: &Session,
        matcher: Matcher,
        timing_mode: StreamTimingMode,
    ) -> Result<Self, ReplayError> {
        Self::load_inner(session, matcher, None, timing_mode)
    }

    fn load_inner(
        session: &Session,
        matcher: Matcher,
        scenario: Option<ScenarioRuntime>,
        timing_mode: StreamTimingMode,
    ) -> Result<Self, ReplayError> {
        for extension in &session.manifest().extensions {
            if extension.required_for_replay && !(extension.name == "rules" && scenario.is_some()) {
                return Err(ReplayError::State(format!(
                    "required extension {:?} is not enabled",
                    extension.name
                )));
            }
        }
        let mut candidates = Vec::new();
        for item in session.iter_flows()? {
            let flow = item?;
            candidates.push(MatchCandidate::from_flow(flow));
        }
        let mut stream_events = HashMap::new();
        if let Some(bytes) = session.read_extension("stream-events")? {
            let decoded: StreamEvents = serde_json::from_slice(&bytes).map_err(|error| {
                ReplayError::State(format!("invalid stream-events extension: {error}"))
            })?;
            decoded.validate().map_err(ReplayError::State)?;
            for flow_events in decoded.flows {
                stream_events.insert(flow_events.flow_id.clone(), flow_events);
            }
        } else if timing_mode != StreamTimingMode::Immediate {
            return Err(ReplayError::State(
                "timed replay requires a stream-events extension".into(),
            ));
        }
        for candidate in &candidates {
            if let Some(events) = stream_events.get(&candidate.flow.id) {
                let response_length = events.response.iter().fold(0u64, |total, event| {
                    total.saturating_add(match &event.event {
                        eggreplay_core::StreamEventKind::Data { length, .. } => *length,
                        _ => 0,
                    })
                });
                let expected_length = match &candidate.flow.outcome {
                    FlowOutcome::Response(response) => response.body.len().unwrap_or(0),
                    FlowOutcome::Error(_) => 0,
                };
                if response_length != expected_length {
                    return Err(ReplayError::State(format!(
                        "stream event byte count disagrees with flow {} response body",
                        candidate.flow.id
                    )));
                }
            }
        }
        if timing_mode != StreamTimingMode::Immediate
            && candidates.iter().any(|candidate| {
                matches!(&candidate.flow.outcome, FlowOutcome::Response(response) if !matches!(response.body, BodyRef::Absent | BodyRef::Empty))
                    && !stream_events.contains_key(&candidate.flow.id)
            })
        {
            return Err(ReplayError::State("timed replay is missing response event metadata".into()));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ReplayState {
                candidates,
                candidate_in_append: vec![false; session.manifest().flow_count],
                matcher,
                session: MatcherSession::new(),
                store: session.clone(),
                scenario,
                stream_events,
                timing_mode,
            })),
        })
    }

    /// Load a fixture and select one authored scenario. Scenario state belongs
    /// to this replay fixture instance and is never shared across servers.
    pub fn load_with_scenario(
        session: &Session,
        matcher: Matcher,
        scenario_id: &str,
    ) -> Result<Self, ReplayError> {
        Self::load_with_scenario_and_redaction(
            session,
            matcher,
            scenario_id,
            eggreplay_core::RedactionConfig::default_secure(),
        )
    }

    /// Load a named scenario with an explicit protected-field redaction policy.
    pub fn load_with_scenario_and_redaction(
        session: &Session,
        matcher: Matcher,
        scenario_id: &str,
        redaction: eggreplay_core::RedactionConfig,
    ) -> Result<Self, ReplayError> {
        Self::load_with_scenario_and_redaction_and_timing(
            session,
            matcher,
            scenario_id,
            redaction,
            StreamTimingMode::Immediate,
        )
    }

    /// Load a named scenario with protected-field policy and body timing mode.
    pub fn load_with_scenario_and_redaction_and_timing(
        session: &Session,
        matcher: Matcher,
        scenario_id: &str,
        redaction: eggreplay_core::RedactionConfig,
        timing_mode: StreamTimingMode,
    ) -> Result<Self, ReplayError> {
        let bytes = session
            .read_extension("rules")?
            .ok_or_else(|| ReplayError::State("fixture has no rules extension".into()))?;
        let rules: ScenarioRules = serde_json::from_slice(&bytes)
            .map_err(|error| ReplayError::State(format!("invalid rules extension: {error}")))?;
        let scenario = rules
            .runtime_with_redaction(scenario_id, redaction)
            .map_err(ReplayError::State)?;
        Self::load_inner(session, matcher, Some(scenario), timing_mode)
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
        self.start_inner(bind, max_body_bytes, None).await
    }

    /// Start replay with explicit upstream append-on-miss behavior. Existing
    /// candidates replay first; only exact no-match results reach EggFetch.
    #[cfg(feature = "eggserve")]
    #[allow(clippy::too_many_arguments)]
    pub async fn start_append_new(
        self,
        bind: SocketAddr,
        max_body_bytes: u64,
        upstream_base: http::Uri,
        client: eggfetch_core::Client,
        recording: eggreplay_store::RecordingSession,
        redaction: eggreplay_core::RedactionConfig,
        profile_id: String,
        max_structured_bytes: u64,
        physical_route: eggreplay_core::PhysicalRoute,
    ) -> Result<ServerHandle, ReplayError> {
        self.start_inner(
            bind,
            max_body_bytes,
            Some(AppendContext {
                upstream_base,
                client,
                recording,
                redaction,
                profile_id,
                max_structured_bytes,
                physical_route,
                miss_locks: Arc::new(MissLocks::default()),
            }),
        )
        .await
    }

    #[cfg(feature = "eggserve")]
    async fn start_inner(
        self,
        bind: SocketAddr,
        max_body_bytes: u64,
        append: Option<AppendContext>,
    ) -> Result<ServerHandle, ReplayError> {
        let state = self.state.clone();
        let service = service_fn_with_policy(
            move |request| {
                let state = state.clone();
                let append = append.clone();
                async move { handle_request(state, request, append).await }
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
    append: Option<AppendContext>,
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
    {
        let mut guard = state
            .lock()
            .map_err(|_| ServiceError::internal("replay state poisoned"))?;
        if let Some(scenario) = guard.scenario.as_mut() {
            let step = scenario
                .advance(&actual, &body)
                .map_err(ServiceError::internal)?;
            if let Some(step) = step {
                return response_bytes(
                    step.response.status,
                    &step.response.headers,
                    step.response.body,
                );
            }
        }
    }
    let selection = {
        let mut guard = state
            .lock()
            .map_err(|_| ServiceError::internal("replay state poisoned"))?;
        let candidates = guard.candidates.clone();
        let matcher = guard.matcher.clone();
        let store = guard.store.clone();
        let store_for_loader = store.clone();
        let append_store = append.as_ref().map(|context| context.recording.clone());
        let candidate_in_append = guard.candidate_in_append.clone();
        let mut loader = |idx: usize, candidate: &MatchCandidate| -> Option<Vec<u8>> {
            match &candidate.flow.request.body {
                BodyRef::Blob(blob) if candidate_in_append.get(idx).copied().unwrap_or(false) => {
                    append_store
                        .as_ref()?
                        .read_body(&BodyRef::Blob(blob.clone()))
                        .ok()
                }
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
            if append.is_some() {
                ConsumptionMode::Unlimited
            } else {
                ConsumptionMode::Once
            },
            &mut guard.session,
            &mut loader,
        );
        match result {
            MatchResult::Matched(index) => {
                let in_append = guard.candidate_in_append[index];
                let flow = guard.candidates[index].flow.clone();
                match flow.outcome {
                    FlowOutcome::Response(response) => Ok((
                        index,
                        response.headers.clone(),
                        response.status,
                        response.body.clone(),
                        response.trailers.clone(),
                        store,
                        in_append,
                        if in_append {
                            Vec::new()
                        } else {
                            guard
                                .stream_events
                                .get(&flow.id)
                                .map(|events| events.response.clone())
                                .unwrap_or_default()
                        },
                        guard.timing_mode,
                    )),
                    FlowOutcome::Error(error) => {
                        return response_bytes(
                            502,
                            &[],
                            format!("recorded upstream error: {:?}\n", error.category),
                        );
                    }
                }
            }
            MatchResult::Exhausted { .. } => {
                return response_bytes(409, &[], "eggreplay replay fixture exhausted\n");
            }
            MatchResult::NoMatch { .. } => match append.clone() {
                Some(context) => Err(context),
                None => return response_bytes(404, &[], "eggreplay replay no match\n"),
            },
        }
    };
    let (
        index,
        response_headers,
        response_status,
        response_body_ref,
        response_trailers,
        store,
        in_append,
        stream_events,
        timing_mode,
    ) = match selection {
        Ok(selection) => selection,
        Err(context) => return append_miss(state, context, actual, body.to_vec()).await,
    };
    let _ = index;
    if in_append {
        let Some(context) = append else {
            return Err(ServiceError::internal(
                "append candidate has no recording session",
            ));
        };
        if response_body_ref == BodyRef::Empty || response_body_ref == BodyRef::Absent {
            let status = StatusCode::new(response_status)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            let mut builder = Response::builder().status(status);
            for header in response_headers {
                builder = builder
                    .header(header.name, header.value)
                    .map_err(|error| ServiceError::internal(error.to_string()))?;
            }
            return builder
                .body(ResponseBody::Empty)
                .map_err(|error| ServiceError::internal(error.to_string()));
        }
        let path = context
            .recording
            .body_path(&response_body_ref)
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        let stream = crate::recording::response_stream(
            path,
            response_body_ref.len().unwrap_or(0),
            response_trailers,
        );
        let status = StatusCode::new(response_status)
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        let mut builder = Response::builder().status(status);
        for header in response_headers {
            builder = builder
                .header(header.name, header.value)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
        }
        return builder
            .body(ResponseBody::Stream(stream))
            .map_err(|error| ServiceError::internal(error.to_string()));
    }
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
            let mut schedule = Vec::<TimedStreamStep>::new();
            let mut previous_delta = 0u64;
            let mut accumulated_delay = 0u64;
            let mut trailer_delay = 0u64;
            let mut terminal_delay = 0u64;
            let mut terminal_error = false;
            for event in &stream_events {
                let delay = match timing_mode.delay_ns(
                    event.delta_ns.saturating_sub(previous_delta),
                    accumulated_delay,
                ) {
                    Ok(delay) => delay,
                    Err(error) => {
                        return response_bytes(
                            500,
                            &[],
                            format!("invalid replay timing: {error}\n"),
                        );
                    }
                };
                previous_delta = event.delta_ns;
                accumulated_delay = accumulated_delay.saturating_add(delay);
                match &event.event {
                    eggreplay_core::StreamEventKind::Data { length, .. } => {
                        schedule.push(TimedStreamStep::Data {
                            delay_ns: delay,
                            length: *length,
                        });
                    }
                    eggreplay_core::StreamEventKind::Trailers { .. } => {
                        trailer_delay = delay;
                    }
                    eggreplay_core::StreamEventKind::Error {
                        category, phase, ..
                    } => {
                        terminal_error = true;
                        schedule.push(TimedStreamStep::Error {
                            delay_ns: delay,
                            message: format!("recorded stream error: {category}/{phase}"),
                        });
                    }
                    eggreplay_core::StreamEventKind::End => terminal_delay = delay,
                }
            }
            let schedule = Arc::new(schedule);
            let byte_stream = futures_util::stream::unfold(
                (
                    Some(tokio_file),
                    Some(Sha256::new()),
                    0u64,
                    false,
                    0usize,
                    0u64,
                    schedule,
                ),
                move |(
                    mut file_opt,
                    mut hasher_opt,
                    mut read,
                    terminal,
                    mut schedule_index,
                    mut segment_remaining,
                    schedule,
                )| {
                    let digest = digest.clone();
                    async move {
                        if terminal || file_opt.is_none() {
                            return None;
                        }
                        if segment_remaining == 0 && schedule_index < schedule.len() {
                            let step = schedule[schedule_index].clone();
                            schedule_index += 1;
                            let delay_ns = match &step {
                                TimedStreamStep::Data { delay_ns, .. }
                                | TimedStreamStep::Error { delay_ns, .. } => *delay_ns,
                            };
                            if delay_ns > 0 {
                                tokio::time::sleep(std::time::Duration::from_nanos(delay_ns)).await;
                            }
                            match step {
                                TimedStreamStep::Data { length, .. } => segment_remaining = length,
                                TimedStreamStep::Error { message, .. } => {
                                    file_opt = None;
                                    hasher_opt = None;
                                    return Some((
                                        Err(ResponseStreamError::new(message)),
                                        (
                                            file_opt,
                                            hasher_opt,
                                            read,
                                            true,
                                            schedule_index,
                                            segment_remaining,
                                            schedule,
                                        ),
                                    ));
                                }
                            }
                        }
                        let file = file_opt.as_mut().expect("file present");
                        let max_chunk = if segment_remaining == 0 {
                            64 * 1024
                        } else {
                            segment_remaining.min(64 * 1024) as usize
                        };
                        let mut buf = vec![0u8; max_chunk];
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
                                        (
                                            file_opt,
                                            hasher_opt,
                                            read,
                                            true,
                                            schedule_index,
                                            segment_remaining,
                                            schedule,
                                        ),
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
                                        (
                                            file_opt,
                                            hasher_opt,
                                            read,
                                            true,
                                            schedule_index,
                                            segment_remaining,
                                            schedule,
                                        ),
                                    ));
                                }
                                segment_remaining = segment_remaining.saturating_sub(n as u64);
                                let bytes = bytes::Bytes::from(buf);
                                Some((
                                    Ok(bytes),
                                    (
                                        file_opt,
                                        hasher_opt,
                                        read,
                                        false,
                                        schedule_index,
                                        segment_remaining,
                                        schedule,
                                    ),
                                ))
                            }
                            Err(e) => {
                                file_opt = None;
                                hasher_opt = None;
                                Some((
                                    Err(ResponseStreamError::new(e.to_string())),
                                    (
                                        file_opt,
                                        hasher_opt,
                                        read,
                                        true,
                                        schedule_index,
                                        segment_remaining,
                                        schedule,
                                    ),
                                ))
                            }
                        }
                    }
                },
            );
            let trailers_owned = response_trailers.clone();
            let terminal_delay = if terminal_error { 0 } else { terminal_delay };
            let trailer_delay = if terminal_error { 0 } else { trailer_delay };
            let trailer_future = async move {
                let delay = trailer_delay.saturating_add(terminal_delay);
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_nanos(delay)).await;
                }
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
            };
            let stream = if terminal_error {
                ResponseStream::with_trailers(byte_stream, trailer_future)
            } else {
                ResponseStream::with_known_length_and_trailers(byte_stream, length, trailer_future)
            };
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
async fn append_miss(
    state: Arc<Mutex<ReplayState>>,
    context: AppendContext,
    actual: eggreplay_core::HttpRequest,
    body: Vec<u8>,
) -> Result<Response, ServiceError> {
    use sha2::{Digest, Sha256};

    let mut key_hash = Sha256::new();
    key_hash.update(
        serde_json::to_vec(&actual).map_err(|error| ServiceError::internal(error.to_string()))?,
    );
    key_hash.update(&body);
    let lock = context
        .miss_locks
        .lock_for(format!("{:x}", key_hash.finalize()))?;
    let _miss_guard = lock.lock().await;

    // Another request with the same key may have filled this miss while we
    // waited. Re-run selection under the ordinary replay-state lock before
    // opening another upstream transaction.
    let replayed = {
        let mut guard = state
            .lock()
            .map_err(|_| ServiceError::internal("replay state poisoned"))?;
        let candidates = guard.candidates.clone();
        let matcher = guard.matcher.clone();
        let store = guard.store.clone();
        let store_for_loader = store.clone();
        let append_store = context.recording.clone();
        let sources = guard.candidate_in_append.clone();
        let mut loader = |index: usize, candidate: &MatchCandidate| -> Option<Vec<u8>> {
            match &candidate.flow.request.body {
                BodyRef::Blob(blob) if sources.get(index).copied().unwrap_or(false) => {
                    append_store.read_body(&BodyRef::Blob(blob.clone())).ok()
                }
                BodyRef::Blob(blob) => store_for_loader.read_blob(blob).ok(),
                BodyRef::Absent | BodyRef::Empty => Some(Vec::new()),
            }
        };
        match matcher.select_with_loader(
            &actual,
            &body,
            &candidates,
            ConsumptionMode::Unlimited,
            &mut guard.session,
            &mut loader,
        ) {
            MatchResult::Matched(index) if sources.get(index).copied().unwrap_or(false) => {
                Some(guard.candidates[index].flow.clone())
            }
            MatchResult::Matched(_) => None,
            MatchResult::Exhausted { .. } => {
                return response_bytes(409, &[], "eggreplay replay fixture exhausted\n");
            }
            MatchResult::NoMatch { .. } => None,
        }
    };
    if let Some(flow) = replayed {
        return response_from_append_flow(&flow, &context.recording);
    }

    let Some(scheme) = context.upstream_base.scheme_str() else {
        return Err(ServiceError::internal("upstream URI has no scheme"));
    };
    let Some(authority) = context.upstream_base.authority() else {
        return Err(ServiceError::internal("upstream URI has no authority"));
    };
    let query = {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for pair in &actual.query {
            query.append_pair(&pair.key, &pair.value);
        }
        query.finish()
    };
    let target = if query.is_empty() {
        actual.path.clone()
    } else {
        format!("{}?{query}", actual.path)
    };
    let uri: http::Uri = format!("{scheme}://{authority}{target}")
        .parse()
        .map_err(|error: http::uri::InvalidUri| ServiceError::internal(error.to_string()))?;
    let mut request = http::Request::builder()
        .method(actual.method.as_str())
        .uri(uri)
        .body(http_body_util::Full::new(bytes::Bytes::from(body.clone())))
        .map_err(|error| ServiceError::internal(error.to_string()))?;
    for header in actual.headers {
        if header.name.eq_ignore_ascii_case("host") {
            continue;
        }
        let name = http::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        let value = http::header::HeaderValue::from_bytes(header.value.as_bytes())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        request.headers_mut().append(name, value);
    }
    let flow = crate::recording::record_request_with_session(
        &context.client,
        &context.recording,
        request,
        &context.redaction,
        &context.profile_id,
        context.max_structured_bytes,
        Some(context.physical_route.clone()),
    )
    .await
    .map_err(|error| ServiceError::rejected(502, format!("upstream recording failed: {error}")))?;
    {
        let mut guard = state
            .lock()
            .map_err(|_| ServiceError::internal("replay state poisoned"))?;
        guard
            .candidates
            .push(MatchCandidate::from_flow(flow.clone()));
        guard.candidate_in_append.push(true);
    }
    response_from_append_flow(&flow, &context.recording)
}

#[cfg(feature = "eggserve")]
fn response_from_append_flow(
    flow: &eggreplay_core::Flow,
    recording: &eggreplay_store::RecordingSession,
) -> Result<Response, ServiceError> {
    use eggserve_primitives::{ResponseBody, StatusCode};
    let response = match &flow.outcome {
        FlowOutcome::Response(response) => response,
        FlowOutcome::Error(error) => {
            return response_bytes(
                502,
                &[],
                format!("upstream failure: {:?}\n", error.category),
            );
        }
    };
    let status = StatusCode::new(response.status)
        .map_err(|error| ServiceError::internal(error.to_string()))?;
    let mut builder = Response::builder().status(status);
    for header in &response.headers {
        builder = builder
            .header(header.name.clone(), header.value.clone())
            .map_err(|error| ServiceError::internal(error.to_string()))?;
    }
    match &response.body {
        BodyRef::Absent | BodyRef::Empty => builder
            .body(ResponseBody::Empty)
            .map_err(|error| ServiceError::internal(error.to_string())),
        body => {
            let path = recording
                .body_path(body)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            let stream = crate::recording::response_stream(
                path,
                body.len().unwrap_or(0),
                response.trailers.clone(),
            );
            builder
                .body(ResponseBody::Stream(stream))
                .map_err(|error| ServiceError::internal(error.to_string()))
        }
    }
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
    let authority = head
        .authority()
        .map(|value| value.as_str().to_owned())
        .or_else(|| {
            head.headers()
                .iter()
                .find(|field| field.name.as_str().eq_ignore_ascii_case("host"))
                .map(|field| String::from_utf8_lossy(field.value.as_bytes()).into_owned())
        })
        .unwrap_or_default();
    let headers = eggserve_headers(head.headers());
    let trailers = trailers.map(eggserve_trailers).unwrap_or_default();
    Ok(HttpRequest {
        method: head.method().as_str().into(),
        scheme: connection.scheme.as_str().into(),
        authority,
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
        .filter(|field| !field.name.as_str().eq_ignore_ascii_case("host"))
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
            schema_version: eggreplay_core::FLOW_SCHEMA_VERSION,
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

    #[tokio::test]
    async fn recorded_timing_delays_between_semantic_data_events() {
        use eggfetch_core::Client;
        use http_body_util::BodyExt;

        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let authority = format!("127.0.0.1:{port}");
        let dir = temp_path("timed-replay");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let mut sink = writer.begin_blob().unwrap();
        sink.write_all(b"abcd").unwrap();
        let body = sink.finish().unwrap();
        let flow = flow_with_bodies(
            "timed",
            "GET",
            &authority,
            "/timed",
            BodyRef::Empty,
            body,
            vec![],
        );
        writer.append_flow(&flow).unwrap();
        let events = StreamEvents {
            schema_version: eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
            flows: vec![eggreplay_core::FlowStreamEvents {
                flow_id: flow.id.clone(),
                start_offset_ns: 0,
                request: vec![],
                response: vec![
                    eggreplay_core::StreamEvent {
                        delta_ns: 0,
                        event: eggreplay_core::StreamEventKind::Data {
                            offset: 0,
                            length: 2,
                        },
                    },
                    eggreplay_core::StreamEvent {
                        delta_ns: 40_000_000,
                        event: eggreplay_core::StreamEventKind::Data {
                            offset: 2,
                            length: 2,
                        },
                    },
                    eggreplay_core::StreamEvent {
                        delta_ns: 80_000_000,
                        event: eggreplay_core::StreamEventKind::Error {
                            offset: 4,
                            category: "other".into(),
                            phase: "body".into(),
                        },
                    },
                ],
            }],
        };
        writer
            .write_extension(
                "stream-events",
                eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION,
                "stream-events.json",
                false,
                &serde_json::to_vec(&events).unwrap(),
            )
            .unwrap();
        let session = writer.finish().unwrap();
        let mut matcher = eggreplay_core::Matcher::practical(8);
        for header in ["host", "accept", "accept-encoding", "connection"] {
            matcher.ignore_header(header);
        }
        let fixture =
            ReplayFixture::load_with_timing(&session, matcher, StreamTimingMode::Recorded).unwrap();
        let bind: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let server = fixture
            .start(bind, test_limits().max_blob_bytes)
            .await
            .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let request = http::Request::builder()
            .uri(
                format!("http://{authority}/timed")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .unwrap();
        let response = client.execute_http_body_default(request).await.unwrap();
        let (_, mut body) = response.into_parts();
        let mut times = Vec::new();
        let start = tokio::time::Instant::now();
        let mut bytes = Vec::new();
        let mut stream_error = false;
        while let Some(frame) = body.frame().await {
            let frame = match frame {
                Ok(frame) => frame,
                Err(_) => {
                    stream_error = true;
                    break;
                }
            };
            if frame.is_data() {
                times.push(start.elapsed());
                bytes.extend_from_slice(&frame.into_data().unwrap());
            }
        }
        assert_eq!(bytes, b"abcd");
        assert!(stream_error, "recorded terminal stream error is replayed");
        assert!(times.len() >= 2);
        assert!(times[1] - times[0] >= std::time::Duration::from_millis(30));
        server.shutdown();
        server.wait().await;
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

    #[tokio::test]
    async fn selected_scenario_precedes_recorded_matching_and_renders_request_values() {
        use eggfetch_core::Client;

        let dir = temp_path("scenario-server");
        let mut writer =
            SessionWriter::create(&dir, SessionMetadata::default(), test_limits()).unwrap();
        let rules = ScenarioRules {
            schema_version: eggreplay_core::RULES_SCHEMA_VERSION,
            scenarios: vec![eggreplay_core::Scenario {
                id: "users".into(),
                initial_state: "start".into(),
                states: vec!["start".into(), "done".into()],
                transitions: vec![eggreplay_core::ScenarioTransition {
                    from: "start".into(),
                    when: vec![eggreplay_core::RequestPredicate::Path {
                        value: "/users/alice".into(),
                    }],
                    extract: vec![eggreplay_core::VariableExtraction {
                        name: "user".into(),
                        source: eggreplay_core::VariableSource::PathSegment { index: 1 },
                    }],
                    extraction_failure: eggreplay_core::ExtractionFailureBehavior::Abort,
                    response: eggreplay_core::ScenarioResponse {
                        status: 201,
                        headers: vec![HeaderEntry {
                            name: "content-type".into(),
                            value: "text/plain".into(),
                        }],
                        body_template: "created {{user}}".into(),
                        json_pointer_replacements: vec![],
                    },
                    next_state: "done".into(),
                }],
            }],
        };
        writer
            .write_extension(
                "rules",
                eggreplay_core::RULES_SCHEMA_VERSION,
                "rules.json",
                true,
                &serde_json::to_vec(&rules).unwrap(),
            )
            .unwrap();
        let session = writer.finish().unwrap();
        let fixture = ReplayFixture::load_with_scenario(
            &session,
            eggreplay_core::Matcher::strict(8),
            "users",
        )
        .unwrap();
        let server = fixture
            .start("127.0.0.1:0".parse().unwrap(), test_limits().max_blob_bytes)
            .await
            .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let uri: http::Uri = format!("http://{}/users/alice", server.local_addr())
            .parse()
            .unwrap();
        let request = http::Request::builder()
            .uri(uri)
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .unwrap();
        let response = client.execute_http_body_default(request).await.unwrap();
        assert_eq!(response.status().as_u16(), 201);
        use http_body_util::BodyExt;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"created alice");
        server.shutdown();
        server.wait().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn append_new_records_one_miss_and_replays_it_without_second_upstream_call() {
        use eggfetch_core::Client;
        use http_body_util::BodyExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let source_dir = temp_path("append-source");
        let source_writer =
            SessionWriter::create(&source_dir, SessionMetadata::default(), test_limits()).unwrap();
        let source = source_writer.finish().unwrap();
        let append_dir = temp_path("append-recording");
        let recording = eggreplay_store::RecordingSession::create(
            &append_dir,
            SessionMetadata::default(),
            test_limits(),
        )
        .unwrap();
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_authority = upstream_addr.to_string();
        let secret = "M009-APPEND-SECRET-SENTINEL";
        let upstream_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let upstream_hits_task = upstream_hits.clone();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.unwrap();
            upstream_hits_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh",
                )
                .await
                .unwrap();
        });
        let fixture = ReplayFixture::load(&source).unwrap();
        let server = fixture
            .start_append_new(
                "127.0.0.1:0".parse().unwrap(),
                test_limits().max_blob_bytes,
                format!("http://{upstream_authority}").parse().unwrap(),
                Client::builder().retry_canceled_requests(false).build(),
                recording.clone(),
                eggreplay_core::RedactionConfig::default_secure(),
                "default-v1".into(),
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                eggreplay_core::PhysicalRoute {
                    kind: "direct".into(),
                    description: Some("direct".into()),
                },
            )
            .await
            .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let fetch = |client: Client, server_addr: SocketAddr, host: String, secret: String| async move {
            let uri: http::Uri = format!("http://{server_addr}/new").parse().unwrap();
            let request = http::Request::builder()
                .uri(uri)
                .header("host", host)
                .header("authorization", secret)
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .unwrap();
            let response = client.execute_http_body_default(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(status.as_u16(), 200, "response body: {:?}", bytes);
            bytes
        };
        let first = fetch(
            client.clone(),
            server.local_addr(),
            upstream_authority.clone(),
            secret.to_owned(),
        );
        let second = fetch(
            client,
            server.local_addr(),
            upstream_authority,
            secret.to_owned(),
        );
        let (first_body, second_body) = tokio::join!(first, second);
        assert_eq!(first_body, "fresh");
        assert_eq!(second_body, "fresh");
        assert_eq!(upstream_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(recording.flow_count(), 1);
        upstream_task.await.unwrap();
        server.shutdown();
        server.wait().await;
        recording.shutdown();
        let appended = recording.finish().unwrap();
        assert_eq!(appended.manifest().flow_count, 1);
        let recorded_flows = appended
            .iter_flows()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            !serde_json::to_string(&recorded_flows[0])
                .unwrap()
                .contains(secret)
        );
        std::fs::remove_dir_all(&append_dir).ok();
        std::fs::remove_dir_all(&source_dir).ok();
    }

    #[tokio::test]
    async fn unrelated_append_new_misses_overlap_upstream_work() {
        use eggfetch_core::Client;
        use http_body_util::BodyExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let source_dir = temp_path("append-overlap-source");
        let source_writer =
            SessionWriter::create(&source_dir, SessionMetadata::default(), test_limits()).unwrap();
        let source = source_writer.finish().unwrap();
        let append_dir = temp_path("append-overlap-recording");
        let recording = eggreplay_store::RecordingSession::create(
            &append_dir,
            SessionMetadata::default(),
            test_limits(),
        )
        .unwrap();
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_authority = upstream_addr.to_string();
        let active = std::sync::Arc::new(AtomicUsize::new(0));
        let max_active = std::sync::Arc::new(AtomicUsize::new(0));
        let active_task = active.clone();
        let max_task = max_active.clone();
        let upstream_task = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = upstream_listener.accept().await.unwrap();
                let active = active_task.clone();
                let max_active = max_task.clone();
                tokio::spawn(async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0; 1024];
                        let read = stream.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await
                        .unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        let fixture = ReplayFixture::load(&source).unwrap();
        let server = fixture
            .start_append_new(
                "127.0.0.1:0".parse().unwrap(),
                test_limits().max_blob_bytes,
                format!("http://{upstream_authority}").parse().unwrap(),
                Client::builder().retry_canceled_requests(false).build(),
                recording.clone(),
                eggreplay_core::RedactionConfig::default_secure(),
                "default-v1".into(),
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                eggreplay_core::PhysicalRoute {
                    kind: "direct".into(),
                    description: Some("direct".into()),
                },
            )
            .await
            .unwrap();
        let client = Client::builder().retry_canceled_requests(false).build();
        let server_addr = server.local_addr();
        let fetch = |path: &'static str| {
            let client = client.clone();
            let host = upstream_authority.clone();
            async move {
                let uri: http::Uri = format!("http://{server_addr}{path}").parse().unwrap();
                let request = http::Request::builder()
                    .uri(uri)
                    .header("host", host)
                    .body(http_body_util::Full::new(bytes::Bytes::new()))
                    .unwrap();
                let response = client.execute_http_body_default(request).await.unwrap();
                assert_eq!(response.status().as_u16(), 200);
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(&bytes[..], b"ok");
            }
        };
        tokio::join!(fetch("/a"), fetch("/b"));
        upstream_task.await.unwrap();
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert_eq!(recording.flow_count(), 2);
        server.shutdown();
        server.wait().await;
        recording.shutdown();
        recording.finish().unwrap();
        std::fs::remove_dir_all(&append_dir).ok();
        std::fs::remove_dir_all(&source_dir).ok();
    }
}
