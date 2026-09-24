//! Published transport/TLS substrate preflight proofs for M013A.

use std::sync::Arc;

use eggserve_primitives::{Response, ResponseBody, StatusCode, connection_info::TlsInfo};
use eggserve_server::{
    ConnectionContext, ConnectionShutdown, RuntimeConfig, RuntimeState,
    connection::serve_http1_connection, service_fn,
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

        let config = Arc::new(RuntimeConfig {
            max_requests_per_connection: Some(1),
            ..RuntimeConfig::default()
        });
        let state = Arc::new(RuntimeState::new(&config));
        let shutdown = ConnectionShutdown::new();
        let service = service_fn(|_request| async {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(ResponseBody::Bytes(b"substrate-ok".to_vec()))
                .unwrap())
        });
        serve_http1_connection(stream, service, config, context, state, &shutdown).await
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
