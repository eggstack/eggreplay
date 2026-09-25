//! M006 transport isolation: graceful-vs-abrupt TLS shutdown and
//! raw-`rustls`/minimal-`Hyper` bisection for the Windows large-response
//! truncation.
//!
//! Background: on Windows runners, a 131190-byte (and 300118-byte) TLS
//! response delivered only `floor(total / 64 KiB) * 64 KiB` bytes before the
//! stream errored, while the origin provably wrote every byte. The loss was
//! identical with and without the proxy, so these tests bisect the remaining
//! layers without touching production transport code:
//!
//! - raw `rustls` byte transfer with graceful (`close_notify`) vs abrupt
//!   (flush + drop, no alert) server shutdown;
//! - minimal `Hyper` H1 client/server over plaintext (no TLS at all);
//! - minimal `Hyper` H1 client/server over `rustls` (no `EggFetch` glue).
//!
//! Hermetic and local-only: every listener, certificate, and client is a
//! loopback construct. No Internet access occurs. Each test asserts exact
//! delivery; a platform-specific failure here names the faulty layer.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1 as client1;
use hyper::server::conn::http1 as server1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const TIMEOUT: Duration = Duration::from_secs(15);
const RAW_128K_TOTAL: usize = 131_190;
const RAW_300K_TOTAL: usize = 300_118;
const SMALL_TOTAL: usize = 1000;
const HYPER_BODY_LARGE: usize = 128 * 1024;
const HYPER_BODY_SMALL: usize = 1024;

struct TlsIdentity {
    cert_der: CertificateDer<'static>,
    key_der: PrivateKeyDer<'static>,
}

fn make_identity() -> TlsIdentity {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("test identity generates");
    TlsIdentity {
        cert_der: CertificateDer::from(generated.cert.der().to_vec()),
        key_der: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der())),
    }
}

fn server_config(identity: &TlsIdentity) -> Arc<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions resolve")
            .with_no_client_auth()
            .with_single_cert(
                vec![identity.cert_der.clone()],
                identity.key_der.clone_key(),
            )
            .expect("test certificate loads"),
    )
}

fn client_config(identity: &TlsIdentity) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(identity.cert_der.clone()).expect("root loads");
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions resolve")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Raw `rustls` transfer: server writes `payload_len` bytes after a 3-byte
/// rendezvous, then ends the stream gracefully (`close_notify`) or abruptly
/// (flush + drop, TCP FIN without the alert).
///
/// Returns the bytes the client collected plus any terminal read error. The
/// error kind is recorded for forensics; only the byte count is asserted.
async fn run_raw_case(payload_len: usize, graceful: bool) -> (Vec<u8>, Option<String>) {
    let identity = make_identity();
    let server_tls = server_config(&identity);
    let client_tls = client_config(&identity);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("raw listener binds");
    let addr = listener.local_addr().expect("listener address");
    let payload = vec![0xABu8; payload_len];
    let server_payload = payload.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("raw accept");
        let mut tls = TlsAcceptor::from(server_tls)
            .accept(socket)
            .await
            .expect("raw handshake");
        let mut go = [0u8; 3];
        tls.read_exact(&mut go).await.expect("rendezvous read");
        assert_eq!(&go, b"go!");
        tls.write_all(&server_payload)
            .await
            .expect("raw payload writes");
        if graceful {
            tls.shutdown().await.expect("close_notify sends");
        } else {
            tls.flush().await.expect("session flushes");
            // Drop without `close_notify`: abrupt TLS shutdown over a
            // graceful TCP FIN. Every flushed byte must still arrive.
        }
    });
    let socket = TcpStream::connect(addr).await.expect("raw connect");
    let name = ServerName::try_from("localhost").expect("valid name");
    let mut tls = TlsConnector::from(client_tls)
        .connect(name, socket)
        .await
        .expect("raw client handshake");
    tls.write_all(b"go!").await.expect("rendezvous write");
    let (collected, terminal) = tokio::time::timeout(TIMEOUT, async {
        let mut out = Vec::new();
        let mut terminal = None;
        loop {
            let mut chunk = vec![0u8; 8192];
            match tls.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(error) => {
                    terminal = Some(error.to_string());
                    break;
                }
            }
        }
        (out, terminal)
    })
    .await
    .expect("raw exchange completes");
    server.abort();
    eprintln!(
        "raw diag: payload={payload_len} graceful={graceful} delivered={} terminal={terminal:?}",
        collected.len(),
    );
    (collected, terminal)
}

fn assert_full_delivery(collected: &[u8], expected: usize, fill: u8, context: &str) {
    assert_eq!(
        collected.len(),
        expected,
        "{context}: byte count must be exact"
    );
    assert!(
        collected.iter().all(|byte| *byte == fill),
        "{context}: payload bytes must be intact"
    );
}

#[tokio::test]
async fn raw_tls_graceful_shutdown_delivers_1024() {
    let (collected, terminal) = run_raw_case(SMALL_TOTAL, true).await;
    assert!(terminal.is_none(), "clean EOF expected, got {terminal:?}");
    assert_full_delivery(&collected, SMALL_TOTAL, 0xAB, "graceful/1K");
}

#[tokio::test]
async fn raw_tls_graceful_shutdown_delivers_131190() {
    let (collected, terminal) = run_raw_case(RAW_128K_TOTAL, true).await;
    assert!(terminal.is_none(), "clean EOF expected, got {terminal:?}");
    assert_full_delivery(&collected, RAW_128K_TOTAL, 0xAB, "graceful/128K");
}

#[tokio::test]
async fn raw_tls_graceful_shutdown_delivers_300118() {
    let (collected, terminal) = run_raw_case(RAW_300K_TOTAL, true).await;
    assert!(terminal.is_none(), "clean EOF expected, got {terminal:?}");
    assert_full_delivery(&collected, RAW_300K_TOTAL, 0xAB, "graceful/300K");
}

#[tokio::test]
async fn raw_tls_abrupt_drop_delivers_131190() {
    let (collected, _terminal) = run_raw_case(RAW_128K_TOTAL, false).await;
    assert_full_delivery(&collected, RAW_128K_TOTAL, 0xAB, "abrupt/128K");
}

#[tokio::test]
async fn raw_tls_abrupt_drop_delivers_300118() {
    let (collected, _terminal) = run_raw_case(RAW_300K_TOTAL, false).await;
    assert_full_delivery(&collected, RAW_300K_TOTAL, 0xAB, "abrupt/300K");
}

#[tokio::test]
async fn hyper_plain_delivers_1024() {
    let body = hyper_plain_exchange(HYPER_BODY_SMALL).await;
    assert_full_delivery(&body, HYPER_BODY_SMALL, 0xCD, "hyper-plain/1K");
}

#[tokio::test]
async fn hyper_plain_delivers_131072() {
    let body = hyper_plain_exchange(HYPER_BODY_LARGE).await;
    assert_full_delivery(&body, HYPER_BODY_LARGE, 0xCD, "hyper-plain/128K");
}

#[tokio::test]
async fn hyper_tls_delivers_1024() {
    let body = hyper_tls_exchange(HYPER_BODY_SMALL).await;
    assert_full_delivery(&body, HYPER_BODY_SMALL, 0xCD, "hyper-tls/1K");
}

#[tokio::test]
async fn hyper_tls_delivers_131072() {
    let body = hyper_tls_exchange(HYPER_BODY_LARGE).await;
    assert_full_delivery(&body, HYPER_BODY_LARGE, 0xCD, "hyper-tls/128K");
}

/// Minimal `Hyper` H1 GET over plaintext loopback TCP.
async fn hyper_plain_exchange(body_len: usize) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("hyper listener binds");
    let addr = listener.local_addr().expect("listener address");
    let body = Arc::new(vec![0xCDu8; body_len]);
    let serve_body = Arc::clone(&body);
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("hyper accept");
        let service = service_fn(move |_req: Request<Incoming>| {
            let body = Arc::clone(&serve_body);
            async move { Ok::<_, hyper::Error>(Response::new(Full::new(Bytes::from(body.to_vec())))) }
        });
        server1::Builder::new()
            .serve_connection(TokioIo::new(socket), service)
            .await
    });
    let socket = TcpStream::connect(addr).await.expect("hyper connect");
    let (mut sender, connection) = client1::handshake(TokioIo::new(socket))
        .await
        .expect("hyper handshake");
    let driver = tokio::spawn(connection);
    let request = Request::builder()
        .method("GET")
        .uri("http://localhost/big")
        .body(Full::new(Bytes::new()))
        .expect("request builds");
    let response = tokio::time::timeout(TIMEOUT, sender.send_request(request))
        .await
        .expect("request completes")
        .expect("response arrives");
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let collected = response
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes()
        .to_vec();
    eprintln!(
        "hyper-plain diag: expected={body_len} delivered={}",
        collected.len(),
    );
    driver.abort();
    server.abort();
    collected
}

/// Minimal `Hyper` H1 GET over a `rustls` loopback stream (no `EggFetch`).
async fn hyper_tls_exchange(body_len: usize) -> Vec<u8> {
    let identity = make_identity();
    let server_tls = server_config(&identity);
    let client_tls = client_config(&identity);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("hyper-tls listener binds");
    let addr = listener.local_addr().expect("listener address");
    let body = Arc::new(vec![0xCDu8; body_len]);
    let serve_body = Arc::clone(&body);
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("hyper-tls accept");
        let tls = TlsAcceptor::from(server_tls)
            .accept(socket)
            .await
            .expect("hyper-tls handshake");
        let service = service_fn(move |_req: Request<Incoming>| {
            let body = Arc::clone(&serve_body);
            async move { Ok::<_, hyper::Error>(Response::new(Full::new(Bytes::from(body.to_vec())))) }
        });
        server1::Builder::new()
            .serve_connection(TokioIo::new(tls), service)
            .await
    });
    let socket = TcpStream::connect(addr).await.expect("hyper-tls connect");
    let name = ServerName::try_from("localhost").expect("valid name");
    let tls = TlsConnector::from(client_tls)
        .connect(name, socket)
        .await
        .expect("hyper-tls client handshake");
    let (mut sender, connection) = client1::handshake(TokioIo::new(tls))
        .await
        .expect("hyper handshake");
    let driver = tokio::spawn(connection);
    let request = Request::builder()
        .method("GET")
        .uri("https://localhost/big")
        .body(Full::new(Bytes::new()))
        .expect("request builds");
    let response = tokio::time::timeout(TIMEOUT, sender.send_request(request))
        .await
        .expect("request completes")
        .expect("response arrives");
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let collected = response
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes()
        .to_vec();
    eprintln!(
        "hyper-tls diag: expected={body_len} delivered={}",
        collected.len(),
    );
    driver.abort();
    server.abort();
    collected
}
