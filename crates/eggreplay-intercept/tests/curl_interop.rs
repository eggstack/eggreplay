//! M013F independent interop: drive the proxy with the `curl` command-line
//! client (an implementation independent of the scripted rustls and `EggFetch`
//! clients used elsewhere).
//!
//! Hermetic and local-only: every listener, origin, and CA directory is a
//! loopback/`tempfile` construct. No Internet access occurs.
//!
//! Both tests skip (without failing) when `curl` is absent from `PATH`, so
//! minimal hosted images stay green while qualifying images prove the
//! command-line path. Qualifies plain HTTP proxying and HTTPS CONNECT/MITM.

use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use eggreplay_core::SessionMetadata;
use eggreplay_intercept::{
    CaAuthority, CaOptions, ConnectAction, ExplicitProxyConfig, HostMatch, LeafIssuer, LeafOptions,
    MitmConfig, PortMatch, ProxyListenerConfig, ProxyRoute, RequestKind, Rule, RuleAction,
    TargetPolicy, export_ca_cert, start_explicit_proxy,
};
use eggreplay_store::{RecordingSession, StoreLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn curl_available() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Run curl without starving the (current-thread) test runtime: the blocking
/// child-wait runs on the blocking pool while server tasks keep progressing.
async fn run_curl(args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args(&args)
            .output()
            .expect("curl must run once detected")
    })
    .await
    .expect("blocking pool must run curl")
}

/// Minimal plaintext HTTP origin answering a fixed body.
async fn start_plain_origin() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    if stream.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                    if head.len() > 65_536 || head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let body = b"curl-plain-ok";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut out = response.into_bytes();
                out.extend_from_slice(body);
                stream.write_all(&out).await.ok();
            });
        }
    });
    (addr, task)
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
    _ca_scratch: Option<tempfile::TempDir>,
    ca_der: Vec<u8>,
    ca_cert_pem_path: Option<std::path::PathBuf>,
}

async fn start_plain_proxy() -> ProxyFixture {
    let dir = tempfile::TempDir::new().unwrap();
    let session = RecordingSession::create(
        dir.path().join("fixture"),
        SessionMetadata::default(),
        StoreLimits::default(),
    )
    .unwrap();
    let config = ExplicitProxyConfig::new(
        session.clone(),
        allow_127_any_port(ConnectAction::Deny),
        ProxyRoute::direct(),
    );
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    ProxyFixture {
        handle,
        session,
        dir,
        _ca_scratch: None,
        ca_der: Vec::new(),
        ca_cert_pem_path: None,
    }
}

async fn finish_proxy(fixture: ProxyFixture) {
    // Keep the fixture directory alive until the session finalized; the
    // binding above owns it, this read pins the field against dead-code.
    debug_assert!(fixture.dir.path().exists());
    fixture.handle.shutdown();
    fixture.handle.wait().await;
    fixture.session.shutdown();
}

#[tokio::test]
async fn curl_plain_http_proxies_and_records() {
    if !curl_available() {
        eprintln!("SKIP curl_plain_http_proxies_and_records: curl not in PATH");
        return;
    }
    let (origin_addr, origin_task) = start_plain_origin().await;
    let proxy = start_plain_proxy().await;
    let proxy_addr = proxy.handle.local_addr();

    let output = run_curl(vec![
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--max-time".to_owned(),
        "20".to_owned(),
        "--proxy".to_owned(),
        format!("http://{proxy_addr}"),
        format!("http://127.0.0.1:{}/hello", origin_addr.port()),
    ])
    .await;
    assert!(
        output.status.success(),
        "curl through explicit proxy must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "curl-plain-ok",
        "curl must receive the origin body through the proxy"
    );
    assert_eq!(proxy.session.flow_count(), 1, "plain curl flow must record");

    finish_proxy(proxy).await;
    origin_task.abort();
}

// --- MITM leg: single-stack TLS origin + curl --cacert ---

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

async fn start_tls_origin(cert: OriginCert) -> (u16, tokio::task::JoinHandle<()>) {
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
    let task = tokio::spawn(async move {
        use tokio_rustls::TlsAcceptor;
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = TlsAcceptor::from(server_tls.clone());
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(socket).await else {
                    return;
                };
                let mut tls = tls;
                loop {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
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
                    let body = b"curl-mitm-ok";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let mut out = response.into_bytes();
                    out.extend_from_slice(body);
                    if tls.write_all(&out).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, task)
}

async fn start_mitm_proxy(origin_pem: &[u8]) -> ProxyFixture {
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
    let ca_cert_pem_path = ca_scratch.path().join("ca-cert.pem");
    export_ca_cert(&ca_scratch.path().join("ca"), &ca_cert_pem_path).unwrap();
    let issuer = Arc::new(LeafIssuer::new(ca, &LeafOptions::default()).unwrap());
    let mut mitm = MitmConfig::new(issuer);
    mitm.upstream_tls = Some(
        eggfetch_core::TlsConfig::builder()
            .ca_certificate_pem(origin_pem)
            .unwrap()
            .build(),
    );
    let mut config = ExplicitProxyConfig::new(
        session.clone(),
        allow_127_any_port(ConnectAction::Intercept),
        ProxyRoute::direct(),
    );
    config.mitm = Some(mitm);
    let listener = ProxyListenerConfig::loopback("127.0.0.1:0".parse().unwrap()).unwrap();
    let handle = start_explicit_proxy(listener, config).await.unwrap();
    ProxyFixture {
        handle,
        session,
        dir,
        _ca_scratch: Some(ca_scratch),
        ca_der,
        ca_cert_pem_path: Some(ca_cert_pem_path),
    }
}

#[tokio::test]
async fn curl_https_connect_mitm_records() {
    if !curl_available() {
        eprintln!("SKIP curl_https_connect_mitm_records: curl not in PATH");
        return;
    }
    let origin = make_origin_cert();
    let origin_pem = origin.cert_pem.clone();
    let (origin_port, origin_task) = start_tls_origin(origin).await;
    let proxy = start_mitm_proxy(origin_pem.as_bytes()).await;
    let proxy_addr = proxy.handle.local_addr();
    let ca_pem = proxy.ca_cert_pem_path.clone().unwrap();

    // Independent client: curl performs CONNECT, validates the minted leaf
    // against the exported interception CA, and negotiates http/1.1.
    // (`--proxy-cacert` exists only on newer curl; the proxy leg itself is
    // plaintext HTTP so it is unnecessary here — probe before using it.)
    let version_output = Command::new("curl").arg("--version").output();
    let version_text = version_output.map_or_else(
        |_| String::new(),
        |output| String::from_utf8_lossy(&output.stdout).into_owned(),
    );
    let help_text = Command::new("curl").arg("--help").output().map_or_else(
        |_| String::new(),
        |output| String::from_utf8_lossy(&output.stdout).into_owned(),
    );
    let supports_proxy_cacert = help_text.contains("proxy-cacert");
    // Windows schannel curl performs revocation checking even against
    // `--cacert` anchors; M013 mints test leaves without CRL/OCSP (no
    // OCSP/CRL plumbing by plan), so schannel reports
    // CERT_TRUST_REVOCATION_STATUS_UNKNOWN. Disable only that check for the
    // hermetic test CA. The flag is schannel-only: probe before using it so
    // OpenSSL/Rustls curl builds never see an unknown option.
    let is_schannel = version_text.to_lowercase().contains("schannel");
    let mut args: Vec<String> = vec![
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--max-time".to_owned(),
        "20".to_owned(),
        "--proxy".to_owned(),
        format!("http://{proxy_addr}"),
        "--cacert".to_owned(),
        ca_pem.to_str().unwrap().to_owned(),
    ];
    if is_schannel {
        args.push("--ssl-no-revoke".to_owned());
    }
    if supports_proxy_cacert {
        args.push("--proxy-cacert".to_owned());
        args.push(ca_pem.to_str().unwrap().to_owned());
    }
    args.push(format!("https://127.0.0.1:{origin_port}/secure"));
    let output = run_curl(args).await;
    assert!(
        output.status.success(),
        "curl CONNECT+MITM must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "curl-mitm-ok",
        "curl must receive the origin body through MITM"
    );
    assert_eq!(proxy.session.flow_count(), 1, "curl MITM flow must record");
    // The minted leaf chains to the interception CA (curl verified it).
    assert!(!proxy.ca_der.is_empty());

    finish_proxy(proxy).await;
    origin_task.abort();
}
