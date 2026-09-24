use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use eggreplay_core::{
    ComparisonPolicy, FlowStreamEvents, Matcher, PhysicalRoute, RedactionConfig, ReportScheduler,
    SessionMetadata, StreamTimingMode,
};
use eggreplay_http::{EggressDialer, ReplayFixture, execute_candidate};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use pyo3::prelude::*;

use crate::{
    errors::{ConfigurationError, FixtureError, NetworkError},
    fixture::PyFixture,
};

enum Finalization {
    None,
    Once(RecordingSession),
    ReRecord {
        session: RecordingSession,
        staging: std::path::PathBuf,
        target: std::path::PathBuf,
    },
    Append {
        session: RecordingSession,
        source: Box<Session>,
        temporary: std::path::PathBuf,
        combined: std::path::PathBuf,
        target: std::path::PathBuf,
    },
}

/// Owned `EggServe` lifecycle. Explicit close is idempotent and its Rust cleanup
/// task survives cancellation of the Python waiter.
#[pyclass(name = "Server")]
pub struct PyServer {
    address: String,
    handle: Mutex<Option<eggserve_server::ServerHandle>>,
    finalization: Mutex<Finalization>,
    closing: AtomicBool,
    completion: tokio::sync::watch::Sender<Option<Result<(), String>>>,
}

impl PyServer {
    fn new(handle: eggserve_server::ServerHandle, finalization: Finalization) -> Self {
        let address = handle.local_addr().to_string();
        let (completion, _) = tokio::sync::watch::channel(None);
        Self {
            address,
            handle: Mutex::new(Some(handle)),
            finalization: Mutex::new(finalization),
            closing: AtomicBool::new(false),
            completion,
        }
    }
}

#[pymethods]
impl PyServer {
    #[getter]
    fn address(&self) -> &str {
        &self.address
    }

    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// Stop admission, drain requests, and atomically finalize recordings.
    fn aclose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if !self.closing.swap(true, Ordering::AcqRel) {
            let handle = self
                .handle
                .lock()
                .map_err(|_| NetworkError::new_err("server lifecycle unavailable"))?
                .take();
            let finalization = std::mem::replace(
                &mut *self
                    .finalization
                    .lock()
                    .map_err(|_| NetworkError::new_err("recording lifecycle unavailable"))?,
                Finalization::None,
            );
            let completion = self.completion.clone();
            pyo3_async_runtimes::tokio::get_runtime().spawn(async move {
                let result = async {
                    if let Some(handle) = handle {
                        handle.shutdown();
                        handle.wait().await;
                    }
                    match finalization {
                        Finalization::Once(session) => drop(finish_session(session).await?),
                        Finalization::ReRecord {
                            session,
                            staging,
                            target,
                        } => {
                            drop(finish_session(session).await?);
                            replace_fixture(&staging, &target)
                                .map_err(|_| "fixture replacement failed".to_owned())?;
                        }
                        Finalization::Append {
                            session,
                            source,
                            temporary,
                            combined,
                            target,
                        } => {
                            let additional = finish_session(session).await?;
                            let merged = source
                                .merge_to(
                                    &additional,
                                    &combined,
                                    eggreplay_core::SESSION_SCHEMA_VERSION,
                                )
                                .map_err(|error| format!("fixture merge failed: {error}"))?;
                            drop(merged);
                            replace_fixture(&combined, &target)
                                .map_err(|_| "fixture replacement failed".to_owned())?;
                            let _ = std::fs::remove_dir_all(temporary);
                        }
                        Finalization::None => {}
                    }
                    Ok::<(), String>(())
                }
                .await;
                completion.send_replace(Some(result));
            });
        }
        cleanup_future(py, self.completion.subscribe())
    }

    /// Wait for explicit shutdown to finish without stopping the server.
    fn wait<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        cleanup_future(py, self.completion.subscribe())
    }

    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(slf) })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn __aexit__<'py>(
        slf: Py<Self>,
        py: Python<'py>,
        _ty: Option<Bound<'py, PyAny>>,
        _value: Option<Bound<'py, PyAny>>,
        _tb: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        slf.borrow(py).aclose(py)
    }
}

fn cleanup_future(
    py: Python<'_>,
    mut completion: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        loop {
            if let Some(result) = completion.borrow().clone() {
                return result.map_err(|_| FixtureError::new_err("server cleanup failed"));
            }
            completion
                .changed()
                .await
                .map_err(|_| NetworkError::new_err("server cleanup task failed"))?;
        }
    })
}

/// Start the existing Rust replay authority on `bind` (for example
/// `127.0.0.1:0`).
#[pyfunction]
#[pyo3(signature = (fixture, bind="127.0.0.1:0", matcher_profile="strict", timing_mode="immediate", max_body_bytes=67_108_864, scenario_id=None))]
#[allow(clippy::needless_pass_by_value)]
fn replay_server<'py>(
    py: Python<'py>,
    fixture: PyRef<'py, PyFixture>,
    bind: &str,
    matcher_profile: &str,
    timing_mode: &str,
    max_body_bytes: u64,
    scenario_id: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let session = Arc::clone(&fixture.session);
    let bind = parse_bind(bind)?;
    let matcher = matcher(matcher_profile)?;
    let timing = StreamTimingMode::parse(timing_mode).map_err(ConfigurationError::new_err)?;
    let scenario_id = scenario_id.map(str::to_owned);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let replay = match scenario_id {
            Some(scenario) => ReplayFixture::load_with_scenario_and_redaction_and_timing(
                &session,
                matcher,
                &scenario,
                RedactionConfig::default_secure(),
                timing,
            ),
            None => ReplayFixture::load_with_timing(&session, matcher, timing),
        }
        .map_err(|_| FixtureError::new_err("fixture cannot be loaded for replay"))?;
        let handle = replay
            .start(bind, max_body_bytes)
            .await
            .map_err(|_| NetworkError::new_err("replay server could not start"))?;
        Ok(PyServer::new(handle, Finalization::None))
    })
}

/// Start a Rust recording gateway. The destination must not exist and is
/// published only after `aclose` has stopped and drained the gateway.
#[pyfunction]
#[pyo3(signature = (fixture_path, upstream, bind="127.0.0.1:0", route="direct", record_mode="once", websockets=false, max_body_bytes=67_108_864))]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn recording_gateway<'py>(
    py: Python<'py>,
    fixture_path: &str,
    upstream: &str,
    bind: &str,
    route: &str,
    record_mode: &str,
    websockets: bool,
    max_body_bytes: u64,
) -> PyResult<Bound<'py, PyAny>> {
    let path = Path::new(fixture_path).to_owned();
    let upstream: http::Uri = upstream
        .parse()
        .map_err(|_| ConfigurationError::new_err("invalid upstream URI"))?;
    let bind = parse_bind(bind)?;
    let (client, physical) = route_client(route)?;
    let safe_target = eggreplay_http::redact_route_credentials(&upstream.to_string());
    let mode = serde_json::from_value::<eggreplay_core::RecordMode>(serde_json::Value::String(
        record_mode.to_owned(),
    ))
    .map_err(|_| {
        ConfigurationError::new_err("record mode must be once, append-new, or re-record")
    })?;
    if mode == eggreplay_core::RecordMode::Sealed {
        return Err(ConfigurationError::new_err(
            "recording gateway cannot use sealed mode",
        ));
    }
    if websockets && mode == eggreplay_core::RecordMode::AppendNew {
        return Err(ConfigurationError::new_err(
            "WebSocket append-new is not supported",
        ));
    }
    let target = path.clone();
    let exists = target.exists();
    let policy = mode
        .resolve(exists, true)
        .map_err(ConfigurationError::new_err)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        if policy.mode == eggreplay_core::RecordMode::Sealed {
            let source = Session::open(&target, StoreLimits::default())
                .map_err(|_| FixtureError::new_err("existing once fixture is invalid"))?;
            let replay = ReplayFixture::load_with_matcher(&source, Matcher::strict(8))
                .map_err(|_| FixtureError::new_err("existing once fixture cannot be replayed"))?;
            let handle = replay
                .start(bind, max_body_bytes)
                .await
                .map_err(|_| NetworkError::new_err("replay server could not start"))?;
            return Ok(PyServer::new(handle, Finalization::None));
        }
        let (recording_path, finalization_kind) = match policy.mode {
            eggreplay_core::RecordMode::Once => (target.clone(), "python-gateway"),
            eggreplay_core::RecordMode::ReRecord => {
                (sibling_path(&target, "rerecord"), "python-re-record")
            }
            eggreplay_core::RecordMode::AppendNew => {
                (sibling_path(&target, "misses"), "python-append-new")
            }
            eggreplay_core::RecordMode::Sealed => unreachable!(),
        };
        let source = if policy.mode == eggreplay_core::RecordMode::AppendNew {
            let source = Session::open(&target, StoreLimits::default())
                .map_err(|_| FixtureError::new_err("append-new fixture is invalid"))?;
            if source.manifest().metadata.redaction_profile != "default-v1" {
                return Err(ConfigurationError::new_err(
                    "append-new requires the fixture default-v1 redaction profile",
                ));
            }
            Some(source)
        } else {
            None
        };
        let session = RecordingSession::create(
            &recording_path,
            SessionMetadata {
                capture_mode: finalization_kind.into(),
                target: Some(safe_target),
                ..Default::default()
            },
            StoreLimits::default(),
        )
        .map_err(|_| FixtureError::new_err("recording destination could not be created"))?;
        let ws = eggreplay_http::recording::WebSocketRecordingOptions {
            enabled: websockets,
            ..Default::default()
        };
        let (handle, finalization) = if let Some(source) = source {
            let replay = ReplayFixture::load_with_matcher(&source, Matcher::strict(8))
                .map_err(|_| FixtureError::new_err("append-new fixture cannot be replayed"))?;
            let handle = replay
                .start_append_new(
                    bind,
                    max_body_bytes,
                    upstream,
                    client,
                    session.clone(),
                    RedactionConfig::default_secure(),
                    "default-v1".into(),
                    eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                    physical,
                )
                .await
                .map_err(|_| NetworkError::new_err("append-new replay server could not start"))?;
            let combined = sibling_path(&target, "combined");
            (
                handle,
                Finalization::Append {
                    session,
                    source: Box::new(source),
                    temporary: recording_path,
                    combined,
                    target,
                },
            )
        } else {
            let handle = eggreplay_http::recording::start_recording_gateway_with_websockets(
                bind,
                upstream,
                client,
                session.clone(),
                max_body_bytes,
                RedactionConfig::default_secure(),
                "default-v1".into(),
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                physical,
                ws,
            )
            .await
            .map_err(|_| NetworkError::new_err("recording gateway could not start"))?;
            let finalization = if policy.mode == eggreplay_core::RecordMode::ReRecord {
                Finalization::ReRecord {
                    session,
                    staging: recording_path,
                    target,
                }
            } else {
                Finalization::Once(session)
            };
            (handle, finalization)
        };
        Ok(PyServer::new(handle, finalization))
    })
}

/// Execute one stored baseline request against a candidate and return the
/// Rust-authored regression report.
#[pyfunction]
#[pyo3(signature = (fixture, flow_id, target, route="direct", max_body_bytes=16_777_216, compare_sse=false, compare_stream_events=false, cadence_tolerance_ns=None))]
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
fn regress_flow<'py>(
    py: Python<'py>,
    fixture: PyRef<'py, PyFixture>,
    flow_id: &str,
    target: &str,
    route: &str,
    max_body_bytes: u64,
    compare_sse: bool,
    compare_stream_events: bool,
    cadence_tolerance_ns: Option<u64>,
) -> PyResult<Bound<'py, PyAny>> {
    let session = Arc::clone(&fixture.session);
    let flow_id = flow_id.to_owned();
    let target: http::Uri = target
        .parse()
        .map_err(|_| ConfigurationError::new_err("invalid candidate target URI"))?;
    let (client, physical) = route_client(route)?;
    let policy = ComparisonPolicy {
        compare_stream_events,
        cadence_tolerance_ns,
        compare_sse,
        ..Default::default()
    };
    policy.validate().map_err(ConfigurationError::new_err)?;
    let read_session = Arc::clone(&session);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let (baseline, request_body, baseline_body, baseline_stream, websocket) =
            tokio::task::spawn_blocking(move || {
                let mut iter = read_session.iter_flows().map_err(|_| ())?;
                let flow = iter
                    .find_map(|item| item.ok().filter(|flow| flow.id == flow_id))
                    .ok_or(())?;
                let response_len = match &flow.outcome {
                    eggreplay_core::FlowOutcome::Response(response) => {
                        response.body.len().unwrap_or(0)
                    }
                    eggreplay_core::FlowOutcome::Error(_) => 0,
                };
                if flow.request.body.len().unwrap_or(0) > max_body_bytes
                    || response_len > max_body_bytes
                {
                    return Err(());
                }
                let request_body: Vec<u8> = match &flow.request.body {
                    eggreplay_core::BodyRef::Blob(blob) => {
                        read_session.read_blob(blob).map_err(|_| ())?
                    }
                    _ => Vec::new(),
                };
                let baseline_body: Vec<u8> = match &flow.outcome {
                    eggreplay_core::FlowOutcome::Response(response) => match &response.body {
                        eggreplay_core::BodyRef::Blob(blob) => {
                            read_session.read_blob(blob).map_err(|_| ())?
                        }
                        _ => Vec::new(),
                    },
                    eggreplay_core::FlowOutcome::Error(_) => Vec::new(),
                };
                let baseline_stream = read_session
                    .read_extension("stream-events")
                    .ok()
                    .flatten()
                    .and_then(|bytes| {
                        serde_json::from_slice::<eggreplay_core::StreamEvents>(&bytes).ok()
                    })
                    .and_then(|document| {
                        document
                            .flows
                            .into_iter()
                            .find(|item| item.flow_id == flow.id)
                    })
                    .unwrap_or_else(|| FlowStreamEvents {
                        flow_id: flow.id.clone(),
                        start_offset_ns: 0,
                        request: Vec::new(),
                        response: Vec::new(),
                    });
                let websocket = read_session
                    .read_extension("websocket-messages")
                    .ok()
                    .flatten()
                    .and_then(|bytes| {
                        serde_json::from_slice::<eggreplay_core::WebSocketTranscript>(&bytes).ok()
                    })
                    .and_then(|transcript| {
                        transcript
                            .conversations
                            .into_iter()
                            .find(|item| item.flow_id == flow.id)
                    });
                Ok((
                    flow,
                    request_body,
                    baseline_body,
                    baseline_stream,
                    websocket,
                ))
            })
            .await
            .map_err(|_| crate::errors::RegressionError::new_err("baseline flow read failed"))?
            .map_err(|()| {
                crate::errors::RegressionError::new_err(
                    "baseline flow unavailable or exceeds body limit",
                )
            })?;
        if let Some(conversation) = websocket {
            let findings = eggreplay_http::compare_websocket_candidate(
                &client,
                &session,
                &baseline.request,
                &target,
                &conversation,
                policy.websocket_cadence_tolerance_ns,
            )
            .await
            .map_err(|_| {
                crate::errors::RegressionError::new_err("WebSocket candidate execution failed")
            })?;
            let report = eggreplay_core::RegressionReport {
                schema_version: eggreplay_core::REPORT_SCHEMA_VERSION,
                scheduler: ReportScheduler::Sequential,
                baseline_flow_ids: vec![baseline.id.clone()],
                findings,
            };
            return Ok(crate::report::ReportView::from_report(report));
        }
        let observation = execute_candidate(
            &client,
            &baseline.request,
            &request_body,
            &target,
            max_body_bytes,
            Some(physical),
        )
        .await
        .map_err(|_| crate::errors::RegressionError::new_err("candidate execution failed"))?;
        let mut report = eggreplay_core::compare_flows_with_policy(
            &baseline,
            &observation.flow,
            &baseline_body,
            &observation.response_body,
            ReportScheduler::Sequential,
            &policy,
        );
        if policy.is_stream_enabled() {
            // Candidate execution materializes the baseline request body; it
            // does not observe candidate request frame cadence. Compare only
            // the response stream instead of fabricating request events.
            let mut comparable_baseline_stream = baseline_stream;
            comparable_baseline_stream.request.clear();
            let candidate_stream = FlowStreamEvents {
                flow_id: baseline.id.clone(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: observation.response_events,
            };
            report
                .findings
                .extend(eggreplay_core::compare_stream_events(
                    &comparable_baseline_stream,
                    &candidate_stream,
                    policy.cadence_tolerance_ns,
                ));
            report.findings.sort_by(|left, right| {
                (format!("{:?}", left.kind), &left.field)
                    .cmp(&(format!("{:?}", right.kind), &right.field))
            });
        }
        Ok(crate::report::ReportView::from_report(report))
    })
}

fn parse_bind(value: &str) -> PyResult<SocketAddr> {
    value
        .parse()
        .map_err(|_| ConfigurationError::new_err("bind must be a socket address"))
}

async fn finish_session(session: RecordingSession) -> Result<Session, String> {
    session.shutdown();
    let mut spins = 0;
    while session.active_blobs() != 0 && spins < 1000 {
        tokio::task::yield_now().await;
        spins += 1;
    }
    session
        .finish()
        .map_err(|_| "recording finalization failed".to_owned())
}

fn sibling_path(target: &Path, label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("fixture.eggr");
    target.with_file_name(format!(".{name}.{label}-{}-{nonce}", std::process::id()))
}

fn replace_fixture(staged: &Path, target: &Path) -> Result<(), std::io::Error> {
    if !target.exists() {
        return std::fs::rename(staged, target);
    }
    let backup = sibling_path(target, "backup");
    std::fs::rename(target, &backup)?;
    if let Err(error) = std::fs::rename(staged, target) {
        let _ = std::fs::rename(&backup, target);
        return Err(error);
    }
    std::fs::remove_dir_all(backup)
}

fn matcher(profile: &str) -> PyResult<Matcher> {
    match profile {
        "strict" => Ok(Matcher::strict(8)),
        "practical" => Ok(Matcher::practical(8)),
        _ => Err(ConfigurationError::new_err("unknown matcher profile")),
    }
}

fn route_client(route: &str) -> PyResult<(eggfetch_core::Client, PhysicalRoute)> {
    let connector = eggreplay_http::parse_route(route).map_err(ConfigurationError::new_err)?;
    let physical = eggreplay_http::physical_route_for(route, &connector);
    let dialer = connector.map_or_else(EggressDialer::direct, EggressDialer::new);
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .dialer(dialer)
        .build();
    Ok((client, physical))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyServer>()?;
    module.add_function(wrap_pyfunction!(replay_server, module)?)?;
    module.add_function(wrap_pyfunction!(recording_gateway, module)?)?;
    module.add_function(wrap_pyfunction!(regress_flow, module)?)?;
    Ok(())
}
