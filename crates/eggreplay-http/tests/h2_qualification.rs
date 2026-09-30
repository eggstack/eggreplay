#![cfg(feature = "h2")]
//! M014B HTTP/2 end-to-end qualification (experimental outbound tier).
//!
//! Product path under test is EggFetch with `native-http2` (ALPN `h2` over
//! local TLS). The hyper H2 server below is test harness only, never product
//! transport: EggReplay must not gain a parallel HTTP stack. Interop breadth
//! comes from three client families: EggFetch (product), hyper (independent
//! H2 client), and the raw `h2` crate (independent framing client).
//!
//! Tier under qualification: H2 record + regression-candidate execution are
//! experimental; H2 inbound serving (EggServe replay/gateway), H2
//! interception, and cleartext prior-knowledge remain unsupported.

use bytes::Bytes;
use eggreplay_core::{
    BodyRef, ConsumptionMode, ExtractionFailureBehavior, Flow, FlowOutcome, MatchCandidate,
    MatchResult, Matcher, MatcherSession, RedactionConfig, ReportScheduler, RequestPredicate,
    Scenario as ScenarioDef, ScenarioResponse, ScenarioRules, ScenarioTransition, SessionMetadata,
    compare_flows,
};
use eggreplay_http::h2::{HttpVersionPolicy, check_h2_headers};
use eggreplay_http::{execute_candidate, record_request, record_request_with_session};
use eggreplay_store::{RecordingSession, Session, SessionWriter, StoreLimits};
use http::{Method, Request, Response, StatusCode, Uri, Version};
use http_body::Frame;
use http_body_util::{BodyExt, Empty, StreamBody, combinators::BoxBody};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const LARGE_BODY_BYTES: usize = 8 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct SvcError(String);

impl std::fmt::Display for SvcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SvcError {}

type H2Body = BoxBody<Bytes, SvcError>;

fn boxed_full(body: &[u8]) -> H2Body {
    let stream = futures_util::stream::iter(vec![Ok::<_, SvcError>(Frame::data(
        Bytes::copy_from_slice(body),
    ))]);
    StreamBody::new(stream).boxed()
}

fn ok(body: &[u8]) -> Result<Response<H2Body>, SvcError> {
    Response::builder()
        .status(StatusCode::OK)
        .body(boxed_full(body))
        .map_err(|error| SvcError(error.to_string()))
}

fn router(request: Request<hyper::body::Incoming>) -> Result<Response<H2Body>, SvcError> {
    let path = request.uri().path().to_owned();
    if path == "/trailers" {
        let frames = vec![
            Ok::<_, SvcError>(Frame::data(Bytes::from_static(b"chunk-a"))),
            Ok::<_, SvcError>(Frame::data(Bytes::from_static(b"chunk-b"))),
        ];
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-trailer-echo", "yes".parse().expect("valid trailer"));
        let mut frames = frames;
        frames.push(Ok(Frame::trailers(trailers)));
        let body = StreamBody::new(futures_util::stream::iter(frames)).boxed();
        return Response::builder()
            .status(StatusCode::OK)
            .body(body)
            .map_err(|error| SvcError(error.to_string()));
    }
    if path == "/headers" {
        let names: Vec<String> = request
            .headers()
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect();
        let payload = serde_json::to_vec(&names).map_err(|error| SvcError(error.to_string()))?;
        return ok(&payload);
    }
    if path == "/large" {
        let chunk = Bytes::from(vec![0xAB; CHUNK_BYTES]);
        let frames = (0..LARGE_BODY_BYTES / CHUNK_BYTES)
            .map(|_| Ok::<_, SvcError>(Frame::data(chunk.clone())))
            .collect::<Vec<_>>();
        let body = StreamBody::new(futures_util::stream::iter(frames)).boxed();
        return Response::builder()
            .status(StatusCode::OK)
            .body(body)
            .map_err(|error| SvcError(error.to_string()));
    }
    if let Some(echo) = path.strip_prefix("/echo/") {
        return ok(echo.as_bytes());
    }
    if path == "/scenario" || path == "/match" {
        return ok(path.as_bytes());
    }
    if path == "/" {
        return ok(b"h2-ok");
    }
    if path == "/slow" || path == "/delayed" {
        return Err(SvcError("async-route".into()));
    }
    if path == "/never" {
        return Err(SvcError("never-route".into()));
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(boxed_full(b"nope"))
        .map_err(|error| SvcError(error.to_string()))
}

async fn async_router(
    request: Request<hyper::body::Incoming>,
) -> Result<Response<H2Body>, SvcError> {
    let path = request.uri().path().to_owned();
    if path == "/slow" || path == "/delayed" {
        tokio::time::sleep(Duration::from_millis(300)).await;
        return ok(b"slow-ok");
    }
    if path == "/never" {
        tokio::time::sleep(Duration::from_secs(30)).await;
        return ok(b"unreached");
    }
    router(request)
}

struct TestCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    ca_pem: String,
}

fn test_cert() -> TestCert {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("test cert");
    TestCert {
        cert_der: certified.cert.der().to_vec(),
        key_der: certified.key_pair.serialize_der(),
        ca_pem: certified.cert.pem(),
    }
}

fn server_tls(cert: &TestCert) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                cert.cert_der.clone(),
            )],
            rustls::pki_types::PrivateKeyDer::try_from(cert.key_der.clone())
                .expect("valid test key"),
        )
        .expect("valid test cert");
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
}

fn client_tls(cert: &TestCert) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            cert.cert_der.clone(),
        ))
        .expect("valid test CA");
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
}

/// Shared hyper H2-over-TLS harness (test transport only).
struct Harness {
    address: SocketAddr,
    cert: TestCert,
    task: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn base(&self) -> String {
        format!("https://localhost:{}", self.address.port())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_harness() -> Harness {
    let cert = test_cert();
    let config = server_tls(&cert);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("harness bind");
    let address = listener.local_addr().expect("harness addr");
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tls), service_fn(async_router))
                    .await;
            });
        }
    });
    Harness {
        address,
        cert,
        task,
    }
}

fn temp_fixture(name: &str) -> std::path::PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    std::env::temp_dir().join(format!(
        "eggreplay-h2-{name}-{}-{millis}",
        std::process::id()
    ))
}

fn h2_client(cert: &TestCert) -> eggfetch_core::Client {
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(cert.ca_pem.as_bytes())
        .expect("valid test CA")
        .build();
    eggfetch_core::Client::builder()
        .http_version_policy(HttpVersionPolicy::Http2Only)
        .tls_config(tls)
        .retry_canceled_requests(false)
        .build()
}

fn get_request(url: &str) -> Request<http_body_util::Full<Bytes>> {
    Request::builder()
        .method(Method::GET)
        .uri(url)
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("valid request")
}

async fn record_once(
    client: &eggfetch_core::Client,
    url: &str,
    name: &str,
    physical: Option<eggreplay_core::PhysicalRoute>,
) -> (Flow, Session) {
    let destination = temp_fixture(name);
    let mut writer = SessionWriter::create(
        &destination,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    let flow = record_request(
        client,
        &mut writer,
        get_request(url),
        &RedactionConfig::default_secure(),
        "h2-test-v1",
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical,
    )
    .await
    .expect("record");
    let session = writer.finish().expect("finish");
    (flow, session)
}

fn response_body(session: &Session, flow: &Flow) -> Vec<u8> {
    match &flow.outcome {
        FlowOutcome::Response(response) => match &response.body {
            BodyRef::Blob(blob) => session.read_blob(blob).expect("blob"),
            BodyRef::Empty | BodyRef::Absent => Vec::new(),
        },
        FlowOutcome::Error(error) => panic!("expected response, got error {error:?}"),
    }
}

fn has_annotation(flow: &Flow, key: &str, value: &str) -> bool {
    flow.annotations
        .iter()
        .any(|entry| entry.0 == key && entry.1 == value)
}

#[tokio::test]
async fn alpn_h2_records_with_version_annotation() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);

    let mut raw = client
        .request(Method::GET, &format!("{}/", harness.base()))
        .expect("request")
        .send()
        .await
        .expect("h2 response");
    assert_eq!(raw.version(), Version::HTTP_2);
    assert_eq!(raw.status(), StatusCode::OK);
    let body = raw.bytes().await.expect("body");
    assert_eq!(&body[..], b"h2-ok");

    let (flow, session) = record_once(&client, &format!("{}/", harness.base()), "alpn", None).await;
    assert!(has_annotation(&flow, "transport", "http-version:h2"));
    assert_eq!(response_body(&session, &flow).as_slice(), b"h2-ok");
    assert!(matches!(flow.outcome, FlowOutcome::Response(_)));
}

#[tokio::test]
async fn hyper_client_interop_h2() {
    let harness = start_harness().await;
    let tcp = TcpStream::connect(harness.address).await.expect("connect");
    let connector = tokio_rustls::TlsConnector::from(client_tls(&harness.cert));
    let server_name = rustls::pki_types::ServerName::try_from("localhost").expect("sni");
    let tls = connector.connect(server_name, tcp).await.expect("tls");
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("https://localhost:{}/", harness.address.port()))
        .body(Empty::<Bytes>::new())
        .expect("request");
    let response = sender.send_request(request).await.expect("response");
    assert_eq!(response.version(), Version::HTTP_2);
    let collected = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    assert_eq!(&collected[..], b"h2-ok");
}

#[tokio::test]
async fn raw_h2_crate_interop() {
    let harness = start_harness().await;
    let tcp = TcpStream::connect(harness.address).await.expect("connect");
    let connector = tokio_rustls::TlsConnector::from(client_tls(&harness.cert));
    let server_name = rustls::pki_types::ServerName::try_from("localhost").expect("sni");
    let tls = connector.connect(server_name, tcp).await.expect("tls");
    let (mut sender, connection) = h2::client::handshake(tls).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("https://localhost:{}/", harness.address.port()))
        .body(())
        .expect("request");
    let (future, _stream) = sender.send_request(request, true).expect("send");
    let response = future.await.expect("response");
    assert_eq!(response.version(), Version::HTTP_2);
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        bytes.extend_from_slice(&chunk.expect("data"));
    }
    assert_eq!(bytes.as_slice(), b"h2-ok");
}

#[tokio::test]
async fn concurrent_streams_record_independently() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let destination = temp_fixture("concurrent");
    let session = RecordingSession::create(
        &destination,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("session");
    let mut tasks = Vec::new();
    for index in 0..8 {
        let task_client = client.clone();
        let task_session = session.clone();
        let url = format!("{}/echo/stream-{index}", harness.base());
        tasks.push(tokio::spawn(async move {
            record_request_with_session(
                &task_client,
                &task_session,
                get_request(&url),
                &RedactionConfig::default_secure(),
                "h2-test-v1",
                eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
                None,
            )
            .await
            .expect("record")
        }));
    }
    let mut bodies = Vec::new();
    for task in tasks {
        let flow = task.await.expect("join");
        assert!(has_annotation(&flow, "transport", "http-version:h2"));
        match &flow.outcome {
            FlowOutcome::Response(response) => match &response.body {
                BodyRef::Blob(blob) => bodies.push(blob.sha256.clone()),
                other => panic!("expected blob body, got {other:?}"),
            },
            FlowOutcome::Error(error) => panic!("expected response, got {error:?}"),
        }
    }
    assert_eq!(bodies.len(), 8);
    session.shutdown();
    let finished = session.finish().expect("finish");
    let flows: Vec<Flow> = finished
        .iter_flows()
        .expect("flows")
        .collect::<Result<_, _>>()
        .expect("parse");
    assert_eq!(flows.len(), 8);
    let _ = std::fs::remove_dir_all(destination);
}

#[tokio::test]
async fn response_trailers_captured_with_events() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let (flow, _session) = record_once(
        &client,
        &format!("{}/trailers", harness.base()),
        "trailers",
        None,
    )
    .await;
    let FlowOutcome::Response(response) = &flow.outcome else {
        panic!("expected response");
    };
    assert!(
        response
            .trailers
            .iter()
            .any(|entry| entry.name == "x-trailer-echo" && entry.value == "yes"),
        "h2 trailers must be captured: {:?}",
        response.trailers
    );

    // Candidate execution must observe the same trailer + terminal events.
    let target: Uri = harness.base().parse().expect("uri");
    let observation = execute_candidate(&client, &flow.request, &[], &target, 16 << 20, None)
        .await
        .expect("candidate");
    assert_eq!(observation.response_body, b"chunk-achunk-b");
    let kinds: Vec<String> = observation
        .response_events
        .iter()
        .map(|event| match &event.event {
            eggreplay_core::StreamEventKind::Data { .. } => "data".to_string(),
            eggreplay_core::StreamEventKind::Trailers { .. } => "trailers".to_string(),
            eggreplay_core::StreamEventKind::End => "end".to_string(),
            eggreplay_core::StreamEventKind::Error { .. } => "error".to_string(),
        })
        .collect();
    assert!(
        kinds.contains(&"trailers".to_string()),
        "candidate events must include trailers: {kinds:?}"
    );
    assert_eq!(kinds.last().expect("events"), "end");
}

#[tokio::test]
async fn large_streaming_body_completes() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let (flow, session) =
        record_once(&client, &format!("{}/large", harness.base()), "large", None).await;
    let body = response_body(&session, &flow);
    assert_eq!(body.len(), LARGE_BODY_BYTES);
    assert!(body.iter().all(|byte| *byte == 0xAB));
}

#[tokio::test]
async fn cancelled_stream_leaves_sibling_intact() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);

    // Drop one stream mid-body while a sibling completes on the same client.
    let doomed = client
        .request(Method::GET, &format!("{}/never", harness.base()))
        .expect("request")
        .send()
        .await
        .expect("headers");
    assert_eq!(doomed.version(), Version::HTTP_2);
    drop(doomed);

    let (flow, session) =
        record_once(&client, &format!("{}/", harness.base()), "sibling", None).await;
    assert_eq!(response_body(&session, &flow).as_slice(), b"h2-ok");

    // The client connection is uncorrupted: a third stream succeeds.
    let mut third = client
        .request(Method::GET, &format!("{}/", harness.base()))
        .expect("request")
        .send()
        .await
        .expect("third response");
    assert_eq!(third.version(), Version::HTTP_2);
    assert_eq!(&third.bytes().await.expect("body")[..], b"h2-ok");
}

#[tokio::test]
async fn target_remap_preserves_h2() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let (flow, _) = record_once(
        &client,
        &format!("{}/echo/remapped", harness.base()),
        "remap",
        None,
    )
    .await;
    assert_eq!(flow.request.path, "/echo/remapped");
    assert!(has_annotation(&flow, "transport", "http-version:h2"));
}

#[tokio::test]
async fn h1_connection_headers_do_not_leak() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    // EggFetch strips H1-only headers for H2; the server must never see them.
    let mut raw = client
        .request(Method::GET, &format!("{}/headers", harness.base()))
        .expect("request")
        .header("connection", "keep-alive")
        .header("transfer-encoding", "chunked")
        .send()
        .await
        .expect("response");
    assert_eq!(raw.version(), Version::HTTP_2);
    let payload = raw.bytes().await.expect("body");
    let names: Vec<String> = serde_json::from_slice(&payload).expect("json");
    assert!(
        !names.iter().any(|name| name == "connection"),
        "connection header leaked: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "transfer-encoding"),
        "transfer-encoding header leaked: {names:?}"
    );

    // The EggReplay boundary helper rejects the same headers up front.
    let mut forbidden = http::HeaderMap::new();
    forbidden.insert("connection", "keep-alive".parse().expect("value"));
    check_h2_headers(&forbidden).expect_err("connection must be rejected");
}

#[tokio::test]
async fn cleartext_prior_knowledge_fails_closed() {
    // Plain H1 server: Http2Only without TLS must fail, never downgrade.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let Ok(read) = socket.read(&mut buffer).await else {
                    return;
                };
                let _ = read;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nh1")
                    .await;
            });
        }
    });
    let client = h2_client(&test_cert());
    let result = client
        .request(Method::GET, &format!("http://{address}/"))
        .expect("request")
        .send()
        .await;
    assert!(
        result.is_err(),
        "Http2Only against cleartext must fail closed"
    );
}

#[tokio::test]
async fn graceful_shutdown_completes_inflight() {
    let cert = test_cert();
    let config = server_tls(&cert);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let (signal, receiver) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let Ok(tls) = acceptor.accept(tcp).await else {
            return;
        };
        let mut connection = Box::pin(
            hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service_fn(async_router)),
        );
        tokio::select! {
            result = &mut connection => {
                let _ = result;
            }
            _ = receiver => {
                connection.as_mut().graceful_shutdown();
                let _ = connection.await;
            }
        }
    });

    let client = h2_client(&cert);
    let task_client = client.clone();
    let task_base = format!("https://localhost:{}", address.port());
    let recording = tokio::spawn(async move {
        let destination = temp_fixture("goaway");
        let mut writer = SessionWriter::create(
            &destination,
            SessionMetadata::default(),
            StoreLimits::default(),
        )
        .expect("writer");
        let flow = record_request(
            &task_client,
            &mut writer,
            get_request(&format!("{task_base}/slow")),
            &RedactionConfig::default_secure(),
            "h2-test-v1",
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            None,
        )
        .await
        .expect("in-flight record survives GOAWAY");
        let session = writer.finish().expect("finish");
        (flow, session, destination)
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = signal.send(());
    let (flow, session, destination) = recording.await.expect("join");
    assert_eq!(response_body(&session, &flow).as_slice(), b"slow-ok");
    let _ = std::fs::remove_dir_all(destination);
}

#[cfg(feature = "eggress")]
#[tokio::test]
async fn routed_h2_via_eggress_tcp() {
    use eggreplay_http::{EggressDialer, physical_route_for};

    let harness = start_harness().await;
    let (socks, socks_task) = start_socks5(harness.address).await;
    let route = format!("socks5://127.0.0.1:{}", socks.port());
    let physical = physical_route_for(
        &route,
        &Some(eggress_outbound::OutboundConnector::from_pproxy_uri(&route).expect("route parses")),
    );
    assert_eq!(physical.kind, "eggress");
    let connector =
        eggress_outbound::OutboundConnector::from_pproxy_uri(&route).expect("route parses");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(harness.cert.ca_pem.as_bytes())
        .expect("valid test CA")
        .build();
    // SNI/ALPN ownership stays in EggFetch: the dialer only moves TCP bytes.
    let client = eggfetch_core::Client::builder()
        .http_version_policy(HttpVersionPolicy::Http2Only)
        .tls_config(tls)
        .retry_canceled_requests(false)
        .dialer(EggressDialer::new(connector))
        .build();
    let (flow, session) = record_once(
        &client,
        &format!("{}/", harness.base()),
        "routed",
        Some(physical),
    )
    .await;
    assert_eq!(response_body(&session, &flow).as_slice(), b"h2-ok");
    assert!(has_annotation(&flow, "transport", "http-version:h2"));
    let stored = flow.physical_route.expect("physical route");
    assert_eq!(stored.kind, "eggress");
    socks_task.abort();
}

/// Minimal test-only SOCKS5 CONNECT relay (no auth, TCP only). The relay
/// always dials `upstream`; request parsing exists only to consume the
/// handshake bytes a real client sends.
#[cfg(feature = "eggress")]
async fn start_socks5(upstream: SocketAddr) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("socks bind");
    let address = listener.local_addr().expect("socks addr");
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = relay_socks(socket, upstream).await;
            });
        }
    });
    (address, task)
}

#[cfg(feature = "eggress")]
async fn relay_socks(
    mut socket: TcpStream,
    upstream: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut header = [0u8; 2];
    socket.read_exact(&mut header).await?;
    let methods = usize::from(header[1]);
    let mut offered = vec![0u8; methods];
    socket.read_exact(&mut offered).await?;
    socket.write_all(&[0x05, 0x00]).await?;
    let mut request = [0u8; 4];
    socket.read_exact(&mut request).await?;
    match request[3] {
        0x01 => {
            let mut rest = [0u8; 6];
            socket.read_exact(&mut rest).await?;
        }
        0x03 => {
            let mut length = [0u8; 1];
            socket.read_exact(&mut length).await?;
            let mut rest = vec![0u8; usize::from(length[0]) + 2];
            socket.read_exact(&mut rest).await?;
        }
        0x04 => {
            let mut rest = [0u8; 18];
            socket.read_exact(&mut rest).await?;
        }
        _ => return Err("unsupported SOCKS address type".into()),
    }
    socket
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let mut target = TcpStream::connect(upstream).await?;
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut target).await;
    Ok(())
}

#[tokio::test]
async fn strict_and_practical_matching_on_h2_flows() {
    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let (flow, _) = record_once(
        &client,
        &format!("{}/match?stable=1", harness.base()),
        "matching",
        None,
    )
    .await;
    let request_body: Vec<u8> = Vec::new();
    let candidates = vec![MatchCandidate::new(flow.clone(), request_body.clone())];

    let strict = Matcher::strict(8);
    let mut strict_session = MatcherSession::new();
    assert!(matches!(
        strict.select(
            &flow.request,
            &request_body,
            &candidates,
            ConsumptionMode::Once,
            &mut strict_session,
        ),
        MatchResult::Matched(0)
    ));

    // Practical ignores volatile headers; strict does not.
    let mut volatile = flow.request.clone();
    volatile.headers.push(eggreplay_core::HeaderEntry {
        name: "x-request-id".to_string(),
        value: "different".to_string(),
    });
    let practical = Matcher::practical(8);
    let mut practical_session = MatcherSession::new();
    assert!(matches!(
        practical.select(
            &volatile,
            &request_body,
            &candidates,
            ConsumptionMode::Once,
            &mut practical_session,
        ),
        MatchResult::Matched(0)
    ));
    let mut strict_session_b = MatcherSession::new();
    assert!(matches!(
        strict.select(
            &volatile,
            &request_body,
            &candidates,
            ConsumptionMode::Once,
            &mut strict_session_b,
        ),
        MatchResult::NoMatch { .. }
    ));
}

#[tokio::test]
async fn scenario_advance_on_h2_recorded_request() {
    use eggreplay_core::RULES_SCHEMA_VERSION;

    let harness = start_harness().await;
    let client = h2_client(&harness.cert);
    let (flow, _) = record_once(
        &client,
        &format!("{}/scenario", harness.base()),
        "scenario",
        None,
    )
    .await;
    let rules = ScenarioRules {
        schema_version: RULES_SCHEMA_VERSION,
        scenarios: vec![ScenarioDef {
            id: "h2".to_string(),
            initial_state: "s0".to_string(),
            states: vec!["s0".to_string(), "s1".to_string()],
            transitions: vec![ScenarioTransition {
                from: "s0".to_string(),
                when: vec![RequestPredicate::Path {
                    value: "/scenario".to_string(),
                }],
                extract: Vec::new(),
                extraction_failure: ExtractionFailureBehavior::Abort,
                response: ScenarioResponse {
                    status: 200,
                    headers: Vec::new(),
                    body_template: "scenario-h2".to_string(),
                    json_pointer_replacements: Vec::new(),
                },
                next_state: "s1".to_string(),
            }],
        }],
    };
    let mut runtime = rules.runtime("h2").expect("runtime");
    let step = runtime
        .advance(&flow.request, &[])
        .expect("advance")
        .expect("transition");
    assert_eq!(step.response.body.as_slice(), b"scenario-h2");
    assert_eq!(runtime.state(), "s1");
}

#[cfg(feature = "eggserve")]
#[tokio::test]
async fn regression_over_h2_and_scenario_replay_over_h1() {
    use eggreplay_core::{Matcher as CoreMatcher, RULES_SCHEMA_VERSION};
    use eggreplay_http::ReplayFixture;

    let harness = start_harness().await;
    let client = h2_client(&harness.cert);

    // Record the baseline over H2, then author a rules extension so the
    // same session can drive scenario replay.
    let destination = temp_fixture("reg-replay");
    let mut writer = SessionWriter::create(
        &destination,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    let flow = record_request(
        &client,
        &mut writer,
        get_request(&format!("{}/trailers", harness.base())),
        &RedactionConfig::default_secure(),
        "h2-test-v1",
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        None,
    )
    .await
    .expect("record");
    let rules = ScenarioRules {
        schema_version: RULES_SCHEMA_VERSION,
        scenarios: vec![ScenarioDef {
            id: "h2replay".to_string(),
            initial_state: "s0".to_string(),
            states: vec!["s0".to_string()],
            transitions: vec![ScenarioTransition {
                from: "s0".to_string(),
                when: vec![RequestPredicate::Path {
                    value: "/trailers".to_string(),
                }],
                extract: Vec::new(),
                extraction_failure: ExtractionFailureBehavior::Abort,
                response: ScenarioResponse {
                    status: 200,
                    headers: Vec::new(),
                    body_template: "replayed-over-h1".to_string(),
                    json_pointer_replacements: Vec::new(),
                },
                next_state: "s0".to_string(),
            }],
        }],
    };
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
    let baseline_body = response_body(&session, &flow);

    // Regression re-executes over H2; deterministic trailers must match.
    let target: Uri = harness.base().parse().expect("uri");
    let observation = execute_candidate(&client, &flow.request, &[], &target, 16 << 20, None)
        .await
        .expect("candidate");
    let report = compare_flows(
        &flow,
        &observation.flow,
        &baseline_body,
        &observation.response_body,
        ReportScheduler::Sequential,
    );
    assert!(
        report.is_success(),
        "h2 regression must be clean: {:?}",
        report.findings
    );

    // Replay stays H1 (EggServe direct runtime is H1-only): strict
    // same-origin matching cannot cross the https/http scheme boundary by
    // design, so the H2-recorded session drives H1 scenario replay instead.
    // The flow store is version-neutral; only the transport differs.
    let fixture = ReplayFixture::load_with_scenario(&session, CoreMatcher::strict(8), "h2replay")
        .expect("scenario fixture");
    let server = fixture
        .start("127.0.0.1:0".parse().expect("addr"), 16 << 20)
        .await
        .expect("replay server");
    let plain = eggfetch_core::Client::builder()
        .http_version_policy(HttpVersionPolicy::Http1Only)
        .retry_canceled_requests(false)
        .build();
    let mut replayed = plain
        .request(
            Method::GET,
            &format!("http://{}/trailers", server.local_addr()),
        )
        .expect("request")
        .send()
        .await
        .expect("replay response");
    assert_eq!(replayed.version(), Version::HTTP_11);
    assert_eq!(
        &replayed.bytes().await.expect("body")[..],
        b"replayed-over-h1"
    );
    server.shutdown();
    server.wait().await;
    let _ = std::fs::remove_dir_all(destination);
}
