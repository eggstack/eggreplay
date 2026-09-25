//! M013E proxy statistics and shutdown/finalization tests.
//!
//! Starts a real explicit-proxy listener on loopback, drives one denied and
//! one allowed plain request plus one denied `CONNECT` over raw TCP, asserts
//! the [`ProxyStats`] counters, then covers the Ctrl-C shutdown/finalization
//! path programmatically (`shutdown`/`wait`/session `finish`).

use std::net::SocketAddr;
use std::time::Duration;

use eggreplay_core::SessionMetadata;
use eggreplay_intercept::{
    ConnectAction, ExplicitProxyConfig, FAILURE_POLICY_DENIED, HostMatch, PortMatch,
    ProxyListenerConfig, ProxyRoute, RequestKind, Rule, RuleAction, TargetPolicy, TunnelLimits,
    start_explicit_proxy,
};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const TIMEOUT: Duration = Duration::from_secs(10);

fn deny_all() -> TargetPolicy {
    TargetPolicy::new(Vec::new(), ConnectAction::Deny).unwrap()
}

fn allow_loopback() -> TargetPolicy {
    TargetPolicy::new(
        vec![Rule::new(
            HostMatch::exact_ip("127.0.0.1".parse().unwrap()),
            PortMatch::Any,
            RequestKind::Any,
            RuleAction::Allow,
        )],
        ConnectAction::Deny,
    )
    .unwrap()
}

async fn proxy_round_trip(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut out))
        .await
        .expect("proxy must close a non-tunnel response")
        .unwrap();
    out
}

/// Read one response head through the blank line (denials may keep the
/// connection alive, so `read_to_end` would hang).
async fn proxy_response_head(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw).await.unwrap();
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
    .expect("proxy must answer with a response head");
    head
}

fn status_line(response: &[u8]) -> String {
    String::from_utf8_lossy(response)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Minimal fixed upstream: one HTTP/1.1 200 with `connection: close`.
async fn start_fixed_upstream() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            if stream.read_exact(&mut byte).await.is_err() {
                return;
            }
            head.push(byte[0]);
            if head.len() > 8192 || head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
            .await;
    });
    (addr, task)
}

#[tokio::test]
async fn denied_requests_count_as_rejections() {
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let config = ExplicitProxyConfig::new(session.clone(), deny_all(), ProxyRoute::direct());
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    let addr = handle.local_addr();
    assert!(addr.ip().is_loopback());

    let plain = proxy_response_head(
        addr,
        b"GET http://blocked.test/ HTTP/1.1\r\nHost: blocked.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        status_line(&plain).starts_with("HTTP/1.1 403"),
        "denied plain request must be refused; got: {}",
        status_line(&plain)
    );

    let connect = proxy_response_head(
        addr,
        b"CONNECT blocked.test:443 HTTP/1.1\r\nHost: blocked.test:443\r\n\r\n",
    )
    .await;
    assert!(
        status_line(&connect).starts_with("HTTP/1.1 403"),
        "denied CONNECT must be refused; got: {}",
        status_line(&connect)
    );

    let snapshot = handle.stats().snapshot();
    assert_eq!(snapshot.rejected, 2);
    assert_eq!(snapshot.accepted, 0);
    assert_eq!(snapshot.tunneled, 0);
    assert_eq!(snapshot.intercepted, 0);
    assert_eq!(snapshot.flows, 0);
    assert_eq!(snapshot.failures[FAILURE_POLICY_DENIED], 2);

    handle.shutdown();
    tokio::time::timeout(TIMEOUT, handle.wait())
        .await
        .expect("proxy shutdown must drain");
    session.shutdown();
}

#[tokio::test]
async fn allowed_request_records_a_flow_and_finalizes() {
    let (upstream, upstream_task) = start_fixed_upstream().await;
    let dir = tempfile::TempDir::new().unwrap();
    let fixture_path = dir.path().join("fixture");
    let session = RecordingSession::create(
        &fixture_path,
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let config = ExplicitProxyConfig::new(session.clone(), allow_loopback(), ProxyRoute::direct());
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();

    let request = format!(
        "GET http://127.0.0.1:{}/path HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(handle.local_addr(), request.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "allowed request must succeed; got: {}",
        status_line(&response)
    );

    let snapshot = handle.stats().snapshot();
    assert_eq!(snapshot.accepted, 1);
    assert_eq!(snapshot.rejected, 0);
    assert_eq!(snapshot.flows, 1);

    // Shutdown/finalization path: drain, finish, and reopen the fixture.
    handle.shutdown();
    tokio::time::timeout(TIMEOUT, handle.wait())
        .await
        .expect("proxy shutdown must drain");
    session.shutdown();
    for _ in 0..100 {
        if session.active_blobs() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let finished = session.finish().expect("session must finalize");
    assert_eq!(finished.manifest().flow_count, 1);
    drop(finished);
    Session::open(&fixture_path, StoreLimits::default()).expect("fixture must reopen");
    upstream_task.abort();
}

#[tokio::test]
async fn tunnel_limits_reject_zero_bounds() {
    assert!(
        TunnelLimits::new(
            0,
            Duration::from_secs(1),
            Duration::from_secs(1),
            1,
            Duration::from_secs(1)
        )
        .is_err()
    );
}
