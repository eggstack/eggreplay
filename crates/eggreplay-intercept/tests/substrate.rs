//! Published transport/TLS substrate preflight proofs for M013A.

use std::sync::Arc;

use eggserve_primitives::{
    Response, ResponseBody, StatusCode, connection_info::TlsInfo, request_target::RequestTargetForm,
};
use eggserve_server::{
    AdmissionOwnership, ConnectionContext, ConnectionShutdown, H1PolicyOwnership,
    Http1RequestTargetMode, RuntimeConfig, RuntimeState,
    connection::serve_http1_connection_with_policy, service_fn,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[tokio::test]
async fn published_eggserve_h1_driver_accepts_decrypted_tls_stream() {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = CertificateDer::from(generated.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let client_tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_config = Arc::new(server_tls);
    let task = tokio::spawn(async move {
        let (socket, peer) = listener.accept().await.unwrap();
        let local = socket.local_addr().unwrap();
        let stream = TlsAcceptor::from(server_config)
            .accept(socket)
            .await
            .unwrap();
        let connection = stream.get_ref().1;
        let tls = TlsInfo {
            protocol_version: connection.protocol_version().map(|version| match version {
                rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_owned(),
                rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_owned(),
                _ => "unknown".to_owned(),
            }),
            server_name: connection.server_name().map(str::to_owned),
            alpn: connection
                .alpn_protocol()
                .map(|protocol| String::from_utf8_lossy(protocol).into_owned()),
            ..TlsInfo::default()
        };
        assert_eq!(tls.server_name.as_deref(), Some("localhost"));
        assert_eq!(tls.alpn, None);
        let context = ConnectionContext::for_tcp(local, peer, Some(tls));
        assert_eq!(
            context.scheme,
            eggserve_primitives::connection_info::Scheme::Https
        );

        let config = Arc::new(
            RuntimeConfig::builder()
                .http1_request_target_mode(Http1RequestTargetMode::OriginOnly)
                .policy_ownership(H1PolicyOwnership::eggserve_owned())
                .admission_ownership(AdmissionOwnership::eggserve_owned())
                .max_requests_per_connection(Some(1))
                .build()
                .expect("qualified M013A caller-owned profile"),
        );
        let policy = Arc::new(
            config
                .h1_connection_policy()
                .expect("validated H1 policy projection"),
        );
        let state =
            Arc::new(RuntimeState::try_new(&config).expect("validated runtime admission state"));
        let shutdown = ConnectionShutdown::new();
        let service = service_fn(|_request| async {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(ResponseBody::Bytes(b"substrate-ok".to_vec()))
                .unwrap())
        });
        serve_http1_connection_with_policy(stream, service, policy, context, state, &shutdown).await
    });

    let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let connector = TlsConnector::from(Arc::new(client_tls));
    let mut client = connector
        .connect(
            ServerName::try_from("localhost".to_owned()).unwrap(),
            socket,
        )
        .await
        .unwrap();
    client
        .write_all(b"GET /substrate HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).contains("substrate-ok"));
    assert!(task.await.unwrap().is_clean());
}

#[tokio::test]
async fn eggress_raw_connect_routes_through_local_http_proxy_without_fallback() {
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_task = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        stream.write_all(b"pong").await.unwrap();
    });

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let proxy_task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            client.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 4096, "CONNECT header bound exceeded");
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with(&format!("CONNECT {target_addr} HTTP/1.1\r\n")));
        let mut upstream = tokio::net::TcpStream::connect(target_addr).await.unwrap();
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .unwrap();
    });

    let connector = egress_connector(&format!("http://{proxy_addr}"));
    let (mut stream, _) = connector
        .connect_tcp_detailed("127.0.0.1", target_addr.port())
        .await
        .unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut response = [0; 4];
    stream.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"pong");
    drop(stream);
    target_task.await.unwrap();
    proxy_task.await.unwrap();

    let direct_target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_addr = direct_target.local_addr().unwrap();
    let direct_task = tokio::spawn(async move {
        let (mut stream, _) = direct_target.accept().await.unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte[0], b'd');
        stream.write_all(b"D").await.unwrap();
    });
    let (mut direct, _) = eggress_outbound::OutboundConnector::direct()
        .connect_tcp_detailed("127.0.0.1", direct_addr.port())
        .await
        .unwrap();
    direct.write_all(b"d").await.unwrap();
    let mut direct_response = [0];
    direct.read_exact(&mut direct_response).await.unwrap();
    assert_eq!(direct_response[0], b'D');
    drop(direct);
    direct_task.await.unwrap();

    // A configured dead proxy fails despite a live direct target. This is the
    // substrate-level no-fallback guarantee used by the interception route.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let connector = egress_connector(&format!("http://{dead_addr}"));
    assert!(
        connector
            .connect_tcp_detailed("127.0.0.1", target_addr.port())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn live_opaque_egress_relay_closes_when_route_task_is_cancelled() {
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut opaque = Vec::new();
        stream.read_to_end(&mut opaque).await.unwrap();
    });
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let relay_task = tokio::spawn(async move {
        let (mut client, _) = proxy.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            client.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 4096);
        }
        let mut upstream = tokio::net::TcpStream::connect(origin_addr).await.unwrap();
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .unwrap();
    });
    let connector = egress_connector(&format!("http://{proxy_addr}"));
    let (mut tunnel, _) = connector
        .connect_tcp_detailed("127.0.0.1", origin_addr.port())
        .await
        .unwrap();
    relay_task.abort();
    let mut byte = [0];
    let closed = tokio::time::timeout(std::time::Duration::from_secs(2), tunnel.read(&mut byte))
        .await
        .expect("cancelled opaque relay releases the route")
        .map_or(true, |count| count == 0);
    assert!(closed);
    drop(tunnel);
    origin_task.await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn eggfetch_requires_explicit_ca_and_checks_hostname_and_sni() {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert_der = CertificateDer::from(generated.cert.der().to_vec());
    let cert_pem = generated.cert.pem();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_tls = Arc::new(server_tls);
    let observed_sni = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let server_observed_sni = observed_sni.clone();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let Ok(Ok((socket, _))) =
                tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await
            else {
                break;
            };
            let acceptor = TlsAcceptor::from(server_tls.clone());
            let Ok(mut tls) = acceptor.accept(socket).await else {
                continue;
            };
            server_observed_sni
                .lock()
                .await
                .push(tls.get_ref().1.server_name().map(str::to_owned));
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                if tls.read_exact(&mut byte).await.is_err() {
                    break;
                }
                request.push(byte[0]);
                assert!(request.len() < 8192, "HTTP request header bound exceeded");
            }
            if !request.is_empty() {
                tls.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .await
                .unwrap();
            }
        }
    });

    // EggFetch uses the same Eggress route adapter as EggReplay's existing
    // record path. The local HTTP proxy carries CONNECT but does not terminate
    // TLS, so the origin's SNI and certificate checks remain EggFetch-owned.
    let route_proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let route_proxy_addr = route_proxy.local_addr().unwrap();
    let route_proxy_task = tokio::spawn(async move {
        for _ in 0..3 {
            let Ok(Ok((mut client, _))) =
                tokio::time::timeout(std::time::Duration::from_secs(2), route_proxy.accept()).await
            else {
                break;
            };
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                client.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 4096, "CONNECT header bound exceeded");
            }
            let received = String::from_utf8_lossy(&headers);
            if !(received.starts_with(&format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n"))
                || received.starts_with(&format!("CONNECT localhost:{port} HTTP/1.1\r\n")))
            {
                panic!("unexpected CONNECT prelude: {received:?}");
            }
            let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
    });

    let connector = egress_connector(&format!("http://{route_proxy_addr}"));
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .dialer(eggreplay_http::EggressDialer::new(connector))
        .tls_config(
            eggfetch_core::TlsConfig::builder()
                .ca_certificate_pem(cert_pem.as_bytes())
                .unwrap()
                .build(),
        )
        .build();
    let mut response = client
        .get(&format!("https://localhost:{port}/secure"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");

    let untrusted = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .dialer(eggreplay_http::EggressDialer::direct())
        .build();
    assert!(
        untrusted
            .get(&format!("https://localhost:{port}/untrusted"))
            .unwrap()
            .send()
            .await
            .is_err()
    );

    let wrong_host = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client
            .get(&format!("https://127.0.0.1:{port}/wrong-host"))
            .unwrap()
            .send(),
    )
    .await
    .expect("hostname verification completes promptly");
    assert!(wrong_host.is_err());
    server.await.unwrap();
    route_proxy_task.await.unwrap();
    let sni = observed_sni.lock().await;
    assert!(sni.iter().any(|name| name.as_deref() == Some("localhost")));
}

fn egress_connector(route: &str) -> eggress_outbound::OutboundConnector {
    eggress_outbound::OutboundConnector::from_pproxy_uri(route)
        .expect("qualified pproxy-compatible route")
}

#[derive(Debug, Default)]
struct AbsoluteObservation {
    target_form: Option<eggserve_primitives::request_target::RequestTargetForm>,
    scheme: Option<String>,
    uri_authority: Option<String>,
    path: Option<String>,
    query: Option<String>,
    raw_target: Option<String>,
    authority: Option<String>,
    header_order: Vec<String>,
    body_bytes: Vec<u8>,
    body_complete: bool,
    trailers: Vec<(String, String)>,
    called: bool,
}

async fn observe_request(
    request: eggserve_primitives::Request,
    observation: std::sync::Arc<tokio::sync::Mutex<AbsoluteObservation>>,
) -> Result<eggserve_primitives::Response, eggserve_server::ServiceError> {
    let (head, mut body) = request.into_head_and_body();
    let header_order = head
        .headers()
        .iter()
        .map(|field| {
            format!(
                "{}:{}",
                field.name.as_str(),
                std::str::from_utf8(field.value.as_bytes()).unwrap_or("<binary>")
            )
        })
        .collect();
    let mut collected = AbsoluteObservation {
        target_form: Some(head.target().form()),
        scheme: head.target().scheme().map(str::to_owned),
        uri_authority: head
            .target()
            .uri_authority()
            .map(|authority| authority.as_str().to_owned()),
        path: Some(head.target().path().to_owned()),
        query: head.target().query().map(str::to_owned),
        raw_target: Some(head.target().raw().to_owned()),
        authority: head
            .authority()
            .map(|authority| authority.as_str().to_owned()),
        header_order,
        body_bytes: Vec::new(),
        body_complete: false,
        trailers: Vec::new(),
        called: true,
    };
    while let Some(chunk) = body
        .next_chunk()
        .await
        .map_err(|error| eggserve_server::ServiceError::internal(error.to_string()))?
    {
        collected.body_bytes.extend_from_slice(&chunk);
    }
    collected.body_complete = true;
    if let Some(trailers) = body
        .trailers()
        .await
        .map_err(|error| eggserve_server::ServiceError::internal(error.to_string()))?
    {
        collected.trailers = trailers
            .as_block()
            .iter()
            .map(|field| {
                (
                    field.name.as_str().to_owned(),
                    std::str::from_utf8(field.value.as_bytes())
                        .unwrap_or("")
                        .to_owned(),
                )
            })
            .collect();
    }
    *observation.lock().await = collected;
    Ok(eggserve_primitives::Response::builder()
        .status(eggserve_primitives::StatusCode::OK)
        .body(eggserve_primitives::ResponseBody::Empty)
        .unwrap())
}

async fn drive_request(
    config: eggserve_server::RuntimeConfig,
    body_policy: eggserve_primitives::RequestBodyPolicy,
    request: &[u8],
) -> (Vec<u8>, AbsoluteObservation) {
    let config = std::sync::Arc::new(config);
    let policy = std::sync::Arc::new(
        config
            .h1_connection_policy()
            .expect("validated H1 policy projection"),
    );
    let state = std::sync::Arc::new(
        eggserve_server::RuntimeState::try_new(&config).expect("validated runtime state"),
    );
    let observation: std::sync::Arc<tokio::sync::Mutex<AbsoluteObservation>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(AbsoluteObservation::default()));
    let observation_service = observation.clone();
    let service = eggserve_server::service_fn_with_policy(
        move |request: eggserve_primitives::Request| {
            observe_request(request, observation_service.clone())
        },
        body_policy,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = eggserve_server::ConnectionShutdown::new();
    let driver_task = tokio::spawn(async move {
        let (socket, _peer) = listener.accept().await.unwrap();
        let local = socket.local_addr().unwrap();
        let context = eggserve_server::ConnectionContext::for_tcp(local, addr, None);
        eggserve_server::connection::serve_http1_connection_with_policy(
            socket, service, policy, context, state, &shutdown,
        )
        .await
    });

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(request).await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    let _ = driver_task.await.unwrap();
    let observation = std::sync::Arc::try_unwrap(observation)
        .expect("service observation must be uniquely owned after driver drains")
        .into_inner();
    (response, observation)
}

fn interception_profile_config() -> eggserve_server::RuntimeConfig {
    eggreplay_intercept::InterceptionProfile::loopback("127.0.0.1:0".parse().unwrap())
        .build_runtime_config()
        .expect("qualified M013B interception profile")
}

#[tokio::test]
async fn interception_listener_exposes_absolute_target_metadata() {
    let raw = b"GET http://example.test:8080/a?b=1 HTTP/1.1\r\nHost: example.test:8080\r\nConnection: close\r\nX-Dup: first\r\nX-Dup: second\r\n\r\n";
    let (response, observation) = drive_request(
        interception_profile_config(),
        eggserve_primitives::RequestBodyPolicy::Reject,
        raw,
    )
    .await;
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "absolute-form request must reach the service; got: {}",
        String::from_utf8_lossy(&response)
    );

    assert!(
        observation.called,
        "service must observe absolute-form request"
    );
    assert_eq!(observation.target_form, Some(RequestTargetForm::Absolute));
    assert_eq!(observation.scheme.as_deref(), Some("http"));
    assert_eq!(
        observation.uri_authority.as_deref(),
        Some("example.test:8080")
    );
    assert_eq!(observation.path.as_deref(), Some("/a"));
    assert_eq!(observation.query.as_deref(), Some("b=1"));
    assert_eq!(
        observation.raw_target.as_deref(),
        Some("http://example.test:8080/a?b=1")
    );
    assert_eq!(
        observation.authority.as_deref(),
        Some("example.test:8080"),
        "canonical request authority must align with URI authority + Host"
    );
    let dup_indices: Vec<_> = observation
        .header_order
        .iter()
        .enumerate()
        .filter(|(_, value)| value.starts_with("x-dup:"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        dup_indices.len(),
        2,
        "duplicate non-Host headers must be preserved"
    );
    let first = observation.header_order[dup_indices[0]].clone();
    let second = observation.header_order[dup_indices[1]].clone();
    assert_eq!(first, "x-dup:first");
    assert_eq!(second, "x-dup:second");
}

#[tokio::test]
async fn interception_listener_rejects_host_authority_mismatch_before_service() {
    let (response, observation) = drive_request(
        interception_profile_config(),
        eggserve_primitives::RequestBodyPolicy::Reject,
        b"GET http://example.test:8080/a HTTP/1.1\r\nHost: other.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 400"),
        "Host/URI authority mismatch must yield 400; got: {text}"
    );
    assert!(
        !observation.called,
        "service must not run when authority is contradictory"
    );
}

#[tokio::test]
async fn interception_listener_rejects_oversized_absolute_target_before_service() {
    let mut path = String::from("/");
    while path.len() < 300 {
        path.push('a');
    }
    let request = format!(
        "GET http://example.test{path} HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n"
    );
    let config = eggserve_server::RuntimeConfig::builder()
        .bind("127.0.0.1:0".parse().unwrap())
        .http1_request_target_mode(eggserve_server::Http1RequestTargetMode::OriginOrAbsolute)
        .policy_ownership(eggserve_server::H1PolicyOwnership::eggserve_owned())
        .admission_ownership(eggserve_server::AdmissionOwnership::eggserve_owned())
        .max_request_target_bytes(128)
        .build()
        .expect("qualified narrowed target ceiling");
    let (response, observation) = drive_request(
        config,
        eggserve_primitives::RequestBodyPolicy::Reject,
        request.as_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 414"),
        "absolute-form target over the ceiling must yield 414; got: {text}"
    );
    assert!(
        !observation.called,
        "service must not run when target exceeds the ceiling"
    );
}

#[tokio::test]
async fn interception_listener_origin_form_still_succeeds_under_origin_or_absolute() {
    let (response, observation) = drive_request(
        interception_profile_config(),
        eggserve_primitives::RequestBodyPolicy::Reject,
        b"GET /origin HTTP/1.1\r\nHost: example.test:8080\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "origin-form must still succeed under OriginOrAbsolute; got: {text}"
    );
    assert!(observation.called);
    assert_eq!(observation.target_form, Some(RequestTargetForm::Origin));
    assert_eq!(observation.scheme, None);
    assert_eq!(observation.uri_authority, None);
    assert_eq!(observation.path.as_deref(), Some("/origin"));
    assert_eq!(observation.authority.as_deref(), Some("example.test:8080"));
}

#[tokio::test]
async fn interception_listener_connect_authority_form_takes_the_tunnel_path() {
    use eggserve_server::tunnel::TunnelIo;
    let config = std::sync::Arc::new(interception_profile_config());
    let policy = std::sync::Arc::new(
        config
            .h1_connection_policy()
            .expect("validated H1 policy projection"),
    );
    let state = std::sync::Arc::new(
        eggserve_server::RuntimeState::try_new(&config).expect("validated runtime state"),
    );
    let service = eggserve_server::service_fn_with_tunnel(
        |request: eggserve_primitives::Request,
         tunnel: Option<eggserve_server::tunnel::TunnelCapability>| async move {
            if let Some(capability) = tunnel {
                let target = capability
                    .request()
                    .authority()
                    .map(|authority| authority.as_str().to_owned())
                    .unwrap_or_default();
                let echo = format!(
                    "CONNECT {}\r\nEchoed-Target: {}\r\n",
                    target,
                    request.head().target().raw()
                );
                let handler = move |mut io: TunnelIo| async move {
                    let _ = io.write_all(echo.as_bytes()).await;
                };
                return capability
                    .accept(eggserve_primitives::HeaderBlock::new(), handler)
                    .map_err(|error| eggserve_server::ServiceError::internal(error.to_string()));
            }
            Ok(eggserve_primitives::Response::builder()
                .status(eggserve_primitives::StatusCode::OK)
                .body(eggserve_primitives::ResponseBody::Empty)
                .unwrap())
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = eggserve_server::ConnectionShutdown::new();
    let driver_task = tokio::spawn(async move {
        let (socket, _peer) = listener.accept().await.unwrap();
        let local = socket.local_addr().unwrap();
        let context = eggserve_server::ConnectionContext::for_tcp(local, addr, None);
        eggserve_server::connection::serve_http1_connection_with_policy(
            socket, service, policy, context, state, &shutdown,
        )
        .await
    });
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"CONNECT example.test:443 HTTP/1.1\r\nHost: example.test:443\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "CONNECT must reach the tunnel acceptance path; got: {text}"
    );
    assert!(
        text.contains("CONNECT example.test:443"),
        "tunnel echo must include the authority-form target, not an absolute form"
    );
    let _ = driver_task.await.unwrap();
}

#[tokio::test]
async fn interception_listener_streams_absolute_form_chunked_body_with_trailers() {
    let head = b"POST http://example.test:8080/upload?kind=chunked HTTP/1.1\r\nHost: example.test:8080\r\nTransfer-Encoding: chunked\r\nX-Dup: one\r\nX-Dup: two\r\nConnection: close\r\n\r\n";
    let chunks = b"5\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: done\r\nX-Other: ok\r\n\r\n";
    let mut request = Vec::new();
    request.extend_from_slice(head);
    request.extend_from_slice(chunks);

    let (response, observation) = drive_request(
        interception_profile_config(),
        eggserve_primitives::RequestBodyPolicy::Stream { max_bytes: 1024 },
        &request,
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "absolute-form chunked POST must reach the service; got: {text}"
    );
    assert!(observation.called);
    assert_eq!(observation.target_form, Some(RequestTargetForm::Absolute));
    assert_eq!(observation.scheme.as_deref(), Some("http"));
    assert_eq!(
        observation.uri_authority.as_deref(),
        Some("example.test:8080")
    );
    assert_eq!(observation.path.as_deref(), Some("/upload"));
    assert_eq!(observation.query.as_deref(), Some("kind=chunked"));
    assert_eq!(
        observation.raw_target.as_deref(),
        Some("http://example.test:8080/upload?kind=chunked")
    );
    assert_eq!(observation.authority.as_deref(), Some("example.test:8080"));
    let dup_indices: Vec<_> = observation
        .header_order
        .iter()
        .enumerate()
        .filter(|(_, value)| value.starts_with("x-dup:"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        dup_indices.len(),
        2,
        "duplicate end-to-end headers must preserve original order"
    );
    assert_eq!(observation.header_order[dup_indices[0]], "x-dup:one");
    assert_eq!(observation.header_order[dup_indices[1]], "x-dup:two");
    assert_eq!(observation.body_bytes, b"hello world");
    assert!(observation.body_complete);
    assert!(
        observation
            .trailers
            .iter()
            .any(|(name, _)| name == "x-trailer"),
        "terminal trailers must reach the service; got: {:?}",
        observation.trailers
    );
}

#[tokio::test]
async fn ordinary_origin_only_listener_rejects_absolute_form_before_service() {
    let config = eggserve_server::RuntimeConfig::builder()
        .bind("127.0.0.1:0".parse().unwrap())
        .http1_request_target_mode(eggserve_server::Http1RequestTargetMode::OriginOnly)
        .policy_ownership(eggserve_server::H1PolicyOwnership::eggserve_owned())
        .admission_ownership(eggserve_server::AdmissionOwnership::eggserve_owned())
        .build()
        .expect("qualified OriginOnly profile");
    let (response, observation) = drive_request(
        config,
        eggserve_primitives::RequestBodyPolicy::Reject,
        b"GET http://example.test:8080/a HTTP/1.1\r\nHost: example.test:8080\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 400"),
        "ordinary OriginOnly listener must reject absolute-form with 400; got: {text}"
    );
    assert!(
        !observation.called,
        "service must not run for an absolute-form request on an OriginOnly listener"
    );
}

#[test]
fn interception_profile_keeps_eggserve_owned_defaults_with_origin_or_absolute() {
    let profile =
        eggreplay_intercept::InterceptionProfile::loopback("127.0.0.1:0".parse().unwrap());
    let config = profile
        .build_runtime_config()
        .expect("qualified M013B interception profile");
    assert_eq!(
        config.http1_request_target_mode,
        eggserve_server::Http1RequestTargetMode::OriginOrAbsolute
    );
    let policy = config.policy_ownership;
    assert_eq!(
        policy.handler_deadline,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        policy.request_body_deadline,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        policy.keep_alive_idle_deadline,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        policy.response_write_progress_deadline,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        policy.global_request_body_ceiling,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        policy.request_target_ceiling,
        eggserve_server::PolicyOwner::EggServe
    );
    assert_eq!(
        config.admission_ownership.tunnels,
        eggserve_server::AdmissionOwner::EggServe
    );
    assert_eq!(
        config.admission_ownership.service_calls,
        eggserve_server::AdmissionOwner::EggServe
    );
    assert_eq!(config.bind, profile.bind);
    assert_eq!(config.max_connections, profile.max_connections);
    assert_eq!(
        config.max_in_flight_requests,
        profile.max_in_flight_requests
    );
    assert_eq!(config.max_active_tunnels, profile.max_active_tunnels);
    assert_eq!(
        config.max_request_body_bytes,
        profile.max_request_body_bytes
    );
    assert_eq!(
        config.max_request_target_bytes,
        profile.max_request_target_bytes
    );
    assert_eq!(
        config.connection_total_timeout,
        profile.connection_total_timeout
    );
    assert_eq!(
        config.keep_alive_idle_timeout,
        profile.keep_alive_idle_timeout
    );
    assert_eq!(
        config.response_write_timeout,
        profile.response_write_timeout
    );
}
