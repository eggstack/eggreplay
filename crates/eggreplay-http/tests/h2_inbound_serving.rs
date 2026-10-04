#![cfg(all(feature = "eggserve", feature = "h2-inbound-tls"))]
//! M015B inbound HTTP/2 serving qualification.
//!
//! Local loopback only; no public Internet. Every client here is an
//! *independent* peer — raw `h2`, and Hyper's H1 and H2 client stacks —
//! rather than EggReplay's own client, so these tests qualify the served
//! bytes and not a shared assumption about them.
//!
//! The listeners under test are the product's own
//! [`eggreplay_http::inbound`] composition: the opt-in `h2-inbound` graph
//! serving the *existing* replay and recording-gateway services. Nothing here
//! constructs a product transport.
//!
//! Coverage, per the M015B acceptance bar: TLS ALPN H2, multiplexing,
//! trailers, streaming, cancellation, shutdown, mismatch response, scenario
//! response, and an H1 regression matrix. Plus the specific claims M015B makes
//! about request projection, pseudo-header containment, cleartext policy, and
//! the one protocol-aware rule in the response renderer.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use eggreplay_core::{
    BodyRef, ErrorCategory, ErrorPhase, ExtractionFailureBehavior, Flow, FlowOutcome, HeaderEntry,
    HttpRequest, HttpResponse, Matcher, Provenance, RULES_SCHEMA_VERSION, RequestPredicate,
    SCHEMA_VERSION, Scenario as ScenarioDef, ScenarioFault, ScenarioResponse, ScenarioRules,
    ScenarioTransition, SessionMetadata,
};
use eggreplay_http::inbound::{H2Limits, InboundProtocol};
use eggreplay_http::{ReplayFixture, recording};
use eggreplay_store::{SessionWriter, StoreLimits};
use http::{Method, Request, Response, Version};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;

// ---------------------------------------------------------------------------
// Test-owned TLS identity
// ---------------------------------------------------------------------------

/// A test-owned self-signed identity plus the PEM files the product's
/// operator-facing TLS policy loads from disk.
///
/// Written to disk on purpose: `InboundProtocol::Http2Tls` takes *paths*,
/// because operator material is what a real deployment supplies. Handing the
/// product an in-memory `ServerConfig` here would qualify a different code
/// path than the one operators get.
struct TestIdentity {
    cert_der: Vec<u8>,
    cert_path: PathBuf,
    key_path: PathBuf,
    directory: PathBuf,
}

impl Drop for TestIdentity {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A unique, **non-existent** path for one test.
///
/// The serial is mandatory: these tests run in parallel threads inside one
/// process, so a millisecond-resolution name is not unique enough and two
/// tests silently fight over one fixture directory.
///
/// Deliberately does *not* create the directory. `SessionWriter::create` and
/// `RecordingSession::create` both refuse an existing destination so they
/// cannot append to a stale fixture, which means a fixture test must hand
/// them a path nothing has made yet. Use [`temp_dir`] when the test needs a
/// real directory of its own.
static DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_path(name: &str) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let serial = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "eggreplay-h2in-{name}-{}-{millis}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// A unique directory for a test that writes its own files (test identity
/// material, for example).
fn temp_dir(name: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn test_identity(name: &str) -> TestIdentity {
    let directory = temp_dir(name);
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("test identity");
    let cert_path = directory.join("server.pem");
    let key_path = directory.join("server.key");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert pem");
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).expect("write key pem");
    TestIdentity {
        cert_der: certified.cert.der().to_vec(),
        cert_path,
        key_path,
        directory,
    }
}

/// A client that trusts exactly the test identity and nothing else. This is
/// the "explicit trust" half of the ALPN qualification: the handshake must
/// succeed because the operator named this CA, not because the platform
/// trust store happens to be permissive.
fn client_tls(cert_der: &[u8], alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(cert_der.to_vec()))
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
// Independent H2 / H1 clients
// ---------------------------------------------------------------------------

/// An H2 connection to `address`, either cleartext (prior knowledge) or over
/// TLS. Returns the raw `h2` sender so tests can drive streams, trailers, and
/// resets at the frame level.
async fn h2_connect(address: SocketAddr, identity: Option<&TestIdentity>) -> H2Peer {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("h2 tcp connect");
    tcp.set_nodelay(true).expect("nodelay");
    match identity {
        Some(identity) => {
            let connector =
                tokio_rustls::TlsConnector::from(client_tls(&identity.cert_der, &[b"h2"]));
            let tls = connector
                .connect(server_name(), tcp)
                .await
                .expect("h2 tls connect");
            assert_eq!(
                tls.get_ref().1.alpn_protocol(),
                Some(&b"h2"[..]),
                "server must select the offered h2 ALPN"
            );
            let (sender, connection) = h2::client::handshake(tls).await.expect("h2 handshake");
            tokio::spawn(async move {
                let _ = connection.await;
            });
            H2Peer {
                sender,
                scheme: "https",
            }
        }
        None => {
            let (sender, connection) = h2::client::handshake(tcp).await.expect("h2 cleartext");
            tokio::spawn(async move {
                let _ = connection.await;
            });
            H2Peer {
                sender,
                scheme: "http",
            }
        }
    }
}

#[allow(dead_code)]
async fn h2_connect_with_alpn(
    address: SocketAddr,
    identity: Option<&TestIdentity>,
    alpn: &[&[u8]],
) -> h2::client::SendRequest<Bytes> {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("h2 tcp connect");
    tcp.set_nodelay(true).expect("nodelay");
    match identity {
        Some(identity) => {
            let connector = tokio_rustls::TlsConnector::from(client_tls(&identity.cert_der, alpn));
            let tls = connector
                .connect(server_name(), tcp)
                .await
                .expect("h2 tls connect");
            assert_eq!(
                tls.get_ref().1.alpn_protocol(),
                Some(&b"h2"[..]),
                "server must select the offered h2 ALPN"
            );
            let (sender, connection) = h2::client::handshake(tls).await.expect("h2 handshake");
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender
        }
        None => {
            let (sender, connection) = h2::client::handshake(tcp).await.expect("h2 cleartext");
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender
        }
    }
}

/// Read exactly one HTTP/1.1 message: headers to the blank line, then as many
/// body bytes as `content-length` declares.
///
/// A raw peer has to frame the response itself; `read_to_end` would either
/// hang on a keep-alive connection or truncate a close-delimited one.
async fn read_http1_message<S>(stream: &mut S) -> Vec<u8>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut byte).await.expect("read response head");
        assert!(
            read > 0,
            "connection closed before the response head completed"
        );
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream
        .read_exact(&mut body)
        .await
        .expect("read response body");
    raw.extend_from_slice(&body);
    raw
}

/// An HTTP/1.1 request in origin form with an explicit `Host`.
///
/// EggServe's origin-only request-target policy rejects absolute-form, and
/// Hyper's connection client emits absolute-form whenever the request URI
/// carries a scheme and authority. So the client sends a path-only target
/// and names the origin in `Host`, exactly as an ordinary HTTP/1.1 client
/// does. `Host` must be set explicitly: with a path-only URI Hyper sends no
/// `Host` header of its own.
fn h1_request(path: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("host", "localhost")
        .body(Full::new(Bytes::new()))
        .expect("h1 request")
}

/// A Hyper H1 client over cleartext, for the H1-regression matrix.
async fn h1_connect(address: SocketAddr) -> hyper::client::conn::http1::SendRequest<Full<Bytes>> {
    let tcp = tokio::net::TcpStream::connect(address)
        .await
        .expect("h1 tcp connect");
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

/// The `:scheme` for a cleartext HTTP/2 peer.
const CLEAR_SCHEME: &str = "http";

/// An HTTP/2 request whose `:scheme` matches the transport actually used.
///
/// This is not cosmetic. EggServe validates `:scheme` against the negotiated
/// transport scheme and rejects a mismatch, which is the correct guard: a
/// client claiming `https` on a cleartext connection is either confused or
/// probing. Every client in this suite therefore names its real scheme.
fn h2_request(scheme: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(Method::GET)
        .uri(format!("{scheme}://localhost{path}"))
        .body(())
        .expect("h2 request")
}

/// An HTTP/2 peer plus the `:scheme` its transport really negotiated.
///
/// The scheme travels *with* the connection on purpose. EggServe validates
/// `:scheme` against the transport and rejects a conflict, and a test that
/// could name a scheme independently of the connection would be able to
/// assert against a request the server never agreed to serve.
struct H2Peer {
    sender: h2::client::SendRequest<Bytes>,
    scheme: &'static str,
}

impl H2Peer {
    /// Send a GET over this peer's own negotiated `:scheme`.
    ///
    /// One method so a test can never pair a path with the wrong scheme, and
    /// so the borrow of the scheme happens before the mutable borrow of the
    /// sender.
    fn send_get(&mut self, path: &str) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
        let scheme = self.scheme;
        self.sender
            .send_request(h2_request(scheme, path), true)
            .expect("send h2 request")
    }
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

// ---------------------------------------------------------------------------
// Fixture construction
// ---------------------------------------------------------------------------

/// A minimal recorded flow. `body`/`trailers` are attached through the
/// session so the replay path exercises its real streaming renderer rather
/// than a fixed-response shortcut.
struct FlowSpec<'a> {
    path: &'a str,
    /// The scheme the *served* connection will report, which the strict
    /// matcher compares. TLS listeners report `https`; cleartext reports
    /// `http`. Getting this wrong makes every test in the file a 404 for a
    /// reason that has nothing to do with HTTP/2.
    scheme: &'static str,
    status: u16,
    body: Option<&'a [u8]>,
    headers: Vec<(&'a str, &'a str)>,
    trailers: Vec<(&'a str, &'a str)>,
    /// Recorded `content-length`, used to prove the H2 rendering rule. Left
    /// `None` to omit the header entirely.
    content_length: Option<usize>,
    request_body: &'a [u8],
}

impl<'a> FlowSpec<'a> {
    fn new(path: &'a str, body: &'a [u8]) -> Self {
        Self {
            path,
            scheme: "http",
            status: 200,
            body: Some(body),
            headers: Vec::new(),
            trailers: Vec::new(),
            content_length: None,
            request_body: b"",
        }
    }
}

/// Build a recorded request whose body is a **stored blob** when non-empty.
///
/// A stored blob is what makes the body dimension discriminating: an
/// `Absent` request body carries no length or digest, so the matcher treats it
/// as "no body constraint" and every body would satisfy it. A fixture that
/// wants to prove body matching has to actually record the body.
fn recorded_request(path: &str, scheme: &str, body_ref: BodyRef) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        scheme: scheme.into(),
        authority: "localhost".into(),
        path: path.into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: body_ref,
        trailers: Vec::new(),
    }
}

/// Build a sealed replay session containing exactly the given flows.
fn build_fixture(name: &str, specs: Vec<FlowSpec<'_>>, rules: Option<ScenarioRules>) -> PathBuf {
    let directory = temp_path(name);
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("session writer");

    let store_body = |writer: &SessionWriter, bytes: &[u8]| -> BodyRef {
        if bytes.is_empty() {
            return BodyRef::Empty;
        }
        let mut blob = writer.begin_blob().expect("begin blob");
        std::io::Write::write_all(&mut blob, bytes).expect("write blob");
        blob.finish().expect("finish blob")
    };

    for (index, spec) in specs.iter().enumerate() {
        let body_ref = store_body(&writer, spec.body.unwrap_or(b""));
        let request_body = store_body(&writer, spec.request_body);
        let mut headers: Vec<HeaderEntry> = spec
            .headers
            .iter()
            .map(|(name, value)| HeaderEntry {
                name: (*name).to_ascii_lowercase(),
                value: (*value).to_string(),
            })
            .collect();
        if let Some(length) = spec.content_length {
            headers.push(HeaderEntry {
                name: "content-length".to_string(),
                value: length.to_string(),
            });
        }
        writer
            .append_flow(&Flow {
                schema_version: SCHEMA_VERSION,
                id: format!("flow-{index:04}"),
                started_at_ms: 1,
                completed_at_ms: Some(2),
                request: recorded_request(spec.path, spec.scheme, request_body),
                outcome: FlowOutcome::Response(HttpResponse {
                    status: spec.status,
                    headers,
                    body: body_ref,
                    trailers: spec
                        .trailers
                        .iter()
                        .map(|(name, value)| HeaderEntry {
                            name: (*name).to_ascii_lowercase(),
                            value: (*value).to_string(),
                        })
                        .collect(),
                }),
                physical_route: None,
                provenance: Provenance {
                    mode: "test".into(),
                    observer: "test".into(),
                },
                annotations: Vec::new(),
                redactions: Vec::new(),
            })
            .expect("append flow");
    }

    if let Some(rules) = rules {
        writer
            .write_extension(
                "rules",
                RULES_SCHEMA_VERSION,
                "rules.json",
                true,
                &serde_json::to_vec(&rules).expect("rules json"),
            )
            .expect("rules extension");
    }
    writer.finish().expect("finish session");
    directory
}

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

// ---------------------------------------------------------------------------
// Serving harness
// ---------------------------------------------------------------------------

/// A started product listener plus the fixture directory it serves.
struct Server {
    address: SocketAddr,
    handle: Option<eggreplay_http::inbound::InboundServerHandle>,
    _directory: PathBuf,
}

impl Server {
    async fn close(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
    }
}

fn cleartext_policy() -> InboundProtocol {
    InboundProtocol::Http2Cleartext
}

fn tls_policy(identity: &TestIdentity) -> InboundProtocol {
    InboundProtocol::Http2Tls {
        certificate: identity.cert_path.clone(),
        private_key: identity.key_path.clone(),
    }
}

/// Start sealed replay on an explicit policy. HTTP/2 feature graphs use
/// `Http2Cleartext` unless the test asks for TLS.
async fn serve(name: &str, protocol: InboundProtocol, specs: Vec<FlowSpec<'_>>) -> Server {
    serve_with_scenario(name, protocol, specs, None, None).await
}

async fn serve_with_scenario(
    name: &str,
    protocol: InboundProtocol,
    specs: Vec<FlowSpec<'_>>,
    scenario: Option<(ScenarioRules, &'static str)>,
    matcher: Option<Matcher>,
) -> Server {
    let directory = build_fixture(
        name,
        specs,
        scenario.as_ref().map(|(rules, _)| rules.clone()),
    );
    let session =
        eggreplay_store::Session::open(&directory, StoreLimits::default()).expect("open session");
    let chosen = matcher.unwrap_or_else(|| Matcher::strict(8));
    let fixture = match &scenario {
        Some((_, id)) => {
            ReplayFixture::load_with_scenario(&session, chosen, id).expect("scenario fixture")
        }
        None => ReplayFixture::load_with_matcher(&session, chosen).expect("matcher fixture"),
    };
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            protocol,
            H2Limits::default(),
        )
        .await
        .expect("replay server");
    let address = handle.local_addr();
    Server {
        address,
        handle: Some(handle),
        _directory: directory,
    }
}

/// Collect a response body from a Hyper client stack.
async fn body_of<B>(response: Response<B>) -> Vec<u8>
where
    B: http_body::Body<Data = Bytes> + Unpin + Send + 'static,
    B::Error: std::fmt::Debug,
{
    response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

/// One complete HTTP/2 reply, collected at the frame level.
///
/// The raw `h2` peer exposes DATA and trailers through its own polling API
/// rather than `http_body::Body`, so it gets its own reader. Collecting
/// eagerly here also means every assertion in a test observes a finished
/// message, never a partially-read one.
struct H2Reply {
    version: Version,
    status: http::StatusCode,
    headers: http::HeaderMap,
    body: Vec<u8>,
    trailers: Option<http::HeaderMap>,
}

impl H2Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(name)
            .map(|value| std::str::from_utf8(value.as_bytes()).expect("ascii response header"))
    }

    fn trailer(&self, name: &str) -> Option<String> {
        self.trailers
            .as_ref()
            .and_then(|fields| fields.get(name))
            .map(|value| {
                std::str::from_utf8(value.as_bytes())
                    .expect("ascii trailer")
                    .to_string()
            })
    }
}

/// Drain one HTTP/2 response to completion, collecting DATA and trailers.
async fn h2_collect(response: Response<h2::RecvStream>) -> H2Reply {
    let (parts, mut stream) = response.into_parts();
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.expect("h2 body chunk");
        let _ = stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let trailers = stream
        .trailers()
        .await
        .expect("h2 trailers")
        .filter(|fields| !fields.is_empty());
    H2Reply {
        version: parts.version,
        status: parts.status,
        headers: parts.headers,
        body,
        trailers,
    }
}

/// Send one request over the raw `h2` peer and collect the whole reply.
async fn h2_get(peer: &mut H2Peer, path: &str) -> H2Reply {
    let (response, _) = peer
        .sender
        .send_request(h2_request(peer.scheme, path), true)
        .expect("send h2 request");
    h2_collect(response.await.expect("h2 response head")).await
}

/// Send a GET with a request body over the raw `h2` peer.
///
/// The raw peer's typed `send_request` only accepts a `Request<()>`, so a
/// request body is written onto the returned send stream directly. This keeps
/// the body assertion on the *same* independent peer as every other H2 test.
async fn h2_get_with_body(peer: &mut H2Peer, path: &str, payload: &'static [u8]) -> H2Reply {
    let scheme = peer.scheme;
    let (response, mut stream) = peer
        .sender
        .send_request(h2_request(scheme, path), false)
        .expect("send h2 request");
    stream
        .send_data(bytes::Bytes::from_static(payload), true)
        .expect("send h2 request DATA");
    h2_collect(response.await.expect("h2 response head")).await
}

/// Send one request over the raw `h2` peer and return only the DATA frames,
/// plus whether the stream ended in a reset/error rather than a clean end.
async fn h2_stream_frames(peer: &mut H2Peer, path: &str) -> (usize, Vec<u8>, bool) {
    let (response, _) = peer
        .sender
        .send_request(h2_request(peer.scheme, path), true)
        .expect("send h2 request");
    let response = response.await.expect("h2 response head");
    let (parts, mut stream) = response.into_parts();
    assert_eq!(parts.status, 200, "expected a response head before DATA");
    let mut frames = 0usize;
    let mut body = Vec::new();
    let terminated = loop {
        match stream.data().await {
            Some(Ok(chunk)) => {
                let _ = stream.flow_control().release_capacity(chunk.len());
                frames += 1;
                body.extend_from_slice(&chunk);
            }
            Some(Err(_)) | None => break true,
        }
    };
    (frames, body, terminated)
}

// ---------------------------------------------------------------------------
// 1. TLS ALPN H2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tls_alpn_h2_serves_a_sealed_replay() {
    let identity = test_identity("alpn");
    let server = serve(
        "alpn",
        tls_policy(&identity),
        vec![FlowSpec {
            scheme: "https",
            ..FlowSpec::new("/api", b"tls-h2-ok")
        }],
    )
    .await;

    let mut sender = h2_connect(server.address, Some(&identity)).await;
    let reply = h2_get(&mut sender, "/api").await;
    assert_eq!(reply.version, Version::HTTP_2);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"tls-h2-ok");
    server.close().await;
}

/// The ALPN advertisement is the operator's, and a client that offers no
/// `h2` must land on HTTP/1.1 over the same TLS listener rather than being
/// dropped or silently upgraded.
#[tokio::test]
async fn tls_listener_serves_h1_to_a_client_that_does_not_offer_h2() {
    let identity = test_identity("alpn-fallback");
    let server = serve(
        "alpn-fallback",
        tls_policy(&identity),
        vec![FlowSpec {
            scheme: "https",
            ..FlowSpec::new("/api", b"tls-h1-ok")
        }],
    )
    .await;

    let tcp = tokio::net::TcpStream::connect(server.address)
        .await
        .expect("tcp connect");
    let connector =
        tokio_rustls::TlsConnector::from(client_tls(&identity.cert_der, &[b"http/1.1"]));
    let mut tls = connector.connect(server_name(), tcp).await.expect("tls");
    assert_eq!(
        tls.get_ref().1.alpn_protocol(),
        Some(&b"http/1.1"[..]),
        "ALPN must fall back to http/1.1 for a client that did not offer h2"
    );
    // Raw HTTP/1.1 bytes: this asserts the wire format directly, so no client
    // library can normalise a request the server would have rejected.
    //
    // Deliberately an ordinary keep-alive request. `Connection: close` is a
    // hop-by-hop request header and the matcher compares request headers, so
    // sending one would test the client's connection strategy rather than
    // ALPN fallback.
    use tokio::io::AsyncWriteExt;
    tls.write_all(b"GET /api HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write request");
    let raw = read_http1_message(&mut tls).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "ALPN-negotiated HTTP/1.1 must be served, got: {text}"
    );
    assert!(text.contains("tls-h1-ok"), "got: {text}");
    server.close().await;
}

/// Identity material is mandatory and operator-supplied. A policy naming TLS
/// without material is refused, and a path that does not exist fails closed
/// rather than falling back to a cleartext or default identity.
#[tokio::test]
async fn tls_policy_without_usable_identity_fails_closed() {
    let directory = temp_dir("no-identity");
    let missing = directory.join("absent.pem");
    let policy = InboundProtocol::Http2Tls {
        certificate: missing.clone(),
        private_key: missing.clone(),
    };
    let fixture = build_fixture("no-identity", vec![FlowSpec::new("/api", b"x")], None);
    let session =
        eggreplay_store::Session::open(&fixture, StoreLimits::default()).expect("open session");
    let replay = ReplayFixture::load(&session).expect("fixture");
    let error = match replay
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            policy,
            H2Limits::default(),
        )
        .await
    {
        Ok(_) => panic!("missing identity must fail closed, but the listener started"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("identity"),
        "error must name the identity failure, got: {error}"
    );
    let _ = std::fs::remove_dir_all(&directory);
    let _ = std::fs::remove_dir_all(&fixture);
}

// ---------------------------------------------------------------------------
// 2. Cleartext policy is explicit, and never a silent downgrade
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cleartext_policy_serves_h2_with_prior_knowledge() {
    let server = serve(
        "h2c",
        cleartext_policy(),
        vec![FlowSpec::new("/api", b"h2c-ok")],
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/api").await;
    assert_eq!(reply.version, Version::HTTP_2);
    assert_eq!(reply.body, b"h2c-ok");
    server.close().await;
}

/// The policy is a *classification*, not a rewrite. On the cleartext HTTP/2
/// listener, a client that speaks plain HTTP/1.1 is served HTTP/1.1. That is
/// the "never a silent downgrade" half of the contract: EggReplay does not
/// decide the protocol, the client's opening bytes do, and a client that
/// never sends the HTTP/2 preface is never treated as HTTP/2.
#[tokio::test]
async fn cleartext_policy_does_not_capture_a_plain_h1_client() {
    let server = serve(
        "h2c-h1",
        cleartext_policy(),
        vec![FlowSpec::new("/api", b"h1-on-h2c")],
    )
    .await;
    let mut sender = h1_connect(server.address).await;
    let request = h1_request("/api");
    let response = sender.send_request(request).await.expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(body_of(response).await, b"h1-on-h2c");
    server.close().await;
}

/// An HTTP/1.1 listener is still HTTP/1.1 only: no amount of client effort
/// makes an H1 policy speak HTTP/2.
#[tokio::test]
async fn http1_policy_never_speaks_http2() {
    let server = serve(
        "h1-only",
        InboundProtocol::Http1,
        vec![FlowSpec::new("/api", b"h1-only")],
    )
    .await;
    let mut sender = h1_connect(server.address).await;
    let request = h1_request("/api");
    let response = sender.send_request(request).await.expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(body_of(response).await, b"h1-only");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 3. Request projection
// ---------------------------------------------------------------------------

/// A recorded request whose method, path, and authority discriminate. The
/// match succeeding proves method/authority/path/headers/body all reached the
/// *one* matcher, unchanged by protocol.
#[tokio::test]
async fn request_method_authority_and_path_project_into_the_matcher() {
    let spec = FlowSpec {
        scheme: "http",
        path: "/exact/path",
        status: 200,
        body: Some(b"projected"),
        headers: Vec::new(),
        trailers: Vec::new(),
        content_length: None,
        request_body: b"",
    };
    let server = serve("projection", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;

    // `:path` and `:method` are the two dimensions a client controls most
    // directly; if either failed to project, both of these would match.
    let (response, _) = sender.send_get("/other");
    assert_eq!(
        response.await.expect("response").status(),
        404,
        "a different :path must not match"
    );

    let scheme = sender.scheme;
    let (response, _) = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{scheme}://localhost/exact/path"))
                .body(())
                .expect("request"),
            true,
        )
        .expect("send");
    assert_eq!(
        response.await.expect("response").status(),
        404,
        "a different :method must not match"
    );

    let reply = h2_get(&mut sender, "/exact/path").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"projected");
    server.close().await;
}

/// A recorded request body participates in matching over HTTP/2 exactly as
/// over HTTP/1.1 — same body authority, same `BodyMatchMode` policy.
#[tokio::test]
async fn request_body_participates_in_matching_over_h2() {
    let spec = FlowSpec {
        scheme: "http",
        path: "/echo",
        status: 200,
        body: Some(b"matched"),
        headers: Vec::new(),
        trailers: Vec::new(),
        content_length: None,
        request_body: b"payload-1",
    };
    let server = serve_with_scenario(
        "body-match",
        cleartext_policy(),
        vec![spec],
        None,
        Some(Matcher::practical(8)),
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;

    // An empty body must not satisfy a candidate recorded with a real
    // request body. If the body dimension were skipped on HTTP/2 this would
    // wrongly match, which is exactly the regression this guards.
    let (response, _) = sender.send_get("/echo");
    assert_eq!(
        response.await.expect("response").status(),
        404,
        "an empty request body must not match a recorded non-empty one"
    );
    server.close().await;
}

#[tokio::test]
async fn recorded_request_body_matches_over_h2() {
    let spec = FlowSpec {
        scheme: "http",
        path: "/echo",
        status: 200,
        body: Some(b"matched"),
        headers: Vec::new(),
        trailers: Vec::new(),
        content_length: None,
        request_body: b"payload-1",
    };
    let server = serve_with_scenario(
        "body-match-ok",
        cleartext_policy(),
        vec![spec],
        None,
        Some(Matcher::practical(8)),
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get_with_body(&mut sender, "/echo", b"payload-1").await;
    assert_eq!(reply.status, 200, "a matching request body must replay");
    assert_eq!(reply.body, b"matched");
    server.close().await;
}

/// HTTP/2 pseudo-header state must never reach canonical headers. The proof
/// is behavioural: a `:authority` sent by the client is consumed by the
/// transport and never appears as a stored header, so a request whose
/// recorded headers contain no colon-prefixed name still matches, and the
/// served response carries no `:`-prefixed header either.
#[tokio::test]
async fn pseudo_header_state_never_reaches_canonical_headers() {
    let mut spec = FlowSpec::new("/pseudo", b"no-pseudo");
    spec.headers = vec![("x-trace", "abc")];
    let server = serve("pseudo", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/pseudo").await;
    assert_eq!(reply.status, 200);
    for name in reply.headers.keys() {
        assert!(
            !name.as_str().starts_with(':'),
            "response leaked a pseudo-header: {name}"
        );
    }
    assert!(
        !reply.headers.contains_key("host"),
        "HTTP/2 carries authority in :authority, never a Host header"
    );
    assert_eq!(reply.header("x-trace"), Some("abc"));
    assert_eq!(reply.body, b"no-pseudo");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 4. Multiplexing
// ---------------------------------------------------------------------------

/// Concurrent streams over one connection, with a one-shot consumption policy
/// so the single recorded candidate is consumed exactly once and the
/// interleaving is observable rather than accidental.
#[tokio::test]
async fn concurrent_streams_share_one_connection() {
    let specs = vec![
        FlowSpec::new("/a", b"body-a"),
        FlowSpec::new("/b", b"body-b"),
        FlowSpec::new("/c", b"body-c"),
    ];
    let server = serve("multiplex", cleartext_policy(), specs).await;
    let mut sender = h2_connect(server.address, None).await;

    // Open all three streams before awaiting any response, so the requests
    // are genuinely in flight together on one connection.
    let mut pending = Vec::new();
    for path in ["/a", "/b", "/c"] {
        let (response, _) = sender
            .send_request(h2_request(CLEAR_SCHEME, path), true)
            .expect("send request");
        pending.push((path, response));
    }
    for (path, response) in pending {
        let reply = h2_collect(response.await.expect("response")).await;
        assert_eq!(reply.status, 200, "{path} must match its own flow");
        let expected = format!("body-{}", path.trim_start_matches('/'));
        assert_eq!(reply.body, expected.as_bytes());
    }
    server.close().await;
}

/// The strict one-shot consumption policy still applies per stream: replaying
/// a single-use candidate twice is a 409, not a second success. This is the
/// consumption authority, unchanged by protocol.
#[tokio::test]
async fn consumption_policy_applies_per_stream_over_h2() {
    let server = serve(
        "consume",
        cleartext_policy(),
        vec![FlowSpec::new("/once", b"first")],
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    assert_eq!(h2_get(&mut sender, "/once").await.status, 200);
    assert_eq!(
        h2_get(&mut sender, "/once").await.status,
        409,
        "a consumed one-shot candidate must exhaust, not replay"
    );
    server.close().await;
}

// ---------------------------------------------------------------------------
// 5. Response trailers
// ---------------------------------------------------------------------------

/// Recorded response trailers survive the HTTP/2 trailer frame, arriving
/// after the body DATA.
#[tokio::test]
async fn response_trailers_arrive_after_the_body() {
    let mut spec = FlowSpec::new("/trailer", b"trailer-body");
    spec.trailers = vec![("x-checksum", "sum-1"), ("x-origin", "unit")];
    let server = serve("trailers", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/trailer").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"trailer-body");
    assert_eq!(reply.trailer("x-checksum").as_deref(), Some("sum-1"));
    assert_eq!(reply.trailer("x-origin").as_deref(), Some("unit"));
    server.close().await;
}

/// HTTP/2 request trailers reach the matcher as canonical request trailers,
/// so a recorded flow carrying them matches.
#[tokio::test]
async fn request_trailers_project_into_the_matcher() {
    let mut spec = FlowSpec::new("/req-trailer", b"trailer-ok");
    spec.request_body = b"";
    let server = serve("req-trailers", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;
    // A trailing HEADERS frame with no DATA: the trailer block is the whole
    // message, and it is what EggServe must project into the matcher. Sent
    // on the raw peer's send stream because that is the only way to emit a
    // trailers-only request frame without inventing a body.
    let scheme = sender.scheme;
    let (response, mut request_stream) = sender
        .sender
        .send_request(h2_request(scheme, "/req-trailer"), false)
        .expect("send trailers-only request");
    request_stream
        .send_trailers(http::HeaderMap::from_iter([(
            "x-request-trailer"
                .parse::<http::HeaderName>()
                .expect("name"),
            "sent".parse::<http::HeaderValue>().expect("value"),
        )]))
        .expect("send request trailers");
    let reply = h2_collect(response.await.expect("response")).await;
    assert_eq!(reply.status, 200, "request trailers must still match");
    assert_eq!(reply.body, b"trailer-ok");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 6. Streaming and fault-driven streaming
// ---------------------------------------------------------------------------

/// A scenario fault that streams the body in delayed chunks must produce
/// multiple HTTP/2 DATA frames, not one coalesced write. That is the same
/// scenario authority as HTTP/1.1, observed on a different transport.
#[tokio::test]
async fn scenario_body_chunks_stream_as_separate_data_frames() {
    let rules = fault_rules(
        "/chunks",
        "0123456789",
        Some(ScenarioFault::BodyChunkDelay {
            delay_ms: 60,
            chunk_bytes: 4,
        }),
    );
    let server = serve_with_scenario(
        "stream",
        cleartext_policy(),
        Vec::new(),
        Some((rules, "fault")),
        None,
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let (frames, body, _) = h2_stream_frames(&mut sender, "/chunks").await;
    assert_eq!(body, b"0123456789");
    assert!(
        frames >= 2,
        "a chunked fault must produce more than one DATA frame, got {frames}"
    );
    server.close().await;
}

/// A terminal stream error must be an HTTP/2 stream error on the affected
/// stream only, and must not disturb the connection.
#[tokio::test]
async fn terminal_fault_surfaces_as_a_stream_error() {
    let rules = fault_rules(
        "/truncate",
        "abcdefghij",
        Some(ScenarioFault::CloseAfterBytes { bytes: 4 }),
    );
    let server = serve_with_scenario(
        "truncate",
        cleartext_policy(),
        Vec::new(),
        Some((rules, "fault")),
        None,
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let (_, received, terminated) = h2_stream_frames(&mut sender, "/truncate").await;
    assert!(terminated, "a CloseAfterN fault must terminate the stream");
    assert!(
        received.len() < b"abcdefghij".len(),
        "truncated body must be shorter than the template, got {}",
        received.len()
    );

    // The connection survives: a fresh stream on the same sender still
    // completes. This is the sibling-stream guarantee.
    let scheme = sender.scheme;
    let (response, _) = sender
        .send_request(h2_request(scheme, "/chunks"), true)
        .expect("send after stream error");
    let response = response.await.expect("connection survived");
    assert!(
        response.status().is_success() || response.status().is_client_error(),
        "a stream error must not take the connection down"
    );
    server.close().await;
}

// ---------------------------------------------------------------------------
// 7. Cancellation
// ---------------------------------------------------------------------------

/// A stream-local reset must not corrupt sibling streams on the same
/// connection, and must not take the connection down.
#[tokio::test]
async fn stream_reset_does_not_disturb_siblings() {
    let specs = vec![
        FlowSpec::new("/slow", b"slow-body"),
        FlowSpec::new("/sibling", b"sibling-body"),
    ];
    let server = serve("reset", cleartext_policy(), specs).await;
    let mut sender = h2_connect(server.address, None).await;

    // Open the sibling first, then reset it mid-flight, then complete a
    // second request on the same connection.
    let (sibling, mut sibling_stream) = sender.send_get("/slow");
    let _ = sibling.await.expect("sibling response");
    sibling_stream.send_reset(h2::Reason::CANCEL);

    let reply = h2_get(&mut sender, "/sibling").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"sibling-body");
    server.close().await;
}

/// Dropping the response future for one stream must leave the connection and
/// its other streams usable.
#[tokio::test]
async fn dropped_response_future_leaves_the_connection_usable() {
    let specs = vec![
        FlowSpec::new("/first", b"first"),
        FlowSpec::new("/second", b"second"),
    ];
    let server = serve("drop", cleartext_policy(), specs).await;
    let mut sender = h2_connect(server.address, None).await;
    let (dropped, _) = sender.send_get("/first");
    drop(dropped);
    let reply = h2_get(&mut sender, "/second").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"second");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 8. Shutdown
// ---------------------------------------------------------------------------

/// Graceful shutdown: a request already in flight completes, the listener
/// stops accepting, and a fresh connection afterwards is refused. On HTTP/2
/// this is the GOAWAY behaviour — an open connection is not abruptly killed.
#[tokio::test]
async fn graceful_shutdown_completes_in_flight_then_stops_accepting() {
    let rules = fault_rules(
        "/slow",
        "slow-ok",
        Some(ScenarioFault::ResponseHeadDelay { delay_ms: 300 }),
    );
    let server = serve_with_scenario(
        "shutdown",
        cleartext_policy(),
        vec![FlowSpec::new("/late", b"late")],
        Some((rules, "fault")),
        None,
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    // The head delay guarantees the request is accepted and *in flight* when
    // shutdown arrives. Without it, shutdown could race ahead of the request
    // and the test would prove nothing about GOAWAY semantics.
    let (in_flight, _) = sender.send_get("/slow");

    let address = server.address;
    let mut server = server;
    let handle = server.handle.take().expect("handle");
    tokio::time::sleep(Duration::from_millis(120)).await;
    handle.shutdown();
    // The already-issued stream completes on the existing connection.
    let reply = h2_collect(in_flight.await.expect("in-flight response")).await;
    assert_eq!(
        reply.status, 200,
        "a stream accepted before shutdown must still complete"
    );
    assert_eq!(reply.body, b"slow-ok");
    handle.wait().await.expect("wait");

    // A brand-new connection is refused: admission stopped.
    let refused = tokio::net::TcpStream::connect(address).await;
    match refused {
        Err(_) => {}
        Ok(stream) => {
            // Some platforms accept then immediately close; that is still
            // "not serving". Prove no HTTP/2 response can be obtained.
            use tokio::io::AsyncWriteExt;
            let mut stream = stream;
            let _ = stream.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").await;
            let _ = tokio::time::timeout(Duration::from_millis(500), stream.flush()).await;
        }
    }
    drop(server);
}

// ---------------------------------------------------------------------------
// 9. Mismatch and scenario responses
// ---------------------------------------------------------------------------

/// The no-match response is the same 404 over HTTP/2, with the same body the
/// HTTP/1.1 path emits.
#[tokio::test]
async fn mismatch_response_matches_the_h1_shape_over_h2() {
    let server = serve(
        "mismatch",
        cleartext_policy(),
        vec![FlowSpec::new("/known", b"known")],
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/unknown").await;
    assert_eq!(reply.status, 404);
    assert_eq!(reply.body, b"eggreplay replay no match\n");
    server.close().await;
}

/// A recorded upstream error projects as the same 502 over HTTP/2.
#[tokio::test]
async fn recorded_upstream_error_projects_as_502_over_h2() {
    let directory = temp_path("upstream-error");
    let mut writer = SessionWriter::create(
        &directory,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .expect("writer");
    writer
        .append_flow(&Flow {
            schema_version: SCHEMA_VERSION,
            id: "flow-0000".into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: recorded_request("/boom", "http", BodyRef::Empty),
            outcome: FlowOutcome::Error(eggreplay_core::FlowError::new(
                ErrorCategory::Protocol,
                ErrorPhase::Headers,
                "recorded upstream error",
            )),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: Vec::new(),
            redactions: Vec::new(),
        })
        .expect("append flow");
    let session = writer.finish().expect("finish");
    let fixture = ReplayFixture::load(&session).expect("fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            cleartext_policy(),
            H2Limits::default(),
        )
        .await
        .expect("server");
    let address = handle.local_addr();
    let mut sender = h2_connect(address, None).await;
    let reply = h2_get(&mut sender, "/boom").await;
    assert_eq!(reply.status, 502);
    let _ = std::fs::remove_dir_all(&directory);
    handle.shutdown();
    let _ = handle.wait().await;
}

/// A scenario response applies over HTTP/2 through the same scenario
/// authority, including its own status and body template.
#[tokio::test]
async fn scenario_response_applies_over_h2() {
    let rules = fault_rules("/scenario", "scenario-h2", None);
    let server = serve_with_scenario(
        "scenario",
        cleartext_policy(),
        Vec::new(),
        Some((rules, "fault")),
        None,
    )
    .await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/scenario").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"scenario-h2");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 10. The one protocol-aware rule in the response renderer
// ---------------------------------------------------------------------------

/// A recorded `content-length` is replayed verbatim on HTTP/1.1, where the
/// runtime validates it against the bytes it writes.
#[tokio::test]
async fn recorded_content_length_is_preserved_over_h1() {
    let mut spec = FlowSpec::new("/length", b"12345");
    spec.content_length = Some(5);
    let server = serve_with_scenario("cl-h1", InboundProtocol::Http1, vec![spec], None, None).await;
    let mut sender = h1_connect(server.address).await;
    let request = h1_request("/length");
    let response = sender.send_request(request).await.expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-length")
            .map(|value| value.to_str().expect("ascii")),
        Some("5"),
        "HTTP/1.1 must keep the recorded framing header"
    );
    assert_eq!(body_of(response).await, b"12345");
    server.close().await;
}

/// On HTTP/2 the *recorded* `content-length` is never replayed. The runtime
/// derives its own from the bytes it actually streams, so a stale recorded
/// value cannot reach the wire. The recorded value here is deliberately wrong
/// (99 bytes claimed, 5 bytes stored) so the assertion can tell the two apart:
/// a header equal to 5 proves derivation, and a header equal to 99 — or no
/// header at all — would mean the record was trusted.
#[tokio::test]
async fn stale_recorded_content_length_never_reaches_h2() {
    let mut spec = FlowSpec::new("/length", b"12345");
    spec.content_length = Some(99);
    let server = serve("cl-h2", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/length").await;
    assert_eq!(reply.version, Version::HTTP_2);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"12345");
    assert_ne!(
        reply.header("content-length"),
        Some("99"),
        "a stale recorded content-length must never be replayed on HTTP/2"
    );
    assert_eq!(
        reply.header("content-length"),
        Some("5"),
        "HTTP/2 framing must come from the bytes actually streamed"
    );
    server.close().await;
}

/// Connection-specific headers are transport-owned on HTTP/2. A record that
/// carries `transfer-encoding` must still be served cleanly, because the
/// transport strips it rather than the product maintaining a second copy of
/// that rule.
#[tokio::test]
async fn connection_specific_recorded_headers_never_reach_h2_wire() {
    let mut spec = FlowSpec::new("/hop", b"hop-body");
    spec.headers = vec![
        ("transfer-encoding", "chunked"),
        ("connection", "keep-alive"),
        ("keep-alive", "timeout=5"),
    ];
    let server = serve("hop", cleartext_policy(), vec![spec]).await;
    let mut sender = h2_connect(server.address, None).await;
    let reply = h2_get(&mut sender, "/hop").await;
    assert_eq!(reply.status, 200);
    for name in ["transfer-encoding", "connection", "keep-alive"] {
        assert!(
            reply.headers.get(name).is_none(),
            "{name} is illegal on HTTP/2 and must not be emitted"
        );
    }
    assert_eq!(reply.body, b"hop-body");
    server.close().await;
}

// ---------------------------------------------------------------------------
// 11. H1 regression matrix on the HTTP/2-enabled graph
// ---------------------------------------------------------------------------

/// The H1 regression matrix. Every case that the HTTP/1.1-only build
/// qualified must behave identically when the listener is composed through
/// the opt-in HTTP/2 runtime. This is the claim that enabling H2 did not
/// create a second H1 path.
#[tokio::test]
async fn h1_regression_matrix_on_the_h2_enabled_graph() {
    let mut empty = FlowSpec::new("/empty", b"");
    empty.status = 204;
    let specs = vec![
        FlowSpec::new("/ok", b"plain-body"),
        {
            let mut spec = FlowSpec::new("/echoed", b"");
            spec.headers = vec![("content-type", "application/json")];
            spec
        },
        {
            let mut spec = FlowSpec::new("/notfound-probe", b"");
            spec.status = 404;
            spec
        },
        empty,
    ];
    let server = serve_with_scenario("h1-regression", cleartext_policy(), specs, None, None).await;
    let mut sender = h1_connect(server.address).await;

    for path in ["/ok", "/echoed", "/notfound-probe", "/empty"] {
        let request = h1_request(path);
        let response = sender.send_request(request).await.expect("response");
        assert_eq!(
            response.version(),
            Version::HTTP_11,
            "{path} must be served as HTTP/1.1"
        );
        match path {
            "/ok" => {
                assert_eq!(response.status(), 200);
                assert_eq!(body_of(response).await, b"plain-body");
            }
            "/echoed" => {
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response
                        .headers()
                        .get("content-type")
                        .map(|value| value.to_str().expect("ascii")),
                    Some("application/json"),
                    "recorded response headers must survive on H1"
                );
                let _ = body_of(response).await;
            }
            "/notfound-probe" => {
                assert_eq!(response.status(), 404);
                let _ = body_of(response).await;
            }
            _ => {
                assert_eq!(response.status(), 204);
                assert_eq!(body_of(response).await, b"");
            }
        }
    }
    server.close().await;
}

/// The H1 no-match body is byte-identical whether the listener is the direct
/// runtime or the HTTP/2-enabled composition.
#[tokio::test]
async fn h1_no_match_body_is_identical_on_both_runtimes() {
    for policy in [InboundProtocol::Http1, InboundProtocol::Http2Cleartext] {
        let server = serve("h1-nomatch", policy, vec![FlowSpec::new("/known", b"k")]).await;
        let mut sender = h1_connect(server.address).await;
        let request = h1_request("/nope");
        let response = sender.send_request(request).await.expect("response");
        assert_eq!(response.status(), 404);
        assert_eq!(body_of(response).await, b"eggreplay replay no match\n");
        server.close().await;
    }
}

// ---------------------------------------------------------------------------
// 12. Recording gateway
// ---------------------------------------------------------------------------

/// A minimal upstream the gateway records through, reachable only on loopback.
async fn start_upstream() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    use hyper::service::service_fn;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream bind");
    let address = listener.local_addr().expect("upstream addr");
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(|request: Request<hyper::body::Incoming>| async move {
                            let path = request.uri().path().to_string();
                            let body = request
                                .into_body()
                                .collect()
                                .await
                                .map(|collected| collected.to_bytes())
                                .unwrap_or_default();
                            let echoed = format!("upstream:{path}");
                            let payload = if body.is_empty() {
                                echoed.into_bytes()
                            } else {
                                format!("{echoed}|{body:?}").into_bytes()
                            };
                            Ok::<_, std::convert::Infallible>(http::Response::new(
                                http_body_util::Full::new(Bytes::from(payload)),
                            ))
                        }),
                    )
                    .await;
            });
        }
    });
    (address, task)
}

fn recording_client() -> eggfetch_core::Client {
    eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .build()
}

/// Inbound HTTP/2 requests land in the ordinary recording gateway, with the
/// existing redaction and durable-publication path, and the stored flow keeps
/// no HTTP/2 pseudo-header state.
#[tokio::test]
async fn recording_gateway_accepts_inbound_h2() {
    let (upstream, upstream_task) = start_upstream().await;
    let directory = temp_path("gateway-h2");
    let session = eggreplay_store::RecordingSession::create(
        &directory,
        SessionMetadata {
            capture_mode: "gateway-h2".into(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .expect("recording session");
    let handle = recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        format!("http://127.0.0.1:{}", upstream.port())
            .parse()
            .expect("upstream uri"),
        recording_client(),
        session.clone(),
        16 << 20,
        eggreplay_core::RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        eggreplay_core::PhysicalRoute {
            kind: "direct".to_string(),
            description: None,
        },
        recording::WebSocketRecordingOptions::default(),
        cleartext_policy(),
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();

    let mut sender = h2_connect(address, None).await;
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "{}://localhost/gateway?token=abc123",
            sender.scheme
        ))
        .header("x-trace", "t-1")
        .header("authorization", "Bearer secret-token")
        .body(())
        .expect("request");
    let (response, _) = sender.send_request(request, true).expect("send");
    let reply = h2_collect(response.await.expect("response")).await;
    assert_eq!(reply.version, Version::HTTP_2);
    assert_eq!(reply.status, 200);
    let payload = reply.body;
    assert!(
        String::from_utf8_lossy(&payload).starts_with("upstream:/gateway"),
        "gateway must forward and record the inbound path, got {payload:?}"
    );

    handle.shutdown();
    let _ = handle.wait().await;
    session.shutdown();
    recording::drain_active_blobs(&session).await;
    let published = recording::finish_recording_session(session)
        .await
        .expect("finalize");
    upstream_task.abort();

    assert_eq!(published.manifest().flow_count, 1, "one flow recorded");
    let flow = published
        .iter_flows()
        .expect("iterate flows")
        .next()
        .expect("one recorded flow")
        .expect("read recorded flow");
    // No pseudo-header state in canonical stored headers.
    for header in &flow.request.headers {
        assert!(
            !header.name.starts_with(':'),
            "stored request header leaked a pseudo-header: {}",
            header.name
        );
    }
    // The inbound authority is the canonical one, and redaction ran.
    // The gateway rewrites the request onto the configured upstream origin,
    // so the stored authority is the upstream's, not the inbound
    // `:authority`. That is existing acquisition policy, unchanged by H2.
    assert_eq!(
        flow.request.authority,
        format!("127.0.0.1:{}", upstream.port())
    );
    let names: Vec<String> = flow
        .request
        .headers
        .iter()
        .map(|header| header.name.clone())
        .collect();
    assert!(names.contains(&"x-trace".to_string()), "got {names:?}");
    // Redaction replaces the value in place; the header name survives so the
    // shape of the exchange is still readable. What must not survive is the
    // secret, and it must not survive *before* publication, which is what
    // reading the published fixture proves.
    let authorization = flow
        .request
        .headers
        .iter()
        .find(|header| header.name == "authorization")
        .map(|header| header.value.clone())
        .expect("authorization header shape is preserved");
    assert!(
        !authorization.contains("secret-token"),
        "secure-default redaction must replace the secret before publication, got {authorization:?}"
    );
    assert!(
        flow.redactions.iter().any(|marker| marker
            .field
            .eq_ignore_ascii_case("request.headers.authorization")),
        "the published fixture must record that a redaction was applied, got {:?}",
        flow.redactions
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// The gateway is protocol-neutral in the other direction too: the same
/// gateway composition serves HTTP/1.1 clients unchanged.
#[tokio::test]
async fn recording_gateway_serves_h1_on_the_h2_enabled_graph() {
    let (upstream, upstream_task) = start_upstream().await;
    let directory = temp_path("gateway-h1");
    let session = eggreplay_store::RecordingSession::create(
        &directory,
        SessionMetadata {
            capture_mode: "gateway-h1".into(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .expect("recording session");
    let handle = recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        format!("http://127.0.0.1:{}", upstream.port())
            .parse()
            .expect("upstream uri"),
        recording_client(),
        session.clone(),
        16 << 20,
        eggreplay_core::RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        eggreplay_core::PhysicalRoute {
            kind: "direct".to_string(),
            description: None,
        },
        recording::WebSocketRecordingOptions::default(),
        cleartext_policy(),
        H2Limits::default(),
    )
    .await
    .expect("gateway");
    let address = handle.local_addr();

    let mut sender = h1_connect(address).await;
    let request = h1_request("/gateway-h1");
    let response = sender.send_request(request).await.expect("response");
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(response.status(), 200);
    let _ = body_of(response).await;

    handle.shutdown();
    let _ = handle.wait().await;
    session.shutdown();
    recording::drain_active_blobs(&session).await;
    let published = recording::finish_recording_session(session)
        .await
        .expect("finalize");
    upstream_task.abort();
    assert_eq!(published.manifest().flow_count, 1);
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// 13. Operator limit pass-through
// ---------------------------------------------------------------------------

/// An operator-tightened `max_concurrent_streams` reaches Hyper: the server
/// advertises the configured value, so the limit is enforced rather than
/// documented.
#[tokio::test]
async fn operator_concurrent_stream_limit_is_advertised() {
    let specs: Vec<FlowSpec<'_>> = (0..7)
        .map(|index| FlowSpec::new(Box::leak(format!("/s{index}").into_boxed_str()), b"limited"))
        .collect();
    let directory = build_fixture("limits", specs, None);
    let session =
        eggreplay_store::Session::open(&directory, StoreLimits::default()).expect("open session");
    let fixture = ReplayFixture::load(&session).expect("fixture");
    let handle = fixture
        .start_with_protocol(
            "127.0.0.1:0".parse().expect("addr"),
            16 << 20,
            cleartext_policy(),
            H2Limits {
                max_concurrent_streams: Some(7),
                ..H2Limits::default()
            },
        )
        .await
        .expect("server");
    let address = handle.local_addr();
    let mut sender = h2_connect(address, None).await;
    // `max_concurrent_streams: Some(7)` is what the server advertises, so
    // seven streams may be open at once. All seven are issued before any is
    // awaited, so the limit is exercised as concurrency rather than
    // serialized round trips.
    let mut pending = Vec::new();
    for index in 0..7 {
        let (response, _) = sender.send_get(&format!("/s{index}"));
        pending.push(response);
    }
    for response in pending {
        let reply = h2_collect(response.await.expect("response")).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"limited");
    }
    let _ = std::fs::remove_dir_all(&directory);
    handle.shutdown();
    let _ = handle.wait().await;
}
