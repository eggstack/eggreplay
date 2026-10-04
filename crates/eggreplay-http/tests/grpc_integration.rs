#![cfg(all(feature = "h2", feature = "h2-inbound", feature = "grpc"))]
//! M015D gRPC-over-HTTP/2 integration qualification.
//!
//! M014D shipped a gRPC *derived view*: ordered 5-byte envelopes,
//! `grpc-status`, and caller-supplied descriptor decoding projected over an
//! already-recorded flow. It was qualified against hand-framed bytes on a
//! synthetic Hyper route, and no product code path ever called it. M015D binds
//! that view to real traffic from an independent, maintained gRPC
//! implementation.
//!
//! # The oracle
//!
//! Tonic 0.14.6, dev-dependency only, on both sides. A real Tonic server over
//! local TLS is the acquisition target, and a real Tonic client drives
//! acquisition, offline replay, and every streaming class. Nothing in the
//! product graph gains a gRPC stack: CI's dependency-boundary lanes resolve
//! `--edges normal`, which excludes dev edges, and the manifest carries an
//! explicit note saying so.
//!
//! What is genuinely Tonic's here is the part that matters for this
//! qualification — `tonic::server::Grpc` and `tonic::client::Grpc`, the
//! `ProstCodec` that writes and reads the 5-byte envelopes, Tonic's HTTP/2
//! transport, and its `Status` trailer handling. What is hand-written is the
//! per-method dispatch glue, which Tonic's own codegen also hand-writes; M015D
//! forbids a generated *service*, and a build script requiring `protoc` in
//! `PATH` would make the crate unbuildable for anyone without it.
//!
//! # Why the harness is qualified before EggReplay is involved
//!
//! `tonic_oracle_round_trips_before_eggreplay_is_involved` exercises the
//! harness end to end first. Without it, a bug in the hand-written glue would
//! be indistinguishable from an EggReplay defect — and the blame would land on
//! the wrong crate.
//!
//! # Authority
//!
//! The raw body blob and the raw trailers are the source of truth. The gRPC
//! view is a *projection* over them, and several tests assert that
//! relationship directly rather than assuming it.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use eggreplay_core::{
    BodyRef, Flow, FlowOutcome, HeaderEntry, Matcher, RedactionConfig, ReportScheduler,
    SessionMetadata, compare_flows,
};
use eggreplay_http::grpc::{
    GRPC_MAX_DESCRIPTOR_BYTES, GRPC_MAX_FRAMES, GrpcError, decode_grpc_payload,
    grpc_status_from_trailers, grpc_view, is_grpc_content_type, parse_grpc_frames,
};
use eggreplay_http::inbound::{H2Limits, InboundProtocol, InboundServerHandle};
use eggreplay_http::{ReplayFixture, execute_candidate};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use http::{Request as HttpRequest, Response as HttpResponse};
use hyper_util::rt::TokioIo;
use tonic::codegen::tokio_stream::{self, Stream};
use tonic::transport::{Channel, Server};
use tonic::{Code, Request, Response, Status};

// `prost::Message` is what puts `decode` on the hand-written messages.
use prost::Message as _;

const SERVICE: &str = "grpc.test.Echo";
const UNARY_PATH: &str = "/grpc.test.Echo/Unary";
const SERVER_STREAM_PATH: &str = "/grpc.test.Echo/ServerStream";
const CLIENT_STREAM_PATH: &str = "/grpc.test.Echo/ClientStream";
const BIDI_PATH: &str = "/grpc.test.Echo/Bidi";

/// The client codec is `<sent, received>`; Tonic's server `Grpc` uses the
/// same `Codec` associated types in the opposite roles, so the server codec
/// is parameterized `<response, request>`. Both spellings come from the
/// bounds in `tonic::client::Grpc` and `tonic::server::Grpc`.
type ClientCodec = tonic_prost::ProstCodec<EchoRequest, EchoReply>;
type ServerCodec = tonic_prost::ProstCodec<EchoReply, EchoRequest>;
type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;
type ConnectBoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
type ServerFuture = Pin<
    Box<
        dyn Future<Output = Result<HttpResponse<tonic::body::Body>, std::convert::Infallible>>
            + Send,
    >,
>;

// ---------------------------------------------------------------------------
// Protobuf messages, hand-written
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
struct EchoRequest {
    #[prost(string, tag = "1")]
    message: String,
    #[prost(uint32, tag = "2")]
    count: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct EchoReply {
    #[prost(string, tag = "1")]
    message: String,
    #[prost(uint32, tag = "2")]
    ordinal: u32,
}

impl EchoRequest {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
            count: 1,
        }
    }

    fn with_count(message: &str, count: u32) -> Self {
        Self {
            message: message.to_string(),
            count,
        }
    }
}

/// An error RPC: `message == "fail"` becomes a non-OK `grpc-status` carried
/// in Tonic's response trailers.
fn reject(request: &EchoRequest) -> Result<(), Status> {
    if request.message == "fail" {
        return Err(Status::not_found("no such thing"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The Tonic service
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct EchoService;

// `tonic::server::{UnaryService, ServerStreamingService, ClientStreamingService,
// StreamingService}` are blanket-implemented for any `tower::Service` with the
// matching `Request`/`Response`, so each method is a four-line adapter over the
// service itself.

impl tower::Service<Request<EchoRequest>> for EchoService {
    type Response = Response<EchoReply>;
    type Error = Status;
    type Future =
        Pin<Box<dyn Future<Output = Result<Response<EchoReply>, Status>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<EchoRequest>) -> Self::Future {
        Box::pin(async move {
            let request = request.into_inner();
            reject(&request)?;
            Ok(Response::new(EchoReply {
                message: request.message,
                ordinal: 1,
            }))
        })
    }
}

#[derive(Clone)]
struct ServerStreamService;

impl tower::Service<Request<EchoRequest>> for ServerStreamService {
    type Response = Response<BoxStream<EchoReply>>;
    type Error = Status;
    type Future = Pin<
        Box<dyn Future<Output = Result<Response<BoxStream<EchoReply>>, Status>> + Send + 'static>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<EchoRequest>) -> Self::Future {
        Box::pin(async move {
            let request = request.into_inner();
            reject(&request)?;
            // `count` replies, ordinal 1..=count, on one stream. This is the
            // class that must produce several ordered envelopes in a single
            // recorded body.
            let messages = (1..=request.count.max(1))
                .map(|ordinal| {
                    Ok(EchoReply {
                        message: format!("{}#{ordinal}", request.message),
                        ordinal,
                    })
                })
                .collect::<Vec<_>>();
            Ok(Response::new(
                Box::pin(tokio_stream::iter(messages)) as BoxStream<EchoReply>
            ))
        })
    }
}

#[derive(Clone)]
struct ClientStreamService;

impl tower::Service<Request<tonic::Streaming<EchoRequest>>> for ClientStreamService {
    type Response = Response<EchoReply>;
    type Error = Status;
    type Future =
        Pin<Box<dyn Future<Output = Result<Response<EchoReply>, Status>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<tonic::Streaming<EchoRequest>>) -> Self::Future {
        Box::pin(async move {
            let mut stream = request.into_inner();
            let mut seen = 0usize;
            let mut last = String::new();
            // Drain the client's half before answering. This is the ordering
            // constraint the canonical model has to survive.
            while let Some(item) = stream.message().await? {
                seen += 1;
                last = item.message;
            }
            Ok(Response::new(EchoReply {
                message: format!("{last}/{seen}"),
                ordinal: seen as u32,
            }))
        })
    }
}

#[derive(Clone)]
struct BidiService;

impl tower::Service<Request<tonic::Streaming<EchoRequest>>> for BidiService {
    type Response = Response<BoxStream<EchoReply>>;
    type Error = Status;
    type Future = Pin<
        Box<dyn Future<Output = Result<Response<BoxStream<EchoReply>>, Status>> + Send + 'static>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<tonic::Streaming<EchoRequest>>) -> Self::Future {
        Box::pin(async move {
            let stream = request.into_inner();
            // A bidirectional call interleaves both halves on one HTTP/2
            // stream. `unfold` over the inbound messages is the honest shape
            // for that: a reply is produced for each client message as it
            // arrives, rather than after the client half closes.
            let outbound = futures_util::stream::unfold(
                (stream, 0u32),
                move |(mut stream, ordinal)| async move {
                    match stream.message().await {
                        Ok(Some(message)) => {
                            let reply = EchoReply {
                                message: format!("bidi:{}", message.message),
                                ordinal: ordinal + 1,
                            };
                            Some((Ok(reply), (stream, ordinal + 1)))
                        }
                        // The client half closed. A bidi call has no terminal
                        // status of its own, so this is the honest outcome
                        // rather than a fabricated success.
                        Ok(None) => Some((
                            Err(Status::new(Code::Cancelled, "client half closed")),
                            (stream, ordinal),
                        )),
                        Err(status) => Some((Err(status), (stream, ordinal))),
                    }
                },
            );
            Ok(Response::new(Box::pin(outbound) as BoxStream<EchoReply>))
        })
    }
}

// ---------------------------------------------------------------------------
// The Tonic server dispatcher
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct EchoServer;

impl tonic::codegen::Service<HttpRequest<tonic::body::Body>> for EchoServer {
    type Response = HttpResponse<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = ServerFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: HttpRequest<tonic::body::Body>) -> Self::Future {
        let path = req.uri().path().to_string();
        let method = req
            .extensions()
            .get::<tonic::GrpcMethod>()
            .map(|value| format!("{}.{}", value.service(), value.method()))
            .unwrap_or_default();
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(ServerCodec::default());
            let _ = method;
            let response = match path.as_str() {
                UNARY_PATH => grpc.unary(EchoService, req).await,
                SERVER_STREAM_PATH => grpc.server_streaming(ServerStreamService, req).await,
                CLIENT_STREAM_PATH => grpc.client_streaming(ClientStreamService, req).await,
                BIDI_PATH => grpc.streaming(BidiService, req).await,
                _ => {
                    let mut response = HttpResponse::new(tonic::body::Body::default());
                    response.headers_mut().insert(
                        tonic::Status::GRPC_STATUS,
                        (Code::Unimplemented as i32).into(),
                    );
                    response.headers_mut().insert(
                        http::header::CONTENT_TYPE,
                        tonic::metadata::GRPC_CONTENT_TYPE,
                    );
                    return Ok(response);
                }
            };
            Ok(response)
        })
    }
}

impl tonic::server::NamedService for EchoServer {
    const NAME: &'static str = SERVICE;
}

// ---------------------------------------------------------------------------
// Test-owned TLS identity
// ---------------------------------------------------------------------------

struct TestCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    ca_pem: String,
    directory: std::path::PathBuf,
}

fn test_cert(name: &str) -> TestCert {
    let directory = temp_dir(name);
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("test cert");
    std::fs::write(directory.join("server.pem"), certified.cert.pem()).expect("cert");
    std::fs::write(
        directory.join("server.key"),
        certified.key_pair.serialize_pem(),
    )
    .expect("key");
    TestCert {
        cert_der: certified.cert.der().to_vec(),
        key_der: certified.key_pair.serialize_der(),
        ca_pem: certified.cert.pem(),
        directory,
    }
}

impl Drop for TestCert {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
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
// Unique paths
// ---------------------------------------------------------------------------

static DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_path(name: &str) -> std::path::PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let serial = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "eggreplay-grpc-{name}-{}-{millis}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let path = temp_path(name);
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

// ---------------------------------------------------------------------------
// The Tonic server harness
// ---------------------------------------------------------------------------

struct GrpcServer {
    address: SocketAddr,
    cert: TestCert,
    accept: Option<tokio::task::JoinHandle<()>>,
    serve: Option<tokio::task::JoinHandle<()>>,
}

impl GrpcServer {
    fn base(&self) -> String {
        format!("https://localhost:{}", self.address.port())
    }
}

impl Drop for GrpcServer {
    fn drop(&mut self) {
        if let Some(task) = self.accept.take() {
            task.abort();
        }
        if let Some(task) = self.serve.take() {
            task.abort();
        }
    }
}

/// A real Tonic server over local TLS.
///
/// TLS is terminated by the test's own acceptor and the decrypted stream is
/// handed to Tonic, rather than enabling Tonic's TLS feature. EggServe is the
/// only TLS terminator in the product, and the oracle should not quietly add a
/// second one — otherwise "TLS ALPN worked" would be a statement about the
/// harness.
async fn start_grpc_server(name: &str) -> GrpcServer {
    let cert = test_cert(name);
    let acceptor = tokio_rustls::TlsAcceptor::from(server_tls(&cert));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("grpc bind");
    let address = listener.local_addr().expect("grpc addr");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let accept = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = tx.send(Ok::<_, std::io::Error>(tls));
            });
        }
    });
    let incoming = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let serve = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(EchoServer)
            .serve_with_incoming(incoming)
            .await;
    });
    GrpcServer {
        address,
        cert,
        accept: Some(accept),
        serve: Some(serve),
    }
}

// ---------------------------------------------------------------------------
// The Tonic client
// ---------------------------------------------------------------------------

/// A real Tonic client that trusts the test identity.
async fn tonic_client(cert: &TestCert, address: SocketAddr) -> Channel {
    let endpoint =
        Channel::from_shared(format!("https://localhost:{}", address.port())).expect("endpoint");
    let tls = client_tls(cert);
    endpoint
        .connect_with_connector(tower::service_fn(move |_: http::Uri| {
            let tls = tls.clone();
            async move {
                let tcp = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|error| Box::new(error) as ConnectBoxError)?;
                let tls = tokio_rustls::TlsConnector::from(tls)
                    .connect(server_name(), tcp)
                    .await
                    .map_err(|error| Box::new(error) as ConnectBoxError)?;
                // Tonic's `GrpcService` bound is hyper's `Read`/`Write`, and
                // `tokio-rustls` speaks tokio's. The adapter is the documented
                // bridge, not a workaround.
                Ok::<_, ConnectBoxError>(TokioIo::new(tls))
            }
        }))
        .await
        .expect("tonic channel connects")
}

fn grpc_request<M>(message: M, method: &'static str) -> Request<M> {
    let mut request = Request::new(message);
    request
        .extensions_mut()
        .insert(tonic::GrpcMethod::new(SERVICE, method));
    request
}

async fn unary(channel: &Channel, message: EchoRequest) -> Result<EchoReply, Status> {
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    let response = grpc
        .unary(
            grpc_request(message, "Unary"),
            http::uri::PathAndQuery::from_static(UNARY_PATH),
            ClientCodec::default(),
        )
        .await?;
    Ok(response.into_inner())
}

async fn server_stream(
    channel: &Channel,
    message: EchoRequest,
) -> Result<tonic::Streaming<EchoReply>, Status> {
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    grpc.server_streaming(
        grpc_request(message, "ServerStream"),
        http::uri::PathAndQuery::from_static(SERVER_STREAM_PATH),
        ClientCodec::default(),
    )
    .await
    .map(|response| response.into_inner())
}

async fn client_stream(channel: &Channel, messages: Vec<EchoRequest>) -> Result<EchoReply, Status> {
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    // Tonic maps each stream item to `Ok` itself, so the outbound stream
    // yields bare messages rather than `Result`s.
    let outbound = tokio_stream::iter(messages);
    let response = grpc
        .client_streaming(
            grpc_request(outbound, "ClientStream"),
            http::uri::PathAndQuery::from_static(CLIENT_STREAM_PATH),
            ClientCodec::default(),
        )
        .await?;
    Ok(response.into_inner())
}

async fn bidi(
    channel: &Channel,
    messages: Vec<EchoRequest>,
) -> Result<tonic::Streaming<EchoReply>, Status> {
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    // Tonic maps each stream item to `Ok` itself, so the outbound stream
    // yields bare messages rather than `Result`s.
    let outbound = tokio_stream::iter(messages);
    grpc.streaming(
        grpc_request(outbound, "Bidi"),
        http::uri::PathAndQuery::from_static(BIDI_PATH),
        ClientCodec::default(),
    )
    .await
    .map(|response| response.into_inner())
}

// ---------------------------------------------------------------------------
// Oracle qualification
// ---------------------------------------------------------------------------

/// The oracle qualifies itself before EggReplay is involved.
///
/// Every other test in this file is only meaningful if a Tonic client and a
/// Tonic server can round-trip here, on this harness, with this hand-written
/// dispatch glue. Without this test, a bug in the glue would be
/// indistinguishable from an EggReplay defect — and the blame would land on
/// the wrong crate.
#[tokio::test]
async fn tonic_oracle_round_trips_before_eggreplay_is_involved() {
    let server = start_grpc_server("oracle").await;
    let channel = tonic_client(&server.cert, server.address).await;

    // Unary.
    let reply = unary(&channel, EchoRequest::new("hello"))
        .await
        .expect("unary");
    assert_eq!(reply.message, "hello");
    assert_eq!(reply.ordinal, 1);

    // Server streaming: several messages, one stream.
    let mut stream = server_stream(&channel, EchoRequest::with_count("tick", 4))
        .await
        .expect("server stream");
    let mut ordinals = Vec::new();
    while let Some(item) = stream.message().await.expect("stream message") {
        ordinals.push(item.ordinal);
    }
    assert_eq!(ordinals, vec![1, 2, 3, 4]);

    // Client streaming: the server drains the client's half first.
    let reply = client_stream(&channel, vec![EchoRequest::new("a"), EchoRequest::new("b")])
        .await
        .expect("client stream");
    assert_eq!(reply.message, "b/2");
    assert_eq!(reply.ordinal, 2);

    // Bidirectional: replies are interleaved with the client's messages, and
    // the call ends on the client's half close. The terminal status is part
    // of the answer, not an error in the observation.
    let mut stream = bidi(&channel, vec![EchoRequest::new("x"), EchoRequest::new("y")])
        .await
        .expect("bidi");
    let mut messages = Vec::new();
    let terminal = loop {
        match stream.message().await {
            Ok(Some(message)) => messages.push(message.message),
            Ok(None) => break None,
            Err(status) => break Some(status),
        }
    };
    assert_eq!(messages, vec!["bidi:x", "bidi:y"]);
    let terminal = terminal.expect("a bidi call must end with a terminal status");
    assert_eq!(terminal.code(), Code::Cancelled);
    assert_eq!(terminal.message(), "client half closed");

    // A non-OK status is carried in trailers by a real Tonic server.
    let status = unary(&channel, EchoRequest::new("fail"))
        .await
        .expect_err("fails");
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(status.message(), "no such thing");
}

// ---------------------------------------------------------------------------
// The EggReplay recording gateway
// ---------------------------------------------------------------------------

fn direct_route() -> eggreplay_core::PhysicalRoute {
    eggreplay_core::PhysicalRoute {
        kind: "direct".into(),
        description: Some("direct".into()),
    }
}

struct Gateway {
    address: SocketAddr,
    handle: Option<InboundServerHandle>,
    session: RecordingSession,
    directory: std::path::PathBuf,
}

impl Gateway {
    async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.wait().await;
        }
    }

    async fn finalize(mut self) -> (Session, std::path::PathBuf) {
        self.stop().await;
        let directory = self.directory.clone();
        self.session.shutdown();
        eggreplay_http::recording::drain_active_blobs(&self.session).await;
        let session = std::mem::replace(
            &mut self.session,
            RecordingSession::create(
                temp_path("grpc-unused-slot"),
                SessionMetadata::default(),
                StoreLimits::default(),
            )
            .expect("placeholder session"),
        );
        let published = eggreplay_http::recording::finish_recording_session(session)
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

/// Start the recording gateway in front of `upstream`, served over cleartext
/// HTTP/2.
///
/// The gateway is what makes this an HTTP/2 *inbound* test: a real Tonic
/// client speaks H2 to EggReplay, and EggReplay speaks H2 to Tonic. Neither
/// leg is H1.
async fn start_gateway(name: &str, upstream: &GrpcServer, identity: &TestCert) -> Gateway {
    start_gateway_with_timeout(name, upstream, identity, None).await
}

/// As `start_gateway`, with an explicit outbound timeout.
///
/// A bidirectional call cannot complete through the gateway, so a test that
/// proves *how* it fails needs the wait to be short and the failure to be
/// bounded rather than "however long the default is".
async fn start_gateway_with_timeout(
    name: &str,
    upstream: &GrpcServer,
    identity: &TestCert,
    timeout: Option<std::time::Duration>,
) -> Gateway {
    let directory = temp_path(name);
    let session = RecordingSession::create(
        &directory,
        SessionMetadata {
            capture_mode: "grpc-h2-e2e".into(),
            target: Some(upstream.base()),
            redaction_profile: "default-v1".into(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .expect("recording session");
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(identity.ca_pem.as_bytes())
        .expect("valid test CA")
        .build();
    // `Http2Only`, not `Auto`: the outbound leg of this test is meant to be
    // HTTP/2, and a policy that would accept H1 could not tell the two apart.
    let mut builder = eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .retry_canceled_requests(false)
        .tls_config(tls);
    if let Some(timeout) = timeout {
        // `from_secs` alone is not enough here, and M016 is why: it sets the
        // per-phase budgets but NOT `total`, and the per-phase `read` budget
        // only starts once a body chunk has arrived. Without a `total` cap a
        // call whose request half never closes is not ended by the deadline at
        // all — hyper tears the stream down with RST_STREAM instead, and the
        // recorded failure is a protocol error rather than a timeout. Setting
        // `total` is what makes "the outbound timeout ends the call" the real
        // mechanism, which is the scenario this test claims to exercise.
        let mut budget = eggfetch_core::Timeout::from_secs(timeout.as_secs());
        budget.total = Some(timeout);
        builder = builder.timeout(budget);
    }
    let client = builder.build();
    let handle = eggreplay_http::recording::start_recording_gateway_with_protocol(
        "127.0.0.1:0".parse().expect("addr"),
        upstream.base().parse().expect("upstream uri"),
        client,
        session.clone(),
        16 << 20,
        RedactionConfig::default_secure(),
        "default-v1".to_string(),
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        direct_route(),
        eggreplay_http::recording::WebSocketRecordingOptions::default(),
        InboundProtocol::Http2Cleartext,
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

/// Serve a published fixture over TLS HTTP/2, the same policy an operator
/// selects for a sealed replay.
async fn start_replay(name: &str, directory: &std::path::PathBuf) -> (Replay, TestCert) {
    start_replay_with_matcher(name, directory, Matcher::strict(8)).await
}

/// Start a replay server with an explicit matcher profile.
///
/// Needed where the request that was recorded came from a raw H2 peer rather
/// than from a Tonic client: such a peer sends no `user-agent`, so a Tonic
/// client replaying it differs by a volatile header that the strict profile
/// treats as significant. `practical` ignores exactly those volatile headers
/// while still requiring an exact body, so the match stays meaningful.
async fn start_replay_with_matcher(
    name: &str,
    directory: &std::path::PathBuf,
    matcher: Matcher,
) -> (Replay, TestCert) {
    let identity = test_cert(name);
    let session = Session::open(directory, StoreLimits::default()).expect("open session");
    let fixture = ReplayFixture::load_with_matcher(&session, matcher).expect("replay fixture");
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
    (
        Replay {
            address,
            handle: Some(handle),
        },
        identity,
    )
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn published_flows(session: &Session) -> Vec<Flow> {
    session
        .iter_flows()
        .expect("iterable")
        .collect::<Result<Vec<_>, _>>()
        .expect("every published flow is readable")
}

fn header<'a>(entries: &'a [HeaderEntry], name: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(name))
        .map(|entry| entry.value.as_str())
}

/// Project an `http::HeaderMap` into the canonical header entries the gRPC
/// view reads, so a value observed on the wire can be checked through the
/// same API a recorded fixture uses.
fn header_entries(map: Option<&http::HeaderMap>) -> Vec<HeaderEntry> {
    map.map(|map| {
        map.iter()
            .map(|(name, value)| HeaderEntry {
                name: name.as_str().to_string(),
                value: value.as_bytes().escape_ascii().to_string(),
            })
            .collect()
    })
    .unwrap_or_default()
}

fn response_of(flow: &Flow) -> &eggreplay_core::HttpResponse {
    match &flow.outcome {
        FlowOutcome::Response(response) => response,
        FlowOutcome::Error(error) => panic!("expected a response, got error {error:?}"),
    }
}

fn stored_body(session: &Session, flow: &Flow) -> Vec<u8> {
    match &response_of(flow).body {
        BodyRef::Blob(blob) => session.read_blob(blob).expect("stored body"),
        BodyRef::Empty | BodyRef::Absent => Vec::new(),
    }
}

/// `date` is origin-generated and second-granular, and the regression
/// authority compares it. A comparison about protocol *semantics* normalizes
/// this one wall-clock field, exactly as the pre-existing outbound-H2
/// qualification does.
fn without_volatile_date(flow: &Flow) -> Flow {
    let mut normalized = flow.clone();
    if let FlowOutcome::Response(response) = &mut normalized.outcome {
        response
            .headers
            .retain(|entry| !entry.name.eq_ignore_ascii_case("date"));
    }
    normalized
}

/// A cleartext Tonic client, for the gateway's cleartext HTTP/2 listener.
async fn tonic_client_cleartext(address: SocketAddr) -> Channel {
    Channel::from_shared(format!("http://localhost:{}", address.port()))
        .expect("endpoint")
        .connect_with_connector(tower::service_fn(move |_: http::Uri| async move {
            let tcp = tokio::net::TcpStream::connect(address)
                .await
                .map_err(|error| Box::new(error) as ConnectBoxError)?;
            Ok::<_, ConnectBoxError>(TokioIo::new(tcp))
        }))
        .await
        .expect("tonic channel connects")
}

/// The payload of the single envelope in `body`.
fn frames_payload(body: &[u8]) -> Vec<u8> {
    let frames = parse_grpc_frames(body).expect("one well-formed envelope");
    frames[0].payload.clone()
}

fn decode_reply(frame: &eggreplay_http::grpc::GrpcFrame) -> EchoReply {
    EchoReply::decode(frame.payload.as_slice()).expect("payload is a protobuf EchoReply")
}

/// Record a unary RPC through the gateway and return the published session.
async fn acquire_unary(name: &str, message: &str) -> (Session, std::path::PathBuf, GrpcServer) {
    let server = start_grpc_server(&format!("{name}-upstream")).await;
    let gateway = start_gateway(name, &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    unary(&channel, EchoRequest::new(message))
        .await
        .unwrap_or_else(|status| panic!("unary must succeed: {status:?}"));
    let (published, directory) = gateway.finalize().await;
    (published, directory, server)
}

/// A real Tonic unary call survives EggReplay's HTTP/2 recording gateway with
/// its envelope, its `content-type`, and its `grpc-status` intact.
///
/// The point of the test is that every one of those values was produced by
/// Tonic, not by the fixture. `content-type` is asserted to be the exact string
/// Tonic sends, and the envelope payload is decoded back into the protobuf
/// message Tonic encoded.
#[tokio::test]
async fn unary_grpc_over_h2_records_envelopes_and_status() {
    let (published, directory, _server) = acquire_unary("grpc-unary", "hello").await;
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 1);
    let flow = &flows[0];
    let response = response_of(flow);

    assert_eq!(flow.request.method, "POST");
    assert_eq!(flow.request.path, UNARY_PATH);
    let content_type = header(&response.headers, "content-type").expect("content-type");
    assert_eq!(
        content_type, "application/grpc",
        "a real Tonic client sends the bare gRPC content type, and the gate must accept it"
    );
    assert!(is_grpc_content_type(Some(content_type)));
    assert_eq!(response.status, 200);

    let body = stored_body(&published, flow);
    let frames = parse_grpc_frames(&body).expect("a real unary body is one well-formed envelope");
    assert_eq!(frames.len(), 1, "a unary call carries exactly one message");
    assert_eq!(frames[0].index, 0);
    assert!(!frames[0].compressed, "Tonic does not compress by default");
    assert_eq!(
        frames[0].length as usize,
        frames[0].payload.len(),
        "the envelope length must agree with its payload"
    );
    let reply = decode_reply(&frames[0]);
    assert_eq!(reply.message, "hello");
    assert_eq!(reply.ordinal, 1);

    let status = grpc_status_from_trailers(&response.trailers).expect("grpc-status recorded");
    assert_eq!(status.code, 0);
    assert_eq!(status.message, "", "an OK status carries no message");

    let view = grpc_view(&body, &response.trailers, None).expect("derived view");
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.status, Some(status));
    // No descriptor was supplied, so nothing is decoded: the view reports the
    // envelope, and refuses to guess.
    assert!(
        view.messages[0].decoded.is_none(),
        "without a caller-supplied descriptor the view must not decode"
    );

    // Raw body and trailers stay authoritative: a second projection of the
    // same bytes is identical.
    let again = grpc_view(&body, &response.trailers, None).expect("derived view");
    assert_eq!(
        serde_json::to_value(&view).expect("json"),
        serde_json::to_value(&again).expect("json")
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// Several gRPC messages in one HTTP/2 stream are recorded as several ordered
/// envelopes in one body, in order.
#[tokio::test]
async fn server_streaming_envelopes_are_ordered_in_one_recorded_stream() {
    let server = start_grpc_server("grpc-stream-upstream").await;
    let gateway = start_gateway("grpc-stream", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    let mut stream = server_stream(&channel, EchoRequest::with_count("tick", 5))
        .await
        .expect("server stream");
    let mut observed = Vec::new();
    while let Some(message) = stream.message().await.expect("stream message") {
        observed.push(message.ordinal);
    }
    assert_eq!(observed, vec![1, 2, 3, 4, 5]);

    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 1, "one stream is one flow");
    let response = response_of(&flows[0]);
    let body = stored_body(&published, &flows[0]);
    let frames = parse_grpc_frames(&body).expect("five well-formed envelopes");
    assert_eq!(
        frames.len(),
        5,
        "one envelope per message, all on one stream"
    );
    let mut ordinals = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.index, index, "envelopes keep their order");
        assert!(!frame.compressed);
        ordinals.push(decode_reply(frame).ordinal);
    }
    assert_eq!(ordinals, vec![1, 2, 3, 4, 5]);
    assert!(
        response
            .trailers
            .iter()
            .any(|entry| entry.name == "grpc-status"),
        "a completed server stream still carries its terminal status"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// A non-OK `grpc-status` reaches the fixture, wherever Tonic puts it.
///
/// Tonic may answer a server-side error either as a trailers-only response —
/// `grpc-status` in the HEADERS — or as headers plus a trailing HEADERS block.
/// The product must preserve whichever Tonic actually sent rather than
/// normalising it, so the assertion is on the value's presence in the recorded
/// response, not on which frame carried it.
#[tokio::test]
async fn error_status_travels_in_the_recorded_response() {
    let server = start_grpc_server("grpc-error-upstream").await;
    let gateway = start_gateway("grpc-error", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    let status = unary(&channel, EchoRequest::new("fail"))
        .await
        .expect_err("the oracle rejects this message");
    assert_eq!(status.code(), Code::NotFound);

    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    assert_eq!(flows.len(), 1);
    let response = response_of(&flows[0]);

    let mut carried: Vec<HeaderEntry> = response.headers.clone();
    carried.extend(response.trailers.iter().cloned());
    let status = grpc_status_from_trailers(&carried)
        .unwrap_or_else(|| panic!("grpc-status must be recorded, got {carried:?}"));
    assert_eq!(status.code, Code::NotFound as u32);
    assert_eq!(
        status.message, "no such thing",
        "grpc-message must survive percent-encoding and decoding"
    );

    let body = stored_body(&published, &flows[0]);
    let view = grpc_view(&body, &carried, None).expect("derived view");
    assert!(
        view.messages.is_empty(),
        "a failed call carries no message envelope, got {:?}",
        view.messages
    );
    assert_eq!(view.status, Some(status));

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Offline replay to a real gRPC client
// ---------------------------------------------------------------------------

/// A Tonic client that *claims* `authority` on the wire but dials `dial`.
///
/// The recorded authority of a gateway flow is the upstream origin, not the
/// gateway's own listener, so a faithful offline replay has to be addressed at
/// that origin. In production that means DNS or a host mapping; here the
/// connector pins the transport instead. Everything above the TCP connection —
/// `:authority`, `:scheme`, `:path`, headers, body — is produced by Tonic
/// exactly as it would be in a deployment where the replay server really is at
/// the recorded origin.
async fn tonic_client_at(cert: &TestCert, authority: &str, dial: SocketAddr) -> Channel {
    let endpoint = Channel::from_shared(format!("https://{authority}")).expect("endpoint");
    let tls = client_tls(cert);
    endpoint
        .connect_with_connector(tower::service_fn(move |_: http::Uri| {
            let tls = tls.clone();
            async move {
                let tcp = tokio::net::TcpStream::connect(dial)
                    .await
                    .map_err(|error| Box::new(error) as ConnectBoxError)?;
                let tls = tokio_rustls::TlsConnector::from(tls)
                    .connect(server_name(), tcp)
                    .await
                    .map_err(|error| Box::new(error) as ConnectBoxError)?;
                Ok::<_, ConnectBoxError>(TokioIo::new(tls))
            }
        }))
        .await
        .expect("tonic channel connects")
}

/// The headline claim: a gRPC call recorded over HTTP/2 replays over HTTP/2 to
/// a real gRPC client, with its envelope and status intact.
///
/// Nothing in the replay path knows what gRPC is. Tonic is asked the same
/// question it asked the live server and must get the same answer, which is
/// the only way to say that the H2 gateway and the H2 replay server preserve
/// gRPC semantics rather than merely serving bytes that happen to parse.
#[tokio::test]
async fn recorded_grpc_flow_replays_to_a_real_grpc_client() {
    let (published, directory, _server) = acquire_unary("grpc-replay", "hello").await;
    let flow = &published_flows(&published)[0];
    let recorded_authority = flow.request.authority.clone();
    let recorded_body = stored_body(&published, flow);

    let (replay, identity) = start_replay("grpc-replay-listener", &directory).await;
    let channel = tonic_client_at(&identity, &recorded_authority, replay.address).await;

    let reply = unary(&channel, EchoRequest::new("hello"))
        .await
        .expect("a real gRPC client must be able to call the replay server");
    assert_eq!(reply.message, "hello");
    assert_eq!(reply.ordinal, 1);

    // The client saw the recorded envelope byte for byte, not merely an
    // equivalent message.
    let served = reply.encode_to_vec();
    let frames = parse_grpc_frames(&recorded_body).expect("recorded envelope");
    assert_eq!(
        served, frames[0].payload,
        "the replayed protobuf bytes must be exactly the recorded ones"
    );

    // And a server-streaming call replays as several ordered messages.
    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// Server-streaming traffic replays as the same ordered message sequence.
#[tokio::test]
async fn recorded_server_stream_replays_as_ordered_messages() {
    let server = start_grpc_server("grpc-stream-replay-upstream").await;
    let gateway = start_gateway("grpc-stream-replay", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    let mut live = server_stream(&channel, EchoRequest::with_count("tick", 5))
        .await
        .expect("server stream");
    let mut live_messages = Vec::new();
    while let Some(message) = live.message().await.expect("stream message") {
        live_messages.push(message);
    }
    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    let recorded_authority = flow.request.authority.clone();

    let (replay, identity) = start_replay("grpc-stream-replay-listener", &directory).await;
    let channel = tonic_client_at(&identity, &recorded_authority, replay.address).await;
    let mut replayed = server_stream(&channel, EchoRequest::with_count("tick", 5))
        .await
        .expect("replayed server stream");
    let mut replay_messages = Vec::new();
    while let Some(message) = replayed.message().await.expect("replay message") {
        replay_messages.push(message);
    }

    assert_eq!(
        replay_messages, live_messages,
        "every message must replay in order, with the same ordinals"
    );
    assert_eq!(replay_messages.len(), 5);

    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Caller-supplied descriptors
// ---------------------------------------------------------------------------

/// A `FileDescriptorSet` describing `grpc.test.Echo`, matching `EchoReply`.
///
/// Built declaratively rather than embedded as bytes: a reader can check the
/// descriptor against the message struct without a protobuf tool. The
/// descriptor is *caller supplied* — nothing in the product fetches one, and
/// nothing here is a reflection lookup.
fn echo_reply_descriptor_set() -> Vec<u8> {
    use prost_types::{
        DescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
    };
    // LABEL_OPTIONAL = 1, TYPE_STRING = 9, TYPE_UINT32 = 13.
    let field = |name: &str, number: i32, kind: i32| FieldDescriptorProto {
        name: Some(name.to_string()),
        json_name: Some(name.to_string()),
        number: Some(number),
        label: Some(1),
        r#type: Some(kind),
        ..Default::default()
    };
    let file = FileDescriptorProto {
        name: Some("echo.proto".to_string()),
        package: Some("grpc.test".to_string()),
        syntax: Some("proto3".to_string()),
        message_type: vec![DescriptorProto {
            name: Some("Echo".to_string()),
            field: vec![field("message", 1, 9), field("ordinal", 2, 13)],
            ..Default::default()
        }],
        ..Default::default()
    };
    FileDescriptorSet { file: vec![file] }.encode_to_vec()
}

/// A caller-supplied descriptor set decodes messages out of a *recorded*
/// flow.
///
/// The descriptor path is unit-tested in `grpc.rs`, but until now no
/// integration test ever passed a descriptor to `grpc_view`, so the wiring
/// between a stored body and a caller-supplied schema was never exercised
/// outside the module. That is the largest hole M015D closes.
#[tokio::test]
async fn caller_supplied_descriptor_decodes_a_recorded_message() {
    let (published, directory, _server) = acquire_unary("grpc-descriptor", "hello").await;
    let flow = &published_flows(&published)[0];
    let response = response_of(flow);
    let body = stored_body(&published, flow);
    let descriptor = echo_reply_descriptor_set();

    let view = grpc_view(
        &body,
        &response.trailers,
        Some((descriptor.as_slice(), "grpc.test.Echo")),
    )
    .expect("a recorded message decodes against the caller's descriptor");
    assert_eq!(view.messages.len(), 1);
    let decoded = view.messages[0]
        .decoded
        .as_ref()
        .expect("a supplied descriptor must decode the message");
    assert_eq!(
        decoded.get("message").and_then(|value| value.as_str()),
        Some("hello")
    );
    assert_eq!(
        decoded.get("ordinal").and_then(|value| value.as_u64()),
        Some(1)
    );

    // Determinism: the same bytes and the same descriptor give the same view.
    let again = grpc_view(
        &body,
        &response.trailers,
        Some((descriptor.as_slice(), "grpc.test.Echo")),
    )
    .expect("view");
    assert_eq!(
        serde_json::to_value(&view).expect("json"),
        serde_json::to_value(&again).expect("json")
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// Malformed protobuf, an unknown message name, and an oversized descriptor
/// are *derived-view* errors. The fixture must be untouched by all three.
#[tokio::test]
async fn descriptor_and_envelope_failures_do_not_corrupt_the_fixture() {
    let (published, directory, _server) = acquire_unary("grpc-bounds", "hello").await;
    let flow = &published_flows(&published)[0];
    let response = response_of(flow);
    let body = stored_body(&published, flow);
    let body_before = body.clone();
    let descriptor = echo_reply_descriptor_set();

    // An unknown message name. `decode_grpc_payload` is the strict API and
    // fails closed; `grpc_view` is the lenient projection and reports the
    // failure by *not* decoding. The distinction is deliberate — a caller that
    // asked for a schema gets an error, a caller that only wanted an envelope
    // summary gets a summary — and both must leave the fixture alone.
    let unknown = decode_grpc_payload(
        &frames_payload(&body),
        &descriptor,
        "grpc.test.NoSuchMessage",
    );
    assert!(
        matches!(unknown, Err(GrpcError::UnknownMessage(_))),
        "the strict API must fail closed on an unknown message, got {unknown:?}"
    );
    let lenient = grpc_view(
        &body,
        &response.trailers,
        Some((descriptor.as_slice(), "grpc.test.NoSuchMessage")),
    )
    .expect("the lenient projection still reports the envelope");
    assert_eq!(lenient.messages.len(), 1);
    assert!(
        lenient.messages[0].decoded.is_none(),
        "a failed decode must produce no decoded value, never a wrong one"
    );

    // An oversized descriptor.
    let oversized = vec![0u8; GRPC_MAX_DESCRIPTOR_BYTES + 1];
    let too_large = decode_grpc_payload(&frames_payload(&body), &oversized, "grpc.test.Echo");
    assert!(
        matches!(too_large, Err(GrpcError::DescriptorTooLarge)),
        "an oversized descriptor must fail closed, got {too_large:?}"
    );

    // A malformed descriptor.
    let malformed = decode_grpc_payload(&frames_payload(&body), &[0xFFu8; 8], "grpc.test.Echo");
    assert!(
        matches!(malformed, Err(GrpcError::DescriptorInvalid(_))),
        "a malformed descriptor must fail closed, got {malformed:?}"
    );

    // A descriptor at exactly the limit is still accepted, so the bound is a
    // bound and not an accident of padding.
    let at_limit = vec![0u8; GRPC_MAX_DESCRIPTOR_BYTES];
    let at_limit = decode_grpc_payload(&frames_payload(&body), &at_limit, "grpc.test.Echo");
    assert!(
        !matches!(at_limit, Err(GrpcError::DescriptorTooLarge)),
        "a descriptor exactly at the limit must not be rejected for its size"
    );

    // A frame count past the cap is rejected as a derived-view error.
    let many = vec![0u8; 5 * (GRPC_MAX_FRAMES + 1)];
    assert!(
        matches!(parse_grpc_frames(&many), Err(GrpcError::TooManyFrames)),
        "the envelope cap must be enforced on real traffic"
    );

    // A malformed *envelope* is also a derived-view error, and the raw body is
    // still readable afterwards.
    let mut truncated = body.clone();
    truncated.truncate(3);
    assert!(
        matches!(parse_grpc_frames(&truncated), Err(GrpcError::Truncated)),
        "a truncated envelope must be reported, not guessed at"
    );
    let mut overrunning = body.clone();
    overrunning[1..5].copy_from_slice(&9_000u32.to_be_bytes());
    assert!(
        matches!(parse_grpc_frames(&overrunning), Err(GrpcError::Overrun)),
        "an overrunning envelope must be reported, not guessed at"
    );

    // The fixture itself is unchanged: the stored body is byte-identical and
    // the session still validates.
    assert_eq!(
        stored_body(&published, flow),
        body_before,
        "a failed projection must not modify the recorded body"
    );
    flow.validate().expect("the recorded flow still validates");
    assert_eq!(
        published
            .iter_flows()
            .expect("iterable")
            .collect::<Result<Vec<_>, _>>()
            .expect("readable")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Candidate regression and derived diagnostics
// ---------------------------------------------------------------------------

fn eggfetch_h2(identity: &TestCert) -> eggfetch_core::Client {
    let tls = eggfetch_core::tls::TlsConfig::builder()
        .ca_certificate_pem(identity.ca_pem.as_bytes())
        .expect("valid test CA")
        .build();
    eggfetch_core::Client::builder()
        .http_version_policy(eggfetch_core::HttpVersionPolicy::Http2Only)
        .retry_canceled_requests(false)
        .tls_config(tls)
        .build()
}

/// A gRPC candidate is regressed like any other flow, and the derived view is
/// identical on both sides.
///
/// The report is the existing authority and is not gRPC-aware — which is the
/// point. gRPC semantics live in the raw body and trailers, so the ordinary
/// report already covers them. The derived view is then *additional*
/// diagnostics computed from the same bytes on both sides.
#[tokio::test]
async fn grpc_candidate_regression_is_clean_and_derives_the_same_view() {
    let (published, directory, server) = acquire_unary("grpc-regression", "hello").await;
    let flows = published_flows(&published);
    let flow = &flows[0];
    let baseline_body = stored_body(&published, flow);
    let descriptor = echo_reply_descriptor_set();

    // The request body *is* the gRPC envelope, so a candidate that omits it is
    // not a candidate for this flow — the server answers a trailers-only
    // decode error instead. Supplying the recorded bytes is the whole point.
    let baseline_request_body = match &flow.request.body {
        BodyRef::Blob(blob) => published.read_blob(blob).expect("request body"),
        other => panic!("a gRPC request body must be stored, got {other:?}"),
    };
    let request_frames = parse_grpc_frames(&baseline_request_body).expect("request envelope");
    assert_eq!(request_frames.len(), 1);
    let request_message = EchoRequest::decode(request_frames[0].payload.as_slice())
        .expect("request payload is a protobuf EchoRequest");
    assert_eq!(request_message.message, "hello");

    let client = eggfetch_h2(&server.cert);
    let target: http::Uri = server.base().parse().expect("target");
    let observation = execute_candidate(
        &client,
        &flow.request,
        &baseline_request_body,
        &target,
        16 << 20,
        Some(direct_route()),
    )
    .await
    .expect("candidate executes against the live gRPC server");

    let report = compare_flows(
        &without_volatile_date(flow),
        &without_volatile_date(&observation.flow),
        &baseline_body,
        &observation.response_body,
        ReportScheduler::Sequential,
    );
    assert!(
        report.is_success(),
        "a gRPC candidate must compare clean against its own baseline, got {:?}",
        report.findings
    );

    // The derived view, computed independently from each side's own bytes and
    // trailers, must agree.
    let baseline_view = grpc_view(
        &baseline_body,
        &response_of(flow).trailers,
        Some((descriptor.as_slice(), "grpc.test.Echo")),
    )
    .expect("baseline view");
    let candidate_response = response_of(&observation.flow);
    let candidate_view = grpc_view(
        &observation.response_body,
        &candidate_response.trailers,
        Some((descriptor.as_slice(), "grpc.test.Echo")),
    )
    .expect("candidate view");
    assert_eq!(
        serde_json::to_value(&baseline_view).expect("json"),
        serde_json::to_value(&candidate_view).expect("json"),
        "the derived gRPC view must be identical on both sides of a clean regression"
    );
    assert_eq!(baseline_view.status.map(|status| status.code), Some(0));
    assert_eq!(
        baseline_view.messages[0]
            .decoded
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(|value| value.as_str()),
        Some("hello")
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Redaction applies before persistence, and the derived view cannot resurrect
/// a redacted secret.
///
/// The second half is the part that is easy to get wrong: a derived view built
/// from a *redacted* body must not contain the value, and a caller-supplied
/// descriptor must not be able to reconstruct it from a protobuf field the
/// redactor already removed.
#[tokio::test]
async fn redaction_precedes_persistence_and_the_view_resurrects_nothing() {
    const SECRET: &str = "super-secret-token-value";
    let server = start_grpc_server("grpc-redaction-upstream").await;
    let gateway = start_gateway("grpc-redaction", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;

    let mut request = grpc_request(EchoRequest::new("hello"), "Unary");
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {SECRET}").parse().expect("ascii"),
    );
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))
        .expect("ready");
    let _ = grpc
        .unary(
            request,
            http::uri::PathAndQuery::from_static(UNARY_PATH),
            ClientCodec::default(),
        )
        .await
        .expect("unary");

    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    let descriptor = echo_reply_descriptor_set();

    // Nothing in the persisted flow carries the secret.
    let persisted = format!("{:?}", flow);
    assert!(
        !persisted.contains(SECRET),
        "the secret must not survive redaction into the published flow"
    );
    assert!(
        !header(&flow.request.headers, "authorization")
            .unwrap_or_default()
            .contains(SECRET),
        "the authorization header must be redacted before publication"
    );
    assert!(
        flow.redactions
            .iter()
            .any(|entry| entry.field.contains("authorization")),
        "the redaction must itself be recorded, got {:?}",
        flow.redactions
    );

    // And the derived view, built from the redacted fixture, does not contain
    // it either.
    let view = grpc_view(
        &stored_body(&published, flow),
        &response_of(flow).trailers,
        Some((descriptor.as_slice(), "grpc.test.Echo")),
    )
    .expect("view");
    let rendered = format!("{view:?}");
    assert!(
        !rendered.contains(SECRET),
        "a derived view must not resurrect a redacted secret: {rendered}"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// The compressed flag
// ---------------------------------------------------------------------------

/// The compressed flag is reported, and nothing is decompressed.
///
/// Sent from the raw `h2` crate rather than Tonic, because the claim is about
/// what EggReplay does with a flag it did not set: the payload is a *valid*
/// protobuf message, so an implementation that silently decompressed would
/// both change the bytes and start decoding. Neither happens.
#[tokio::test]
async fn the_compressed_flag_is_reported_and_nothing_is_decompressed() {
    let server = start_grpc_server("grpc-compression-upstream").await;
    let gateway = start_gateway("grpc-compression", &server, &server.cert).await;

    // Hand-frame a gRPC request: 1-byte compressed flag, 4-byte big-endian
    // length, then a real, decodable EchoRequest.
    let payload = EchoRequest::new("hello").encode_to_vec();
    let mut body = vec![0x01u8];
    body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    body.extend_from_slice(&payload);

    // An unimplemented method keeps the upstream out of the claim: the
    // recorded *request* is the subject, and this path is answered with a
    // clean `Unimplemented` status.
    let tcp = tokio::net::TcpStream::connect(gateway.address)
        .await
        .expect("connect");
    let (mut sender, connection) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/grpc.test.Echo/Compression")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .expect("h2 request");
    // `false` leaves the stream open: a gRPC request carries DATA after its
    // headers, and ending the stream there would be a framing error.
    let (response, mut stream) = sender.send_request(request, false).expect("send");
    stream
        .send_data(Bytes::from(body.clone()), true)
        .expect("send data");
    let _ = response.await.expect("response");

    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    let recorded = match &flow.request.body {
        BodyRef::Blob(blob) => published.read_blob(blob).expect("request body"),
        other => panic!("the request body must be stored, got {other:?}"),
    };
    assert_eq!(
        recorded, body,
        "the recorded request body must be the exact bytes that arrived"
    );

    let frames = parse_grpc_frames(&recorded).expect("one well-formed envelope");
    assert_eq!(frames.len(), 1);
    assert!(
        frames[0].compressed,
        "the compressed flag is a wire fact and must be reported as set"
    );
    assert_eq!(
        frames[0].payload, payload,
        "the payload must be reported verbatim, never decompressed"
    );

    // The lenient view reports the flag and refuses to decode a frame it
    // cannot read.
    let view = grpc_view(
        &recorded,
        &response_of(flow).trailers,
        Some((echo_reply_descriptor_set().as_slice(), "grpc.test.Echo")),
    )
    .expect("view");
    assert_eq!(view.messages.len(), 1);
    assert!(view.messages[0].compressed);
    assert!(
        view.messages[0].decoded.is_none(),
        "a compressed frame must not be decoded, even when the payload happens to be a valid message"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A cancelled gRPC call is an observable HTTP/2 stream outcome, not a
/// corrupted fixture.
///
/// The client drops a long server stream mid-flight. Two things must hold: the
/// reset is local to that stream — a sibling call on the same connection still
/// completes — and whatever the gateway publishes still validates. A cancelled
/// stream is allowed to produce no flow, or one error flow; what it must not
/// do is publish half a message.
#[tokio::test]
async fn cancellation_is_an_observable_h2_stream_outcome() {
    let server = start_grpc_server("grpc-cancel-upstream").await;
    let gateway = start_gateway("grpc-cancel", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;

    let mut stream = server_stream(&channel, EchoRequest::with_count("tick", 64))
        .await
        .expect("server stream");
    let first = stream
        .message()
        .await
        .expect("stream message")
        .expect("a first message arrives");
    assert_eq!(first.ordinal, 1);
    // Dropping the response stream resets the HTTP/2 stream. The 63 unread
    // messages are deliberately never observed.
    drop(stream);

    // The connection must survive: a sibling call on the same channel
    // completes normally.
    let reply = unary(&channel, EchoRequest::new("after-cancel"))
        .await
        .expect("a sibling call must complete after a stream reset");
    assert_eq!(reply.message, "after-cancel");

    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    for flow in &flows {
        flow.validate().expect("every published flow must validate");
        // Whatever the gateway chose to record, no flow may claim a partially
        // consumed message.
        let response = response_of(flow);
        if !response.body.is_empty() {
            let body = stored_body(&published, flow);
            parse_grpc_frames(&body)
                .unwrap_or_else(|error| panic!("a published body must be whole: {error:?}"));
        }
    }
    assert!(
        flows.iter().any(|flow| flow.request.path == UNARY_PATH),
        "the completed sibling call must be recorded"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Streaming classes
// ---------------------------------------------------------------------------

/// Client streaming is representable: several request envelopes arrive as one
/// stored request body, and the call's single response replays intact.
#[tokio::test]
async fn client_streaming_is_representable_in_the_canonical_model() {
    let server = start_grpc_server("grpc-clientstream-upstream").await;
    let gateway = start_gateway("grpc-clientstream", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    let reply = client_stream(
        &channel,
        vec![
            EchoRequest::new("a"),
            EchoRequest::new("b"),
            EchoRequest::new("c"),
        ],
    )
    .await
    .expect("client stream");
    assert_eq!(reply.message, "c/3");
    assert_eq!(reply.ordinal, 3);

    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    assert_eq!(flow.request.path, CLIENT_STREAM_PATH);
    let request_body = match &flow.request.body {
        BodyRef::Blob(blob) => published.read_blob(blob).expect("request body"),
        other => panic!("the streamed request body must be stored, got {other:?}"),
    };
    let frames = parse_grpc_frames(&request_body).expect("three well-formed envelopes");
    assert_eq!(
        frames.len(),
        3,
        "a client stream's messages are one stored body with one envelope each"
    );
    let mut messages = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.index, index);
        messages.push(
            EchoRequest::decode(frame.payload.as_slice())
                .expect("EchoRequest")
                .message,
        );
    }
    assert_eq!(messages, vec!["a", "b", "c"]);

    // The response is one envelope, and it replays.
    let response_body = stored_body(&published, flow);
    let response_frames = parse_grpc_frames(&response_body).expect("response envelope");
    assert_eq!(response_frames.len(), 1);

    let recorded_authority = flow.request.authority.clone();
    let (replay, identity) = start_replay("grpc-clientstream-listener", &directory).await;
    let channel = tonic_client_at(&identity, &recorded_authority, replay.address).await;
    let replayed = client_stream(
        &channel,
        vec![
            EchoRequest::new("a"),
            EchoRequest::new("b"),
            EchoRequest::new("c"),
        ],
    )
    .await
    .expect("replayed client stream");
    assert_eq!(replayed, reply, "a client-streaming call replays intact");

    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

/// An un-terminated bidirectional call is recorded faithfully, and replays
/// faithfully.
///
/// The gateway forwards a streaming request body, so a bidirectional call gets
/// somewhere: the server replies to each message as it arrives, and the client
/// sees those replies. What never happens is a *terminal* status, because the
/// client never half-closes and the outbound timeout eventually ends the call.
///
/// M015D deferred this case on the belief that a replay of such a fixture
/// "would be worse than not replaying it". M017 found that premise did not hold
/// and rewrote this test to prove the opposite:
///
/// - The gateway is already full-duplex (hyper's `ResponseFuture` resolves on
///   response *headers* while the connection task pumps the request body), so
///   the transport was never the blocker.
/// - The cut-off is already recorded: the deadline surfaces as a response-body
///   error, which the recorder turns into a terminal `Error` stream event and
///   suppresses the `End` event. M015D only ever asserted on trailers.
/// - Replay is safe rather than dangerous, and this test shows it by
///   observing a real client rather than predicting one. M017's research had
///   predicted `Code::Unknown`, reasoning that replay would serve a clean 200
///   with no `grpc-status` and that a gRPC client maps that to Unknown.
///   Observing it disproved that specific prediction: replay reproduces the
///   recorded terminal `Error` stream event as a broken stream, so the client
///   sees `Internal`. The conclusion survives — the client sees a failure and
///   never a false success — and the live-versus-replay code difference is
///   documented at the assertion.
///
/// So the call's *ending* is not recorded because it had none, and its partial
/// progress is recorded whole. That is a truthful fixture, and this test is what
/// makes the support-matrix row "supported" rather than "deferred" load-bearing
/// rather than a matter of opinion.
#[tokio::test]
async fn an_unterminated_bidi_call_records_its_cut_off_and_replays_faithfully() {
    let server = start_grpc_server("grpc-bidi-upstream").await;
    let gateway = start_gateway_with_timeout(
        "grpc-bidi",
        &server,
        &server.cert,
        Some(std::time::Duration::from_secs(2)),
    )
    .await;

    // A raw H2 peer keeps its request half open, which is what a real
    // bidirectional client does. Half-closing first would prove nothing.
    let tcp = tokio::net::TcpStream::connect(gateway.address)
        .await
        .expect("connect");
    let (mut sender, connection) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("http://localhost{BIDI_PATH}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .expect("h2 request");
    let (response, mut stream) = sender.send_request(request, false).expect("send");
    let payload = EchoRequest::new("one").encode_to_vec();
    let mut frame = vec![0u8];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    stream
        .send_data(Bytes::from(frame), false)
        .expect("send request message");
    // Deliberately no half-close.

    let response = response.await.expect("the gateway must answer, not hang");
    let (parts, mut recv) = response.into_parts();
    let mut body = Vec::new();
    while let Some(chunk) = recv.data().await {
        body.extend_from_slice(&chunk.expect("body chunk"));
    }
    let trailers = header_entries(recv.trailers().await.expect("trailers read").as_ref());

    assert_eq!(parts.status, 200);
    // The reply the server did send arrives intact.
    let frames = parse_grpc_frames(&body).expect("the replies that did arrive are whole");
    assert_eq!(frames.len(), 1);
    let reply = EchoReply::decode(frames[0].payload.as_slice()).expect("EchoReply");
    assert_eq!(reply.message, "bidi:one");
    assert_eq!(reply.ordinal, 1);
    // And there is no terminal status, which is the whole finding.
    assert!(
        grpc_status_from_trailers(&trailers).is_none(),
        "an un-terminated call must carry no grpc-status, got {trailers:?}"
    );

    // The connection survives: a sibling call completes normally.
    let channel = tonic_client_cleartext(gateway.address).await;
    let sibling = unary(&channel, EchoRequest::new("after-bidi"))
        .await
        .expect("a sibling call must complete after an un-completable bidi call");
    assert_eq!(sibling.message, "after-bidi");

    // The recorded call is a valid flow with a whole envelope, and its missing
    // terminal status is what tells a reader the call never finished.
    let (published, directory) = gateway.finalize().await;
    let flows = published_flows(&published);
    let bidi_flow = flows
        .iter()
        .find(|flow| flow.request.path == BIDI_PATH)
        .expect("the bidi call must be recorded");
    bidi_flow.validate().expect("a recorded flow must validate");
    let response = response_of(bidi_flow);
    assert_eq!(response.status, 200);
    let recorded = stored_body(&published, bidi_flow);
    assert_eq!(
        parse_grpc_frames(&recorded)
            .expect("the recorded envelope is whole")
            .len(),
        1
    );
    assert!(
        grpc_status_from_trailers(&response.trailers).is_none(),
        "the missing terminal status must be visible in the fixture, so a reader can tell the call never completed"
    );

    // The *other* half of the truth: the fixture records why the call stopped.
    // M015D's deferral rested on the belief that nothing recorded the cut-off
    // beyond the absent trailer. It did — the outbound deadline surfaces as a
    // response-body error, which becomes a terminal `Error` stream event and
    // suppresses the `End` event. Before M017 this category was hardcoded to
    // "other", so a deadline looked identical to a reset.
    let stream_events: eggreplay_core::StreamEvents = serde_json::from_slice(
        &published
            .read_extension("stream-events")
            .expect("read extension")
            .expect("a gateway-acquired session records stream events"),
    )
    .expect("stream events decode");
    stream_events
        .validate()
        .expect("an interrupted stream must still produce valid stream events");
    let bidi_events = stream_events
        .flows
        .iter()
        .find(|flow| flow.flow_id == bidi_flow.id)
        .expect("the bidi call must carry stream events");
    let terminal = bidi_events
        .response
        .last()
        .expect("a terminal event is recorded");
    match &terminal.event {
        eggreplay_core::StreamEventKind::Error {
            category, phase, ..
        } => {
            assert_eq!(
                category, "timeout",
                "an outbound deadline must be recorded as a timeout, not as an unnamed failure"
            );
            assert_eq!(phase, "timeout");
        }
        other => panic!("an un-terminated call must record a terminal error, got {other:?}"),
    }
    assert!(
        !bidi_events
            .response
            .iter()
            .any(|event| matches!(event.event, eggreplay_core::StreamEventKind::End)),
        "a cut-off call must not also record a clean End"
    );

    // And the replay is faithful. A real gRPC client receiving HTTP 200 with no
    // `grpc-status` is required by the spec to treat the call as having no
    // final status; tonic reports that as `Code::Unknown` with a protocol
    // diagnostic. That is the same class of outcome the live client saw when
    // the gateway timeout cut the call, which is what makes replaying this
    // fixture honest rather than harmful.
    let recorded_authority = bidi_flow.request.authority.clone();
    // `practical`, not `strict`: the request was recorded from a raw H2 peer
    // that sent no `user-agent`, so a Tonic client differs by that one volatile
    // header. Everything else — path, scheme, authority, and the exact request
    // body — must still match for the replay to be served at all.
    let (replay, identity) = start_replay_with_matcher(
        "grpc-bidi-unterminated-listener",
        &directory,
        Matcher::practical(8),
    )
    .await;
    let channel = tonic_client_at(&identity, &recorded_authority, replay.address).await;
    let mut replayed = bidi(&channel, vec![EchoRequest::new("one")])
        .await
        .expect("the recorded call must be selectable for replay");
    let mut replay_messages = Vec::new();
    let replay_terminal = loop {
        match replayed.message().await {
            Ok(Some(message)) => replay_messages.push(message.message),
            Ok(None) => break None,
            Err(status) => break Some(status),
        }
    };
    // Both facts are reported together: a status-less replay is only
    // meaningful alongside whatever the client did with the body first.
    assert_eq!(
        replay_messages,
        vec!["bidi:one"],
        "the partial progress the call made is replayed whole (terminal: {:?})",
        replay_terminal
            .as_ref()
            .map(|status| status.message().to_string())
    );
    let replay_terminal = replay_terminal.expect(
        "a replayed call with no recorded terminal status must not look complete to a gRPC client",
    );
    // The safety property, and the one that decides the support-matrix row: a
    // gRPC client must NOT observe a successful call.
    //
    // M017's research predicted `Code::Unknown`, on the theory that replay
    // would serve a clean 200 with no `grpc-status` and that tonic maps that
    // to Unknown. Observing it disproved the prediction and kept the
    // conclusion. Replay does not serve a clean end: it reproduces the
    // recorded terminal `Error` stream event as a broken stream
    // (`replay.rs:832-845` -> `TimedStreamStep::Error`), so the client's body
    // read fails and tonic reports the h2 error as `Internal`.
    //
    // So the live and replayed clients see different *codes* — and that
    // asymmetry is recorded rather than papered over:
    //
    // - Live: the gateway ends the downstream response cleanly after the
    //   outbound timeout, so the client gets 200 + partial body + no trailers,
    //   which tonic reports as `Unknown`.
    // - Replay: the recorded stream events say the *outbound* leg was cut off,
    //   and replay applies that termination to the downstream response too, so
    //   the client sees the body read fail.
    //
    // Both are failures, and neither is a false success. The recorded
    // truncation is upstream reality; whether replay should reproduce a
    // downstream *client experience* different from that is a separate
    // question this milestone does not open.
    assert_eq!(
        replay_terminal.code(),
        Code::Internal,
        "replay reproduces the recorded truncation, so the body read fails (message: {})",
        replay_terminal.message()
    );
    assert_ne!(
        replay_terminal.code(),
        Code::Ok,
        "a call with no recorded terminal status must never replay as success"
    );
    replay.close().await;

    let _ = std::fs::remove_dir_all(&directory);
}

/// A bidirectional call that *does* terminate — the client half-closes — is
/// recorded and replayed normally.
///
/// This is the boundary that makes the deferral above a scoped one rather than
/// a blanket refusal. The canonical model has no problem with a bidirectional
/// call that ends; what it cannot express is a call whose two directions have
/// no shared ending, and the gateway cannot manufacture one.
#[tokio::test]
async fn a_terminated_bidi_call_records_and_replays_normally() {
    let server = start_grpc_server("grpc-bidi-terminated-upstream").await;
    let gateway = start_gateway("grpc-bidi-terminated", &server, &server.cert).await;
    let channel = tonic_client_cleartext(gateway.address).await;
    let mut stream = bidi(
        &channel,
        vec![EchoRequest::new("one"), EchoRequest::new("two")],
    )
    .await
    .expect("bidi");
    let mut messages = Vec::new();
    let terminal = loop {
        match stream.message().await {
            Ok(Some(message)) => messages.push(message.message),
            Ok(None) => break None,
            Err(status) => break Some(status),
        }
    };
    assert_eq!(messages, vec!["bidi:one", "bidi:two"]);
    let terminal = terminal.expect("a terminated bidi call has a terminal status");
    assert_eq!(terminal.code(), Code::Cancelled);

    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    flow.validate().expect("a recorded flow must validate");
    let response = response_of(flow);
    let recorded = stored_body(&published, flow);
    let frames = parse_grpc_frames(&recorded).expect("response envelopes");
    assert_eq!(frames.len(), 2, "both replies are recorded, in order");
    let status = grpc_status_from_trailers(&response.trailers).expect("grpc-status recorded");
    assert_eq!(
        status.code,
        Code::Cancelled as u32,
        "the terminal status of a terminated call is recorded like any other"
    );

    // And it replays to a real client.
    let recorded_authority = flow.request.authority.clone();
    let (replay, identity) = start_replay("grpc-bidi-terminated-listener", &directory).await;
    let channel = tonic_client_at(&identity, &recorded_authority, replay.address).await;
    let mut replayed = bidi(
        &channel,
        vec![EchoRequest::new("one"), EchoRequest::new("two")],
    )
    .await
    .expect("replayed bidi");
    let mut replay_messages = Vec::new();
    let replay_terminal = loop {
        match replayed.message().await {
            Ok(Some(message)) => replay_messages.push(message.message),
            Ok(None) => break None,
            Err(status) => break Some(status),
        }
    };
    assert_eq!(replay_messages, messages, "both replies replay in order");
    assert_eq!(
        replay_terminal.map(|status| status.code()),
        Some(Code::Cancelled),
        "the terminal status replays too"
    );

    replay.close().await;
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Not gRPC
// ---------------------------------------------------------------------------

/// Ordinary HTTP/2 traffic is recorded normally and is never projected as
/// gRPC.
///
/// Driven through the same gateway as the gRPC tests, with a JSON content type
/// and a body that is not an envelope. The claim is that the gRPC view is
/// strictly a caller-side projection: nothing in recording inspects
/// `content-type` and nothing rewrites, truncates, or rejects the flow.
#[tokio::test]
async fn non_grpc_traffic_is_recorded_normally_and_not_projected_as_grpc() {
    let server = start_grpc_server("grpc-non-grpc-upstream").await;
    let gateway = start_gateway("grpc-non-grpc", &server, &server.cert).await;

    let tcp = tokio::net::TcpStream::connect(gateway.address)
        .await
        .expect("connect");
    let (mut sender, connection) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/api/thing")
        .header("content-type", "application/json")
        .body(())
        .expect("h2 request");
    let (response, mut stream) = sender.send_request(request, false).expect("send");
    stream
        .send_data(Bytes::from_static(b"{\"not\":\"grpc\"}"), true)
        .expect("send");
    let _ = response.await.expect("response");

    let (published, directory) = gateway.finalize().await;
    let flow = &published_flows(&published)[0];
    flow.validate().expect("a recorded flow must validate");
    let recorded_response = response_of(flow);

    // The flow is an ordinary HTTP record: the request body is stored exactly
    // as it arrived.
    assert_eq!(flow.request.method, "POST");
    assert_eq!(flow.request.path, "/api/thing");
    let request_body = match &flow.request.body {
        BodyRef::Blob(blob) => published.read_blob(blob).expect("request body"),
        other => panic!("a non-empty request body must be stored, got {other:?}"),
    };
    assert_eq!(
        request_body, br#"{"not":"grpc"}"#,
        "the body is stored verbatim"
    );

    // The upstream is a Tonic server, so it answers an unknown path with
    // `Unimplemented` — which carries a *gRPC* content-type even though the
    // call was never gRPC. That is worth stating plainly: the M014D gate
    // recognises a gRPC **response framing**, not a gRPC request, so a
    // non-gRPC call can legitimately come back with a gRPC content-type.
    let request_content_type =
        header(&flow.request.headers, "content-type").expect("request content-type");
    assert_eq!(request_content_type, "application/json");
    assert!(
        !is_grpc_content_type(Some(request_content_type)),
        "the call itself was not gRPC, and the gate must say so about the request"
    );
    assert!(!is_grpc_content_type(None));
    assert!(!is_grpc_content_type(Some("APPLICATION/GRPC")));

    // Asking for a view of a non-envelope body is a derived-view error, not a
    // guess.
    let view = grpc_view(
        &request_body,
        &recorded_response.trailers,
        Some((echo_reply_descriptor_set().as_slice(), "grpc.test.Echo")),
    );
    assert!(
        matches!(
            view,
            Err(GrpcError::Truncated | GrpcError::Overrun | GrpcError::TooManyFrames)
        ),
        "a non-gRPC body must fail as a derived-view error, got {view:?}"
    );

    // And a gRPC-shaped content-type on an empty body still yields no
    // messages: recognition alone never invents envelopes.
    assert_eq!(
        header(&recorded_response.headers, "content-type"),
        Some("application/grpc"),
        "Tonic's Unimplemented answer carries a gRPC content-type"
    );
    let response_view = grpc_view(
        &stored_body(&published, flow),
        &recorded_response.trailers,
        Some((echo_reply_descriptor_set().as_slice(), "grpc.test.Echo")),
    )
    .expect("an empty body is a view with no messages, not an error");
    assert!(
        response_view.messages.is_empty(),
        "recognition must never invent envelopes, got {:?}",
        response_view.messages
    );

    let _ = std::fs::remove_dir_all(&directory);
}
