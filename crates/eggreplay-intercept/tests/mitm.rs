//! M013D HTTPS MITM HTTP/1.1 recording integration proofs.
//!
//! Hermetic and local-only: the interception CA, the TLS origins, the route
//! hops, and every client are loopback constructs. No Internet access occurs.
//!
//! Client styles: a scripted `tokio-rustls` client with explicit SNI/ALPN
//! control (EggReplay-owned), plus one interop test driving the proxy with
//! an `eggfetch-core` client through `CONNECT` with a custom CA.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use eggreplay_core::SessionMetadata;
use eggreplay_intercept::{
    CaAuthority, CaOptions, ConnectAction, ExplicitProxyConfig, ExplicitProxyHandle, HostMatch,
    LeafIssuer, LeafOptions, MitmConfig, PortMatch, ProxyListenerConfig, ProxyRoute, RequestKind,
    Rule, RuleAction, TargetPolicy, normalize_host, resolve_connect_target, start_explicit_proxy,
};
use eggreplay_store::{RecordingSession, StoreLimits};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_rustls::{TlsAcceptor, TlsConnector};

const TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Origin (upstream TLS server) fixtures
// ---------------------------------------------------------------------------

/// Self-signed origin identity (never the interception CA).
struct OriginCert {
    cert_der: Vec<u8>,
    cert_pem: String,
    key_der: Vec<u8>,
}

fn make_origin_cert(sans: &[&str]) -> OriginCert {
    let generated = rcgen::generate_simple_self_signed(
        sans.iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<String>>(),
    )
    .expect("origin cert generates");
    OriginCert {
        cert_der: generated.cert.der().to_vec(),
        cert_pem: generated.cert.pem(),
        key_der: generated.key_pair.serialize_der(),
    }
}

/// One HTTP request observed by the TLS origin.
#[derive(Debug, Clone)]
struct CapturedRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    trailers: Vec<(String, String)>,
    sni: Option<String>,
}

fn header_values(captured: &CapturedRequest, name: &str) -> Vec<String> {
    captured
        .headers
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .collect()
}

/// Read one LF-terminated line with a bound.
async fn read_line(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
        line.push(byte[0]);
        if byte[0] == b'\n' || line.len() > 512 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Read one HTTP/1 request head (content-length or chunked + trailers).
async fn read_origin_request(
    stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    sni: Option<String>,
) -> std::io::Result<CapturedRequest> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let count = stream.read(&mut byte).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "origin saw EOF",
            ));
        }
        head.push(byte[0]);
        if head.len() > 65_536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
            headers.push((name, value));
        }
    }
    let mut body = Vec::new();
    let mut trailers = Vec::new();
    if chunked {
        loop {
            let line = read_line(stream).await?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk"))?;
            if size == 0 {
                loop {
                    let trailer = read_line(stream).await?;
                    if trailer.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = trailer.split_once(':') {
                        trailers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
                    }
                }
                break;
            }
            let mut chunk = vec![0u8; size];
            stream.read_exact(&mut chunk).await?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            stream.read_exact(&mut crlf).await?;
        }
    } else if content_length > 0 {
        body.resize(content_length, 0);
        stream.read_exact(&mut body).await?;
    }
    Ok(CapturedRequest {
        method,
        target,
        headers,
        body,
        trailers,
        sni,
    })
}

fn fixed_response(body: &[u8]) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\nX-Upstream: yes\r\nX-Dup-Resp: one\r\nX-Dup-Resp: two\r\n\r\n",
        body.len()
    )
    .into_bytes()
    .into_iter()
    .chain(body.iter().copied())
    .collect()
}

fn chunked_origin_response(chunks: &[&[u8]], trailers: &[(&str, &str)]) -> Vec<u8> {
    let mut out =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\nX-Upstream: yes\r\n\r\n"
            .to_vec();
    for chunk in chunks {
        out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n");
    for (name, value) in trailers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Shared origin responder: maps a captured request to raw response bytes.
type OriginResponder = Arc<dyn Fn(&CapturedRequest) -> Vec<u8> + Send + Sync>;

/// Dual-stack TLS origin: listens on 127.0.0.1 and (best-effort) `::1` with
/// the same port so `localhost` resolves usefully on any stack order.
async fn start_tls_origin(
    cert: OriginCert,
    responder: impl Fn(&CapturedRequest) -> Vec<u8> + Send + Sync + 'static,
) -> (
    u16,
    Arc<Mutex<Vec<CapturedRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_tls = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.cert_der)],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_der),
                ),
            )
            .unwrap(),
    );
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = first.local_addr().unwrap().port();
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let responder = Arc::new(responder);
    let serve = move |listener: TcpListener,
                      server_tls: Arc<rustls::ServerConfig>,
                      captured: Arc<Mutex<Vec<CapturedRequest>>>,
                      responder: OriginResponder| async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = TlsAcceptor::from(server_tls.clone());
            let captured = captured.clone();
            let responder = responder.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(socket).await else {
                    return;
                };
                let sni = tls.get_ref().1.server_name().map(str::to_owned);
                let mut tls = tls;
                if let Ok(Ok(request)) = tokio::time::timeout(
                    Duration::from_secs(5),
                    read_origin_request(&mut tls, sni.clone()),
                )
                .await
                {
                    let response = responder(&request);
                    // Stream large responses in bounded chunks like real
                    // servers do. Captured test output surfaces write health
                    // on failure.
                    let mut written = 0usize;
                    let mut write_error = None;
                    for chunk in response.chunks(32 * 1024) {
                        if let Err(error) = tls.write_all(chunk).await {
                            write_error = Some(error.to_string());
                            break;
                        }
                        written += chunk.len();
                    }
                    eprintln!(
                        "origin diag: response_len={} written={} write_error={:?} method={} target={}",
                        response.len(),
                        written,
                        write_error,
                        request.method,
                        request.target,
                    );
                    captured.lock().await.push(request);
                }
                // Close TLS cleanly after the single request and give the
                // peer time to return its close_notify before dropping TCP.
                // This mirrors a well-behaved origin's shutdown/linger path.
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    if tls.shutdown().await.is_ok() {
                        let mut byte = [0u8; 1];
                        while tls.read(&mut byte).await.unwrap_or(0) != 0 {}
                    }
                })
                .await;
            });
        }
    };
    let task_captured = captured.clone();
    let task_responder = responder.clone();
    let second_captured = captured.clone();
    let task_tls = server_tls.clone();
    let task = tokio::spawn(async move {
        let second = TcpListener::bind(format!("[::1]:{port}")).await.ok();
        let first_task = tokio::spawn(serve(
            first,
            task_tls.clone(),
            task_captured,
            task_responder,
        ));
        if let Some(second) = second {
            let second_task = tokio::spawn(serve(second, task_tls, second_captured, responder));
            let _ = tokio::join!(first_task, second_task);
        } else {
            let _ = first_task.await;
        }
    });
    (port, captured, task)
}

// ---------------------------------------------------------------------------
// MITM proxy fixtures
// ---------------------------------------------------------------------------

struct MitmFixture {
    handle: ExplicitProxyHandle,
    session: RecordingSession,
    dir: tempfile::TempDir,
    _ca_dir: tempfile::TempDir,
    ca_der: Vec<u8>,
}

fn intercept_local_policy() -> TargetPolicy {
    TargetPolicy::new(
        vec![
            Rule::new(
                HostMatch::exact_dns("localhost").unwrap(),
                PortMatch::Any,
                RequestKind::Any,
                RuleAction::Allow,
            ),
            Rule::new(
                HostMatch::exact_ip("127.0.0.1".parse().unwrap()),
                PortMatch::Any,
                RequestKind::Any,
                RuleAction::Allow,
            ),
            Rule::new(
                HostMatch::exact_ip("::1".parse().unwrap()),
                PortMatch::Any,
                RequestKind::Any,
                RuleAction::Allow,
            ),
        ],
        ConnectAction::Intercept,
    )
    .unwrap()
}

async fn start_mitm(
    policy: TargetPolicy,
    route: ProxyRoute,
    origin_trust_pem: Option<&[u8]>,
) -> MitmFixture {
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let ca_scratch = tempfile::TempDir::new().unwrap();
    let ca = CaAuthority::create_new(&ca_scratch.path().join("ca"), &CaOptions::default()).unwrap();
    let ca_der = ca.cert_der().to_vec();
    let issuer = Arc::new(LeafIssuer::new(ca, &LeafOptions::default()).unwrap());
    let mut mitm = MitmConfig::new(issuer);
    if let Some(pem) = origin_trust_pem {
        mitm.upstream_tls = Some(
            eggfetch_core::TlsConfig::builder()
                .ca_certificate_pem(pem)
                .unwrap()
                .build(),
        );
    }
    let mut config = ExplicitProxyConfig::new(session.clone(), policy, route);
    config.mitm = Some(mitm);
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    MitmFixture {
        handle,
        session,
        dir,
        _ca_dir: ca_scratch,
        ca_der,
    }
}

async fn finish_mitm(fixture: MitmFixture) {
    fixture.handle.shutdown();
    fixture.handle.wait().await;
    fixture.session.shutdown();
}

fn dir_contains(dir: &std::path::Path, needle: &str) -> bool {
    let mut stack = vec![dir.to_owned()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path)
                && String::from_utf8_lossy(&bytes).contains(needle)
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Scripted raw TLS client (SNI/ALPN control)
// ---------------------------------------------------------------------------

/// Accept-anything verifier for mismatch tests: the client handshake
/// succeeds so the *server-side* coherence failure is what the test
/// observes.
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

struct TlsClientOptions<'a> {
    sni: Option<&'a str>,
    alpn: Vec<&'a [u8]>,
    danger_no_verify: bool,
    enable_sni: bool,
}

fn client_tls_config(ca_der: &[u8], options: &TlsClientOptions<'_>) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = if options.danger_no_verify {
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(ca_der.to_vec()))
            .expect("intercept CA parses");
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    config.alpn_protocols = options
        .alpn
        .iter()
        .map(|protocol| (*protocol).to_vec())
        .collect();
    config.enable_sni = options.enable_sni;
    Arc::new(config)
}

/// Send `CONNECT` and read the proxy response head.
async fn connect_head(proxy: SocketAddr, authority: &str) -> (TcpStream, Vec<u8>) {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    stream
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let head = read_head(&mut stream).await;
    (stream, head)
}

/// Read response/request head bytes through the blank line.
async fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    tokio::time::timeout(TIMEOUT, async {
        loop {
            stream.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
            if head.len() > 8192 || head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .unwrap();
    head
}

fn status_line(head: &[u8]) -> String {
    String::from_utf8_lossy(head)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned()
}

async fn try_tls_handshake(
    stream: TcpStream,
    ca_der: &[u8],
    options: &TlsClientOptions<'_>,
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let connector = TlsConnector::from(client_tls_config(ca_der, options));
    // SNI is always a valid DNS name here; IP-literal tests disable SNI and
    // verify the IP SAN through the name below.
    let name = options.sni.unwrap_or("localhost").to_owned();
    let server_name = ServerName::try_from(name.as_str()).unwrap().to_owned();
    tokio::time::timeout(TIMEOUT, connector.connect(server_name, stream))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out"))?
}

async fn tls_handshake(
    stream: TcpStream,
    ca_der: &[u8],
    options: &TlsClientOptions<'_>,
) -> tokio_rustls::client::TlsStream<TcpStream> {
    try_tls_handshake(stream, ca_der, options)
        .await
        .expect("TLS handshake succeeds")
}

async fn read_tls_head(stream: &mut tokio_rustls::client::TlsStream<TcpStream>) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let count = stream.read(&mut byte).await.unwrap();
            if count == 0 {
                break;
            }
            head.push(byte[0]);
            if head.len() > 8192 || head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .unwrap();
    head
}

fn content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

async fn read_tls_body(
    stream: &mut tokio_rustls::client::TlsStream<TcpStream>,
    head: &[u8],
) -> Vec<u8> {
    if let Some(length) = content_length(head) {
        let mut body = vec![0u8; length];
        tokio::time::timeout(TIMEOUT, stream.read_exact(&mut body))
            .await
            .unwrap()
            .unwrap();
        body
    } else {
        let mut body = Vec::new();
        tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut body))
            .await
            .unwrap()
            .unwrap();
        body
    }
}

/// Durably recorded response facts for large-transfer diagnostics.
struct RecordedResponse {
    body_len: Option<u64>,
    status: Option<u16>,
    upstream_error: Option<String>,
}

/// Read the finalized fixture's recorded response length/status plus any
/// upstream response stream-error event (which marks a mid-body upstream
/// failure that still records a partial 200 flow).
fn read_recorded_response(fixture: &std::path::Path) -> RecordedResponse {
    let mut out = RecordedResponse {
        body_len: None,
        status: None,
        upstream_error: None,
    };
    let Ok(session) = eggreplay_store::Session::open(fixture, StoreLimits::default()) else {
        return out;
    };
    if let Ok(flows) = session.iter_flows() {
        for flow in flows.flatten() {
            if let eggreplay_core::FlowOutcome::Response(response) = flow.outcome {
                out.status = Some(response.status);
                out.body_len = match &response.body {
                    eggreplay_core::BodyRef::Blob(blob) => Some(blob.length),
                    _ => Some(0),
                };
                break;
            }
        }
    }
    if let Ok(Some(bytes)) = session.read_extension("stream-events")
        && let Ok(events) = serde_json::from_slice::<eggreplay_core::StreamEvents>(&bytes)
    {
        for flow_events in &events.flows {
            for event in &flow_events.response {
                if let eggreplay_core::StreamEventKind::Error {
                    offset,
                    category,
                    phase,
                } = &event.event
                {
                    out.upstream_error =
                        Some(format!("offset={offset} category={category} phase={phase}"));
                }
            }
        }
    }
    out
}

/// Decode a raw chunked H1 body (the MITM re-chunks streamed upstream
/// bodies on egress, mirroring the plain proxy path).
fn decode_chunked(mut bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = bytes.windows(2).position(|window| window == b"\r\n") {
        let size =
            usize::from_str_radix(std::str::from_utf8(&bytes[..end]).unwrap_or("").trim(), 16)
                .unwrap_or(0);
        if size == 0 || bytes.len() < end + 2 + size + 2 {
            break;
        }
        out.extend_from_slice(&bytes[end + 2..end + 2 + size]);
        bytes = &bytes[end + 2 + size + 2..];
    }
    out
}

/// Full HTTPS exchange over an intercepted tunnel with `Connection: close`.
async fn https_exchange(
    proxy: SocketAddr,
    ca_der: &[u8],
    authority: &str,
    options: &TlsClientOptions<'_>,
    request: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let (stream, head) = connect_head(proxy, authority).await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 200"),
        "CONNECT must yield 200; got: {}",
        status_line(&head)
    );
    let mut tls = tls_handshake(stream, ca_der, options).await;
    tls.write_all(request).await.unwrap();
    let response_head = read_tls_head(&mut tls).await;
    let body = read_tls_body(&mut tls, &response_head).await;
    (response_head, body)
}

/// Direct-route client options for IP `CONNECT` targets.
///
/// rustls never sends IP literals as SNI, so SNI stays disabled on the wire
/// while the IP SAN still verifies through the name below.
fn ip_options(sni: Option<&str>) -> TlsClientOptions<'_> {
    TlsClientOptions {
        sni,
        alpn: vec![b"http/1.1"],
        danger_no_verify: false,
        enable_sni: false,
    }
}

/// Direct-route client options for DNS `CONNECT` targets.
fn dns_options(sni: Option<&str>) -> TlsClientOptions<'_> {
    TlsClientOptions {
        sni,
        alpn: vec![b"http/1.1"],
        danger_no_verify: false,
        enable_sni: true,
    }
}

/// Routed `Eggress` path through a local `CONNECT` relay (required for DNS
/// upstream targets: direct dials reject `localhost` as rebinding-risk).
async fn start_relay_route() -> (ProxyRoute, SocketAddr, tokio::task::JoinHandle<()>) {
    let (relay, task) = start_connect_relay().await;
    let route = ProxyRoute::from_pproxy_uri(&format!("http://{relay}")).unwrap();
    (route, relay, task)
}

// ---------------------------------------------------------------------------
// Success paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_records_https_get_with_duplicates_and_query() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"mitm-ok")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!(
        "GET /path?a=1&a=2 HTTP/1.1\r\nHost: {authority}\r\nX-Dup: first\r\nX-Dup: second\r\nConnection: close\r\n\r\n"
    );
    let (head, body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 200"),
        "intercepted GET must succeed; got: {}",
        status_line(&head)
    );
    assert_eq!(body, b"mitm-ok");
    let head_text = String::from_utf8_lossy(&head).into_owned();
    assert_eq!(
        head_text
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("x-dup-resp:"))
            .count(),
        2,
        "duplicate upstream headers must reach the client: {head_text}"
    );
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, "GET");
    assert_eq!(captured[0].target, "/path?a=1&a=2");
    assert_eq!(
        header_values(&captured[0], "x-dup"),
        vec!["first".to_owned(), "second".to_owned()]
    );
    assert_eq!(captured[0].sni, None, "no SNI is sent for IP targets");
    assert_eq!(proxy.session.flow_count(), 1);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_records_post_with_chunked_body_and_response() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) = start_tls_origin(origin_cert, |request| {
        assert_eq!(request.body, b"hello world");
        chunked_origin_response(&[b"hello ", b"world"], &[("x-up-trailer", "yes")])
    })
    .await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!(
        "POST /upload?kind=chunked HTTP/1.1\r\nHost: {authority}\r\nTransfer-Encoding: chunked\r\nX-Dup: one\r\nX-Dup: two\r\nTE: trailers\r\nTrailer: x-trailer\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: done\r\n\r\n"
    );
    let (head, body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 200"),
        "intercepted POST must succeed; got: {}",
        status_line(&head)
    );
    // Streamed upstream bodies are re-chunked on MITM egress (same as the
    // plain proxy path); terminal trailers ride the recorded flow instead.
    assert_eq!(decode_chunked(&body), b"hello world");
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(
        header_values(&captured[0], "x-dup"),
        vec!["one".to_owned(), "two".to_owned()]
    );
    assert!(
        captured[0]
            .trailers
            .iter()
            .any(|(name, _)| name == "x-trailer"),
        "client trailers must reach upstream: {:?}",
        captured[0].trailers
    );
    assert_eq!(proxy.session.flow_count(), 1);
    let dir = proxy.dir.path().to_owned();
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    let flows = std::fs::read_to_string(dir.join("fixture").join("flows.jsonl")).unwrap();
    assert!(
        flows.contains("x-trailer") && flows.contains("done"),
        "recorded flow must carry request trailers: {flows}"
    );
    assert!(
        flows.contains("x-up-trailer") && flows.contains("yes"),
        "recorded flow must carry upstream trailers: {flows}"
    );
    assert!(
        flows.contains("explicit_proxy_mitm"),
        "recorded flow must carry the MITM acquisition mode: {flows}"
    );
    assert!(
        flows.contains("\"scheme\":\"https\"")
            && flows.contains(&format!("\"authority\":\"127.0.0.1:{origin_port}\""))
            && flows.contains("\"path\":\"/upload\""),
        "recorded flow must carry the canonical logical target: {flows}"
    );
    origin_task.abort();
}

#[tokio::test]
async fn mitm_streams_large_bodies_both_directions() {
    const RESPONSE_LEN: usize = 300 * 1024;
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) = start_tls_origin(origin_cert, |request| {
        assert_eq!(request.body.len(), 256 * 1024);
        fixed_response(&vec![b'z'; RESPONSE_LEN])
    })
    .await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let payload = vec![b'y'; 256 * 1024];
    // `Connection: keep-alive` (not close): the client closes explicitly after
    // the exact-length read below, which is the deterministic end of this
    // exchange on every platform.
    let head = format!(
        "POST /big HTTP/1.1\r\nHost: {authority}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        payload.len()
    );
    let (stream, connect_head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&connect_head).starts_with("HTTP/1.1 200"));
    let options = ip_options(Some("127.0.0.1"));
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    tls.write_all(head.as_bytes()).await.unwrap();
    tls.write_all(&payload).await.unwrap();
    let response_head = read_tls_head(&mut tls).await;
    let body = read_tls_body(&mut tls, &response_head).await;
    assert!(status_line(&response_head).starts_with("HTTP/1.1 200"));
    // Explicit client close ends the decrypted H1 connection deterministically
    // on every platform; proxy shutdown then has nothing to race.
    tls.shutdown().await.ok();
    drop(tls);
    assert_eq!(captured.lock().await.len(), 1);
    assert_eq!(proxy.session.flow_count(), 1);
    let dir = proxy.dir.path().to_owned();
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    origin_task.abort();
    // Compare the wire body against the durably recorded body and surface
    // any upstream stream-error event if the two paths disagree.
    let recorded = read_recorded_response(&dir.join("fixture"));
    eprintln!(
        "large-body diag: wire_len={} recorded_len={:?} recorded_status={:?} upstream_error={:?} wire_head={:?}",
        body.len(),
        recorded.body_len,
        recorded.status,
        recorded.upstream_error,
        String::from_utf8_lossy(&response_head)
    );
    assert_eq!(body.len(), RESPONSE_LEN, "wire body must be complete");
    assert!(body.iter().all(|byte| *byte == b'z'));
    assert_eq!(
        recorded.body_len,
        Some(RESPONSE_LEN as u64),
        "recorded flow body must be complete"
    );
    assert!(
        recorded.upstream_error.is_none(),
        "upstream must end cleanly: {:?}",
        recorded.upstream_error
    );
}

/// Leg isolation for large TLS downloads: `EggFetch` straight at the test
/// origin with no proxy/recording in between.
#[tokio::test]
async fn eggfetch_direct_downloads_full_large_tls_response() {
    const DIRECT_LEN: usize = 300 * 1024;
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(&vec![b'z'; DIRECT_LEN])).await;
    // Two dialers pin the defect site: the default TCP stack vs the
    // `Eggress` outbound connector our proxy always routes through. Both
    // must deliver the full body; a split verdict names the faulty layer.
    for (label, dialer) in [
        ("plain-tcp", None),
        (
            "eggress-direct",
            Some(eggreplay_http::EggressDialer::direct()),
        ),
    ] {
        let mut builder = eggfetch_core::Client::builder().retry_canceled_requests(false);
        if let Some(dialer) = dialer {
            builder = builder.dialer(dialer);
        }
        let probe = builder
            .tls_config(
                eggfetch_core::TlsConfig::builder()
                    .ca_certificate_pem(origin_pem.as_bytes())
                    .unwrap()
                    .build(),
            )
            .build();
        let mut response = probe
            .get(&format!("https://127.0.0.1:{origin_port}/big"))
            .unwrap()
            .send()
            .await
            .unwrap_or_else(|error| panic!("direct download ({label}) sends: {error}"));
        eprintln!("direct diag [{label}]: status={}", response.status());
        assert_eq!(response.status(), http::StatusCode::OK);
        let text = response
            .text()
            .await
            .unwrap_or_else(|error| panic!("direct download ({label}) reads: {error}"));
        eprintln!("direct diag [{label}]: downloaded_len={}", text.len());
        assert_eq!(
            text.len(),
            DIRECT_LEN,
            "direct EggFetch download ({label}) must be complete"
        );
        assert!(text.bytes().all(|byte| byte == b'z'));
    }
    origin_task.abort();
}

#[tokio::test]
async fn mitm_serves_concurrent_intercepted_clients() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"parallel")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let proxy_addr = proxy.handle.local_addr();
    let ca_der = proxy.ca_der.clone();
    let mut tasks = Vec::new();
    for index in 0..8 {
        let ca_der = ca_der.clone();
        tasks.push(tokio::spawn(async move {
            let authority = format!("127.0.0.1:{origin_port}");
            let request = format!(
                "GET /slot-{index} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
            );
            let (head, body) = https_exchange(
                proxy_addr,
                &ca_der,
                &authority,
                &ip_options(Some("127.0.0.1")),
                request.as_bytes(),
            )
            .await;
            assert!(status_line(&head).starts_with("HTTP/1.1 200"));
            assert_eq!(body, b"parallel");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(proxy.session.flow_count(), 8);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_intercepts_ip_literal_without_sni() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"ip-ok")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    // No SNI on the wire; the IP SAN still verifies through the name below.
    let options = ip_options(Some("127.0.0.1"));
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    let request = format!("GET /ip HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).await.unwrap();
    let response_head = read_tls_head(&mut tls).await;
    let body = read_tls_body(&mut tls, &response_head).await;
    assert!(status_line(&response_head).starts_with("HTTP/1.1 200"));
    assert_eq!(body, b"ip-ok");
    assert_eq!(captured.lock().await.len(), 1);
    assert_eq!(proxy.session.flow_count(), 1);
    finish_mitm(proxy).await;
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Second client style: EggFetch through the explicit proxy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_interoperates_with_eggfetch_client_through_proxy() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) = start_tls_origin(origin_cert, |request| {
        if request.method == "POST" {
            fixed_response(&request.body)
        } else {
            fixed_response(b"get-ok")
        }
    })
    .await;
    // The EggFetch client trusts the EggReplay CA for the proxy TLS leg and
    // verifies the logical origin hostname itself; upstream verification
    // stays proxy-owned. DNS upstream targets route through the local relay
    // (direct dials reject `localhost` as rebinding-risk).
    let (route, _relay, relay_task) = start_relay_route().await;
    let proxy = start_mitm(intercept_local_policy(), route, Some(origin_pem.as_bytes())).await;
    let proxy_addr = proxy.handle.local_addr();
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .proxy(eggfetch_core::proxy::Proxy::all(&format!("http://{proxy_addr}")).unwrap())
        .tls_config(
            eggfetch_core::TlsConfig::builder()
                .ca_certificate_der(vec![proxy.ca_der.clone()])
                .unwrap()
                .build(),
        )
        .build();
    let mut response = client
        .post(&format!("https://localhost:{origin_port}/echo"))
        .unwrap()
        .body("eggfetch-body")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.unwrap(), "eggfetch-body");
    // Duplicate query keys survive the decrypted path end to end.
    let mut response = client
        .get(&format!("https://localhost:{origin_port}/dup?a=1&a=2"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.unwrap(), "get-ok");
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].method, "POST");
    assert_eq!(captured[0].target, "/echo");
    assert_eq!(captured[1].target, "/dup?a=1&a=2");
    assert_eq!(proxy.session.flow_count(), 2);
    finish_mitm(proxy).await;
    relay_task.abort();
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Coherence failures (fail closed, no flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_rejects_sni_mismatch_without_flow() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    // A DNS SNI must not redirect an IP CONNECT elsewhere. No verification
    // so the client handshake succeeds and the server-side coherence check
    // is what fails.
    let options = TlsClientOptions {
        sni: Some("localhost"),
        alpn: vec![b"http/1.1"],
        danger_no_verify: true,
        enable_sni: true,
    };
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, tls.read_to_end(&mut response)).await;
    assert!(
        response.is_empty() || !status_line(&response).starts_with("HTTP/1.1 200"),
        "SNI mismatch must not produce a successful response"
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    let events = proxy.handle.events().snapshot();
    assert!(
        events.iter().any(|event| event.action == "intercept"),
        "TLS failure must leave a bounded intercept event: {events:?}"
    );
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_rejects_host_mismatch_and_poisons_the_tunnel() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let options = ip_options(Some("127.0.0.1"));
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    // Cross-origin reuse inside one CONNECT: 421, no flow.
    let evil = format!(
        "GET /evil HTTP/1.1\r\nHost: other.test:{origin_port}\r\nContent-Length: 0\r\n\r\n"
    );
    tls.write_all(evil.as_bytes()).await.unwrap();
    let evil_head = read_tls_head(&mut tls).await;
    assert!(
        status_line(&evil_head).starts_with("HTTP/1.1 421"),
        "cross-origin request must yield 421; got: {}",
        status_line(&evil_head)
    );
    // The tunnel is poisoned: even a coherent follow-up fails closed.
    let legit = format!("GET /ok HTTP/1.1\r\nHost: {authority}\r\nContent-Length: 0\r\n\r\n");
    tls.write_all(legit.as_bytes()).await.unwrap();
    let legit_head = read_tls_head(&mut tls).await;
    assert!(
        !status_line(&legit_head).starts_with("HTTP/1.1 200"),
        "poisoned tunnel must not serve follow-ups; got: {}",
        status_line(&legit_head)
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_rejects_dns_sni_mismatch_without_flow() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let (route, _relay_addr, relay_task) = start_relay_route().await;
    let proxy = start_mitm(intercept_local_policy(), route, Some(origin_pem.as_bytes())).await;
    let authority = format!("localhost:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let options = TlsClientOptions {
        sni: Some("other.test"),
        alpn: vec![b"http/1.1"],
        danger_no_verify: true,
        enable_sni: true,
    };
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, tls.read_to_end(&mut response)).await;
    assert!(
        response.is_empty() || !status_line(&response).starts_with("HTTP/1.1 200"),
        "DNS SNI mismatch must not succeed"
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    relay_task.abort();
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Upstream trust stays strict
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_keeps_upstream_certificate_failures_as_failures() {
    // Origin identity chains to an anchor the proxy was never given.
    let rogue = make_origin_cert(&["localhost", "127.0.0.1"]);
    let (origin_port, captured, origin_task) =
        start_tls_origin(rogue, |_| fixed_response(b"unreached")).await;
    let trusted = make_origin_cert(&["localhost", "127.0.0.1"]);
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(trusted.cert_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, _body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 502"),
        "untrusted upstream must yield 502; got: {}",
        status_line(&head)
    );
    assert_eq!(captured.lock().await.len(), 0);
    // Client trust of the EggReplay CA had no effect on upstream trust.
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    origin_task.abort();
}

#[tokio::test]
async fn mitm_keeps_upstream_hostname_failures_as_failures() {
    // Chains to the trusted anchor but names a different host.
    let origin_cert = make_origin_cert(&["wrong.test"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, _body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 502"),
        "hostname mismatch upstream must yield 502; got: {}",
        status_line(&head)
    );
    finish_mitm(proxy).await;
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Eggress-routed upstream
// ---------------------------------------------------------------------------

/// Minimal HTTP `CONNECT` relay used as a routed hop (mirrors the M013B
/// routed-tunnel fixture; it never terminates TLS).
async fn start_connect_relay() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    if client.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                    if head.len() > 4096 || head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let target = text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_owned();
                let Ok(mut upstream) = TcpStream::connect(&target).await else {
                    client
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                        .await
                        .ok();
                    return;
                };
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::io::copy_bidirectional(&mut client, &mut upstream)
                    .await
                    .ok();
            });
        }
    });
    (addr, task)
}

#[tokio::test]
async fn mitm_routes_upstream_through_eggress_with_logical_origin_intact() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"routed-ok")).await;
    let (relay, relay_task) = start_connect_relay().await;
    let relay_port = relay.port();
    let route = ProxyRoute::from_pproxy_uri(&format!("http://{relay}")).unwrap();
    let proxy = start_mitm(intercept_local_policy(), route, Some(origin_pem.as_bytes())).await;
    let authority = format!("localhost:{origin_port}");
    let request = format!("GET /routed HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &dns_options(Some("localhost")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 200"),
        "routed interception must succeed; got: {}",
        status_line(&head)
    );
    assert_eq!(body, b"routed-ok");
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    // TLS ran end to end through the relay: the origin still saw the real
    // logical SNI, not the hop.
    assert_eq!(captured[0].sni.as_deref(), Some("localhost"));
    assert_eq!(captured[0].target, "/routed");
    assert_eq!(proxy.session.flow_count(), 1);
    let dir = proxy.dir.path().to_owned();
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    let flows = std::fs::read_to_string(dir.join("fixture").join("flows.jsonl")).unwrap();
    assert!(
        flows.contains("\"kind\":\"explicit_proxy_mitm\"")
            && flows.contains(&format!("127.0.0.1:{relay_port}")),
        "routed flow must record MITM mode plus the redacted route: {flows}"
    );
    relay_task.abort();
    origin_task.abort();
}

#[tokio::test]
async fn mitm_routed_failure_never_falls_back_to_direct() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached-direct")).await;
    // Dead relay: direct dials would succeed, so any success proves fallback.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let route = ProxyRoute::from_pproxy_uri(&format!("http://{dead_addr}")).unwrap();
    let proxy = start_mitm(intercept_local_policy(), route, Some(origin_pem.as_bytes())).await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, _body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 502"),
        "dead route must yield 502, never direct success; got: {}",
        status_line(&head)
    );
    assert_eq!(
        captured.lock().await.len(),
        0,
        "no request may reach the origin through a dead route"
    );
    finish_mitm(proxy).await;
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Protocol exclusions (fail explicitly, no flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_rejects_malformed_tls_after_connect() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    stream
        .write_all(b"\x16\x03\x01\x00\x04garbage-bytes")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut response)).await;
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_does_not_negotiate_h2() {
    use eggreplay_intercept::build_intercept_server_config;
    let root = tempfile::TempDir::new().unwrap();
    let ca = CaAuthority::create_new(&root.path().join("ca"), &CaOptions::default()).unwrap();
    let ca_der = ca.cert_der().to_vec();
    let issuer = LeafIssuer::new(ca, &LeafOptions::default()).unwrap();
    let leaf = issuer
        .issue(&normalize_host("localhost").unwrap())
        .await
        .unwrap();
    let config = build_intercept_server_config(&leaf, &ca_der).unwrap();
    assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);

    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    // An h2-only client is rejected during the handshake itself: the
    // server advertises only `http/1.1`, so rustls sends a fatal
    // `no_application_protocol` alert instead of serving H2 bytes as H1.
    let options = TlsClientOptions {
        sni: Some("127.0.0.1"),
        alpn: vec![b"h2"],
        danger_no_verify: false,
        enable_sni: false,
    };
    assert!(
        try_tls_handshake(stream, &proxy.ca_der, &options)
            .await
            .is_err(),
        "h2-only ALPN must fail the handshake explicitly"
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    let events = proxy.handle.events().snapshot();
    assert!(
        events.iter().any(|event| event.action == "intercept"),
        "rejected handshake must leave a bounded intercept event: {events:?}"
    );
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_rejects_non_http_tls_payload() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let options = TlsClientOptions {
        sni: Some("127.0.0.1"),
        alpn: vec![],
        danger_no_verify: false,
        enable_sni: false,
    };
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    tls.write_all(b"NOTHTTP opaque-protocol\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, tls.read_to_end(&mut response)).await;
    assert!(
        response.is_empty() || !status_line(&response).starts_with("HTTP/1.1 200"),
        "non-HTTP TLS payload must fail explicitly"
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_rejects_websocket_upgrade_as_unsupported() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let options = ip_options(Some("127.0.0.1"));
    let mut tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    let upgrade = format!(
        "GET /socket HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nContent-Length: 0\r\n\r\n"
    );
    tls.write_all(upgrade.as_bytes()).await.unwrap();
    let response_head = read_tls_head(&mut tls).await;
    assert!(
        status_line(&response_head).starts_with("HTTP/1.1 400"),
        "WSS upgrade must be rejected as unsupported; got: {}",
        status_line(&response_head)
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    origin_task.abort();
}

// ---------------------------------------------------------------------------
// Policy gates before 200 (no flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_denies_unlisted_targets_before_200() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"unreached")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let (_stream, head) = connect_head(
        proxy.handle.local_addr(),
        &format!("denied.test:{origin_port}"),
    )
    .await;
    assert!(
        status_line(&head).starts_with("HTTP/1.1 403"),
        "unlisted CONNECT must yield 403; got: {}",
        status_line(&head)
    );
    // Malformed authorities fail before any issuance.
    let rejection = resolve_connect_target(Some("user@host:443"));
    assert!(rejection.is_err());
    assert_eq!(proxy.session.flow_count(), 0);
    finish_mitm(proxy).await;
    origin_task.abort();
}

#[tokio::test]
async fn mitm_intercept_verdict_without_issuer_fails_closed() {
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    // Armed intercept policy but no MitmConfig: M013B-compatible default.
    let config = ExplicitProxyConfig::new(
        session.clone(),
        intercept_local_policy(),
        ProxyRoute::direct(),
    );
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    let (_stream, head) = connect_head(handle.local_addr(), "localhost:443").await;
    assert!(
        !status_line(&head).starts_with("HTTP/1.1 200"),
        "intercept without an issuer must not send 200; got: {}",
        status_line(&head)
    );
    assert_eq!(session.flow_count(), 0);
    handle.shutdown();
    handle.wait().await;
    session.shutdown();
}

// ---------------------------------------------------------------------------
// Redaction, secrecy, lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mitm_redacts_authorization_before_durable_publication() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) = start_tls_origin(origin_cert, |request| {
        // The secret still travels upstream; redaction applies at rest.
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value.contains("MITM-REDACT-9f2c")),
            "upstream must observe the credential: {:?}",
            request.headers
        );
        fixed_response(b"guarded")
    })
    .await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!(
        "GET /guarded HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer MITM-REDACT-9f2c\r\nConnection: close\r\n\r\n"
    );
    let (head, body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    assert_eq!(body, b"guarded");
    let dir = proxy.dir.path().to_owned();
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    let flows = std::fs::read_to_string(dir.join("fixture").join("flows.jsonl")).unwrap();
    assert!(
        !flows.contains("MITM-REDACT-9f2c"),
        "redaction sentinel must not appear durably: {flows}"
    );
    assert!(
        flows.contains("request.headers.authorization"),
        "redaction marker must record the field: {flows}"
    );
    origin_task.abort();
}

#[tokio::test]
async fn mitm_keeps_keys_out_of_fixtures_reports_and_events() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"ok")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, _body) = https_exchange(
        proxy.handle.local_addr(),
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    assert_eq!(captured.lock().await.len(), 1);
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let dir = proxy.dir.path().to_owned();
    let events_debug = format!("{:?}", proxy.handle.events().snapshot());
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    for needle in ["PRIVATE KEY", "BEGIN PRIVATE", "ca-key.pem"] {
        assert!(
            !dir_contains(&dir, needle),
            "fixture must not contain {needle}"
        );
        assert!(
            !events_debug.contains(needle),
            "operational events must not contain {needle}"
        );
    }
    let flows = std::fs::read_to_string(dir.join("fixture").join("flows.jsonl")).unwrap();
    for needle in ["PRIVATE KEY", "BEGIN PRIVATE"] {
        assert!(!flows.contains(needle), "flow record leaks {needle}");
    }
    origin_task.abort();
}

#[tokio::test]
async fn mitm_shutdown_drains_an_active_decrypted_connection() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"ok")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let authority = format!("127.0.0.1:{origin_port}");
    // Idle keep-alive tunnel: no request yet, connection parked open.
    let (stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let options = ip_options(Some("127.0.0.1"));
    let tls = tls_handshake(stream, &proxy.ca_der, &options).await;
    drop(tls);
    proxy.handle.shutdown();
    tokio::time::timeout(TIMEOUT, proxy.handle.wait())
        .await
        .expect("proxy must drain with an active decrypted connection");
    proxy.session.shutdown();
    proxy.session.finish().unwrap();
    origin_task.abort();
}

#[tokio::test]
async fn mitm_session_finalizes_after_tls_and_protocol_failures() {
    let origin_cert = make_origin_cert(&["localhost", "127.0.0.1"]);
    let origin_pem = origin_cert.cert_pem.clone();
    let (origin_port, _captured, origin_task) =
        start_tls_origin(origin_cert, |_| fixed_response(b"recovered")).await;
    let proxy = start_mitm(
        intercept_local_policy(),
        ProxyRoute::direct(),
        Some(origin_pem.as_bytes()),
    )
    .await;
    let proxy_addr = proxy.handle.local_addr();
    let authority = format!("127.0.0.1:{origin_port}");

    // 1. Malformed TLS after CONNECT.
    let (mut stream, head) = connect_head(proxy_addr, &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    stream.write_all(b"not-tls-at-all").await.unwrap();
    drop(stream);

    // 2. SNI mismatch.
    let (stream, head) = connect_head(proxy_addr, &authority).await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    let mismatch = TlsClientOptions {
        sni: Some("localhost"),
        alpn: vec![b"http/1.1"],
        danger_no_verify: true,
        enable_sni: true,
    };
    let mut tls = tls_handshake(stream, &proxy.ca_der, &mismatch).await;
    let request = format!("GET /x HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut drained = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, tls.read_to_end(&mut drained)).await;

    // 3. A healthy exchange still records exactly one flow afterwards.
    let request = format!("GET /ok HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    let (head, body) = https_exchange(
        proxy_addr,
        &proxy.ca_der,
        &authority,
        &ip_options(Some("127.0.0.1")),
        request.as_bytes(),
    )
    .await;
    assert!(status_line(&head).starts_with("HTTP/1.1 200"));
    assert_eq!(body, b"recovered");
    assert_eq!(proxy.session.flow_count(), 1);
    let dir = proxy.dir.path().to_owned();
    proxy.handle.shutdown();
    proxy.handle.wait().await;
    proxy.session.shutdown();
    proxy
        .session
        .finish()
        .expect("session must finalize after failures");
    let flows = std::fs::read_to_string(dir.join("fixture").join("flows.jsonl")).unwrap();
    assert!(
        flows.contains("/ok"),
        "only the healthy flow persists: {flows}"
    );
    origin_task.abort();
}
