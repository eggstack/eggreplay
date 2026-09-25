//! M013B explicit-proxy and CONNECT policy integration proofs.
//!
//! Hermetic and local-only: all upstreams, routes, and targets are loopback
//! TCP listeners. No Internet access occurs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use eggreplay_core::SessionMetadata;
use eggreplay_intercept::{
    ConnectAction, ExplicitProxyConfig, HostMatch, PortMatch, ProxyListenerConfig, ProxyRoute,
    RequestKind, Rule, RuleAction, TargetPolicy, TunnelLimits, resolve_connect_target,
    start_explicit_proxy,
};
use eggreplay_store::{RecordingSession, StoreLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One HTTP request observed by the fake upstream.
#[derive(Debug, Clone)]
struct CapturedRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    trailers: Vec<(String, String)>,
}

/// Read one LF-terminated line with a bound.
async fn read_line(stream: &mut TcpStream, bound: usize) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
        line.push(byte[0]);
        if byte[0] == b'\n' || line.len() > bound {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Read one HTTP/1 request (content-length or chunked + trailers).
async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<CapturedRequest> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
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
            let line = read_line(stream, 256).await?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk"))?;
            if size == 0 {
                loop {
                    let trailer = read_line(stream, 1024).await?;
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
    })
}

fn header_values(captured: &CapturedRequest, name: &str) -> Vec<String> {
    captured
        .headers
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .collect()
}

/// Fake upstream HTTP origin: records requests, answers with `responder`.
async fn start_upstream(
    responder: impl Fn(&CapturedRequest) -> Vec<u8> + Send + Sync + 'static,
) -> (
    SocketAddr,
    Arc<Mutex<Vec<CapturedRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let task_captured = captured.clone();
    let responder = Arc::new(responder);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let task_captured = task_captured.clone();
            let responder = responder.clone();
            tokio::spawn(async move {
                loop {
                    let Ok(Ok(request)) = tokio::time::timeout(
                        Duration::from_secs(5),
                        read_http_request(&mut stream),
                    )
                    .await
                    else {
                        break;
                    };
                    let response = responder(&request);
                    task_captured.lock().await.push(request);
                    if stream.write_all(&response).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, captured, task)
}

fn fixed_response(body: &[u8]) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\nX-Upstream: yes\r\n\r\n",
        body.len()
    )
    .into_bytes()
    .into_iter()
    .chain(body.iter().copied())
    .collect()
}

fn chunked_response(chunks: &[&[u8]], trailers: &[(&str, &str)]) -> Vec<u8> {
    let mut out =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n".to_vec();
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

/// Byte-echo target for `CONNECT` tunnels.
async fn start_echo() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut reader, mut writer) = stream.split();
                tokio::io::copy(&mut reader, &mut writer).await.ok();
            });
        }
    });
    (addr, task)
}

/// Minimal `CONNECT` proxy used to prove routed tunnels.
async fn start_connect_proxy() -> (SocketAddr, tokio::task::JoinHandle<()>) {
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

fn allow_127_port(port: u16, connect: ConnectAction) -> TargetPolicy {
    TargetPolicy::new(
        vec![Rule::new(
            HostMatch::exact_ip("127.0.0.1".parse().unwrap()),
            PortMatch::exact(port).unwrap(),
            RequestKind::Any,
            RuleAction::Allow,
        )],
        connect,
    )
    .unwrap()
}

fn allow_127_any_port(connect: ConnectAction) -> TargetPolicy {
    TargetPolicy::new(
        vec![Rule::new(
            HostMatch::exact_ip("127.0.0.1".parse().unwrap()),
            PortMatch::Any,
            RequestKind::Any,
            RuleAction::Allow,
        )],
        connect,
    )
    .unwrap()
}

struct ProxyFixture {
    handle: eggreplay_intercept::ExplicitProxyHandle,
    session: RecordingSession,
    dir: tempfile::TempDir,
}

async fn start_proxy(
    policy: TargetPolicy,
    route: ProxyRoute,
    limits: TunnelLimits,
) -> ProxyFixture {
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let mut config = ExplicitProxyConfig::new(session.clone(), policy, route);
    config.tunnel_limits = limits;
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    ProxyFixture {
        handle,
        session,
        dir,
    }
}

async fn finish_proxy(fixture: ProxyFixture) {
    fixture.handle.shutdown();
    fixture.handle.wait().await;
    fixture.session.shutdown();
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

fn status_line(response: &[u8]) -> String {
    String::from_utf8_lossy(response)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Read response headers through the blank line (for tunnels, before relay).
async fn read_response_head(stream: &mut TcpStream) -> Vec<u8> {
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

/// Wait for at least one tunnel operational event (the handler records it
/// as the relay tears down, which can race the client's EOF observation).
async fn await_tunnel_events(proxy: &ProxyFixture) -> Vec<eggreplay_intercept::TunnelEvent> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let snapshot = proxy.handle.events().snapshot();
            if !snapshot.is_empty() {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("tunnel event must be recorded")
}

// ---------------------------------------------------------------------------
// Absolute-form HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn proxy_records_absolute_get_with_duplicates_and_query() {
    let (upstream, captured, upstream_task) =
        start_upstream(|_| fixed_response(b"upstream-ok")).await;
    let proxy = start_proxy(
        allow_127_port(upstream.port(), ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let request = format!(
        "GET http://127.0.0.1:{}/path?a=1&a=2 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Dup: first\r\nX-Dup: second\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), request.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "absolute GET must succeed; got: {}",
        status_line(&response)
    );
    assert!(
        response
            .windows(b"upstream-ok".len())
            .any(|w| w == b"upstream-ok")
    );
    assert!(
        response
            .windows(b"X-Upstream".len())
            .any(|w| w == b"x-upstream" || w == b"X-Upstream")
    );

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, "GET");
    assert_eq!(captured[0].target, "/path?a=1&a=2");
    assert_eq!(
        header_values(&captured[0], "x-dup"),
        vec!["first".to_owned(), "second".to_owned()]
    );
    assert_eq!(proxy.session.flow_count(), 1);
    finish_proxy(proxy).await;
    upstream_task.abort();
}

#[tokio::test]
async fn proxy_records_chunked_post_with_trailers_and_chunked_response() {
    let (upstream, captured, upstream_task) = start_upstream(|request| {
        assert_eq!(request.body, b"hello world");
        assert!(
            request.trailers.iter().any(|(name, _)| name == "x-trailer"),
            "client trailers must reach upstream: {:?}",
            request.trailers
        );
        chunked_response(&[b"hello ", b"world"], &[("x-up-trailer", "yes")])
    })
    .await;
    let proxy = start_proxy(
        allow_127_port(upstream.port(), ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let request = format!(
        "POST http://127.0.0.1:{}/upload?kind=chunked HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nTransfer-Encoding: chunked\r\nX-Dup: one\r\nX-Dup: two\r\nTE: trailers\r\nTrailer: x-trailer, x-other\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: done\r\nX-Other: ok\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), request.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "chunked POST must succeed; got: {}",
        status_line(&response)
    );
    assert!(
        response
            .windows(b"hello world".len())
            .any(|w| w == b"hello world")
    );
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(
        header_values(&captured[0], "x-dup"),
        vec!["one".to_owned(), "two".to_owned()]
    );
    assert_eq!(proxy.session.flow_count(), 1);
    // Terminal trailers are preserved semantically: the recorded flow carries
    // both the client trailer block and the upstream trailer block, even
    // though EggServe 0.3.0 H1 egress does not re-emit the terminal block
    // (it strips the `Trailer` announcement Hyper requires; see M013C notes).
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
    upstream_task.abort();
}

#[tokio::test]
async fn proxy_strips_hop_by_hop_and_origin_form_is_rejected() {
    let (upstream, captured, upstream_task) = start_upstream(|_| fixed_response(b"ok")).await;
    let proxy = start_proxy(
        allow_127_port(upstream.port(), ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let sentinel = "SENTINEL-PROXY-AUTH-7c41";
    let request = format!(
        "GET http://127.0.0.1:{}/a HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nProxy-Connection: keep-alive\r\nProxy-Authorization: Basic {sentinel}\r\nProxy-Authenticate: Basic realm=x\r\nKeep-Alive: timeout=5\r\nConnection: keep-alive, X-Hop\r\nX-Hop: gone\r\nX-Keep: yes\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), request.as_bytes()).await;
    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    let names: Vec<_> = captured[0]
        .headers
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    for stripped in [
        "proxy-connection",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "connection",
        "x-hop",
    ] {
        assert!(
            !names.contains(&stripped.to_owned()),
            "must strip {stripped}"
        );
    }
    assert!(names.contains(&"x-keep".to_owned()));
    let raw: String = captured[0]
        .headers
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!raw.contains(sentinel));

    // Origin-form requests are rejected on the explicit listener.
    let origin = format!(
        "GET /must-not-record HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        upstream.port()
    );
    let denied = proxy_round_trip(proxy.handle.local_addr(), origin.as_bytes()).await;
    assert!(
        status_line(&denied).starts_with("HTTP/1.1 400"),
        "origin-form must be rejected; got: {}",
        status_line(&denied)
    );
    assert_eq!(captured.len(), 1, "origin-form must not reach upstream");
    assert_eq!(proxy.session.flow_count(), 1);
    finish_proxy(proxy).await;
    upstream_task.abort();
}

#[tokio::test]
async fn proxy_auth_sentinel_is_never_persisted() {
    let (upstream, _captured, upstream_task) = start_upstream(|_| fixed_response(b"ok")).await;
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let mut config = ExplicitProxyConfig::new(
        session.clone(),
        allow_127_port(upstream.port(), ConnectAction::Tunnel),
        ProxyRoute::direct(),
    );
    config.tunnel_limits = TunnelLimits::default();
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    let sentinel = "SENTINEL-PROXY-PERSIST-d8e2";
    let request = format!(
        "GET http://127.0.0.1:{}/a HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nProxy-Authorization: Basic {sentinel}\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(handle.local_addr(), request.as_bytes()).await;
    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    handle.shutdown();
    handle.wait().await;
    session.shutdown();
    session.finish().unwrap();
    let found = dir_contains(dir.path(), sentinel);
    assert!(!found, "proxy credential must never reach the fixture");
    upstream_task.abort();
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

#[tokio::test]
async fn proxy_rejects_host_mismatch_https_scheme_and_upgrade() {
    let (upstream, captured, upstream_task) = start_upstream(|_| fixed_response(b"ok")).await;
    let proxy = start_proxy(
        allow_127_port(upstream.port(), ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    // Contradictory Host.
    let mismatch = format!(
        "GET http://127.0.0.1:{}/a HTTP/1.1\r\nHost: other.test\r\nConnection: close\r\n\r\n",
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), mismatch.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 400"),
        "Host mismatch must yield 400; got: {}",
        status_line(&response)
    );
    // Absolute https is rejected (CONNECT is the https path).
    let https = format!(
        "GET https://127.0.0.1:{}/a HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), https.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 400"),
        "https absolute-form must yield 400; got: {}",
        status_line(&response)
    );
    // Upgrade intent with tunnel semantics is denied as 403.
    let upgrade = format!(
        "GET http://127.0.0.1:{}/socket HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), upgrade.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 403"),
        "tunneled upgrade must yield 403; got: {}",
        status_line(&response)
    );
    // A bare Upgrade header without tunnel semantics is rejected as 400.
    let bare = format!(
        "GET http://127.0.0.1:{}/plain HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUpgrade: h2c\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), bare.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 400"),
        "bare upgrade must yield 400; got: {}",
        status_line(&response)
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    upstream_task.abort();
}

#[tokio::test]
async fn proxy_denies_unlisted_host_and_port() {
    let (upstream, captured, upstream_task) = start_upstream(|_| fixed_response(b"ok")).await;
    // Policy allows the host but a different port.
    let proxy = start_proxy(
        allow_127_port(upstream.port().wrapping_add(1), ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let request = format!(
        "GET http://127.0.0.1:{}/a HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        upstream.port(),
        upstream.port()
    );
    let response = proxy_round_trip(proxy.handle.local_addr(), request.as_bytes()).await;
    assert!(
        status_line(&response).starts_with("HTTP/1.1 403"),
        "denied port must yield 403; got: {}",
        status_line(&response)
    );
    assert_eq!(captured.lock().await.len(), 0);
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    upstream_task.abort();
}

// ---------------------------------------------------------------------------
// Target policy (transport-neutral, no network)
// ---------------------------------------------------------------------------

#[test]
fn policy_exact_and_suffix_boundaries() {
    use eggreplay_intercept::normalize_host;
    let policy = TargetPolicy::new(
        vec![
            Rule::new(
                HostMatch::suffix_dns("example.com").unwrap(),
                PortMatch::exact(80).unwrap(),
                RequestKind::Plain,
                RuleAction::Allow,
            ),
            Rule::new(
                HostMatch::exact_dns("example.com").unwrap(),
                PortMatch::exact(443).unwrap(),
                RequestKind::Connect,
                RuleAction::Allow,
            ),
        ],
        ConnectAction::Tunnel,
    )
    .unwrap();
    let dns = |name: &str| normalize_host(name).unwrap();
    assert!(policy.allows_plain(&dns("example.com"), 80));
    assert!(policy.allows_plain(&dns("a.b.example.com"), 80));
    assert!(!policy.allows_plain(&dns("evil-example.com"), 80));
    assert!(!policy.allows_plain(&dns("example.com.evil.test"), 80));
    assert!(!policy.allows_plain(&dns("example.com"), 8080));
    assert_eq!(
        policy.resolve_connect(&dns("example.com"), 443),
        ConnectAction::Tunnel
    );
    assert_eq!(
        policy.resolve_connect(&dns("sub.example.com"), 443),
        ConnectAction::Deny
    );
    assert_eq!(
        policy.resolve_connect(&dns("example.com"), 80),
        ConnectAction::Deny
    );
}

#[test]
fn policy_ip_and_port_dimensions() {
    use eggreplay_intercept::NormalizedHost;
    let v4: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let v6: std::net::IpAddr = "::1".parse().unwrap();
    let policy = TargetPolicy::new(
        vec![
            Rule::new(
                HostMatch::exact_ip(v4),
                PortMatch::range(8000, 8999).unwrap(),
                RequestKind::Any,
                RuleAction::Allow,
            ),
            Rule::new(
                HostMatch::exact_ip(v6),
                PortMatch::set(vec![443, 8443]).unwrap(),
                RequestKind::Connect,
                RuleAction::Allow,
            ),
        ],
        ConnectAction::Tunnel,
    )
    .unwrap();
    assert!(policy.allows_plain(&NormalizedHost::Ip(v4), 8080));
    assert!(!policy.allows_plain(&NormalizedHost::Ip(v4), 9000));
    assert!(!policy.allows_plain(&NormalizedHost::Ip(v6), 443));
    assert_eq!(
        policy.resolve_connect(&NormalizedHost::Ip(v6), 8443),
        ConnectAction::Tunnel
    );
    assert_eq!(
        policy.resolve_connect(&NormalizedHost::Ip(v6), 443),
        ConnectAction::Tunnel
    );
    assert_eq!(
        policy.resolve_connect(&NormalizedHost::Ip(v6), 80),
        ConnectAction::Deny
    );
}

#[test]
fn connect_authority_supports_ipv4_and_ipv6_forms() {
    let v4 = resolve_connect_target(Some("127.0.0.1:443")).unwrap();
    assert_eq!(v4.port, 443);
    let v6 = resolve_connect_target(Some("[::1]:443")).unwrap();
    assert_eq!(v6.port, 443);
    assert_eq!(v6.host.as_str(), "::1");
    assert!(resolve_connect_target(Some("::1:443")).is_err());
    assert_eq!(resolve_connect_target(Some("[::1]")).unwrap().port, 443);
}

// ---------------------------------------------------------------------------
// CONNECT deny/tunnel
// ---------------------------------------------------------------------------

async fn connect_head(addr: SocketAddr, authority: &str) -> (TcpStream, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let head = read_response_head(&mut stream).await;
    (stream, head)
}

#[tokio::test]
async fn connect_denied_target_returns_403_without_flow() {
    let (echo, echo_task) = start_echo().await;
    let proxy = start_proxy(
        TargetPolicy::deny_all(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let (_stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 403"),
        "denied CONNECT must yield 403; got: {}",
        String::from_utf8_lossy(&head)
    );
    assert_eq!(proxy.session.flow_count(), 0);
    assert!(proxy.handle.events().is_empty());
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_direct_tunnel_relays_opaque_bytes_without_flow() {
    let (echo, echo_task) = start_echo().await;
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
        "allowed CONNECT must yield 200; got: {}",
        String::from_utf8_lossy(&head)
    );
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0u8; 4];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"ping");
    drop(stream);
    assert_eq!(proxy.session.flow_count(), 0);
    let events = await_tunnel_events(&proxy).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, "completed");
    assert_eq!(events[0].action, "tunnel");
    assert_eq!(events[0].port, echo.port());
    assert!(events[0].error.is_none());
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_ipv6_tunnel_when_loopback_v6_exists() {
    let Ok(listener) = TcpListener::bind("[::1]:0").await else {
        return;
    };
    let addr = listener.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut reader, mut writer) = stream.split();
                tokio::io::copy(&mut reader, &mut writer).await.ok();
            });
        }
    });
    let policy = TargetPolicy::new(
        vec![Rule::new(
            HostMatch::exact_ip("::1".parse().unwrap()),
            PortMatch::Any,
            RequestKind::Any,
            RuleAction::Allow,
        )],
        ConnectAction::Tunnel,
    )
    .unwrap();
    let proxy = start_proxy(policy, ProxyRoute::direct(), TunnelLimits::default()).await;
    let authority = format!("[::1]:{}", addr.port());
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &authority).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
        "IPv6 CONNECT must yield 200; got: {}",
        String::from_utf8_lossy(&head)
    );
    stream.write_all(b"v6").await.unwrap();
    let mut reply = [0u8; 2];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"v6");
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_routed_tunnel_uses_configured_proxy() {
    let (echo, echo_task) = start_echo().await;
    let (route_proxy, route_task) = start_connect_proxy().await;
    let route = ProxyRoute::from_pproxy_uri(&format!("http://{route_proxy}")).unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        route,
        TunnelLimits::default(),
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
        "routed CONNECT must yield 200; got: {}",
        String::from_utf8_lossy(&head)
    );
    stream.write_all(b"routed").await.unwrap();
    let mut reply = [0u8; 6];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"routed");
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    echo_task.abort();
    route_task.abort();
}

#[tokio::test]
async fn connect_routed_never_falls_back_to_direct() {
    let (echo, echo_task) = start_echo().await;
    // Dead route: the live echo target is reachable directly, but the proxy
    // must fail closed instead of falling back.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let route = ProxyRoute::from_pproxy_uri(&format!("http://{dead_addr}")).unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        route,
        TunnelLimits::default(),
    )
    .await;
    let (_stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    let text = String::from_utf8_lossy(&head).into_owned();
    assert!(
        !text.starts_with("HTTP/1.1 200"),
        "dead route must not yield 200; got: {text}"
    );
    assert!(
        text.starts_with("HTTP/1.1 502"),
        "dead route must yield 502; got: {text}"
    );
    assert_eq!(proxy.session.flow_count(), 0);
    assert!(proxy.handle.events().is_empty());
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_dial_failure_returns_502_before_200() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let (_stream, head) =
        connect_head(proxy.handle.local_addr(), &format!("127.0.0.1:{port}")).await;
    let text = String::from_utf8_lossy(&head).into_owned();
    assert!(
        !text.starts_with("HTTP/1.1 200"),
        "refused target must not yield 200; got: {text}"
    );
    assert!(
        text.starts_with("HTTP/1.1 502"),
        "refused target must yield 502; got: {text}"
    );
    assert_eq!(proxy.session.flow_count(), 0);
    assert!(proxy.handle.events().is_empty());
    finish_proxy(proxy).await;
}

#[tokio::test]
async fn connect_tunnel_applies_backpressure() {
    // Target holds without reading until released; small socket buffers keep
    // kernel buffering far below the payload so a buffering relay would
    // complete the write while a backpressured relay blocks.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(8192).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(8).unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let target = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = release_rx.await;
        let mut sink = Vec::new();
        stream.read_to_end(&mut sink).await.ok();
        sink.len()
    });
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let client_socket = tokio::net::TcpSocket::new_v4().unwrap();
    client_socket.set_send_buffer_size(8192).unwrap();
    let mut stream = client_socket
        .connect(proxy.handle.local_addr())
        .await
        .unwrap();
    stream
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let head = read_response_head(&mut stream).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    let payload = vec![b'p'; 2 * 1024 * 1024];
    let blocked = tokio::time::timeout(Duration::from_secs(1), stream.write_all(&payload)).await;
    assert!(
        blocked.is_err(),
        "relay must exert backpressure instead of buffering unboundedly"
    );
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        // Finish the pending write, then half-close so the target ends.
        stream.write_all(&payload).await.unwrap();
        stream.shutdown().await.unwrap();
    })
    .await
    .expect("write must complete once the target drains");
    let drained = tokio::time::timeout(TIMEOUT, target)
        .await
        .expect("target must finish")
        .unwrap();
    assert!(drained >= payload.len() / 2, "target must drain payload");
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
}

#[tokio::test]
async fn connect_tunnel_supports_half_close() {
    // Target reads to EOF, then answers and closes.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let target = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        stream.read_to_end(&mut data).await.unwrap();
        assert_eq!(data, b"hello");
        stream.write_all(b"got-it").await.unwrap();
    });
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        TunnelLimits::default(),
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &addr.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    stream.write_all(b"hello").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply, b"got-it");
    assert_eq!(proxy.session.flow_count(), 0);
    target.await.unwrap();
    finish_proxy(proxy).await;
}

// ---------------------------------------------------------------------------
// Tunnel bounds
// ---------------------------------------------------------------------------

fn tight_limits() -> TunnelLimits {
    TunnelLimits::new(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_secs(30),
        16,
        Duration::from_secs(5),
    )
    .unwrap()
}

#[tokio::test]
async fn connect_byte_limit_cuts_the_relay() {
    let (echo, echo_task) = start_echo().await;
    let limits = TunnelLimits::new(
        16,
        Duration::from_secs(30),
        Duration::from_secs(30),
        16,
        Duration::from_secs(5),
    )
    .unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        limits,
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    stream.write_all(&[b'x'; 1024]).await.unwrap();
    let mut sink = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut sink))
        .await
        .expect("byte-limited relay must terminate")
        .unwrap();
    let events = await_tunnel_events(&proxy).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, "byte-limit");
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_idle_timeout_cuts_a_quiet_tunnel() {
    let (echo, echo_task) = start_echo().await;
    let limits = TunnelLimits::new(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_millis(100),
        16,
        Duration::from_secs(5),
    )
    .unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        limits,
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    let mut sink = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut sink))
        .await
        .expect("idle relay must terminate")
        .unwrap();
    let events = await_tunnel_events(&proxy).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, "idle-timeout");
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_duration_limit_cuts_a_slow_tunnel() {
    let (echo, echo_task) = start_echo().await;
    let limits = TunnelLimits::new(
        1024 * 1024,
        Duration::from_millis(300),
        Duration::from_secs(30),
        16,
        Duration::from_secs(5),
    )
    .unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        limits,
    )
    .await;
    let (mut stream, head) = connect_head(proxy.handle.local_addr(), &echo.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    let mut sink = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut sink))
        .await
        .expect("duration-limited relay must terminate")
        .unwrap();
    let events = await_tunnel_events(&proxy).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, "duration-limit");
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn connect_concurrency_limit_rejects_overflow() {
    let (echo, echo_task) = start_echo().await;
    let limits = TunnelLimits::new(
        1024 * 1024,
        Duration::from_secs(30),
        Duration::from_secs(30),
        1,
        Duration::from_secs(5),
    )
    .unwrap();
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        limits,
    )
    .await;
    let addr = proxy.handle.local_addr();
    let (first, head) = connect_head(addr, &echo.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    // Second tunnel while the first is held: 503 without a handshake.
    let (_second, head) = connect_head(addr, &echo.to_string()).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 503"),
        "overflow tunnel must yield 503; got: {}",
        String::from_utf8_lossy(&head)
    );
    // Release the first tunnel; a new one succeeds.
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut third, head) = connect_head(addr, &echo.to_string()).await;
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"),
        "tunnel after release must yield 200; got: {}",
        String::from_utf8_lossy(&head)
    );
    third.write_all(b"again").await.unwrap();
    let mut reply = [0u8; 5];
    tokio::time::timeout(TIMEOUT, third.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"again");
    assert_eq!(proxy.session.flow_count(), 0);
    finish_proxy(proxy).await;
    echo_task.abort();
}

#[tokio::test]
async fn proxy_shutdown_drains_with_an_active_tunnel() {
    let (echo, echo_task) = start_echo().await;
    let proxy = start_proxy(
        allow_127_any_port(ConnectAction::Tunnel),
        ProxyRoute::direct(),
        tight_limits(),
    )
    .await;
    let addr = proxy.handle.local_addr();
    let (mut stream, head) = connect_head(addr, &echo.to_string()).await;
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));
    // The tunnel is live: bytes flow before shutdown.
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0u8; 4];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"ping");
    // Shutdown stops admission while the tunnel is active.
    proxy.handle.shutdown();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let refused = match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(addr)).await
    {
        Err(_) | Ok(Err(_)) => true,
        Ok(Ok(mut late)) => {
            // A raced TCP accept must still never complete a handshake.
            late.write_all(format!("CONNECT {echo} HTTP/1.1\r\nHost: {echo}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            let answered = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if late.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                    if head.len() > 8192 || head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
            })
            .await;
            !matches!(answered, Ok(()))
                || !String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200")
        }
    };
    assert!(refused, "shutdown must stop admission");
    // Releasing the tunnel lets the drain complete with an event recorded.
    drop(stream);
    tokio::time::timeout(TIMEOUT, proxy.handle.wait())
        .await
        .expect("drain must complete after the tunnel closes");
    proxy.session.shutdown();
    echo_task.abort();
}

// ---------------------------------------------------------------------------
// Listener boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_loopback_bind_requires_explicit_opt_in() {
    use eggreplay_intercept::validate_bind;
    let remote: SocketAddr = "0.0.0.0:0".parse().unwrap();
    assert!(validate_bind(remote, false).is_err());
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let config = ExplicitProxyConfig::new(
        session,
        TargetPolicy::deny_all(ConnectAction::Deny),
        ProxyRoute::direct(),
    );
    let listener = ProxyListenerConfig {
        bind: remote,
        allow_non_loopback: false,
    };
    assert!(start_explicit_proxy(listener, config).await.is_err());
}
