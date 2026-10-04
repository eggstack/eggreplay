#![cfg(all(
    feature = "h2",
    feature = "h2-inbound-tls",
    feature = "eggress",
    feature = "websocket"
))]
//! M015C HTTP/2 end-to-end semantic and regression qualification.
//!
//! M015B proved an inbound HTTP/2 socket can serve requests. This suite
//! proves the harder claim: inbound H2 composes with the *same* canonical
//! semantics, so the product has one HTTP/2 path rather than an H1 path and an
//! H2 path that happen to share a runtime.
//!
//! # How "one path" is made falsifiable
//!
//! If protocol had leaked into a matching dimension, a rendering rule, or a
//! comparison, the cross-protocol rows would fail: an H1-acquired flow must
//! replay over H2, and an H2-acquired flow must replay over H1, and a flow
//! whose only difference is its `http-version` annotation must compare equal.
//! Those rows are the load-bearing evidence here; the rest of the matrix shows
//! each protocol works on its own terms.
//!
//! # Peers
//!
//! Three independent client families — the raw `h2` crate, Hyper's H1 and H2
//! connection stacks, and `EggFetch` on the product's own outbound path — so
//! a shared implementation bug cannot establish support by self-consistency.
//!
//! Local loopback only. The upstream and the SOCKS5 fixture are test-owned
//! with test-owned `rcgen` identity.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, Flow, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse,
    Matcher, Provenance, QueryPair, ReportScheduler, SCHEMA_VERSION, SessionMetadata,
    compare_flows,
};
use eggreplay_http::inbound::{H2Limits, InboundProtocol, InboundServerHandle};
use eggreplay_http::{
    EggressDialer, ReplayFixture, execute_candidate, physical_route_for, recording,
};
use eggreplay_store::{Session, SessionWriter, StoreLimits};
use http::{Method, Request, Response, StatusCode, Version};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;

// ---------------------------------------------------------------------------
// Unique paths and test-owned identity
// ---------------------------------------------------------------------------

/// A unique, **non-existent** path. Store constructors refuse an existing
/// destination so they cannot append to a stale fixture, and the serial keeps
/// parallel tests from fighting over one directory.
static DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_path(name: &str) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let serial = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "eggreplay-h2e2e-{name}-{}-{millis}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn temp_dir(name: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

/// A test-owned TLS identity, kept as PEM so both the certificate *and* its
/// key can be presented by the upstream and trusted by the clients.
struct TestIdentity {
    cert_der: Vec<u8>,
    cert_pem: String,
    key_der: Vec<u8>,
    key_pem: String,
    directory: PathBuf,
}

impl Drop for TestIdentity {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn test_identity(name: &str) -> TestIdentity {
    let directory = temp_dir(name);
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("test identity");
    TestIdentity {
        cert_der: certified.cert.der().to_vec(),
        cert_pem: certified.cert.pem(),
        key_der: certified.key_pair.serialize_der(),
        key_pem: certified.key_pair.serialize_pem(),
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

fn client_tls(identity: &TestIdentity, alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            identity.cert_der.clone(),
        ))
        .expect("test CA");
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|value| value.to_vec()).collect();
    Arc::new(config)
}

fn server_name() -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::try_from("localhost").expect("sni")
}

// ---------------------------------------------------------------------------
// Test-owned H2 upstream
// ---------------------------------------------------------------------------

/// A deterministic HTTP/2 upstream over local TLS with ALPN `h2`.
///
/// It echoes the request path back and stamps `x-upstream: h2`, so an
/// end-to-end row can prove *which* upstream answered rather than only that
/// something did.
struct Upstream {
    address: SocketAddr,
    identity: TestIdentity,
    task: tokio::task::JoinHandle<()>,
    served: Arc<std::sync::atomic::AtomicU64>,
}

impl Upstream {
    fn base(&self) -> String {
        format!("https://localhost:{}", self.address.port())
    }

    fn served(&self) -> u64 {
        self.served.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_upstream(name: &str) -> Upstream {
    use hyper::service::service_fn;

    let identity = test_identity(name);
    let acceptor = tokio_rustls::TlsAcceptor::from(server_tls(&identity, vec![b"h2".to_vec()]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream bind");
    let address = listener.local_addr().expect("upstream addr");
    let served = Arc::new(std::sync::atomic::AtomicU64::new(0));
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
                assert_eq!(
                    tls.get_ref().1.alpn_protocol(),
                    Some(&b"h2"[..]),
                    "an H2-only client must select h2 at the upstream"
                );
                let _ = hyper::server::conn::http2::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .serve_connection(
                    TokioIo::new(tls),
                    service_fn(move |request: Request<hyper::body::Incoming>| {
                        let counter = counter.clone();
                        async move {
                            let path = request.uri().path().to_string();
                            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let body = request
                                .into_body()
                                .collect()
                                .await
                                .map(|collected| collected.to_bytes())
                                .unwrap_or_default();
                            let payload = if body.is_empty() {
                                format!("upstream:{path}").into_bytes()
                            } else {
                                format!("upstream:{path}|{}", String::from_utf8_lossy(&body))
                                    .into_bytes()
                            };
                            // `/trailers` is the one path that ends with an
                            // HTTP/2 trailer block, so the acquisition
                            // path has something to record. Every other
                            // path stays a single DATA frame.
                            let body: http_body_util::combinators::UnsyncBoxBody<
                                Bytes,
                                std::convert::Infallible,
                            > = if path == "/trailers" {
                                let mut trailers = http::HeaderMap::new();
                                trailers.insert(
                                    "x-upstream-trailer",
                                    http::HeaderValue::from_static("done"),
                                );
                                let frames: Vec<Result<hyper::body::Frame<Bytes>, _>> = vec![
                                    Ok(hyper::body::Frame::data(Bytes::from(payload))),
                                    Ok(hyper::body::Frame::trailers(trailers)),
                                ];
                                http_body_util::StreamBody::new(futures_util::stream::iter(frames))
                                    .boxed_unsync()
                            } else {
                                http_body_util::Full::new(Bytes::from(payload)).boxed_unsync()
                            };
                            http::Response::builder()
                                .status(StatusCode::OK)
                                .header("x-upstream", "h2")
                                .body(body)
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
        task,
        served,
    }
}

// ---------------------------------------------------------------------------
// Test-owned SOCKS5 fixture for Eggress routing
// ---------------------------------------------------------------------------

struct Socks5 {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Socks5 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A minimal SOCKS5 CONNECT proxy that relays to one fixed upstream. It
/// exists to prove the *route* is traversed, not to be a proxy
/// implementation: the payload is opaque bytes in both directions, so
/// whatever protocol the route carries is the client's business.
async fn start_socks5(upstream: SocketAddr) -> Socks5 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("socks bind");
    let address = listener.local_addr().expect("socks addr");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut header = [0u8; 2];
                if socket.read_exact(&mut header).await.is_err() {
                    return;
                }
                let mut offered = vec![0u8; usize::from(header[1])];
                if socket.read_exact(&mut offered).await.is_err() {
                    return;
                }
                if socket.write_all(&[0x05, 0x00]).await.is_err() {
                    return;
                }
                let mut request = [0u8; 4];
                if socket.read_exact(&mut request).await.is_err() {
                    return;
                }
                match request[3] {
                    0x01 => {
                        let mut rest = [0u8; 6];
                        socket.read_exact(&mut rest).await.ok();
                    }
                    0x03 => {
                        let mut length = [0u8; 1];
                        if socket.read_exact(&mut length).await.is_err() {
                            return;
                        }
                        let mut rest = vec![0u8; usize::from(length[0]) + 2];
                        socket.read_exact(&mut rest).await.ok();
                    }
                    0x04 => {
                        let mut rest = [0u8; 18];
                        socket.read_exact(&mut rest).await.ok();
                    }
                    _ => return,
                }
                if socket
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await
                    .is_err()
                {
                    return;
                }
                let Ok(mut target) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut target).await;
            });
        }
    });
    Socks5 { address, task }
}

fn eggress_route(socks: &Socks5) -> String {
    format!("socks5://127.0.0.1:{}", socks.address.port())
}

// ---------------------------------------------------------------------------
// H2 / H1 peers
// ---------------------------------------------------------------------------

/// An H2 peer that carries the `:scheme` its transport actually negotiated,
/// so no call site can pair a path with the wrong scheme. Derefs to the raw
/// sender so `peer.send_request(..)` still reads naturally.
struct H2Peer {
    sender: h2::client::SendRequest<Bytes>,
    scheme: &'static str,
}

impl std::ops::Deref for H2Peer {
    type Target = h2::client::SendRequest<Bytes>;
    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

impl std::ops::DerefMut for H2Peer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sender
    }
}

impl H2Peer {
    fn send_get(&mut self, path: &str) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        self.send_get_at("localhost", path)
    }

    /// Send a GET addressed at an explicit `:authority`.
    fn send_get_at(
        &mut self,
        authority: &str,
        path: &str,
    ) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        let scheme = self.scheme;
        self.sender
            .send_request(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("{scheme}://{authority}{path}"))
                    .body(())
                    .expect("h2 request"),
                true,
            )
            .expect("send h2 request")
    }

    /// Send a request that leaves its stream open, so the caller can supply a
    /// DATA body and a trailer block.
    ///
    /// `send_get` ends the stream at the headers, which is correct for a bodyless
    /// GET; sending DATA after that is a frame-type error, not a test failure.
    fn send_open(
        &mut self,
        authority: &str,
        path: &str,
    ) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        let scheme = self.scheme;
        self.sender
            .send_request(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{scheme}://{authority}{path}"))
                    .body(())
                    .expect("h2 request"),
                false,
            )
            .expect("send h2 request")
    }
}

async fn h2_connect_tls(address: SocketAddr, identity: &TestIdentity) -> H2Peer {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let connector = tokio_rustls::TlsConnector::from(client_tls(identity, &[b"h2"]));
    let tls = connector.connect(server_name(), tcp).await.expect("tls");
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sender, connection) = h2::client::handshake(tls).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    H2Peer {
        sender,
        scheme: "https",
    }
}

async fn h2_connect_cleartext(address: SocketAddr) -> H2Peer {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let (sender, connection) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    H2Peer {
        sender,
        scheme: "http",
    }
}

async fn h2_collect(response: Response<h2::RecvStream>) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let (parts, mut stream) = response.into_parts();
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.expect("body chunk");
        let _ = stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    (parts.status, parts.headers, body)
}

async fn h1_connect(address: SocketAddr) -> hyper::client::conn::http1::SendRequest<Full<Bytes>> {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

/// Collect an H1 response into the same shape `h2_collect` returns, so a
/// parity assertion can compare the two without repeating the extraction.
async fn h1_collect(
    response: Response<hyper::body::Incoming>,
) -> (StatusCode, http::HeaderMap, Vec<u8>) {
    let (parts, body) = response.into_parts();
    let body = body.collect().await.expect("h1 body").to_bytes();
    (parts.status, parts.headers, body.to_vec())
}

/// An HTTP/1.1 request in origin form with an explicit `Host`.
///
/// EggServe's origin-only request-target policy rejects absolute-form, and
/// Hyper's connection client emits absolute-form whenever the request URI
/// carries a scheme and authority. With a path-only URI Hyper sends no `Host`
/// of its own, so the header must be set explicitly.
fn h1_request(path: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("host", "localhost")
        .body(Full::new(Bytes::new()))
        .expect("h1 request")
}

async fn read_http1_message<S>(stream: &mut S) -> Vec<u8>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut byte).await.expect("read head");
        assert!(read > 0, "connection closed before the response head");
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.expect("read body");
    raw.extend_from_slice(&body);
    raw
}

/// An operator TLS serving policy built from a test identity, plus the
/// directory holding its PEM files.
///
/// The scheme is not incidental: a flow acquired over a TLS upstream records
/// `scheme: https`, and the matcher compares scheme, so a faithful offline
/// replay of that flow must also be served over TLS. Serving it cleartext
/// would be a scheme mismatch, which
/// `replay_scheme_must_match_the_acquisition_transport` pins explicitly.
fn tls_policy_for(identity: &TestIdentity, name: &str) -> (InboundProtocol, PathBuf) {
    let directory = temp_dir(name);
    let certificate = directory.join("server.pem");
    let private_key = directory.join("server.key");
    std::fs::write(&certificate, &identity.cert_pem).expect("cert");
    std::fs::write(&private_key, &identity.key_pem).expect("key");
    (
        InboundProtocol::Http2Tls {
            certificate,
            private_key,
        },
        directory,
    )
}

// ---------------------------------------------------------------------------
// Product clients
// ---------------------------------------------------------------------------

/// An `EggFetch` client that trusts the test identity and speaks HTTP/2 only.
///
/// `Http2Only` rather than `Auto`: these rows exist to observe the negotiated
/// protocol, and a policy that would accept HTTP/1.1 could not distinguish
/// "spoke H2" from "silently fell back".
fn eggfetch_h2(identity: &TestIdentity) -> eggfetch_core::Client {
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .tls_config(tls)
        .build()
}

fn eggfetch_h2_routed(identity: &TestIdentity, route: &str) -> eggfetch_core::Client {
    let connector =
        eggress_outbound::OutboundConnector::from_pproxy_uri(route).expect("route parses");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(identity.cert_pem.as_bytes())
        .expect("valid test CA")
        .build();
    eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .dialer(EggressDialer::new(connector))
        .tls_config(tls)
        .build()
}

fn direct_route() -> eggreplay_core::PhysicalRoute {
    eggreplay_core::PhysicalRoute {
        kind: "direct".into(),
        description: Some("direct".into()),
    }
}

fn routed_route(route: &str) -> eggreplay_core::PhysicalRoute {
    physical_route_for(
        route,
        &Some(eggress_outbound::OutboundConnector::from_pproxy_uri(route).expect("route parses")),
    )
}

// ---------------------------------------------------------------------------
// Gateway harness
// ---------------------------------------------------------------------------

/// Everything one gateway invocation owns, so a test can finalize it in the
/// same order the CLI does: shutdown, wait, session shutdown, drain, finish.
struct Gateway {
    address: SocketAddr,
    handle: Option<InboundServerHandle>,
    session: eggreplay_store::RecordingSession,
    directory: PathBuf,
}

impl Gateway {
    /// Stop admission and wait for the accept loop to finish.
    ///
    /// Split from `finalize` because finalization *consumes* the recording
    /// session, and a type that owns a session and also implements `Drop`
    /// cannot move it out. Callers that abandon a gateway still get its
    /// shutdown from `Drop`; callers that finalize get the published session.
    async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
    }

    /// Shut the listener down and finalize the session, in the order the CLI
    /// uses: shutdown, wait, session shutdown, drain, finish.
    async fn finalize(mut self) -> (Session, PathBuf) {
        self.stop().await;
        let directory = self.directory.clone();
        self.session.shutdown();
        recording::drain_active_blobs(&self.session).await;
        let session = std::mem::replace(
            &mut self.session,
            eggreplay_store::RecordingSession::create(
                temp_path("unused-finalize-slot"),
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .expect("placeholder session"),
        );
        let published = recording::finish_recording_session(session)
            .await
            .expect("finalize");
        (published, directory)
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_gateway(
    name: &str,
    upstream: &Upstream,
    client: eggfetch_core::Client,
    physical: eggreplay_core::PhysicalRoute,
    protocol: InboundProtocol,
) -> Gateway {
    let directory = temp_path(name);
    let session = eggreplay_store::RecordingSession::create(
        &directory,
        SessionMetadata {
            capture_mode: "gateway-h2-e2e".into(),
            target: Some(upstream.base()),
            redaction_profile: "default-v1".into(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .expect("recording session");
    let handle = recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        upstream.base().parse().expect("upstream uri"),
        client,
        session.clone(),
        16 << 20,
        eggreplay_core::RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical,
        recording::WebSocketRecordingOptions::default(),
        protocol,
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();
    Gateway {
        address,
        handle: Some(handle),
        session,
        directory,
    }
}

// ---------------------------------------------------------------------------
// Replay harness
// ---------------------------------------------------------------------------

struct Replay {
    address: SocketAddr,
    handle: Option<InboundServerHandle>,
}

impl Replay {
    async fn close(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
    }
}

async fn start_replay(
    name: &str,
    directory: &PathBuf,
    protocol: InboundProtocol,
    matcher: Matcher,
) -> Replay {
    let session = Session::open(directory, StoreLimits::default()).expect("open session");
    let fixture = ReplayFixture::load_with_matcher(&session, matcher).expect("fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            protocol,
            H2Limits::default(),
        )
        .await
        // The listener binds an ephemeral port, so a bare failure would be
        // hard to attribute in a matrix where one row starts several replays.
        .unwrap_or_else(|error| panic!("replay server {name}: {error}"));
    let address = handle.local_addr();
    Replay {
        address,
        handle: Some(handle),
    }
}

// ---------------------------------------------------------------------------
// Canonical fixture helpers
// ---------------------------------------------------------------------------

fn canonical_request(path: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        scheme: "http".into(),
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

/// A canonical flow with no stored body, so it can be written without a
/// writer and used across the cross-protocol rows.
fn plain_flow(id: &str, request: HttpRequest, response: HttpResponse) -> Flow {
    Flow {
        schema_version: SCHEMA_VERSION,
        id: id.to_string(),
        started_at_ms: 1,
        completed_at_ms: Some(2),
        request,
        outcome: FlowOutcome::Response(response),
        physical_route: None,
        provenance: Provenance {
            mode: "test".into(),
            observer: "test".into(),
        },
        annotations: Vec::new(),
        redactions: Vec::new(),
    }
}

fn write_flows(directory: &PathBuf, flows: Vec<Flow>) {
    let mut writer = SessionWriter::create(
        directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    for flow in flows {
        writer.append_flow(&flow).expect("append flow");
    }
    writer.finish().expect("finish");
}

fn published_flows(session: &Session) -> Vec<Flow> {
    session
        .iter_flows()
        .expect("iterable")
        .collect::<Result<Vec<_>, _>>()
        .expect("every published flow is readable")
}

/// Drop `date` from a flow's recorded response before comparing it.
///
/// `date` is origin-generated and second-granular, so two correct answers a
/// second apart differ. The regression authority compares it — that strictness
/// is the product's contract and M015C must not relax it — so a comparison that
/// is about protocol *semantics* normalizes this one wall-clock field. This
/// mirrors what the outbound HTTP/2 qualification already does; without it a
/// report is only clean when acquisition and re-execution land in the same
/// second, which is a coin flip, not a property.
fn without_volatile_date(flow: &Flow) -> Flow {
    let mut normalized = flow.clone();
    if let FlowOutcome::Response(response) = &mut normalized.outcome {
        response
            .headers
            .retain(|entry| !entry.name.eq_ignore_ascii_case("date"));
    }
    normalized
}

// ---------------------------------------------------------------------------
// Row 1: H2 client -> gateway -> direct H2 upstream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn row1_h2_client_to_gateway_to_direct_h2_upstream() {
    let upstream = start_upstream("row1-direct").await;
    let gateway = start_gateway(
        "row1-direct",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;

    let mut peer = h2_connect_cleartext(gateway.address).await;
    let (status, headers, body) =
        h2_collect(peer.send_get("/row1").0.await.expect("response")).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"upstream:/row1");
    assert_eq!(
        headers
            .get("x-upstream")
            .map(|value| value.to_str().expect("ascii")),
        Some("h2"),
        "the upstream's own header must survive the hop through the gateway"
    );
    assert_eq!(
        upstream.served(),
        1,
        "the H2 upstream was reached exactly once"
    );

    let (published, directory) = gateway.finalize().await;
    assert_eq!(published.manifest().flow_count, 1);
    let flows = published_flows(&published);
    let FlowOutcome::Response(response) = &flows[0].outcome else {
        panic!("the gateway records a response for a successful exchange");
    };
    assert_eq!(response.status, 200);
    assert!(
        flows[0]
            .annotations
            .contains(&("transport".to_string(), "http-version:h2".to_string())),
        "the upstream negotiated version must be recorded, got {:?}",
        flows[0].annotations
    );
    for header in flows[0].request.headers.iter().chain(&response.headers) {
        assert!(
            !header.name.starts_with(':'),
            "a stored header leaked a pseudo-header: {}",
            header.name
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Row 2: H2 client -> gateway -> Eggress-routed H2 upstream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn row2_h2_client_to_gateway_to_eggress_routed_h2_upstream() {
    let upstream = start_upstream("row2-routed").await;
    let socks = start_socks5(upstream.address).await;
    let route = eggress_route(&socks);
    let physical = routed_route(&route);
    assert_eq!(physical.kind, "eggress");

    let gateway = start_gateway(
        "row2-routed",
        &upstream,
        eggfetch_h2_routed(&upstream.identity, &route),
        physical.clone(),
        InboundProtocol::Http2Cleartext,
    )
    .await;

    let mut peer = h2_connect_cleartext(gateway.address).await;
    let (status, _, body) = h2_collect(peer.send_get("/row2").0.await.expect("response")).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"upstream:/row2");
    assert_eq!(upstream.served(), 1, "the routed H2 upstream answered");

    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 1);
    // Routing and protocol are independent facts, and both are recorded.
    assert_eq!(flows[0].physical_route.as_ref().expect("route"), &physical);
    assert!(
        flows[0]
            .annotations
            .contains(&("transport".to_string(), "http-version:h2".to_string())),
        "SNI and ALPN stay in EggFetch, but the negotiated version is still recorded"
    );
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Row 3: H2 client -> offline H2 replay of an H2-acquired fixture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn row3_h2_client_to_offline_h2_replay() {
    let upstream = start_upstream("row3").await;
    let gateway = start_gateway(
        "row3",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let mut peer = h2_connect_cleartext(gateway.address).await;
    let (_, _, body) = h2_collect(peer.send_get("/row3").0.await.expect("response")).await;
    assert_eq!(body, b"upstream:/row3");
    let (published, directory) = gateway.finalize().await;
    assert_eq!(published.manifest().flow_count, 1);

    // The gateway rewrites the request onto the configured upstream origin, so
    // the recorded authority is the *upstream's*, not the gateway's own. A
    // replay client therefore addresses the recorded origin. Reading the
    // authority from the fixture rather than hard-coding it keeps this honest:
    // if the acquisition policy changed, the test would notice.
    let recorded_authority = published_flows(&published)[0].request.authority.clone();
    assert_eq!(
        recorded_authority,
        format!("localhost:{}", upstream.address.port()),
        "the gateway records the authority of the configured upstream origin"
    );

    // Offline: no upstream, no network, nothing but the fixture. Served over
    // TLS because the acquisition was over TLS and the matcher compares
    // scheme; the sibling test below pins what happens when they differ.
    let identity = test_identity("row3-replay-identity");
    let (policy, identity_directory) = tls_policy_for(&identity, "row3-replay-pki");
    let replay = start_replay("row3-replay", &directory, policy, Matcher::strict(8)).await;
    let mut replay_peer = h2_connect_tls(replay.address, &identity).await;
    let (status, headers, body) = h2_collect(
        replay_peer
            .send_get_at(&recorded_authority, "/row3")
            .0
            .await
            .expect("response"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, b"upstream:/row3", "the recorded body replays exactly");
    assert_eq!(
        headers
            .get("x-upstream")
            .map(|value| value.to_str().expect("ascii")),
        Some("h2"),
        "recorded response headers replay, including one the gateway never added"
    );
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
    let _ = std::fs::remove_dir_all(&identity_directory);
}

/// The scheme is part of what a fixture records, so a faithful offline
/// replay must be served on the same transport class as the acquisition.
///
/// This is not an HTTP/2 limitation — it is the same on HTTP/1.1 — and it is
/// pinned here because the M015C matrix crosses protocols freely and a reader
/// deserves to know which rows need a matching scheme. The behaviour is
/// fail-closed: a mismatch is a 404, never a silently-relaxed match.
#[tokio::test]
async fn replay_scheme_must_match_the_acquisition_transport() {
    let directory = temp_path("scheme");
    // Recorded as acquired over TLS.
    let mut secure = canonical_request("/scheme");
    secure.scheme = "https".into();
    write_flows(
        &directory,
        vec![plain_flow(
            "flow-0000",
            secure.clone(),
            canonical_response(200),
        )],
    );

    // Served cleartext: no match, and no silent relaxation.
    let replay = start_replay(
        "scheme-cleartext",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let (status, _, _) = h2_collect(peer.send_get("/scheme").0.await.expect("response")).await;
    assert_eq!(
        status, 404,
        "a scheme mismatch must fail closed rather than match anyway"
    );
    replay.close().await;

    // A cleartext-acquired flow replays over cleartext.
    //
    // A *second* fixture, not a rewrite of the first: `SessionWriter::create`
    // refuses an existing destination by design, so reusing `directory` would
    // be testing the writer's guard rather than the matcher.
    let clear_directory = temp_path("scheme-cleartext");
    let clear = plain_flow(
        "flow-0000",
        canonical_request("/scheme"),
        canonical_response(200),
    );
    write_flows(&clear_directory, vec![clear]);
    let replay = start_replay(
        "scheme-cleartext-match",
        &clear_directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let (status, _, _) = h2_collect(peer.send_get("/scheme").0.await.expect("response")).await;
    assert_eq!(status, 200, "a cleartext flow replays over cleartext");
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
    let _ = std::fs::remove_dir_all(&clear_directory);
}

// ---------------------------------------------------------------------------
// Rows 4 + 5: recorded fixture -> direct / routed H2 regression candidate
// ---------------------------------------------------------------------------

/// Acquire three flows over H2 through the gateway, and return the published
/// session with the directory it was written to.
async fn acquire_over_h2(upstream: &Upstream, name: &str) -> (Session, PathBuf) {
    let gateway = start_gateway(
        name,
        upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let mut peer = h2_connect_cleartext(gateway.address).await;
    for path in ["/alpha", "/beta", "/gamma"] {
        let (_, _, body) = h2_collect(peer.send_get(path).0.await.expect("response")).await;
        assert_eq!(body, format!("upstream:{path}").as_bytes());
    }
    let (published, directory) = gateway.finalize().await;
    assert_eq!(published.manifest().flow_count, 3);
    (published, directory)
}

/// Run every recorded flow as a candidate against `client` and return the
/// comparison reports.
async fn regress(
    published: &Session,
    client: &eggfetch_core::Client,
    target: &http::Uri,
    physical: eggreplay_core::PhysicalRoute,
) -> Vec<eggreplay_core::RegressionReport> {
    let mut reports = Vec::new();
    for flow in published_flows(published) {
        // The baseline body must be the *stored* bytes. Comparing against an
        // empty slice would report a body difference for every flow and prove
        // nothing about H2.
        let baseline_body = match &flow.outcome {
            FlowOutcome::Response(response) => match &response.body {
                BodyRef::Blob(blob) => published.read_blob(blob).expect("baseline body"),
                BodyRef::Absent | BodyRef::Empty => Vec::new(),
            },
            FlowOutcome::Error(_) => Vec::new(),
        };
        let observation = execute_candidate(
            client,
            &flow.request,
            b"",
            target,
            16 << 20,
            Some(physical.clone()),
        )
        .await
        .expect("candidate executes");
        reports.push(compare_flows(
            &without_volatile_date(&flow),
            &without_volatile_date(&observation.flow),
            &baseline_body,
            &observation.response_body,
            ReportScheduler::Sequential,
        ));
    }
    reports
}

#[tokio::test]
async fn row4_recorded_fixture_to_direct_h2_regression_candidate() {
    let upstream = start_upstream("row4").await;
    let (published, directory) = acquire_over_h2(&upstream, "row4").await;
    let client = eggfetch_h2(&upstream.identity);
    let target: http::Uri = upstream.base().parse().expect("target");

    let reports = regress(&published, &client, &target, direct_route()).await;
    assert_eq!(reports.len(), 3);
    for report in &reports {
        assert!(
            report.is_success(),
            "an H2-acquired flow must be reproduced by a direct H2 candidate, got {:?}",
            report.findings
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn row5_recorded_fixture_to_eggress_routed_h2_regression_candidate() {
    let upstream = start_upstream("row5").await;
    let socks = start_socks5(upstream.address).await;
    let route = eggress_route(&socks);
    let (published, directory) = acquire_over_h2(&upstream, "row5").await;

    let client = eggfetch_h2_routed(&upstream.identity, &route);
    let target: http::Uri = upstream.base().parse().expect("target");
    let reports = regress(&published, &client, &target, routed_route(&route)).await;
    assert_eq!(reports.len(), 3);
    for report in &reports {
        assert!(
            report.is_success(),
            "a routed H2 candidate must match, got {:?}",
            report.findings
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Rows 6 + 7: cross-protocol replay — the load-bearing rows
// ---------------------------------------------------------------------------

/// Row 6: a flow acquired over HTTP/1.1 replays over HTTP/2.
///
/// If protocol had become a matching dimension, or the H2 rendering path
/// differed semantically, this would 404.
#[tokio::test]
async fn row6_h1_acquired_fixture_replays_over_h2() {
    let directory = temp_path("row6");
    write_flows(
        &directory,
        vec![plain_flow(
            "flow-0000",
            canonical_request("/cross"),
            HttpResponse {
                status: 200,
                headers: vec![HeaderEntry {
                    name: "x-origin".into(),
                    value: "h1-recorded".into(),
                }],
                ..canonical_response(200)
            },
        )],
    );
    let replay = start_replay(
        "row6",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let (status, headers, _) = h2_collect(peer.send_get("/cross").0.await.expect("response")).await;
    assert_eq!(status, 200, "an H1-acquired flow must replay over H2");
    assert_eq!(
        headers
            .get("x-origin")
            .map(|value| value.to_str().expect("ascii")),
        Some("h1-recorded")
    );
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// Row 7: a flow acquired over HTTP/2 replays over HTTP/1.1.
#[tokio::test]
async fn row7_h2_acquired_fixture_replays_over_h1() {
    let directory = temp_path("row7");
    write_flows(
        &directory,
        vec![plain_flow(
            "flow-0000",
            canonical_request("/cross"),
            HttpResponse {
                status: 200,
                headers: vec![HeaderEntry {
                    name: "x-origin".into(),
                    value: "h2-recorded".into(),
                }],
                ..canonical_response(200)
            },
        )],
    );
    let replay = start_replay(
        "row7",
        &directory,
        InboundProtocol::Http1,
        Matcher::strict(8),
    )
    .await;
    let mut sender = h1_connect(replay.address).await;
    let response = sender
        .send_request(h1_request("/cross"))
        .await
        .expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(
        response.status(),
        200,
        "an H2-acquired flow must replay over H1"
    );
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// A flow whose only difference is its `http-version` annotation compares
/// equal, and does so identically served over both protocols.
///
/// This is the direct test of "protocol annotations are observational
/// metadata, not an accidental matching dimension": the annotation is
/// preserved in the fixture and ignored by both the matcher and the report.
#[tokio::test]
async fn version_annotations_are_observational_not_matching_dimensions() {
    for annotation in ["http-version:h1", "http-version:h2", "http-version:absent"] {
        // Matching: the annotation must not affect whether a request matches.
        let directory = temp_path("annotation");
        let mut flow = plain_flow(
            "flow-0000",
            canonical_request("/annotated"),
            canonical_response(200),
        );
        flow.annotations = vec![("transport".to_string(), annotation.to_string())];
        write_flows(&directory, vec![flow]);

        for protocol in [InboundProtocol::Http1, InboundProtocol::Http2Cleartext] {
            let replay = start_replay(
                "annotation",
                &directory,
                protocol.clone(),
                Matcher::strict(8),
            )
            .await;
            let status = match protocol {
                InboundProtocol::Http1 => {
                    let mut sender = h1_connect(replay.address).await;
                    sender
                        .send_request(h1_request("/annotated"))
                        .await
                        .expect("response")
                        .status()
                }
                _ => {
                    let mut peer = h2_connect_cleartext(replay.address).await;
                    let (response, _) = peer.send_get("/annotated");
                    response.await.expect("response").status()
                }
            };
            assert_eq!(
                status, 200,
                "annotation {annotation} must not affect matching"
            );
            replay.close().await;
        }

        // Comparison: the annotation must not affect the report either.
        let baseline = plain_flow(
            "flow-0000",
            canonical_request("/annotated"),
            canonical_response(200),
        );
        let candidate = Flow {
            annotations: vec![("transport".to_string(), annotation.to_string())],
            ..baseline.clone()
        };
        let report = compare_flows(&baseline, &candidate, b"", b"", ReportScheduler::Sequential);
        assert!(
            report.is_success(),
            "differing only in the {annotation} annotation must produce no finding, got {:?}",
            report.findings
        );

        // Non-vacuous: a real difference is still reported.
        let changed = Flow {
            outcome: FlowOutcome::Response(canonical_response(500)),
            ..baseline.clone()
        };
        assert!(
            !compare_flows(&baseline, &changed, b"", b"", ReportScheduler::Sequential,)
                .is_success(),
            "a status change must still be reported, or the check above proves nothing"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}

// ---------------------------------------------------------------------------
// Semantic evidence
// ---------------------------------------------------------------------------

/// Repeated-header and query normalization is identical over both protocols.
#[tokio::test]
async fn header_and_query_normalization_is_protocol_neutral() {
    let directory = temp_path("normalize");
    let mut request = canonical_request("/normalize");
    request.query = vec![QueryPair {
        key: "a".into(),
        value: "1".into(),
    }];
    request.headers = vec![
        HeaderEntry {
            name: "x-multi".into(),
            value: "one".into(),
        },
        HeaderEntry {
            name: "x-multi".into(),
            value: "two".into(),
        },
    ];
    write_flows(
        &directory,
        vec![plain_flow("flow-0000", request, canonical_response(200))],
    );

    // HTTP/1.1.
    let replay = start_replay(
        "normalize-h1",
        &directory,
        InboundProtocol::Http1,
        Matcher::strict(8),
    )
    .await;
    let mut sender = h1_connect(replay.address).await;
    let mut h1 = h1_request("/normalize?a=1");
    h1.headers_mut()
        .append("x-multi", "one".parse().expect("value"));
    h1.headers_mut()
        .append("x-multi", "two".parse().expect("value"));
    let h1_status = sender.send_request(h1).await.expect("response").status();
    replay.close().await;

    // HTTP/2, same request shape.
    let replay = start_replay(
        "normalize-h2",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let mut h2 = Request::builder()
        .method(Method::GET)
        .uri("http://localhost/normalize?a=1")
        .body(())
        .expect("request");
    h2.headers_mut()
        .append("x-multi", "one".parse().expect("value"));
    h2.headers_mut()
        .append("x-multi", "two".parse().expect("value"));
    let (response, _) = peer.sender.send_request(h2, true).expect("send");
    let h2_status = response.await.expect("response").status();
    replay.close().await;

    assert_eq!(h1_status, 200, "normalization must match over HTTP/1.1");
    assert_eq!(h2_status, 200, "normalization must match over HTTP/2");
    let _ = std::fs::remove_dir_all(&directory);
}

/// A body far larger than one H2 frame streams intact — the bounded
/// large-streaming evidence.
#[tokio::test]
async fn large_response_streams_intact_over_h2() {
    let directory = temp_path("large");
    let payload: Vec<u8> = (0..512 * 1024u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    {
        use std::io::Write;
        let mut blob = writer.begin_blob().expect("blob");
        blob.write_all(&payload).expect("write");
        let body = blob.finish().expect("finish blob");
        writer
            .append_flow(&Flow {
                outcome: FlowOutcome::Response(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body,
                    trailers: Vec::new(),
                }),
                ..plain_flow(
                    "flow-0000",
                    canonical_request("/large"),
                    canonical_response(200),
                )
            })
            .expect("append flow");
    }
    writer.finish().expect("finish");

    let replay = start_replay(
        "large",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let (status, _, body) = h2_collect(peer.send_get("/large").0.await.expect("response")).await;
    assert_eq!(status, 200);
    assert_eq!(body.len(), payload.len(), "large body length must be exact");
    assert_eq!(body, payload, "large body bytes must be exact");
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// Concurrent streams on one connection each reach their own candidate, and
/// ordered consumption still serializes the one-shot case. The pair is the
/// evidence: concurrency where it is safe, ordering where it is required.
#[tokio::test]
async fn multiplexing_is_concurrent_where_safe_and_ordered_where_required() {
    let directory = temp_path("multiplex");
    let flows: Vec<Flow> = (0..8)
        .map(|index| {
            plain_flow(
                &format!("flow-{index:04}"),
                canonical_request(&format!("/c{index}")),
                canonical_response(200),
            )
        })
        .chain(std::iter::once(plain_flow(
            "flow-0008",
            canonical_request("/once"),
            canonical_response(200),
        )))
        .collect();
    write_flows(&directory, flows);

    let replay = start_replay(
        "multiplex",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;

    // Open all nine before awaiting any: genuinely concurrent on one
    // connection, each addressed to its own candidate.
    let mut pending = Vec::new();
    for index in 0..8 {
        let (response, _) = peer.send_get(&format!("/c{index}"));
        pending.push((format!("/c{index}"), response));
    }
    let (first_once, _) = peer.send_get("/once");
    let (second_once, _) = peer.send_get("/once");
    for (path, response) in pending {
        let (status, _, _) = h2_collect(response.await.expect("response")).await;
        assert_eq!(status, 200, "{path} must match its own candidate");
    }
    // Ordered consumption: the first takes the one-shot candidate, the
    // second finds it exhausted, even though they overlapped.
    assert_eq!(
        h2_collect(first_once.await.expect("first")).await.0,
        200,
        "the first request takes the one-shot candidate"
    );
    assert_eq!(
        h2_collect(second_once.await.expect("second")).await.0,
        409,
        "consumption ordering must survive multiplexing"
    );
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// Every matcher profile behaves identically over HTTP/1.1 and HTTP/2,
/// including semantic JSON comparison.
#[tokio::test]
async fn matcher_profiles_behave_identically_across_protocols() {
    use eggreplay_core::{BodyMatchMode, MatcherProfile};
    let profiles: Vec<(&str, Matcher)> = vec![
        ("strict", Matcher::strict(8)),
        ("practical", Matcher::practical(8)),
        (
            "semantic-json",
            Matcher::new(MatcherProfile::Practical, BodyMatchMode::SemanticJson, 8),
        ),
    ];
    let directory = temp_path("profiles");
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    {
        use std::io::Write;
        let mut blob = writer.begin_blob().expect("blob");
        blob.write_all(br#"{"b":2,"a":1}"#).expect("write");
        let body = blob.finish().expect("finish");
        writer
            .append_flow(&Flow {
                outcome: FlowOutcome::Response(HttpResponse {
                    status: 200,
                    headers: vec![HeaderEntry {
                        name: "content-type".into(),
                        value: "application/json".into(),
                    }],
                    body,
                    trailers: Vec::new(),
                }),
                ..plain_flow(
                    "flow-0000",
                    canonical_request("/profiles"),
                    canonical_response(200),
                )
            })
            .expect("append flow");
    }
    writer.finish().expect("finish");

    for (name, matcher) in &profiles {
        let mut statuses = Vec::new();
        for protocol in [InboundProtocol::Http1, InboundProtocol::Http2Cleartext] {
            let replay =
                start_replay("profiles", &directory, protocol.clone(), matcher.clone()).await;
            let status = match protocol {
                InboundProtocol::Http1 => {
                    let mut sender = h1_connect(replay.address).await;
                    sender
                        .send_request(h1_request("/profiles"))
                        .await
                        .expect("response")
                        .status()
                }
                _ => {
                    let mut peer = h2_connect_cleartext(replay.address).await;
                    let (response, _) = peer.send_get("/profiles");
                    response.await.expect("response").status()
                }
            };
            statuses.push(status);
            replay.close().await;
        }
        assert_eq!(
            statuses[0], statuses[1],
            "the {name} profile must behave identically over H1 and H2, got {statuses:?}"
        );
        assert_eq!(statuses[0], 200, "the {name} profile must match");
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// A recorded upstream error keeps its semantic category over HTTP/2.
#[tokio::test]
async fn upstream_error_category_survives_h2() {
    let directory = temp_path("error");
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&Flow {
            outcome: FlowOutcome::Error(eggreplay_core::FlowError::new(
                ErrorCategory::Protocol,
                ErrorPhase::Headers,
                "recorded upstream protocol failure",
            )),
            ..plain_flow(
                "flow-0000",
                canonical_request("/boom"),
                canonical_response(200),
            )
        })
        .expect("append flow");
    writer.finish().expect("finish");

    let replay = start_replay(
        "error",
        &directory,
        InboundProtocol::Http2Cleartext,
        Matcher::strict(8),
    )
    .await;
    let mut peer = h2_connect_cleartext(replay.address).await;
    let (status, _, body) = h2_collect(peer.send_get("/boom").0.await.expect("response")).await;
    assert_eq!(status, 502, "a recorded error projects as 502 on H2 too");
    assert!(
        String::from_utf8_lossy(&body).contains("Protocol"),
        "the recorded category must survive, got {:?}",
        String::from_utf8_lossy(&body)
    );
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// `EggFetch` 0.2.2's typed failure classification reports only
/// evidence-backed kinds, and reports nothing when the evidence is absent.
#[tokio::test]
async fn eggfetch_failure_classification_is_evidence_backed() {
    // Bind then drop, so the port is almost certainly closed: a connection
    // establishment failure, which is `Connect` and nothing more specific.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let closed = listener.local_addr().expect("addr");
    drop(listener);

    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http1Only)
        .build();
    let error = client
        .request(
            Method::GET,
            &format!("http://127.0.0.1:{}/anything", closed.port()),
        )
        .expect("request")
        .send()
        .await
        .expect_err("a closed port must fail");
    assert_eq!(
        error.transport_failure_kind(),
        Some(eggfetch_core::TransportFailureKind::Connect),
        "a refused connection is Connect and must not be reported as TLS or protocol"
    );

    // An untrusted certificate is unambiguously a TLS failure: the
    // connection is established and the handshake completes far enough to
    // reject the certificate, so the evidence is specific.
    let presented = test_identity("tls-untrusted-server");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    let acceptor =
        tokio_rustls::TlsAcceptor::from(server_tls(&presented, vec![b"http/1.1".to_vec()]));
    let server = tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await {
            let _ = acceptor.accept(tcp).await;
        }
    });
    let trusted_by_client = test_identity("tls-untrusted-client");
    let tls_client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http1Only)
        .tls_config(
            eggfetch_core::tls::TlsConfig::builder()
                // The client trusts a *different* identity, so verification
                // must fail on evidence rather than on connect.
                .ca_certificate_pem(trusted_by_client.cert_pem.as_bytes())
                .expect("valid test CA")
                .build(),
        )
        .build();
    let error = tls_client
        .request(
            Method::GET,
            &format!("https://localhost:{}/anything", address.port()),
        )
        .expect("request")
        .send()
        .await
        .expect_err("an untrusted certificate must fail verification");
    assert_eq!(
        error.transport_failure_kind(),
        Some(eggfetch_core::TransportFailureKind::Tls),
        "a certificate that fails verification is a TLS failure, not a connect failure"
    );
    server.abort();
}

/// A GOAWAY mid-stream must not publish a partial or corrupt session. The
/// gateway either records a completed flow or records nothing; it never
/// publishes a truncated one, and the published session always validates.
#[tokio::test]
async fn shutdown_cannot_publish_a_partial_session() {
    let upstream = start_upstream("shutdown").await;
    let gateway = start_gateway(
        "shutdown",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let address = gateway.address;

    // Issue a request and shut the listener down immediately, so the request
    // may or may not complete. Both outcomes are legal; a corrupt one is not.
    let mut peer = h2_connect_cleartext(address).await;
    let (response, _) = peer.send_get("/shutdown");
    if let Some(handle) = gateway.handle.as_ref() {
        handle.shutdown();
    }
    let observed = match response.await {
        Ok(response) => Some(h2_collect(response).await.0),
        Err(_) => None,
    };
    let (published, directory) = gateway.finalize().await;

    match observed {
        Some(StatusCode::OK) => assert_eq!(
            published.manifest().flow_count,
            1,
            "a completed exchange must publish exactly one flow"
        ),
        Some(_) => assert_eq!(
            published.manifest().flow_count,
            0,
            "a non-OK response that did not complete must publish no flow"
        ),
        None => assert_eq!(
            published.manifest().flow_count,
            0,
            "a request cut off by shutdown must not publish a partial flow"
        ),
    }
    let flows = published_flows(&published);
    assert_eq!(flows.len(), published.manifest().flow_count);
    for flow in flows {
        flow.validate().expect("every published flow must validate");
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// Cancellation is stream-local end to end: resetting one stream leaves its
/// siblings and the connection intact, and the completed flows are recorded.
#[tokio::test]
async fn cancellation_is_stream_local_end_to_end() {
    let upstream = start_upstream("cancel").await;
    let gateway = start_gateway(
        "cancel",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let mut peer = h2_connect_cleartext(gateway.address).await;

    let (cancelled, mut cancelled_stream) = peer.send_get("/cancelled");
    let (survivor, _) = peer.send_get("/survivor");
    cancelled_stream.send_reset(h2::Reason::CANCEL);

    let (status, _, body) = h2_collect(survivor.await.expect("survivor response")).await;
    assert_eq!(status, 200, "a sibling stream must complete normally");
    assert_eq!(body, b"upstream:/survivor");

    let (again, _) = peer.send_get("/again");
    let (status, _, body) = h2_collect(again.await.expect("follow-up response")).await;
    assert_eq!(status, 200, "the connection must survive a stream reset");
    assert_eq!(body, b"upstream:/again");
    let _ = cancelled.await;

    let (published, directory) = gateway.finalize().await;
    assert!(
        published.manifest().flow_count >= 2,
        "the completed flows must be recorded, got {}",
        published.manifest().flow_count
    );
    for flow in published_flows(&published) {
        flow.validate().expect("a recorded flow must validate");
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// A TLS gateway with the H2 serving policy serves H2 to a client that offers
/// `h2` and H1 to a client that does not, over the same listener.
#[tokio::test]
async fn tls_alpn_h2_gateway_serves_both_protocols() {
    let upstream = start_upstream("tls-gateway").await;
    let pki = temp_dir("tls-gateway-pki");
    let cert = pki.join("server.pem");
    let key = pki.join("server.key");
    let gateway_identity = test_identity("tls-gateway-pki-identity");
    std::fs::write(&cert, &gateway_identity.cert_pem).expect("cert");
    std::fs::write(&key, &gateway_identity.key_pem).expect("key");

    let gateway = start_gateway(
        "tls-gateway",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Tls {
            certificate: cert,
            private_key: key,
        },
    )
    .await;
    let address = gateway.address;
    let client_identity = TestIdentity {
        cert_der: gateway_identity.cert_der.clone(),
        cert_pem: String::new(),
        key_der: Vec::new(),
        key_pem: String::new(),
        directory: pki.clone(),
    };

    // H2 over TLS.
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let connector = tokio_rustls::TlsConnector::from(client_tls(&client_identity, &[b"h2"]));
    let tls = connector.connect(server_name(), tcp).await.expect("tls");
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sender, connection) = h2::client::handshake(tls).await.expect("handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut peer = H2Peer {
        sender,
        scheme: "https",
    };
    let (status, _, body) = h2_collect(peer.send_get("/tls-h2").0.await.expect("response")).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"upstream:/tls-h2");

    // H1 over the same listener, on raw bytes.
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let connector = tokio_rustls::TlsConnector::from(client_tls(&client_identity, &[b"http/1.1"]));
    let mut tls = connector.connect(server_name(), tcp).await.expect("tls");
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    {
        use tokio::io::AsyncWriteExt;
        tls.write_all(b"GET /tls-h1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");
    }
    let raw = read_http1_message(&mut tls).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "ALPN fallback must serve H1 on the same listener, got: {text}"
    );
    assert!(text.contains("upstream:/tls-h1"), "got: {text}");

    let (_published, directory) = gateway.finalize().await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// M010 stream events under multiplexing
// ---------------------------------------------------------------------------

/// Read the M010 stream-event extension a published session carries.
fn read_stream_events(published: &Session) -> eggreplay_core::StreamEvents {
    let bytes = published
        .read_extension("stream-events")
        .expect("read extension")
        .expect("a gateway-acquired session records stream events");
    let decoded: eggreplay_core::StreamEvents =
        serde_json::from_slice(&bytes).expect("stream events decode");
    decoded
        .validate().expect(
        "multiplexed H2 acquisition must produce valid stream events: contiguity, ordering, and terminal state are all checked here",
    );
    decoded
}

/// Multiplexed H2 acquisition keeps the M010 stream-event model coherent, and
/// a candidate re-execution reproduces the event *shape*.
///
/// Three streams are opened before any of them is drained, so the events must
/// be attributed per stream rather than per connection.
///
/// Cadence is deliberately not asserted across re-execution: `delta_ns` is
/// wall-clock and a candidate cannot reproduce it exactly. `cadence_tolerance_ns:
/// None` is precisely the shape comparison — offsets, lengths, trailers, and
/// the terminal event — and it is the same call the CLI splices into a report.
#[tokio::test]
async fn stream_events_stay_coherent_under_multiplexing() {
    let upstream = start_upstream("stream-events").await;
    let gateway = start_gateway(
        "stream-events",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let mut peer = h2_connect_cleartext(gateway.address).await;

    // All three in flight before any response is read.
    let (first, _) = peer.send_get("/one");
    let (second, _) = peer.send_get("/two");
    let (third, _) = peer.send_get("/three");
    // Drained out of order, so ordering by arrival cannot be what makes the
    // attribution look right.
    let (status, _, body) = h2_collect(third.await.expect("third")).await;
    assert_eq!(
        (status.as_u16(), body.as_slice()),
        (200, b"upstream:/three".as_slice())
    );
    let (status, _, body) = h2_collect(first.await.expect("first")).await;
    assert_eq!(
        (status.as_u16(), body.as_slice()),
        (200, b"upstream:/one".as_slice())
    );
    let (status, _, body) = h2_collect(second.await.expect("second")).await;
    assert_eq!(
        (status.as_u16(), body.as_slice()),
        (200, b"upstream:/two".as_slice())
    );

    let (published, directory) = gateway.finalize().await;
    let events = read_stream_events(&published);
    assert_eq!(
        events.schema_version,
        eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION
    );
    assert_eq!(events.flows.len(), 3, "one event sequence per stream");

    // Every flow owns a sequence, every sequence terminates, and the recorded
    // DATA byte count agrees with the stored body.
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 3);
    for flow in &flows {
        let sequence = events
            .flows
            .iter()
            .find(|item| item.flow_id == flow.id)
            .unwrap_or_else(|| panic!("flow {} has no event sequence", flow.id));
        let kinds: Vec<&'static str> = sequence
            .response
            .iter()
            .map(|event| match &event.event {
                eggreplay_core::StreamEventKind::Data { .. } => "data",
                eggreplay_core::StreamEventKind::Trailers { .. } => "trailers",
                eggreplay_core::StreamEventKind::End => "end",
                eggreplay_core::StreamEventKind::Error { .. } => "error",
            })
            .collect();
        assert_eq!(
            kinds.last().copied(),
            Some("end"),
            "flow {} must terminate with End, got {kinds:?}",
            flow.id
        );
        let recorded: u64 = sequence
            .response
            .iter()
            .map(|event| match &event.event {
                eggreplay_core::StreamEventKind::Data { length, .. } => *length,
                _ => 0,
            })
            .sum();
        let expected = match &flow.outcome {
            FlowOutcome::Response(response) => response.body.len().unwrap_or(0),
            FlowOutcome::Error(_) => 0,
        };
        assert_eq!(
            recorded, expected,
            "flow {} event bytes must agree with its stored body",
            flow.id
        );
    }

    // Negative control: the timed-replay gate fails closed on a session with
    // no stream-event metadata, so a positive `load_with_timing` below is
    // evidence rather than a tautology.
    let bare = temp_path("stream-events-bare");
    write_flows(
        &bare,
        vec![plain_flow(
            "flow-0000",
            canonical_request("/one"),
            canonical_response(200),
        )],
    );
    let bare_session = Session::open(&bare, StoreLimits::default()).expect("open bare");
    let refused = ReplayFixture::load_with_timing(
        &bare_session,
        Matcher::strict(8),
        eggreplay_core::StreamTimingMode::Recorded,
    );
    assert!(
        refused.is_err(),
        "timed replay must fail closed without stream-event metadata"
    );

    // Positive control: the H2-acquired session satisfies that same gate.
    ReplayFixture::load_with_timing(
        &published,
        Matcher::strict(8),
        eggreplay_core::StreamTimingMode::Recorded,
    )
    .expect("an H2-acquired session carries the M010 metadata timed replay requires");

    // Shape parity between acquisition and re-execution.
    let client = eggfetch_h2(&upstream.identity);
    let target: http::Uri = upstream.base().parse().expect("target");
    for flow in &flows {
        let baseline = events
            .flows
            .iter()
            .find(|item| item.flow_id == flow.id)
            .expect("baseline events");
        let observation = execute_candidate(
            &client,
            &flow.request,
            b"",
            &target,
            16 << 20,
            Some(direct_route()),
        )
        .await
        .expect("candidate executes");
        // Candidate execution does not observe request-frame cadence, so both
        // sides are compared response-only — the same splice the CLI makes.
        let left = eggreplay_core::FlowStreamEvents {
            flow_id: flow.id.clone(),
            start_offset_ns: 0,
            request: Vec::new(),
            response: baseline.response.clone(),
        };
        let right = eggreplay_core::FlowStreamEvents {
            flow_id: flow.id.clone(),
            start_offset_ns: 0,
            request: Vec::new(),
            response: observation.response_events.clone(),
        };
        let findings = eggreplay_core::compare_stream_events(&left, &right, None);
        assert!(
            findings.is_empty(),
            "H2 acquisition and H2 candidate must agree on stream event shape for {}, got {findings:?}",
            flow.id
        );
    }

    let _ = std::fs::remove_dir_all(&bare);
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Stateful scenarios and deterministic templates
// ---------------------------------------------------------------------------

/// A stateful scenario with a deterministic `{{variable}}` template behaves
/// identically over HTTP/1.1 and HTTP/2.
///
/// The three-request sequence is the whole point:
///
/// 1. `/users/bob` matches no transition, so the scenario yields nothing and
///    the request falls through to recorded matching — which is empty — and
///    404s without advancing the state;
/// 2. `/users/alice` matches, extracts `alice` from the path, renders it into
///    both a body template and a header template, and advances the state;
/// 3. the same `/users/alice` no longer matches, because the scenario is now in
///    its terminal state.
///
/// Step 3 is what distinguishes a stateful scenario from a stateless one: a
/// stateless engine would answer 201 again. Both protocols must produce the
/// identical status/header/body triple at every step.
#[tokio::test]
async fn stateful_scenarios_and_templates_are_identical_across_protocols() {
    use eggreplay_core::{
        ExtractionFailureBehavior, RULES_SCHEMA_VERSION, RequestPredicate, Scenario,
        ScenarioResponse, ScenarioRules, ScenarioTransition, VariableExtraction, VariableSource,
    };

    let directory = temp_path("scenario-parity");
    let rules = ScenarioRules {
        schema_version: RULES_SCHEMA_VERSION,
        scenarios: vec![Scenario {
            id: "users".into(),
            initial_state: "start".into(),
            states: vec!["start".into(), "created".into()],
            transitions: vec![ScenarioTransition {
                from: "start".into(),
                when: vec![RequestPredicate::Path {
                    value: "/users/alice".into(),
                }],
                extract: vec![VariableExtraction {
                    name: "user".into(),
                    source: VariableSource::PathSegment { index: 1 },
                }],
                extraction_failure: ExtractionFailureBehavior::Abort,
                response: ScenarioResponse {
                    status: 201,
                    headers: vec![HeaderEntry {
                        name: "content-type".into(),
                        value: "text/plain".into(),
                    }],
                    body_template: "created {{user}}".into(),
                    json_pointer_replacements: Vec::new(),
                    fault: None,
                },
                next_state: "created".into(),
            }],
        }],
    };
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
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
    assert_eq!(
        session.manifest().flow_count,
        0,
        "a scenario-driven session records no flows, so a 404 can only come from the scenario"
    );

    // Every step, per protocol, as a comparable triple.
    let mut observed: Vec<Vec<(u16, String, Option<String>)>> = Vec::new();
    for protocol in [InboundProtocol::Http1, InboundProtocol::Http2Cleartext] {
        // Scenario state belongs to the fixture instance, so each protocol
        // needs its own load to start from `start`.
        let fixture = ReplayFixture::load_with_scenario(&session, Matcher::strict(8), "users")
            .expect("scenario fixture");
        let handle = fixture
            .start_with_protocol(
                "127.0.0.1:0".parse().expect("addr"),
                16 << 20,
                protocol.clone(),
                H2Limits::default(),
            )
            .await
            .expect("scenario replay server");
        let address = handle.local_addr();

        let mut steps = Vec::new();
        for path in ["/users/bob", "/users/alice", "/users/alice"] {
            let (status, headers, body) = match protocol {
                InboundProtocol::Http1 => {
                    let mut sender = h1_connect(address).await;
                    h1_collect(
                        sender
                            .send_request(h1_request(path))
                            .await
                            .expect("h1 response"),
                    )
                    .await
                }
                _ => {
                    let mut peer = h2_connect_cleartext(address).await;
                    h2_collect(peer.send_get(path).0.await.expect("h2 response")).await
                }
            };
            steps.push((
                status.as_u16(),
                String::from_utf8_lossy(&body).into_owned(),
                headers
                    .get("content-type")
                    .map(|value| value.as_bytes().escape_ascii().to_string()),
            ));
        }
        observed.push(steps);

        handle.shutdown();
        let _ = handle.wait().await;
    }

    let (http1, http2) = (&observed[0], &observed[1]);
    assert_eq!(
        http1, http2,
        "a stateful scenario with a deterministic template must behave identically over both protocols"
    );
    // 1. a non-matching path yields no transition and does not advance state;
    // 2. the matching path renders the extracted variable and advances;
    // 3. the same path no longer matches, because the state moved on.
    let no_match = ("eggreplay replay no match\n".to_string(), None);
    assert_eq!(
        http1[0],
        (404, no_match.0.clone(), no_match.1.clone()),
        "an unmatched path must not advance the scenario, and must not render a template"
    );
    assert_eq!(
        http1[1],
        (
            201,
            "created alice".to_string(),
            Some("text/plain".to_string())
        ),
        "the matching path must render the extracted variable into the body and the headers"
    );
    assert_eq!(
        http1[2].0, 404,
        "the scenario is stateful: once the transition has fired, the same path must not match again"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Target remapping
// ---------------------------------------------------------------------------

/// A candidate executes against a *different* origin than the one the flow was
/// recorded from, and the report stays clean.
///
/// The remap is the point: the recorded authority is the acquisition upstream,
/// the candidate is sent to the remap upstream, and the evidence is which
/// upstream was actually asked — an assertion on the outgoing request could
/// only restate the intent. Both upstreams echo the same body, so a clean
/// report additionally pins that request authority is not a report dimension.
#[tokio::test]
async fn target_remap_rewrites_the_authority_onto_the_candidate() {
    let recorded_origin = start_upstream("remap-baseline").await;
    let candidate_origin = start_upstream("remap-candidate").await;
    let (published, directory) = acquire_over_h2(&recorded_origin, "remap").await;
    let recorded_authority = format!("localhost:{}", recorded_origin.address.port());
    let candidate_authority = format!("localhost:{}", candidate_origin.address.port());
    assert_ne!(recorded_authority, candidate_authority);

    let flows = published_flows(&published);
    let client = eggfetch_h2(&candidate_origin.identity);
    let target: http::Uri = candidate_origin.base().parse().expect("target");

    for flow in &flows {
        assert_eq!(
            flow.request.authority, recorded_authority,
            "acquisition records the upstream it was configured to reach"
        );
        let observation = execute_candidate(
            &client,
            &flow.request,
            b"",
            &target,
            16 << 20,
            Some(direct_route()),
        )
        .await
        .expect("candidate executes against the remapped origin");
        // The remap is a *transport* concern: the observed flow keeps the
        // baseline request verbatim, so a remap can never silently rewrite
        // what a future report compares against. What the observation does
        // change is its provenance — the mode names the candidate executor
        // and the physical route names the path it took.
        assert_eq!(
            observation.flow.request, flow.request,
            "a remapped candidate must still report the baseline request, not a rewritten one"
        );
        assert_eq!(
            observation.flow.provenance.mode,
            "eggfetch-native-candidate"
        );
        assert_eq!(
            observation.flow.physical_route,
            Some(direct_route()),
            "the remap is observable as the candidate's physical route"
        );
        let baseline_body = match &flow.outcome {
            FlowOutcome::Response(response) => match &response.body {
                BodyRef::Blob(blob) => published.read_blob(blob).expect("baseline body"),
                BodyRef::Absent | BodyRef::Empty => Vec::new(),
            },
            FlowOutcome::Error(_) => Vec::new(),
        };
        let report = compare_flows(
            &without_volatile_date(flow),
            &without_volatile_date(&observation.flow),
            &baseline_body,
            &observation.response_body,
            ReportScheduler::Sequential,
        );
        assert!(
            report.is_success(),
            "a remapped authority must not be a regression dimension, got {:?}",
            report.findings
        );
    }

    assert_eq!(
        candidate_origin.served(),
        flows.len() as u64,
        "the remapped origin must be the one that was asked"
    );
    assert_eq!(
        recorded_origin.served(),
        flows.len() as u64,
        "the recorded origin is only asked during acquisition, never as the candidate"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Regression report contract
// ---------------------------------------------------------------------------

/// The regression report's machine-readable contract is stable: version,
/// key names, kind vocabulary, and finding order are all consumer-visible.
///
/// The JUnit projection is intentionally *not* exercised here. There is no
/// library-level exporter — `junit_for_reports` is private to the `eggreplay-cli`
/// binary — and reimplementing it in a test would create a second authority
/// that could drift from the first. That contract is pinned where it lives, in
/// `eggreplay-cli`'s own CLI-contract suite.
#[tokio::test]
async fn regression_report_contracts_are_stable() {
    let baseline = plain_flow(
        "flow-0000",
        canonical_request("/contract"),
        HttpResponse {
            status: 200,
            headers: vec![HeaderEntry {
                name: "x-contract".into(),
                value: "one".into(),
            }],
            ..canonical_response(200)
        },
    );
    let candidate = plain_flow(
        "flow-0000",
        canonical_request("/contract"),
        HttpResponse {
            status: 201,
            headers: vec![HeaderEntry {
                name: "x-contract".into(),
                value: "two".into(),
            }],
            ..canonical_response(201)
        },
    );
    let report = compare_flows(
        &baseline,
        &candidate,
        b"alpha",
        b"beta",
        ReportScheduler::Sequential,
    );

    assert_eq!(report.schema_version, eggreplay_core::REPORT_SCHEMA_VERSION);
    assert_eq!(report.scheduler, ReportScheduler::Sequential);
    assert_eq!(report.baseline_flow_ids, vec!["flow-0000".to_string()]);
    assert!(
        !report.is_success(),
        "a changed status/header/body must be found"
    );

    let kinds: Vec<eggreplay_core::DiffKind> = report
        .findings
        .iter()
        .map(|finding| finding.kind.clone())
        .collect();
    for expected in [
        eggreplay_core::DiffKind::Status,
        eggreplay_core::DiffKind::Header,
        eggreplay_core::DiffKind::Body,
    ] {
        assert!(
            kinds.contains(&expected),
            "a changed status/header/body must produce a {expected:?} finding, got {kinds:?}"
        );
    }
    let mut sorted = report.findings.clone();
    sorted.sort_by(|left, right| {
        (format!("{:?}", left.kind), &left.field).cmp(&(format!("{:?}", right.kind), &right.field))
    });
    assert_eq!(
        report.findings, sorted,
        "findings must already be in the stable (kind, field) order consumers splice against"
    );

    // The serialized shape is the contract a machine consumer parses, so the
    // key names are asserted exactly rather than approximately.
    let value = serde_json::to_value(&report).expect("report serializes");
    let object = value.as_object().expect("a report is a JSON object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "baseline_flow_ids",
            "findings",
            "scheduler",
            "schema_version"
        ],
        "the report's JSON keys are a published contract"
    );
    assert_eq!(object["scheduler"], serde_json::json!("sequential"));
    for finding in object["findings"].as_array().expect("findings array") {
        let finding = finding.as_object().expect("a finding is an object");
        let mut keys: Vec<&str> = finding.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["baseline", "candidate", "field", "kind"],
            "a finding's JSON keys are a published contract"
        );
    }
}

// ---------------------------------------------------------------------------
// Trailers end to end
// ---------------------------------------------------------------------------

/// Drain an H2 response including its trailer block, which `h2_collect` drops.
async fn h2_collect_with_trailers(
    response: Response<h2::RecvStream>,
) -> (
    StatusCode,
    http::HeaderMap,
    Vec<u8>,
    Option<http::HeaderMap>,
) {
    let (parts, mut stream) = response.into_parts();
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.expect("body chunk");
        let _ = stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let trailers = stream.trailers().await.expect("h2 trailers read");
    (parts.status, parts.headers, body, trailers)
}

fn trailer_fields(entries: &[HeaderEntry], name: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
        .map(|entry| entry.value.clone())
}

/// Trailers survive the whole H2 path: client -> gateway -> upstream, into the
/// fixture, and back out of an offline H2 replay.
///
/// This is the acquisition half of the claim. M015B proved the serving half —
/// a fixture with trailers emits them — which is a weaker statement: a trailer
/// the gateway never recorded could never be replayed, and the serving test
/// would still pass.
#[tokio::test]
async fn trailers_survive_h2_acquisition_and_offline_replay() {
    let upstream = start_upstream("trailers").await;
    let gateway = start_gateway(
        "trailers",
        &upstream,
        eggfetch_h2(&upstream.identity),
        direct_route(),
        InboundProtocol::Http2Cleartext,
    )
    .await;
    let mut peer = h2_connect_cleartext(gateway.address).await;

    // A request with a DATA body and a trailer block.
    let (response, mut stream) = peer.send_open("localhost", "/trailers");
    stream
        .send_data(Bytes::from_static(b"request-body"), false)
        .expect("send request data");
    let mut request_trailers = http::HeaderMap::new();
    request_trailers.insert("x-request-trailer", http::HeaderValue::from_static("sent"));
    stream
        .send_trailers(request_trailers)
        .expect("send request trailers");

    let (status, headers, body, trailers) =
        h2_collect_with_trailers(response.await.expect("response")).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"upstream:/trailers|request-body");
    assert_eq!(
        headers
            .get("x-upstream")
            .map(|value| value.as_bytes().escape_ascii().to_string()),
        Some("h2".to_string()),
        "an ordinary recorded header must survive the trailer framing"
    );
    assert_eq!(
        trailers
            .as_ref()
            .and_then(|map| map.get("x-upstream-trailer"))
            .map(|value| value.as_bytes().escape_ascii().to_string()),
        Some("done".to_string()),
        "the upstream trailer block must reach the H2 client"
    );
    assert_eq!(
        upstream.served(),
        1,
        "the gateway must forward the trailers upstream"
    );

    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 1);
    let flow = &flows[0];
    assert_eq!(
        trailer_fields(&flow.request.trailers, "x-request-trailer"),
        Some("sent".to_string()),
        "request trailers must be recorded from the H2 stream"
    );
    let FlowOutcome::Response(response) = &flow.outcome else {
        panic!("expected a recorded response, got {:?}", flow.outcome);
    };
    assert_eq!(
        trailer_fields(&response.trailers, "x-upstream-trailer"),
        Some("done".to_string()),
        "response trailers must be recorded from the H2 stream"
    );
    let recorded_body = match &response.body {
        BodyRef::Blob(blob) => published.read_blob(blob).expect("recorded body"),
        other => panic!("the DATA frame must be stored, got {other:?}"),
    };
    assert_eq!(recorded_body, b"upstream:/trailers|request-body");

    // The recorded scheme is https (the gateway's outbound leg was TLS), so a
    // faithful offline replay is served over TLS — the same rule the scheme
    // test pins.
    let identity = test_identity("trailers-replay-identity");
    let (policy, identity_directory) = tls_policy_for(&identity, "trailers-replay-pki");
    let replay = start_replay("trailers-replay", &directory, policy, Matcher::strict(8)).await;
    let mut replay_peer = h2_connect_tls(replay.address, &identity).await;
    let (response, mut stream) = replay_peer.send_open(&flow.request.authority, "/trailers");
    stream
        .send_data(Bytes::from_static(b"request-body"), false)
        .expect("send replay data");
    let mut replay_request_trailers = http::HeaderMap::new();
    replay_request_trailers.insert("x-request-trailer", http::HeaderValue::from_static("sent"));
    stream
        .send_trailers(replay_request_trailers)
        .expect("send replay trailers");
    let (status, _, body, trailers) =
        h2_collect_with_trailers(response.await.expect("replay response")).await;
    assert_eq!(status, 200, "the recorded flow must match and replay");
    assert_eq!(body, recorded_body, "the recorded body must replay exactly");
    assert_eq!(
        trailers
            .as_ref()
            .and_then(|map| map.get("x-upstream-trailer"))
            .map(|value| value.as_bytes().escape_ascii().to_string()),
        Some("done".to_string()),
        "recorded response trailers must be replayed as an H2 trailer block"
    );

    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
    let _ = std::fs::remove_dir_all(&identity_directory);
}
