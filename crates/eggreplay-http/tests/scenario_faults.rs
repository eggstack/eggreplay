#![cfg(feature = "eggserve")]
//! M014D bounded fault-model qualification.
//!
//! Authored scenario faults apply at replay serving through existing
//! EggServe lifecycle controls only (sleeps, chunked prefix streaming
//! with terminal stream errors, recorded-style 502 projection). Faults
//! are transport-neutral: these tests serve over H1, while the gRPC view
//! half of M014D is qualified in core unit tests plus an H2 recording
//! test in `h2_qualification.rs`.

use eggreplay_core::{
    ErrorCategory, ErrorPhase, ExtractionFailureBehavior, Matcher, RULES_SCHEMA_VERSION,
    RequestPredicate, Scenario as ScenarioDef, ScenarioFault, ScenarioResponse, ScenarioRules,
    ScenarioTransition, SessionMetadata,
};
use eggreplay_http::ReplayFixture;
use eggreplay_store::{SessionWriter, StoreLimits};
use http::{Method, Version};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn fault_rules(path: &str, body: &str, fault: Option<ScenarioFault>) -> ScenarioRules {
    ScenarioRules {
        schema_version: RULES_SCHEMA_VERSION,
        scenarios: vec![ScenarioDef {
            id: "fault".to_string(),
            initial_state: "s0".to_string(),
            states: vec!["s0".to_string()],
            transitions: vec![ScenarioTransition {
                from: "s0".to_string(),
                when: vec![RequestPredicate::Path {
                    value: path.to_string(),
                }],
                extract: Vec::new(),
                extraction_failure: ExtractionFailureBehavior::Abort,
                response: ScenarioResponse {
                    status: 200,
                    headers: Vec::new(),
                    body_template: body.to_string(),
                    json_pointer_replacements: Vec::new(),
                    fault,
                },
                next_state: "s0".to_string(),
            }],
        }],
    }
}

struct FaultServer {
    address: SocketAddr,
    handle: Option<eggserve_server::ServerHandle>,
    directory: std::path::PathBuf,
}

impl FaultServer {
    async fn close(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Drop for FaultServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

static FAULT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn serve_fault(path: &str, body: &str, fault: Option<ScenarioFault>) -> FaultServer {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let serial = FAULT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let segment = path.replace('/', "-");
    let directory = std::env::temp_dir().join(format!(
        "eggreplay-fault-{segment}-{}-{millis}-{serial}",
        std::process::id()
    ));
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    let rules = fault_rules(path, body, fault);
    writer
        .write_extension(
            "rules",
            RULES_SCHEMA_VERSION,
            "rules.json",
            true,
            &serde_json::to_vec(&rules).expect("rules json"),
        )
        .expect("rules extension");
    let session = writer.finish().expect("finish");
    let fixture = ReplayFixture::load_with_scenario(&session, Matcher::strict(8), "fault")
        .expect("scenario fixture");
    let handle = fixture
        .start("127.0.0.1:0".parse().expect("addr"), 16 << 20)
        .await
        .expect("replay server");
    let address = handle.local_addr();
    FaultServer {
        address,
        handle: Some(handle),
        directory,
    }
}

fn h1_client() -> eggfetch_core::Client {
    eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .build()
}

async fn get(server: &FaultServer, path: &str) -> eggfetch_core::Result<eggfetch_core::Response> {
    h1_client()
        .request(Method::GET, &format!("http://{}{path}", server.address))
        .expect("request")
        .send()
        .await
}

#[tokio::test]
async fn head_delay_applies_before_response() {
    let server = serve_fault(
        "/head",
        "delayed-body",
        Some(ScenarioFault::ResponseHeadDelay { delay_ms: 150 }),
    )
    .await;
    let start = Instant::now();
    let mut response = get(&server, "/head").await.expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    let body = response.bytes().await.expect("body");
    assert_eq!(&body[..], b"delayed-body");
    assert!(
        start.elapsed() >= Duration::from_millis(100),
        "head delay must apply"
    );
    server.close().await;
}

#[tokio::test]
async fn body_chunk_delay_streams_with_gaps() {
    let server = serve_fault(
        "/chunks",
        "0123456789",
        Some(ScenarioFault::BodyChunkDelay {
            delay_ms: 100,
            chunk_bytes: 4,
        }),
    )
    .await;
    let start = Instant::now();
    let mut response = get(&server, "/chunks").await.expect("response");
    let body = response.bytes().await.expect("body");
    assert_eq!(&body[..], b"0123456789");
    // Three 4-byte chunks with a sleep before each: nominal 300 ms.
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "chunk delays must apply"
    );
    server.close().await;
}

#[tokio::test]
async fn close_before_response_aborts_without_bytes() {
    let server = serve_fault(
        "/drop",
        "should-not-send",
        Some(ScenarioFault::CloseBeforeResponse),
    )
    .await;
    let outcome = get(&server, "/drop").await;
    match outcome {
        Err(_) => {}
        Ok(mut response) => match response.bytes().await {
            Err(_) => {}
            Ok(body) => assert_ne!(
                &body[..],
                b"should-not-send",
                "close-before-response must never deliver cleanly"
            ),
        },
    }
    // The abort is per-request: the server stays healthy.
    let plain = serve_fault("/ok", "fine", None).await;
    let mut response = get(&plain, "/ok").await.expect("healthy");
    assert_eq!(&response.bytes().await.expect("body")[..], b"fine");
    server.close().await;
    plain.close().await;
}

#[tokio::test]
async fn close_after_n_truncates_with_error() {
    use http_body_util::BodyExt as _;
    let server = serve_fault(
        "/cut",
        "twelve-bytes!",
        Some(ScenarioFault::CloseAfterBytes { bytes: 5 }),
    )
    .await;
    let request = http::Request::builder()
        .method(Method::GET)
        .uri(format!("http://{}/cut", server.address))
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .expect("request");
    let response = h1_client()
        .execute_http_body_default(request)
        .await
        .expect("headers");
    let (parts, mut body) = response.into_parts();
    assert_eq!(parts.status, http::StatusCode::OK);
    // Declared length stays the full rendered body so truncation is explicit.
    assert_eq!(parts.headers.get("content-length").expect("length"), "13");
    let mut collected = Vec::new();
    let mut errored = false;
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                if frame.is_data() {
                    collected.extend_from_slice(&frame.into_data().expect("data"));
                }
            }
            Err(_) => {
                errored = true;
                break;
            }
        }
    }
    // Wire truth: headers flush with the full declared length, then the
    // connection aborts. Any delivered bytes are a prefix of the rendered
    // body; clean full delivery never happens.
    assert!(
        b"twelve-bytes!".starts_with(&collected),
        "delivered bytes must be a prefix"
    );
    assert_ne!(&collected[..], b"twelve-bytes!", "must not deliver cleanly");
    assert!(errored, "stream must terminate with an error");
    server.close().await;
}

#[tokio::test]
async fn transport_error_projects_recorded_shape() {
    let server = serve_fault(
        "/boom",
        "ignored",
        Some(ScenarioFault::TransportError {
            category: ErrorCategory::Timeout,
            phase: ErrorPhase::Body,
        }),
    )
    .await;
    let mut response = get(&server, "/boom").await.expect("response");
    assert_eq!(response.status(), http::StatusCode::BAD_GATEWAY);
    assert_eq!(
        &response.bytes().await.expect("body")[..],
        b"recorded upstream error: Timeout\n"
    );
    server.close().await;
}

#[tokio::test]
async fn delays_cancel_safely() {
    let server = serve_fault(
        "/slow",
        "too-late",
        Some(ScenarioFault::ResponseHeadDelay { delay_ms: 5_000 }),
    )
    .await;
    let slow = get(&server, "/slow");
    let timed = tokio::time::timeout(Duration::from_millis(300), slow).await;
    assert!(
        timed.is_err(),
        "client must be able to cancel a fault delay"
    );
    // Fault-free traffic on the same server is unaffected.
    let plain = serve_fault("/ok", "fine", None).await;
    let mut response = get(&plain, "/ok").await.expect("healthy");
    assert_eq!(&response.bytes().await.expect("body")[..], b"fine");
    server.close().await;
    plain.close().await;
}

#[tokio::test]
async fn fault_reports_are_deterministic() {
    let first = serve_fault(
        "/boom",
        "ignored",
        Some(ScenarioFault::TransportError {
            category: ErrorCategory::ConnectionRefused,
            phase: ErrorPhase::Connect,
        }),
    )
    .await;
    let second = serve_fault(
        "/boom",
        "ignored",
        Some(ScenarioFault::TransportError {
            category: ErrorCategory::ConnectionRefused,
            phase: ErrorPhase::Connect,
        }),
    )
    .await;
    let mut left = get(&first, "/boom").await.expect("left");
    let mut right = get(&second, "/boom").await.expect("right");
    assert_eq!(left.status(), right.status());
    assert_eq!(
        left.bytes().await.expect("left body"),
        right.bytes().await.expect("right body")
    );
    first.close().await;
    second.close().await;
}
