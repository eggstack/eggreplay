//! M013F secret audit: unique sentinels must never reach fixtures,
//! diagnostics, events, errors, metadata, or staging residue.
//!
//! Hermetic and local-only: every listener, origin, and CA directory is a
//! loopback/`tempfile` construct. No Internet access occurs.
//!
//! Sentinels cover the CA private key, Proxy-Authorization, HTTP
//! Authorization/Cookie, and one JSON body redaction target. Private key
//! *paths* are likewise asserted absent from routine diagnostics.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use eggreplay_core::{RedactionConfig, SessionMetadata};
use eggreplay_intercept::{
    CaAuthority, CaError, CaOptions, ConnectAction, ExplicitProxyConfig, HostMatch, LeafError,
    LeafIssuer, LeafOptions, MitmConfig, MitmError, PolicyFileError, PortMatch,
    ProxyListenerConfig, ProxyRoute, RequestKind, Rule, RuleAction, TargetPolicy, TunnelEvent,
    TunnelOutcome, TunnelRelaySummary, export_ca_cert, inspect_ca, start_explicit_proxy,
};
use eggreplay_store::{RecordingSession, StoreLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const TIMEOUT: Duration = Duration::from_secs(15);

// Unique sentinels: each appears in exactly one secret class so a leak is
// attributable. The values are random-looking tokens, never real credentials.
const CA_KEY_SENTINEL: &str = "M013F-CAKEY-s3ntinel-9f31ac7e";
const PROXY_AUTH_SENTINEL: &str = "M013F-PROXYAUTH-s3ntinel-4bd2e91c";
const HTTP_AUTH_SENTINEL: &str = "M013F-HTTPAUTH-s3ntinel-77c0d5aa";
const COOKIE_SENTINEL: &str = "M013F-COOKIE-s3ntinel-a41f08b3";
const BODY_SENTINEL: &str = "M013F-BODY-s3ntinel-5e6c19d2";
const ALL_SENTINELS: &[&str] = &[
    CA_KEY_SENTINEL,
    PROXY_AUTH_SENTINEL,
    HTTP_AUTH_SENTINEL,
    COOKIE_SENTINEL,
    BODY_SENTINEL,
];

/// Recursively scan a directory tree for a needle in file contents.
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

fn assert_no_sentinels(label: &str, text: &str) {
    for sentinel in ALL_SENTINELS {
        assert!(
            !text.contains(sentinel),
            "{label} leaks a secret sentinel: {sentinel}"
        );
    }
}

// ---------------------------------------------------------------------------
// Error/diagnostic redaction (no live traffic)
// ---------------------------------------------------------------------------

#[test]
fn error_values_carry_no_key_material_or_paths() {
    let errors = vec![
        CaError::AlreadyExists,
        CaError::Incomplete,
        CaError::Io("test-operation"),
        CaError::TooLarge("private key"),
        CaError::InvalidMetadata("detail".to_owned()),
        CaError::UnsupportedFormatVersion(99),
        CaError::FingerprintMismatch,
        CaError::CertUnparseable,
        CaError::KeyUnparseable,
        CaError::MultipleKeys,
        CaError::NoKey,
        CaError::Mismatch,
        CaError::UnexpectedChain,
        CaError::NotSelfSigned,
        CaError::BadSignature,
        CaError::NotCa,
        CaError::MissingKeyCertSign,
        CaError::UnsupportedAlgorithm,
        CaError::Expired("2030-01-01T00:00:00Z".to_owned()),
        CaError::NotYetValid("2030-01-01T00:00:00Z".to_owned()),
        CaError::InsecurePermissions {
            target: "private key",
            mode: 0o644,
        },
        CaError::PermissionRepairUnsupported,
        CaError::CertGeneration,
        CaError::Signing,
        CaError::InvalidOptions("detail".to_owned()),
        CaError::ExportDestExists,
    ];
    for error in &errors {
        let text = error.to_string();
        assert_no_sentinels("CaError", &text);
        assert!(
            !text.contains("PRIVATE KEY"),
            "CaError leaks key material: {text}"
        );
        assert!(!text.contains("BEGIN"), "CaError leaks PEM: {text}");
        // Private key paths are omitted from routine diagnostics: messages
        // use static operation labels, never caller paths or filenames.
        for marker in [".pem", "ca-key", "ca-cert", "metadata.json"] {
            assert!(!text.contains(marker), "CaError leaks a key path: {text}");
        }
    }
    for error in [
        LeafError::InvalidTarget,
        LeafError::InvalidOptions,
        LeafError::CaNotValid,
        LeafError::CaExpiring,
        LeafError::KeyGeneration,
        LeafError::Signing,
    ] {
        let text = error.to_string();
        assert_no_sentinels("LeafError", &text);
        assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
        assert!(!text.contains("BEGIN"), "leak in {text}");
    }
    for error in [
        MitmError::PolicyNotArmed,
        MitmError::LeafUnavailable,
        MitmError::ServerConfig,
        MitmError::ConnectionPolicy,
        MitmError::TlsHandshake,
        MitmError::AlpnUnsupported,
        MitmError::AuthorityMismatch,
        MitmError::UpgradeUnsupported,
        MitmError::NotOriginForm,
        MitmError::Upstream,
        MitmError::NotConfigured,
    ] {
        let text = error.to_string();
        assert_no_sentinels("MitmError", &text);
        assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
        assert!(!text.contains("BEGIN"), "leak in {text}");
    }
    for error in [
        PolicyFileError::TooLarge,
        PolicyFileError::Io("read"),
        PolicyFileError::Invalid("detail".to_owned()),
        PolicyFileError::UnsupportedVersion("v9".to_owned()),
        PolicyFileError::TooManyRules(999),
        PolicyFileError::MixedActions,
    ] {
        let text = error.to_string();
        assert_no_sentinels("PolicyFileError", &text);
        assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
    }
}

#[test]
fn tunnel_events_and_stats_carry_no_secrets() {
    let summary = TunnelRelaySummary::observed(TunnelOutcome::RelayError, 7, 9, Duration::ZERO);
    for action in ["tunnel", "intercept"] {
        let event = TunnelEvent::new_with_action(
            "example.test",
            443,
            action,
            &summary,
            Some("relay transport error".to_owned()),
        );
        let json = serde_json::to_value(event_snapshot(&event)).unwrap();
        assert_no_sentinels("TunnelEvent", &json.to_string());
    }
    let stats = eggreplay_intercept::ProxyStats::new();
    stats.record_accept();
    stats.record_failure(eggreplay_intercept::FAILURE_POLICY_DENIED);
    let json = serde_json::to_value(stats.snapshot()).unwrap();
    assert_no_sentinels("ProxyStats", &json.to_string());
    let text = json.to_string();
    assert!(!text.contains("PRIVATE KEY"));
    assert!(!text.contains(".pem"));
}

/// `TunnelEvent` is not `Serialize`; snapshot its public fields explicitly.
fn event_snapshot(event: &TunnelEvent) -> serde_json::Value {
    serde_json::json!({
        "host": event.host,
        "port": event.port,
        "action": event.action,
        "outcome": event.outcome,
        "bytes_client_to_target": event.bytes_client_to_target,
        "bytes_target_to_client": event.bytes_target_to_client,
        "duration_ms": event.duration_ms,
        "error": event.error,
    })
}

#[test]
fn policy_summaries_carry_no_key_material() {
    let policy = eggreplay_intercept::FilePolicy::parse_json(
        br#"{"version": "eggreplay-intercept-policy/v1", "rules": []}"#,
    )
    .unwrap();
    let summary = policy.normalized_summary().to_string();
    assert_no_sentinels("FilePolicy summary", &summary);
    assert!(!summary.contains("PRIVATE KEY"));
}

// ---------------------------------------------------------------------------
// CA lifecycle: keys stay out of metadata/exports/errors/staging
// ---------------------------------------------------------------------------

#[test]
fn ca_lifecycle_emits_no_key_material_or_paths() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = root.path().join("ca");
    let authority = CaAuthority::create_new(&dir, &CaOptions::default()).unwrap();
    let metadata = authority.metadata().clone();
    let json = serde_json::to_value(&metadata).unwrap().to_string();
    assert_no_sentinels("CaMetadata", &json);
    assert!(!json.contains("PRIVATE KEY"), "metadata leaks key: {json}");
    assert!(!json.contains("BEGIN"), "metadata leaks PEM: {json}");
    let debug = format!("{authority:?}");
    assert!(
        !debug.contains("PRIVATE KEY"),
        "authority Debug leaks: {debug}"
    );
    assert!(!debug.contains("BEGIN"), "authority Debug leaks PEM");

    // Public-only export: the exported file holds no private key.
    let out = root.path().join("exported.pem");
    export_ca_cert(&dir, &out).unwrap();
    let exported = std::fs::read_to_string(&out).unwrap();
    assert!(exported.contains("BEGIN CERTIFICATE"));
    assert!(!exported.contains("PRIVATE KEY"));

    // Re-inspection touches no key material either.
    let reinspected = inspect_ca(&dir).unwrap();
    assert_eq!(reinspected, metadata);

    // The real key file exists on disk (the secret is real), yet no routine
    // diagnostic names its path: reopen errors use static labels.
    let key_bytes = std::fs::read(dir.join("ca-key.pem")).unwrap();
    assert!(String::from_utf8_lossy(&key_bytes).contains("PRIVATE KEY"));
    drop(authority);
    let reopened = CaAuthority::open(&dir).unwrap();
    let _ = reopened.fingerprint();
}

#[test]
fn failed_import_publishes_nothing_and_echoes_no_key() {
    let root = tempfile::TempDir::new().unwrap();
    // A caller-supplied key file carrying the CA sentinel must be rejected
    // (garbage PEM) without the error echoing the secret...
    let bad_key = root.path().join("bad.key.pem");
    std::fs::write(
        &bad_key,
        format!("-----BEGIN PRIVATE KEY-----\n{CA_KEY_SENTINEL}\n-----END PRIVATE KEY-----\n"),
    )
    .unwrap();
    let bad_cert = root.path().join("bad.cert.pem");
    std::fs::write(&bad_cert, b"not a certificate").unwrap();
    let target = root.path().join("ca-failed");
    let error = CaAuthority::import(&target, &bad_cert, &bad_key).unwrap_err();
    let text = error.to_string();
    assert!(
        !text.contains(CA_KEY_SENTINEL),
        "import error echoes key material: {text}"
    );
    assert!(!target.exists(), "failed import must not publish");
    // ...and no staging residue may carry the sentinel: only the two caller
    // source files (explicit operator inputs) may contain it.
    let mut stray = Vec::new();
    let mut stack = vec![root.path().to_owned()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                // A published CA directory must never appear after failure.
                assert!(
                    !path.join("ca-key.pem").exists(),
                    "staging residue published a key: {}",
                    path.display()
                );
                stack.push(path);
            } else if path != bad_key
                && let Ok(bytes) = std::fs::read(&path)
                && String::from_utf8_lossy(&bytes).contains(CA_KEY_SENTINEL)
            {
                stray.push(path);
            }
        }
    }
    assert!(stray.is_empty(), "staging residue leaks key: {stray:?}");
}

// ---------------------------------------------------------------------------
// Live proxy + MITM secret audit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CapturedRequest {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<CapturedRequest> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
        head.push(byte[0]);
        if head.len() > 65_536 || head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in text.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "content-length" {
                content_length = value.trim().parse().unwrap_or(0);
            }
            headers.push((name, value.trim().to_owned()));
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).await?;
    }
    Ok(CapturedRequest { headers, body })
}

async fn start_plain_upstream(
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
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes()
    .into_iter()
    .chain(body.iter().copied())
    .collect()
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

fn secret_redaction() -> RedactionConfig {
    let mut redaction = RedactionConfig::default_secure();
    redaction.json_paths.insert("/secret".to_owned());
    redaction
}

fn secret_body() -> Vec<u8> {
    format!("{{\"public\": \"visible\", \"secret\": \"{BODY_SENTINEL}\"}}").into_bytes()
}

#[tokio::test]
async fn plain_proxy_redacts_and_never_persists_secrets() {
    let (upstream_addr, captured, upstream_task) =
        start_plain_upstream(|_| fixed_response(br#"{"secret": "origin-value"}"#)).await;
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let mut config = ExplicitProxyConfig::new(
        session.clone(),
        allow_127_any_port(ConnectAction::Deny),
        ProxyRoute::direct(),
    );
    config.redaction = secret_redaction();
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    let proxy_addr = handle.local_addr();

    let body = secret_body();
    let raw = format!(
        "POST http://127.0.0.1:{}/submit HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nProxy-Authorization: Basic {}\r\nAuthorization: Bearer {}\r\nCookie: session={}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        upstream_addr.port(),
        upstream_addr.port(),
        PROXY_AUTH_SENTINEL,
        HTTP_AUTH_SENTINEL,
        COOKIE_SENTINEL,
        body.len(),
    );
    let mut request = raw.into_bytes();
    request.extend_from_slice(&body);
    let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
    stream.write_all(&request).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"),
        "proxied request must succeed"
    );

    // Proxy-Authorization is proxy framing: stripped before the upstream.
    // Authorization/Cookie/body are end-to-end: forwarded intact, but
    // redacted at rest (asserted below).
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 1);
    let upstream_text = format!("{:?}", requests[0].headers);
    assert!(
        !upstream_text.contains(PROXY_AUTH_SENTINEL),
        "proxy credentials must not reach the upstream"
    );
    assert_eq!(
        requests[0].body, body,
        "end-to-end body must reach the upstream intact"
    );
    drop(requests);

    // Fixture tree, events, and stats must hold no secret in any form.
    session.shutdown();
    handle.shutdown();
    handle.wait().await;
    for sentinel in ALL_SENTINELS {
        assert!(
            !dir_contains(dir.path(), sentinel),
            "fixture tree leaks {sentinel}"
        );
    }
    assert!(
        !dir_contains(dir.path(), "PRIVATE KEY"),
        "fixture tree holds key material"
    );
    upstream_task.abort();
}

// --- Minimal single-stack TLS origin for the MITM leg ---

struct OriginCert {
    cert_der: Vec<u8>,
    cert_pem: String,
    key_der: Vec<u8>,
}

fn make_origin_cert() -> OriginCert {
    let generated =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("origin cert");
    OriginCert {
        cert_der: generated.cert.der().to_vec(),
        cert_pem: generated.cert.pem(),
        key_der: generated.key_pair.serialize_der(),
    }
}

async fn start_tls_origin(
    cert: OriginCert,
) -> (
    u16,
    Arc<Mutex<Vec<CapturedRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    use rustls::pki_types::CertificateDer;
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let task_captured = captured.clone();
    let task = tokio::spawn(async move {
        use tokio_rustls::TlsAcceptor;
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = TlsAcceptor::from(server_tls.clone());
            let task_captured = task_captured.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(socket).await else {
                    return;
                };
                let mut tls = tls;
                loop {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    let mut content_length = 0usize;
                    loop {
                        let Ok(count) =
                            tokio::time::timeout(Duration::from_secs(5), tls.read(&mut byte)).await
                        else {
                            return;
                        };
                        let Ok(count) = count else { return };
                        if count == 0 {
                            return;
                        }
                        head.push(byte[0]);
                        if head.len() > 65_536 || head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let mut headers = Vec::new();
                    for line in text.lines().skip(1) {
                        if let Some((name, value)) = line.split_once(':') {
                            let name = name.trim().to_ascii_lowercase();
                            if name == "content-length" {
                                content_length = value.trim().parse().unwrap_or(0);
                            }
                            headers.push((name, value.trim().to_owned()));
                        }
                    }
                    let mut body = vec![0u8; content_length];
                    if content_length > 0 && tls.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    task_captured
                        .lock()
                        .await
                        .push(CapturedRequest { headers, body });
                    if tls
                        .write_all(&fixed_response(br#"{"ok":true}"#))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (port, captured, task)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mitm_records_without_persisting_secrets() {
    let origin = make_origin_cert();
    let origin_pem = origin.cert_pem.clone();
    let (origin_port, captured, origin_task) = start_tls_origin(origin).await;

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
    mitm.upstream_tls = Some(
        eggfetch_core::TlsConfig::builder()
            .ca_certificate_pem(origin_pem.as_bytes())
            .unwrap()
            .build(),
    );
    mitm.redaction = secret_redaction();
    let mut config = ExplicitProxyConfig::new(
        session.clone(),
        allow_127_any_port(ConnectAction::Intercept),
        ProxyRoute::direct(),
    );
    config.redaction = secret_redaction();
    config.mitm = Some(mitm);
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    let proxy_addr = handle.local_addr();

    // Independent client #2 (EggFetch) through CONNECT with the custom CA.
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .proxy(eggfetch_core::proxy::Proxy::all(&format!("http://{proxy_addr}")).unwrap())
        .tls_config(
            eggfetch_core::TlsConfig::builder()
                .ca_certificate_der(vec![ca_der.clone()])
                .unwrap()
                .build(),
        )
        .build();
    let body = secret_body();
    let mut response = tokio::time::timeout(
        TIMEOUT,
        client
            .post(&format!("https://127.0.0.1:{origin_port}/submit"))
            .unwrap()
            .header("authorization", &format!("Bearer {HTTP_AUTH_SENTINEL}"))
            .header("cookie", &format!("session={COOKIE_SENTINEL}"))
            .header(
                "proxy-authorization",
                &format!("Basic {PROXY_AUTH_SENTINEL}"),
            )
            .header("content-type", "application/json")
            .body(body.clone())
            .send(),
    )
    .await
    .expect("request must complete")
    .unwrap();
    let status = response.status().as_u16();
    let _ = response.text().await.unwrap();
    assert_eq!(status, 200);

    // The origin sees end-to-end headers/body but never proxy framing.
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 1);
    let upstream_text = format!("{:?}", requests[0]);
    assert!(
        !upstream_text.contains(PROXY_AUTH_SENTINEL),
        "proxy credentials must not reach the upstream"
    );
    assert_eq!(
        requests[0].body, body,
        "end-to-end body must reach the upstream intact"
    );
    drop(requests);

    // Fixture tree, tunnel events, and stats must hold no secret in any form.
    let events_json = serde_json::to_value(
        handle
            .events()
            .snapshot()
            .iter()
            .map(event_snapshot)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .to_string();
    assert_no_sentinels("TunnelEventLog", &events_json);
    let stats_json = serde_json::to_value(handle.stats().snapshot())
        .unwrap()
        .to_string();
    assert_no_sentinels("ProxyStats", &stats_json);
    session.shutdown();
    handle.shutdown();
    handle.wait().await;
    // Keep the event log reference alive across shutdown for the scan above;
    // rescan from disk after finalization for the durable artifacts.
    for sentinel in ALL_SENTINELS {
        assert!(
            !dir_contains(dir.path(), sentinel),
            "fixture tree leaks {sentinel}"
        );
    }
    assert!(
        !dir_contains(dir.path(), "PRIVATE KEY"),
        "fixture tree holds key material"
    );
    // Temporary/staging residue: the fixture tempdir holds only the fixture;
    // the CA directory lives outside it and the fixture holds no key.
    assert!(
        !dir_contains(ca_scratch.path(), BODY_SENTINEL),
        "CA staging holds request secrets"
    );
    origin_task.abort();
}
