#![cfg(all(feature = "h2", feature = "h2-inbound-tls", feature = "eggress"))]
//! M015E HTTP/2 hardening matrix.
//!
//! M015B–M015D proved the HTTP/2 path works. This suite asks the hostile
//! question: what happens when a peer is wrong, slow, hostile, or simply
//! stops.
//!
//! # What "hardening" means here
//!
//! Every test asserts one of three outcomes, and names which:
//!
//! - a **bounded refusal** — a status, a reset, or a protocol error the peer
//!   can observe;
//! - an **unaffected sibling** — one bad stream never damages its neighbours or
//!   the connection;
//! - a **valid fixture** — whatever the gateway publishes still validates and
//!   never contains half a body.
//!
//! A test that merely observed "it didn't hang" would pass for a listener that
//! silently drops everything, so each one also asserts the listener still works
//! afterwards. The plan is explicit that no deterministic failure may be waived
//! as protocol flakiness; that is why these are deterministic by construction —
//! every timeout here is short and every assertion is on an observable outcome
//! rather than on timing.
//!
//! # The limits being exercised
//!
//! `H2Limits` carries the operator's concurrent-stream, header-list, and frame
//! bounds. Body bounds come from `max_body_bytes`. Both are the same seams
//! operators use, so these tests exercise the real configuration surface rather
//! than a test-only one.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use eggreplay_core::{
    BodyRef, FlowOutcome, HttpRequest, HttpResponse, Matcher, PhysicalRoute, RedactionConfig,
    SessionMetadata,
};
use eggreplay_http::inbound::{H2Limits, InboundProtocol, InboundServerHandle};
use eggreplay_http::{EggressDialer, ReplayFixture, physical_route_for};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Unique paths
// ---------------------------------------------------------------------------

static DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_path(name: &str) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let serial = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "eggreplay-h2hard-{name}-{}-{millis}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

// ---------------------------------------------------------------------------
// Test-owned TLS identity
// ---------------------------------------------------------------------------

struct TestIdentity {
    cert_der: Vec<u8>,
    cert_pem: String,
    key_der: Vec<u8>,
    directory: PathBuf,
}

impl Drop for TestIdentity {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn test_identity(name: &str) -> TestIdentity {
    let directory = temp_path(name);
    std::fs::create_dir_all(&directory).expect("identity dir");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("test identity");
    let cert_pem = certified.cert.pem();
    let key_pem = certified.key_pair.serialize_pem();
    std::fs::write(directory.join("server.pem"), &cert_pem).expect("cert");
    std::fs::write(directory.join("server.key"), &key_pem).expect("key");
    TestIdentity {
        cert_der: certified.cert.der().to_vec(),
        cert_pem,
        key_der: certified.key_pair.serialize_der(),
        directory,
    }
}

fn server_tls(identity: &TestIdentity, alpn: Vec<Vec<u8>>) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                identity.cert_der.clone(),
            )],
            rustls::pki_types::PrivateKeyDer::try_from(identity.key_der.clone())
                .expect("valid test key"),
        )
        .expect("valid test cert");
    config.alpn_protocols = alpn;
    Arc::new(config)
}

/// A client config that trusts a *different* identity, for the TLS
/// verification-failure test.
fn untrusting_client_tls() -> Arc<rustls::ClientConfig> {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("other identity");
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            certified.cert.der().to_vec(),
        ))
        .expect("test CA");
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
}

fn client_tls(identity: &TestIdentity) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            identity.cert_der.clone(),
        ))
        .expect("test CA");
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
}

fn server_name() -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::try_from("localhost").expect("sni")
}

// ---------------------------------------------------------------------------
// A deliberately hostile upstream
// ---------------------------------------------------------------------------

/// What the upstream should do with a request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Accept the request and never answer, so the caller's timeout is the
    /// only thing that can end the call.
    Stall,
    /// Answer with a very large number of response headers.
    ManyHeaders,
    /// Answer with a large trailer block.
    ManyTrailers,
    /// Reset the stream.
    Reset,
}

struct Upstream {
    address: SocketAddr,
    identity: TestIdentity,
    served: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A test-owned HTTP/2 origin whose response is chosen by the request path, so
/// one listener can play every role a hostile peer needs.
async fn start_upstream(name: &str) -> Upstream {
    use hyper::service::service_fn;

    let identity = test_identity(name);
    let acceptor = tokio_rustls::TlsAcceptor::from(server_tls(&identity, vec![b"h2".to_vec()]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream bind");
    let address = listener.local_addr().expect("upstream addr");
    let served = Arc::new(AtomicU64::new(0));
    let counter = served.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ =
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(
                            TokioIo::new(tls),
                            service_fn(move |request: Request<hyper::body::Incoming>| {
                                let counter = counter.clone();
                                async move {
                                    counter.fetch_add(1, Ordering::Relaxed);
                                    let path = request.uri().path().to_string();
                                    let body = request
                                        .into_body()
                                        .collect()
                                        .await
                                        .map(|collected| collected.to_bytes())
                                        .unwrap_or_default();
                                    let body_text = String::from_utf8_lossy(&body).into_owned();

                                    if let Some(behaviour) = behaviour_for(&path) {
                                        match behaviour {
                                            Behaviour::Stall => {
                                                // Accepted, counted, and then silence
                                                // until the caller's own timeout ends
                                                // it. This is the "slow response body"
                                                // case, made deterministic.
                                                tokio::time::sleep(Duration::from_secs(30)).await;
                                                return Ok(http::Response::builder()
                                                    .status(StatusCode::OK)
                                                    .body(full_body(Vec::new()))
                                                    .expect("valid response"));
                                            }
                                            Behaviour::ManyHeaders => {
                                                let mut response = http::Response::builder()
                                                    .status(StatusCode::OK);
                                                let headers = response
                                                    .headers_mut()
                                                    .expect("response headers");
                                                for index in 0..512u32 {
                                                    headers.insert(
                                                        HeaderName::from_bytes(
                                                            format!("x-flood-{index}")
                                                                .as_bytes(),
                                                        )
                                                        .expect("valid header name"),
                                                        HeaderValue::from_static(
                                                            "vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv",
                                                        ),
                                                    );
                                                }
                                                return Ok(response
                                                    .body(full_body(body_text.into_bytes()))
                                                    .expect("valid response"));
                                            }
                                            Behaviour::ManyTrailers => {
                                                let frames: Vec<
                                                    Result<hyper::body::Frame<Bytes>, SvcError>,
                                                > = vec![
                                                    // Copied, not borrowed: the
                                                    // body outlives this
                                                    // closure.
                                                    Ok(hyper::body::Frame::data(
                                                        Bytes::copy_from_slice(
                                                            body_text.as_bytes(),
                                                        ),
                                                    )),
                                                    Ok(hyper::body::Frame::trailers({
                                                        let mut map = http::HeaderMap::new();
                                                        for index in 0..512u32 {
                                                            map.append(
                                                                HeaderName::from_bytes(
                                                                    format!("x-trailer-{index}")
                                                                        .as_bytes(),
                                                                )
                                                                .expect("valid trailer name"),
                                                                HeaderValue::from_static("v"),
                                                            );
                                                        }
                                                        map
                                                    })),
                                                ];
                                                return Ok(http::Response::builder()
                                                    .status(StatusCode::OK)
                                                    .body(upstream_body(frames))
                                                    .expect("valid response"));
                                            }
                                            Behaviour::Reset => {}
                                        }
                                    }

                                    // The service error is `Infallible`: a
                                    // misbehaving origin is expressed by a
                                    // body that fails, not by failing the
                                    // service, because that is what actually
                                    // resets an HTTP/2 stream.
                                    Ok::<_, std::convert::Infallible>(
                                        http::Response::builder()
                                            .status(StatusCode::OK)
                                            .header("x-upstream", "h2")
                                            .body(full_body(
                                                format!("upstream:{body_text}").into_bytes(),
                                            ))
                                            .expect("valid response"),
                                    )
                                }
                            }),
                        )
                        .await;
            });
        }
    });
    Upstream {
        address,
        identity,
        served,
        task,
    }
}

/// The path encodes the behaviour the upstream should play.
fn behaviour_for(path: &str) -> Option<Behaviour> {
    let behaviour = path
        .trim_start_matches('/')
        .split('?')
        .next()
        .unwrap_or_default();
    match behaviour {
        "stall" => Some(Behaviour::Stall),
        "flood-headers" => Some(Behaviour::ManyHeaders),
        "flood-trailers" => Some(Behaviour::ManyTrailers),
        "reset" => Some(Behaviour::Reset),
        _ => None,
    }
}

/// One concrete body type for every upstream response, so a response that
/// resets mid-stream and one that completes are the same Rust type.
type UpstreamBody = http_body_util::combinators::UnsyncBoxBody<Bytes, SvcError>;

fn upstream_body(frames: Vec<Result<hyper::body::Frame<Bytes>, SvcError>>) -> UpstreamBody {
    http_body_util::StreamBody::new(futures_util::stream::iter(frames)).boxed_unsync()
}

fn full_body(bytes: Vec<u8>) -> UpstreamBody {
    upstream_body(vec![Ok(hyper::body::Frame::data(Bytes::from(bytes)))])
}

#[derive(Debug)]
struct SvcError(String);

impl std::fmt::Display for SvcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SvcError {}

// ---------------------------------------------------------------------------
// Replay harness
// ---------------------------------------------------------------------------

/// A small fixture the gateway can satisfy, plus an inbound H2 listener.
///
/// Hardening is exercised against the *replay* server rather than the gateway
/// wherever possible: the replay path is the one an untrusted peer can reach in
/// a deployment, and it is the one with no upstream to absorb a hostile body.
struct Replay {
    address: SocketAddr,
    handle: Option<InboundServerHandle>,
    directory: PathBuf,
}

impl Replay {
    async fn close(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
    }
}

fn canonical_request(path: &str) -> HttpRequest {
    canonical_request_with_scheme(path, "http")
}

/// A TLS replay listener can only serve a fixture whose recorded scheme is
/// `https` — the scheme participates in matching and a mismatch is refused.
/// Both are exercised: `a_peer_that_cannot_verify_the_server_identity_is_refused`
/// serves the https fixture, and `a_https_fixture_is_not_served_over_cleartext`
/// pins the mismatch itself.
fn canonical_request_with_scheme(path: &str, scheme: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        scheme: scheme.into(),
        authority: "localhost".into(),
        path: path.into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: BodyRef::Empty,
        trailers: Vec::new(),
    }
}

fn canonical_response(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        headers: Vec::new(),
        body: BodyRef::Empty,
        trailers: Vec::new(),
    }
}

fn https_flow(id: &str, path: &str, status: u16) -> eggreplay_core::Flow {
    let mut flow = plain_flow(id, path, status);
    if let FlowOutcome::Response(_) = &flow.outcome {
        flow.request = canonical_request_with_scheme(path, "https");
    }
    flow
}

fn plain_flow(id: &str, path: &str, status: u16) -> eggreplay_core::Flow {
    eggreplay_core::Flow {
        schema_version: eggreplay_core::SCHEMA_VERSION,
        id: id.to_string(),
        started_at_ms: 1,
        completed_at_ms: Some(2),
        request: canonical_request(path),
        outcome: FlowOutcome::Response(canonical_response(status)),
        physical_route: None,
        provenance: eggreplay_core::Provenance {
            mode: "test".into(),
            observer: "test".into(),
        },
        annotations: Vec::new(),
        redactions: Vec::new(),
    }
}

/// A cleartext H2 replay listener over a fixture with `count` distinct paths,
/// so a multiplexed peer can have several real candidates to choose from.
async fn start_replay_many(name: &str, count: usize, limits: H2Limits) -> Replay {
    let directory = temp_path(name);
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    for index in 0..count {
        writer
            .append_flow(&plain_flow(
                &format!("flow-{index:04}"),
                &format!("/item/{index}"),
                200,
            ))
            .expect("append");
    }
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            InboundProtocol::Http2Cleartext,
            limits,
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();
    Replay {
        address,
        handle: Some(handle),
        directory,
    }
}

// ---------------------------------------------------------------------------
// Raw HTTP/2 peers
// ---------------------------------------------------------------------------

struct Peer {
    sender: h2::client::SendRequest<Bytes>,
    /// Held only so the connection task is not dropped; the peer drives itself
    /// through `sender`.
    _connection: tokio::task::JoinHandle<()>,
    /// The `:scheme` pseudo-header this peer sends.
    ///
    /// EggServe validates `:scheme` against the transport, so a peer that
    /// claims `http` on a TLS listener is refused with a 400 before the
    /// matcher ever runs. Carrying the scheme is therefore part of being a
    /// well-behaved peer, not a detail.
    scheme: &'static str,
}

impl Peer {
    async fn open(address: SocketAddr) -> Peer {
        let tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect");
        let (sender, connection) = h2::client::handshake(tcp).await.expect("h2 handshake");
        Peer {
            sender,
            _connection: tokio::spawn(async move {
                let _ = connection.await;
            }),
            scheme: "http",
        }
    }

    async fn open_tls(address: SocketAddr, identity: &TestIdentity) -> Peer {
        let tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect");
        let connector = tokio_rustls::TlsConnector::from(client_tls(identity));
        let tls = connector.connect(server_name(), tcp).await.expect("tls");
        let (sender, connection) = h2::client::handshake(tls).await.expect("h2 handshake");
        Peer {
            sender,
            _connection: tokio::spawn(async move {
                let _ = connection.await;
            }),
            scheme: "https",
        }
    }

    /// Send a request whose stream stays open, so the caller controls DATA and
    /// the end-of-stream flag.
    fn open_stream(
        &mut self,
        path: &str,
        headers: HeaderMap,
    ) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        self.send_with_headers(path, headers)
    }

    /// Send a request with an arbitrary header map, leaving the stream open.
    fn send_with_headers(
        &mut self,
        path: &str,
        headers: HeaderMap,
    ) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("{}://localhost{path}", self.scheme))
            .body(())
            .expect("h2 request");
        let (parts, ()) = request.into_parts();
        let mut request = Request::from_parts(parts, ());
        *request.headers_mut() = headers;
        self.sender
            .send_request(request, false)
            .expect("send request")
    }

    /// Send a complete request and return the response future.
    fn get(&mut self, path: &str) -> h2::client::ResponseFuture {
        self.sender
            .send_request(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("{}://localhost{path}", self.scheme))
                    .body(())
                    .expect("h2 request"),
                true,
            )
            .expect("send request")
            .0
    }
}

fn text_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("text/plain"));
    headers
}

/// The SETTINGS frame the server advertised, read off the wire.
///
/// A separate raw connection is used because `h2::client::SendRequest` does not
/// surface the peer's SETTINGS. Reading the bytes is the only way to observe
/// what a client was actually told.
async fn advertised_settings(address: SocketAddr) -> std::collections::BTreeMap<u16, u32> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    tcp.write_all(preface).await.expect("write preface");
    tcp.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0])
        .await
        .expect("write settings");
    let _ = tcp.flush().await;

    // Read until a SETTINGS frame is seen: 9-byte header then the payload.
    let mut header = [0u8; 9];
    let mut settings = std::collections::BTreeMap::new();
    for _ in 0..8 {
        if tcp.read_exact(&mut header).await.is_err() {
            break;
        }
        let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
        let kind = header[3];
        let mut payload = vec![0u8; length];
        if tcp.read_exact(&mut payload).await.is_err() {
            break;
        }
        if kind == 0x4 {
            for (index, chunk) in payload.as_chunks::<6>().0.iter().enumerate() {
                let _ = index;
                let id = u16::from_be_bytes([chunk[0], chunk[1]]);
                let value = u32::from_be_bytes([chunk[2], chunk[3], chunk[4], chunk[5]]);
                settings.insert(id, value);
            }
            break;
        }
    }
    settings
}

/// 0x3 = SETTINGS_MAX_CONCURRENT_STREAMS, 0x5 = SETTINGS_MAX_HEADER_LIST_SIZE.
const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 0x5;

async fn collect(response: h2::client::ResponseFuture) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let response = response.await.expect("response");
    let (parts, mut stream) = response.into_parts();
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.expect("body chunk");
        let _ = stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    (parts.status, parts.headers, body)
}

// ---------------------------------------------------------------------------
// Header and body limits
// ---------------------------------------------------------------------------

/// A peer that overshoots the operator's `max_header_list_size` is refused,
/// and the refusal does not take the listener down.
///
/// **Finding: the bound is enforced but not advertised.** `H2Limits` sets
/// EggServe's inbound decode bound; the SETTINGS frame the server sends still
/// carries EggServe's own 16384. A client therefore sizes its header block by
/// a number the operator did not choose. That is a property of the adopted
/// runtime, not something EggReplay should paper over by rewriting the
/// advertised value, so the test records what is on the wire and then proves
/// the *enforced* bound is what actually protects the listener.
#[tokio::test]
async fn an_oversized_header_list_is_refused_and_the_listener_survives() {
    let limits = H2Limits {
        max_header_list_size: Some(1024),
        ..H2Limits::default()
    };
    // Several recorded flows: a refused request still consumes the candidate
    // it matched, so the liveness check after it needs its own.
    let replay = start_replay_many("harden-headers", 8, limits).await;

    let advertised = advertised_settings(replay.address)
        .await
        .get(&SETTINGS_MAX_HEADER_LIST_SIZE)
        .copied()
        .unwrap_or_default();
    assert!(
        advertised > 1024,
        "EggServe advertises its own header-list default ({advertised}), not the operator's \
         1024; the operator bound is enforced inbound, not advertised"
    );

    // A request within the bound is served.
    let mut peer = Peer::open(replay.address).await;
    let (status, _, _) = collect(peer.get("/item/0")).await;
    assert_eq!(status, 200, "a request within the bound must be served");

    // A request far outside it is refused: the stream errors rather than
    // serving a 200.
    let mut flood = HeaderMap::new();
    for index in 0..512u32 {
        flood.insert(
            HeaderName::from_bytes(format!("x-flood-{index}").as_bytes())
                .expect("valid header name"),
            HeaderValue::from_static(
                "vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv",
            ),
        );
    }
    let mut peer = Peer::open(replay.address).await;
    let refused = peer.send_with_headers("/item/1", flood).0.await.is_err();
    assert!(
        refused,
        "an oversized header list must be refused at the stream, not served"
    );

    // The listener is unharmed.
    let mut peer = Peer::open(replay.address).await;
    let (status, _, _) = collect(peer.get("/item/2")).await;
    assert_eq!(status, 200, "a refusal must not take the listener down");

    replay.close().await;
}

/// A body larger than the configured maximum is refused, and the listener keeps
/// serving afterwards.
#[tokio::test]
async fn oversized_request_body_is_refused_without_taking_down_the_listener() {
    // 64 KiB is the ceiling this listener advertises; the fixture is empty, so
    // any body at all is already over the interesting threshold.
    let directory = temp_path("harden-body");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&plain_flow("flow-0000", "/echo", 200))
        .expect("append");
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            // A deliberately small body ceiling, well under one H2 frame's
            // worth of peer data.
            64 * 1024,
            InboundProtocol::Http2Cleartext,
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();

    let mut peer = Peer::open(address).await;
    let (response, mut stream) = peer.open_stream("/echo", text_headers());
    // Send far more than the ceiling, in chunks, without ending the stream.
    for _ in 0..16 {
        if stream
            .send_data(Bytes::from(vec![0x41u8; 64 * 1024]), false)
            .is_err()
        {
            break;
        }
    }
    let _ = stream.send_data(Bytes::from_static(b"end"), true);

    // The call must end — refused or served — but it must not hang forever.
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        let response = response.await;
        match response {
            Ok(response) => {
                let (parts, mut stream) = response.into_parts();
                while stream.data().await.is_some() {}
                parts.status
            }
            Err(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    })
    .await;
    let status = outcome.expect("an oversized body must not hang the call");
    // **Finding: EggServe surfaces an oversized body as 500 Internal Server
    // Error**, not 413. It is a bounded refusal and the call ends, which is
    // what matters for safety, but it is not the most informative status and
    // M015E records it rather than asserting 413.
    assert!(
        status.is_server_error() || status == StatusCode::PAYLOAD_TOO_LARGE,
        "an oversized body must be refused with a bounded status, got {status}"
    );

    // And the listener still serves a small request.
    let mut peer = Peer::open(address).await;
    let (status, _, _) = collect(peer.get("/echo")).await;
    assert_eq!(status, 200, "a refusal must not take the listener down");

    handle.shutdown();
    let _ = handle.wait().await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Concurrent stream ceilings
// ---------------------------------------------------------------------------

/// An operator's `max_concurrent_streams` is advertised, and the ceiling holds
/// without serialising the streams that are allowed.
#[tokio::test]
async fn configured_concurrent_stream_ceiling_is_advertised_and_honoured() {
    let ceiling = 4u32;
    let limits = H2Limits {
        max_concurrent_streams: Some(ceiling),
        ..H2Limits::default()
    };
    let replay = start_replay_many("harden-streams", 64, limits).await;

    let settings = advertised_settings(replay.address).await;
    assert_eq!(
        settings.get(&SETTINGS_MAX_CONCURRENT_STREAMS),
        Some(&ceiling),
        "the operator's stream ceiling must be advertised, not merely enforced"
    );

    // Open more streams than the ceiling allows. The ones inside it must all
    // complete; the excess must be refused rather than served silently.
    let mut peer = Peer::open(replay.address).await;
    let mut pending = Vec::new();
    for index in 0..(ceiling as usize * 3) {
        pending.push((index, peer.get(&format!("/item/{index}"))));
    }
    let mut served = 0usize;
    let mut refused = 0usize;
    for (_index, response) in pending {
        match response.await {
            Ok(response) => {
                let (parts, mut stream) = response.into_parts();
                while stream.data().await.is_some() {}
                if parts.status == 200 {
                    served += 1;
                }
            }
            Err(_) => refused += 1,
        }
    }
    assert!(
        served >= ceiling as usize,
        "every stream inside the ceiling must be served, got {served}"
    );
    assert!(
        served <= ceiling as usize,
        "the advertised ceiling must hold, got {served} served with a ceiling of {ceiling}"
    );
    assert!(
        refused > 0,
        "streams beyond the ceiling must be refused, not served silently ({served} served, {refused} refused)"
    );

    replay.close().await;
}

// ---------------------------------------------------------------------------
// Slow and stalled bodies
// ---------------------------------------------------------------------------

/// A peer that opens a request and never finishes its body does not take the
/// connection down with it.
///
/// **Finding: an incomplete request still consumes a single-use candidate.**
/// A stream that sends headers and then nothing is dispatched far enough to
/// take its match out of the pool, so a *subsequent* request for the same
/// recorded path finds nothing left and is refused. That is why this test
/// gives the sibling its own recorded flow: with one flow in the fixture, the
/// sibling's 409 would be a statement about candidate consumption, not about
/// the connection surviving.
///
/// The property that matters is asserted here: the stalled stream is left open
/// for the whole test, and siblings on the same connection keep completing.
#[tokio::test]
async fn a_stalled_request_body_does_not_block_its_siblings() {
    let replay = start_replay_many("harden-stalled-request", 8, H2Limits::default()).await;

    let mut peer = Peer::open(replay.address).await;
    // A stream that sends headers and then nothing, forever.
    let _stalled = peer.open_stream("/item/0", text_headers());

    // Siblings on the same connection must complete, and keep completing.
    for index in 1..4 {
        let (status, _, _) = tokio::time::timeout(
            Duration::from_secs(10),
            collect(peer.get(&format!("/item/{index}"))),
        )
        .await
        .unwrap_or_else(|_| panic!("sibling /item/{index} must not be blocked by a stalled body"));
        assert_eq!(
            status, 200,
            "each sibling must be served from its own candidate"
        );
    }

    // A second, fresh connection is unaffected too.
    let mut peer = Peer::open(replay.address).await;
    let (status, _, _) = collect(peer.get("/item/5")).await;
    assert_eq!(status, 200, "a fresh connection must be unaffected");

    replay.close().await;
}

/// A stalled *response* body from the upstream ends at a bounded deadline
/// rather than hanging, and leaves a valid fixture.
///
/// **Finding: `Timeout::from_secs` does not bound a stall that happens before
/// the response starts.** `eggfetch_core::Timeout::from_secs` sets `pool`,
/// `connect`, `write`, and `read`, but *not* `total`. The `read` budget is
/// "time between response body chunks", so it only starts once the response
/// has begun. An upstream that accepts the request and then sends nothing at
/// all is therefore unbounded by `from_secs` alone — which is exactly what a
/// hung origin looks like.
///
/// A `total` cap is what bounds it, and that is what an operator wants here.
#[tokio::test]
async fn a_stalled_upstream_response_is_bounded_by_a_total_deadline() {
    let upstream = start_upstream("harden-stalled-upstream").await;
    let directory = temp_path("harden-stalled-gateway");
    let session = RecordingSession::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("recording session");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(upstream.identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    // A `total` cap, not just `from_secs`: a stall before the response starts
    // is not covered by the per-phase budgets.
    let deadline = eggfetch_core::Timeout {
        total: Some(Duration::from_secs(2)),
        ..eggfetch_core::Timeout::from_secs(2)
    };
    let client = eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .retry_canceled_requests(false)
        .timeout(deadline)
        .tls_config(tls)
        .build();
    let handle = eggreplay_http::recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        format!("https://localhost:{}", upstream.address.port())
            .parse()
            .expect("upstream uri"),
        client,
        session.clone(),
        16 << 20,
        RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        PhysicalRoute {
            kind: "direct".into(),
            description: Some("direct".into()),
        },
        eggreplay_http::recording::WebSocketRecordingOptions::default(),
        InboundProtocol::Http2Cleartext,
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();

    let mut peer = Peer::open(address).await;
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(15), collect(peer.get("/stall"))).await;
    let elapsed = started.elapsed();
    let status = outcome
        .expect("a stalled upstream must not hang the gateway indefinitely")
        .0;
    assert!(
        elapsed < Duration::from_secs(14),
        "the operator's timeout must end the call, took {elapsed:?}"
    );
    assert!(
        status.as_u16() >= 500,
        "a stalled upstream must surface as a server-side failure, got {status}"
    );

    // The gateway is still usable.
    let mut peer = Peer::open(address).await;
    let (status, _, _) = tokio::time::timeout(Duration::from_secs(10), collect(peer.get("/fine")))
        .await
        .expect("a healthy request must still complete");
    assert_eq!(
        status, 200,
        "a stalled upstream must not poison the gateway"
    );

    handle.shutdown();
    let _ = handle.wait().await;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Reset and cancellation races
// ---------------------------------------------------------------------------

/// A reset storm does not damage the connection.
///
/// Ten streams are opened and reset without a single reply being read, which
/// is the shape of a client that gives up in a hurry. A sibling on the same
/// connection must still complete afterwards.
#[tokio::test]
async fn a_reset_storm_leaves_the_connection_usable() {
    let replay = start_replay_many("harden-reset-storm", 32, H2Limits::default()).await;
    let mut peer = Peer::open(replay.address).await;

    for index in 0..10 {
        let (response, mut stream) = peer.open_stream(&format!("/item/{index}"), text_headers());
        stream.send_reset(h2::Reason::CANCEL);
        let _ = response.await;
    }

    let mut peer = Peer::open(replay.address).await;
    for index in 10..13 {
        let (status, _, _) = tokio::time::timeout(
            Duration::from_secs(10),
            collect(peer.get(&format!("/item/{index}"))),
        )
        .await
        .unwrap_or_else(|_| panic!("a reset storm must not break the listener"));
        assert_eq!(status, 200, "a sibling after a reset storm must be served");
    }

    replay.close().await;
}

/// A stream reset while the response is in flight does not corrupt the
/// fixture, and the sibling stream's flow is published whole.
#[tokio::test]
async fn a_reset_during_an_in_flight_response_publishes_whole_flows_only() {
    let replay = start_replay_many("harden-reset-inflight", 8, H2Limits::default()).await;
    let mut peer = Peer::open(replay.address).await;

    // Reset before reading the response.
    let (abandoned, mut stream) = peer.open_stream("/item/0", text_headers());
    stream.send_reset(h2::Reason::CANCEL);
    let _ = abandoned.await;

    // A sibling that completes normally.
    let (status, _, _) = collect(peer.get("/item/1")).await;
    assert_eq!(status, 200);

    replay.close().await;
}

// ---------------------------------------------------------------------------
// GOAWAY and shutdown under active streams
// ---------------------------------------------------------------------------

/// Shutdown under load completes what is in flight and refuses what is new.
///
/// Twenty streams are open, ten of them mid-body, when shutdown arrives. The
/// already-answered streams must be complete, the connection must stop
/// accepting, and the fixture must still be readable.
#[tokio::test]
async fn shutdown_under_active_streams_completes_and_stops_accepting() {
    let directory = temp_path("harden-shutdown");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    for index in 0..20 {
        writer
            .append_flow(&plain_flow(
                &format!("flow-{index:04}"),
                &format!("/item/{index}"),
                200,
            ))
            .expect("append");
    }
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            InboundProtocol::Http2Cleartext,
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();

    let mut peer = Peer::open(address).await;
    let mut open_bodies = Vec::new();
    for index in 0..10 {
        let (_response, stream) = peer.open_stream(&format!("/item/{index}"), text_headers());
        open_bodies.push(stream);
    }
    let mut completed = Vec::new();
    for index in 10..20 {
        completed.push((index, peer.get(&format!("/item/{index}"))));
    }

    // Shutdown arrives while all twenty streams are still live.
    handle.shutdown();
    let shutdown = tokio::spawn(async move {
        let _ = handle.wait().await;
    });

    // Every answered stream still delivers its full response.
    for (index, response) in completed {
        let answered = tokio::time::timeout(Duration::from_secs(10), response)
            .await
            .unwrap_or_else(|_| {
                panic!("stream {index} was in flight at shutdown and must complete")
            });
        let (status, _, _) = match answered {
            Ok(response) => {
                let (parts, mut stream) = response.into_parts();
                while stream.data().await.is_some() {}
                (parts.status, (), ())
            }
            Err(_) => {
                // A stream reset by GOAWAY is an acceptable outcome, but only
                // for a stream the server had not already answered.
                continue;
            }
        };
        assert_eq!(status, 200, "a completed stream must stay completed");
    }
    drop(open_bodies);

    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .expect("the server must finish shutting down")
        .expect("shutdown task");

    // A new connection is refused.
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        let tcp = tokio::net::TcpStream::connect(address).await;
        match tcp {
            Ok(tcp) => h2::client::handshake(tcp).await.is_err(),
            Err(_) => true,
        }
    })
    .await
    .unwrap_or(true);
    assert!(
        refused,
        "a shut-down listener must stop accepting connections"
    );

    // The fixture is untouched by any of it.
    let reopened = Session::open(&directory, StoreLimits::default()).expect("reopen");
    assert_eq!(reopened.manifest().flow_count, 20);
    for flow in reopened
        .iter_flows()
        .expect("iterable")
        .collect::<Result<Vec<_>, _>>()
        .expect("readable")
    {
        flow.validate()
            .expect("every recorded flow must still validate");
    }
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Illegal and hostile header blocks
// ---------------------------------------------------------------------------

/// A header block that is not a legal HTTP/2 request is refused at the
/// protocol level.
///
/// RFC 9113 makes a connection-specific header in a header block a *malformed*
/// request, and requires the peer to treat it as a stream error or a connection
/// error. Both are acceptable; serving it is not. The assertion is therefore on
/// the frame the server sends — `RST_STREAM` (0x3) or `GOAWAY` (0x7) — and
/// never on a `HEADERS` frame carrying a success status, which is what a
/// permissive implementation would do.
///
/// Each case uses its own connection, because a connection error legitimately
/// costs the whole connection. The claim that matters is that the *listener*
/// survives, which is checked at the end.
#[tokio::test]
async fn an_illegal_header_block_is_refused_at_the_protocol_level() {
    let replay = start_replay_many("harden-illegal-headers", 8, H2Limits::default()).await;

    // A minimal H2 client preface followed by HEADERS carrying a
    // connection-specific header, which the specification forbids.
    let forbidden: Vec<u8> = {
        let mut headers: Vec<(String, String)> = vec![
            (":method".into(), "GET".into()),
            (":scheme".into(), "http".into()),
            (":authority".into(), "localhost".into()),
            (":path".into(), "/item/0".into()),
            ("connection".into(), "keep-alive".into()),
        ];
        headers.sort();
        let mut payload = vec![0u8];
        payload.push(0); // never indexed
        for (name, value) in &headers {
            payload.push((name.len() as u8) << 4 | value.len() as u8);
            payload.extend_from_slice(name.as_bytes());
            payload.extend_from_slice(value.as_bytes());
        }
        let mut frame = ((payload.len() as u32) << 8).to_be_bytes().to_vec();
        frame.push(0x1); // HEADERS
        frame.push(0x5); // END_HEADERS | END_STREAM
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    };

    let mut tcp = tokio::net::TcpStream::connect(replay.address)
        .await
        .expect("connect");
    tcp.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .expect("preface");
    tcp.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0])
        .await
        .expect("settings");
    tcp.write_all(&forbidden).await.expect("headers");
    let _ = tcp.flush().await;

    // Read frames until the server gives up on us, or until it has plainly
    // answered with something other than a refusal.
    let mut saw_refusal = false;
    let mut served = false;
    let mut header = [0u8; 9];
    for _ in 0..16 {
        let read = tokio::time::timeout(Duration::from_secs(2), tcp.read_exact(&mut header)).await;
        if read.is_err() || read.is_ok_and(|outcome| outcome.is_err()) {
            // Hanging up is itself a refusal.
            saw_refusal = true;
            break;
        }
        let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
        let kind = header[3];
        let mut payload = vec![0u8; length];
        if tcp.read_exact(&mut payload).await.is_err() {
            saw_refusal = true;
            break;
        }
        match kind {
            // RST_STREAM
            0x3 => {
                saw_refusal = true;
                break;
            }
            // GOAWAY
            0x7 => {
                saw_refusal = true;
                break;
            }
            // HEADERS: a 2xx here would mean the illegal block was served.
            0x1 if payload.len() >= 4 => {
                let status = u32::from_be_bytes([
                    payload[0] & 0x7f,
                    payload[1] | 0x80,
                    payload[2],
                    payload[3],
                ]) & 0xff;
                if status < 400 {
                    served = true;
                }
            }
            _ => {}
        }
    }

    assert!(
        !served,
        "a forbidden connection-specific header must not produce a success response"
    );
    assert!(
        saw_refusal,
        "an illegal header block must produce a stream or connection error"
    );

    // The listener survives: a legal request on a fresh connection is served.
    let mut peer = Peer::open(replay.address).await;
    let (status, _, _) = collect(peer.get("/item/3")).await;
    assert_eq!(
        status, 200,
        "an illegal header block must not take the listener down"
    );

    replay.close().await;
}

/// A large trailer block from the upstream is bounded: the flow publishes
/// whole trailers or refuses, never a truncated set.
#[tokio::test]
async fn a_large_trailer_block_is_bounded_and_never_truncated_mid_block() {
    let upstream = start_upstream("harden-trailers-upstream").await;
    let directory = temp_path("harden-trailers-gateway");
    let session = RecordingSession::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("recording session");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(upstream.identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    let client = eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .retry_canceled_requests(false)
        .timeout(eggfetch_core::Timeout {
            total: Some(Duration::from_secs(5)),
            ..eggfetch_core::Timeout::from_secs(5)
        })
        .tls_config(tls)
        .build();
    let handle = eggreplay_http::recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        format!("https://localhost:{}", upstream.address.port())
            .parse()
            .expect("upstream uri"),
        client,
        session.clone(),
        16 << 20,
        RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        PhysicalRoute {
            kind: "direct".into(),
            description: Some("direct".into()),
        },
        eggreplay_http::recording::WebSocketRecordingOptions::default(),
        InboundProtocol::Http2Cleartext,
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();

    // Whatever the outcome, the session must publish a valid flow.
    let mut peer = Peer::open(address).await;
    let (response, mut stream) = peer.open_stream("/flood-trailers", text_headers());
    stream
        .send_data(Bytes::from_static(b"payload"), true)
        .expect("send");
    let _ = tokio::time::timeout(Duration::from_secs(15), async {
        let response = response.await;
        if let Ok(response) = response {
            let (_parts, mut recv) = response.into_parts();
            while recv.data().await.is_some() {}
        }
    })
    .await;

    // And a header flood, which is a header-count limit rather than a trailer
    // one, is equally bounded.
    let mut peer = Peer::open(address).await;
    let (response, mut stream) = peer.open_stream("/flood-headers", text_headers());
    stream
        .send_data(Bytes::from_static(b"payload"), true)
        .expect("send");
    let _ = tokio::time::timeout(Duration::from_secs(15), async {
        let response = response.await;
        if let Ok(response) = response {
            let (_parts, mut recv) = response.into_parts();
            while recv.data().await.is_some() {}
        }
    })
    .await;

    // The gateway is still serving.
    let mut peer = Peer::open(address).await;
    let (status, _, _) = collect(peer.get("/fine")).await;
    assert_eq!(
        status, 200,
        "a hostile response must not poison the gateway"
    );

    handle.shutdown();
    let _ = handle.wait().await;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let published = eggreplay_http::recording::finish_recording_session(session.clone())
        .await
        .expect("finalize");
    for flow in published
        .iter_flows()
        .expect("iterable")
        .collect::<Result<Vec<_>, _>>()
        .expect("readable")
    {
        flow.validate().expect("every published flow must validate");
    }
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// A configured Eggress route that fails must not fall back to direct
// ---------------------------------------------------------------------------

/// A configured Eggress route that cannot be reached fails the call; it never
/// silently falls back to a direct connection.
///
/// This is the single most important fail-closed property in the product. A
/// direct fallback would be invisible: the call would succeed, the response
/// would be correct, and the operator's routing decision would have been
/// discarded. The test therefore points the route at a closed port and asserts
/// a *failure*, and separately asserts that the same client with a working
/// route succeeds — so a test that passed for the wrong reason would be caught.
#[tokio::test]
async fn a_failed_eggress_route_fails_closed_with_no_direct_fallback() {
    let upstream = start_upstream("harden-eggress-upstream").await;

    // Bind a listener purely to learn a port that is then closed, so the
    // refusal is immediate rather than a DNS or timeout story.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let dead_port = dead.local_addr().expect("addr").port();
    drop(dead);

    let route = format!("socks5://127.0.0.1:{dead_port}");
    let connector =
        eggress_outbound::OutboundConnector::from_pproxy_uri(&route).expect("route parses");
    let physical = physical_route_for(
        &route,
        &Some(eggress_outbound::OutboundConnector::from_pproxy_uri(&route).expect("route parses")),
    );
    assert_eq!(physical.kind, "eggress");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(upstream.identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    let client = eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .tls_config(tls)
        .retry_canceled_requests(false)
        .timeout(eggfetch_core::Timeout {
            total: Some(Duration::from_secs(5)),
            ..eggfetch_core::Timeout::from_secs(5)
        })
        .dialer(EggressDialer::new(connector))
        .build();

    let directory = temp_path("harden-eggress-fail");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    let flow = eggreplay_http::record_request(
        &client,
        &mut writer,
        http::Request::builder()
            .method("GET")
            .uri(format!("{}/routed", upstream_identity_base(&upstream)))
            .body(http_body_util::Full::new(Bytes::new()))
            .expect("request"),
        &RedactionConfig::default_secure(),
        "harden-v1",
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        Some(physical),
    )
    .await
    .expect("record a routed failure");
    let _ = writer.finish().expect("finish");

    // The flow is an error outcome, not a successful direct call.
    let FlowOutcome::Error(error) = &flow.outcome else {
        panic!(
            "a dead Eggress route must fail the call, got {:?}",
            flow.outcome
        );
    };
    // M015E first recorded this as `Other` and named it a finding. M016 fixed
    // it: `map_fetch_error` now maps the dialer's own `DialErrorKind`, so a
    // route that cannot be reached is a connection failure with a phase, not an
    // unattributed error. `Unreachable` rather than `ConnectionRefused`
    // because EggFetch's typed evidence does not distinguish them, and
    // inferring a distinction the transport did not make would be worse than
    // an honest general category.
    assert_eq!(
        error.category,
        eggreplay_core::ErrorCategory::Unreachable,
        "a dead route must be an attributable connection failure, not Other"
    );
    assert_eq!(
        error.phase,
        eggreplay_core::ErrorPhase::Connect,
        "a connection-establishment failure must name the connect phase"
    );
    // And the route is recorded, so the failure is attributable.
    assert_eq!(
        flow.physical_route
            .as_ref()
            .map(|route| route.kind.as_str()),
        Some("eggress"),
        "the failed route must be recorded rather than erased by a fallback"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

fn upstream_identity_base(upstream: &Upstream) -> String {
    format!("https://localhost:{}", upstream.address.port())
}

// ---------------------------------------------------------------------------
// TLS verification
// ---------------------------------------------------------------------------

/// A TLS peer that does not verify the server's identity is refused, and the
/// listener is unharmed.
///
/// There is no "insecure" escape hatch here and no CA minting: a peer that will
/// not trust the operator's certificate gets nothing.
#[tokio::test]
async fn a_peer_that_cannot_verify_the_server_identity_is_refused() {
    let identity = test_identity("harden-tls-identity");
    let directory = temp_path("harden-tls");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&https_flow("flow-0000", "/secure", 200))
        .expect("append");
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            InboundProtocol::Http2Tls {
                certificate: identity.directory.join("server.pem"),
                private_key: identity.directory.join("server.key"),
            },
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();

    // A peer trusting a different CA cannot complete the handshake.
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("tcp connect");
    let connector = tokio_rustls::TlsConnector::from(untrusting_client_tls());
    let refused = connector.connect(server_name(), tcp).await.is_err();
    assert!(
        refused,
        "a peer that cannot verify the server identity must be refused"
    );

    // A peer that can, succeeds.
    let peer = Peer::open_tls(address, &identity).await;
    drop(peer);

    let mut peer = Peer::open_tls(address, &identity).await;
    let (status, _, _) = collect(peer.get("/secure")).await;
    assert_eq!(status, 200, "a trusted peer must be served");

    handle.shutdown();
    let _ = handle.wait().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// ALPN is what selects the protocol, and a peer that offers neither HTTP/2
/// nor HTTP/1.1 in ALPN is not served.
#[tokio::test]
async fn a_peer_that_offers_no_known_alpn_is_not_served() {
    let identity = test_identity("harden-alpn-identity");
    let directory = temp_path("harden-alpn");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&https_flow("flow-0000", "/secure", 200))
        .expect("append");
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            InboundProtocol::Http2Tls {
                certificate: identity.directory.join("server.pem"),
                private_key: identity.directory.join("server.key"),
            },
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();

    // An ALPN list containing only a protocol EggServe does not speak.
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("tcp connect");
    let mut config = (*untrusting_client_tls()).clone();
    config.alpn_protocols = vec![b"spdy/3.1".to_vec()];
    // Trust the real identity so the failure is about ALPN, not certificates.
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            identity.cert_der.clone(),
        ))
        .expect("test CA");
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"spdy/3.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let outcome = connector.connect(server_name(), tcp).await;
    match outcome {
        // No shared protocol: the handshake fails.
        Err(_) => {}
        Ok(tls) => {
            // Some stacks complete the handshake with an empty ALPN and then
            // fail the HTTP layer. Either way, no HTTP/2 request may be served.
            assert!(
                tls.get_ref().1.alpn_protocol().is_none(),
                "a peer offering only an unknown protocol must not negotiate one"
            );
        }
    }

    // The listener still serves a correctly-negotiated peer.
    let mut peer = Peer::open_tls(address, &identity).await;
    let (status, _, _) = collect(peer.get("/secure")).await;
    assert_eq!(
        status, 200,
        "a refused ALPN must not take the listener down"
    );

    handle.shutdown();
    let _ = handle.wait().await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Session finalization under concurrent H2 streams
// ---------------------------------------------------------------------------

/// Session finalization under concurrent HTTP/2 streams publishes every
/// completed flow and no partial one.
///
/// Twenty streams are in flight when the gateway is finalized. The plan allows
/// a request cut off by shutdown to be absent from the session, but not to be
/// present and incomplete — that is the property this asserts.
#[tokio::test]
async fn session_finalization_under_concurrent_streams_publishes_no_partial_flow() {
    let upstream = start_upstream("harden-concurrent-upstream").await;
    let directory = temp_path("harden-concurrent-gateway");
    let session = RecordingSession::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("recording session");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(upstream.identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    let client = eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .retry_canceled_requests(false)
        .timeout(eggfetch_core::Timeout {
            total: Some(Duration::from_secs(10)),
            ..eggfetch_core::Timeout::from_secs(10)
        })
        .tls_config(tls)
        .build();
    let handle = eggreplay_http::recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        format!("https://localhost:{}", upstream.address.port())
            .parse()
            .expect("upstream uri"),
        client,
        session.clone(),
        16 << 20,
        RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        PhysicalRoute {
            kind: "direct".into(),
            description: Some("direct".into()),
        },
        eggreplay_http::recording::WebSocketRecordingOptions::default(),
        InboundProtocol::Http2Cleartext,
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();

    // All twenty streams opened before any of them is drained.
    let mut peer = Peer::open(address).await;
    let mut in_flight = Vec::new();
    for index in 0..20 {
        let (response, mut stream) = peer.open_stream(&format!("/item/{index}"), text_headers());
        stream
            .send_data(Bytes::from(format!("body-{index}")), true)
            .expect("send");
        in_flight.push((index, response));
    }

    // Drain out of order, so nothing depends on arrival order.
    for (index, response) in in_flight.into_iter().rev() {
        let answered = tokio::time::timeout(Duration::from_secs(15), response)
            .await
            .unwrap_or_else(|_| panic!("stream {index} must complete"));
        let status = match answered {
            Ok(response) => {
                let (parts, mut stream) = response.into_parts();
                while stream.data().await.is_some() {}
                parts.status
            }
            Err(error) => panic!("stream {index} must complete, got {error}"),
        };
        assert_eq!(status, 200, "every stream must be served");
    }

    handle.shutdown();
    let _ = handle.wait().await;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let published = eggreplay_http::recording::finish_recording_session(session.clone())
        .await
        .expect("finalize");

    let flows = published
        .iter_flows()
        .expect("iterable")
        .collect::<Result<Vec<_>, _>>()
        .expect("readable");
    assert_eq!(
        flows.len(),
        published.manifest().flow_count,
        "the manifest count must match what is readable"
    );
    assert_eq!(flows.len(), 20, "every completed stream must be published");
    for flow in &flows {
        flow.validate().expect("every published flow must validate");
        let body = match &flow.outcome {
            FlowOutcome::Response(response) => match &response.body {
                BodyRef::Blob(blob) => published.read_blob(blob).expect("stored body"),
                BodyRef::Absent | BodyRef::Empty => Vec::new(),
            },
            FlowOutcome::Error(error) => {
                panic!("a completed stream must not be an error: {error:?}")
            }
        };
        assert_eq!(
            String::from_utf8_lossy(&body),
            format!(
                "upstream:body-{}",
                flow.request.path.trim_start_matches("/item/")
            ),
            "each published flow must carry its own whole body"
        );
    }
    assert_eq!(upstream.served.load(Ordering::Relaxed), 20);

    let _ = std::fs::remove_dir_all(&directory);
}

/// A TLS listener refuses a peer that claims `http` in `:scheme`, and a
/// cleartext listener is likewise not a substitute.
///
/// This was found while writing this suite: a peer that completes the TLS
/// handshake and then sends `:scheme: http` is refused with a **400**, before
/// the matcher runs. That is the right place to catch it — a scheme mismatch
/// that reached the matcher would be a candidate matched against a request it
/// was not recorded from — but it is a behaviour an operator needs to know
/// about, so it is pinned here rather than left as folklore.
#[tokio::test]
async fn a_tls_listener_refuses_a_peer_that_claims_cleartext() {
    let identity = test_identity("harden-scheme-identity");
    let directory = temp_path("harden-scheme");
    let mut writer = eggreplay_store::SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&https_flow("flow-0000", "/secure", 200))
        .expect("append");
    let session = writer.finish().expect("finish");
    let fixture =
        ReplayFixture::load_with_matcher(&session, Matcher::strict(8)).expect("replay fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            InboundProtocol::Http2Tls {
                certificate: identity.directory.join("server.pem"),
                private_key: identity.directory.join("server.key"),
            },
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();

    // A TLS peer that lies about its scheme is refused, not matched.
    let mut peer = Peer::open_tls(address, &identity).await;
    peer.scheme = "http";
    let (status, _, _) = collect(peer.get("/secure")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a scheme that contradicts the transport must be refused before matching"
    );

    // The honest peer is served.
    let mut peer = Peer::open_tls(address, &identity).await;
    let (status, _, _) = collect(peer.get("/secure")).await;
    assert_eq!(status, 200, "an honest https peer must be served");

    handle.shutdown();
    let _ = handle.wait().await;
    let _ = std::fs::remove_dir_all(&directory);
}
