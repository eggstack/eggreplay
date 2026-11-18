//! M021D: listener exposure and candidate request boundaries.
//!
//! Two operator-visible boundaries are asserted end to end through the built
//! binary:
//!
//! * `serve`/`record` refuse a non-loopback bind unless
//!   `--allow-non-loopback` is passed, and no inbound protocol choice can
//!   bypass the refusal;
//! * a candidate request never forwards a fixture-controlled `Host`, framing,
//!   or proxy-hop credential, while ordinary end-to-end headers still reach
//!   the target.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_eggreplay"))
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("CLI subprocess must run")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("stdout must be a JSON envelope ({error}): {stdout}");
    })
}

fn store_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../eggreplay-store/tests/fixtures")
        .join(name)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eggreplay-m021d-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of a known-good fixture, so a command may mutate its own copy.
fn copied_fixture(name: &str, to: &Path) {
    fn copy(source: &Path, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &destination);
            } else {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
    copy(&store_fixture(name), to);
}

/// Assert the shared refusal shape: exit `2`, `configuration` class in the
/// envelope and on stderr, and a message that names the bind, the missing
/// authentication, and the opt-in.
fn assert_listener_refusal(output: &Output, bind: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "a non-loopback bind without the opt-in must be a configuration failure; stderr={}",
        stderr_text(output)
    );
    let envelope = stdout_json(output);
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["failure_class"], "configuration");
    assert_eq!(envelope["payload"]["listen"], bind);
    let stderr = stderr_text(output);
    assert!(stderr.starts_with("configuration:"), "{stderr}");
    assert!(stderr.contains(bind), "the bind must be named: {stderr}");
    assert!(
        stderr.contains("unauthenticated"),
        "the listener is unauthenticated: {stderr}"
    );
    assert!(
        stderr.contains("TLS does not add authentication"),
        "TLS is not authentication: {stderr}"
    );
    assert!(
        stderr.contains("--allow-non-loopback"),
        "the opt-in must be named: {stderr}"
    );
}

/// Start a long-running command, read the operator line it prints once the
/// listener is bound, then stop it. The line is the only observable proof
/// that the process reached `bind()` rather than stopping at the gate.
fn startup_line(mut command: Command) -> String {
    use std::io::{BufRead, BufReader};
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn listener");
    let stderr = child.stderr.take().expect("stderr pipe");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read startup line");
    let _ = child.kill();
    let _ = child.wait();
    line
}

fn listener_command(args: &[&str]) -> Command {
    let mut command = Command::new(binary());
    command.args(args);
    command
}

#[test]
fn serve_refuses_a_wildcard_bind_without_the_opt_in() {
    let dir = temp_dir("serve-gate");
    let fixture = dir.join("fixture.eggr");
    copied_fixture("schema-1-with-flows", &fixture);
    for bind in ["0.0.0.0:0", "[::]:0"] {
        let output = run_cli(&[
            "serve",
            "--fixture",
            fixture.to_str().unwrap(),
            "--listen",
            bind,
            "--output",
            "json",
        ]);
        assert_listener_refusal(&output, bind);
    }
    // The refusal is configuration-only: the fixture is never opened.
    assert!(fixture.join("manifest.json").exists());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn record_refuses_a_wildcard_bind_without_the_opt_in() {
    let dir = temp_dir("record-gate");
    let fixture = dir.join("fixture.eggr");
    for bind in ["0.0.0.0:0", "[::]:0"] {
        let output = run_cli(&[
            "record",
            "--upstream",
            "http://127.0.0.1:9",
            "--fixture",
            fixture.to_str().unwrap(),
            "--listen",
            bind,
            "--output",
            "json",
        ]);
        assert_listener_refusal(&output, bind);
        assert!(
            !fixture.exists(),
            "a refused bind must not create the fixture"
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn serve_reaches_the_bind_with_the_opt_in() {
    let dir = temp_dir("serve-optin");
    let fixture = dir.join("fixture.eggr");
    copied_fixture("schema-1-with-flows", &fixture);
    let line = startup_line(listener_command(&[
        "serve",
        "--fixture",
        fixture.to_str().unwrap(),
        "--listen",
        "0.0.0.0:0",
        "--allow-non-loopback",
    ]));
    assert!(
        line.contains("serving") && line.contains("0.0.0.0:"),
        "the opt-in must reach bind(), got: {line}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn record_reaches_the_bind_with_the_opt_in() {
    let dir = temp_dir("record-optin");
    let fixture = dir.join("fixture.eggr");
    let line = startup_line(listener_command(&[
        "record",
        "--upstream",
        "http://127.0.0.1:9",
        "--fixture",
        fixture.to_str().unwrap(),
        "--listen",
        "0.0.0.0:0",
        "--allow-non-loopback",
    ]));
    assert!(
        line.contains("recording on") && line.contains("0.0.0.0:"),
        "the opt-in must reach bind(), got: {line}"
    );
    assert!(
        !fixture.exists(),
        "an interrupted recording never publishes a fixture"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn inbound_protocol_flags_cannot_bypass_the_gate() {
    let dir = temp_dir("protocol-bypass");
    let fixture = dir.join("fixture.eggr");
    copied_fixture("schema-1-with-flows", &fixture);
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, b"not-a-cert").unwrap();
    std::fs::write(&key, b"not-a-key").unwrap();

    // Cleartext HTTP/2 names a protocol, not an exposure policy.
    let h2 = run_cli(&[
        "serve",
        "--fixture",
        fixture.to_str().unwrap(),
        "--listen",
        "0.0.0.0:0",
        "--inbound",
        "http2",
        "--output",
        "json",
    ]);
    assert_listener_refusal(&h2, "0.0.0.0:0");

    // TLS protects bytes in transit; it does not authenticate the client.
    let tls = run_cli(&[
        "serve",
        "--fixture",
        fixture.to_str().unwrap(),
        "--listen",
        "[::]:0",
        "--inbound-tls-cert",
        cert.to_str().unwrap(),
        "--inbound-tls-key",
        key.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_listener_refusal(&tls, "[::]:0");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn every_serve_record_mode_is_gated() {
    let dir = temp_dir("modes");
    let missing = dir.join("missing.eggr");
    let existing = dir.join("existing.eggr");
    copied_fixture("schema-1-with-flows", &existing);

    for (mode, fixture, extra) in [
        ("sealed", &missing, Vec::new()),
        ("once", &missing, vec!["--upstream", "http://127.0.0.1:9"]),
        (
            "append-new",
            &existing,
            vec!["--upstream", "http://127.0.0.1:9"],
        ),
        (
            "re-record",
            &existing,
            vec!["--upstream", "http://127.0.0.1:9"],
        ),
    ] {
        let mut args = vec![
            "serve",
            "--fixture",
            fixture.to_str().unwrap(),
            "--record-mode",
            mode,
            "--listen",
            "0.0.0.0:0",
            "--output",
            "json",
        ];
        args.extend(extra);
        let output = run_cli(&args);
        assert_listener_refusal(&output, "0.0.0.0:0");
    }
    // No mode may have staged, deleted, or replaced anything.
    assert!(existing.join("manifest.json").exists());
    std::fs::remove_dir_all(dir).ok();
}

/// Publish a one-flow fixture whose recorded request carries framing, a
/// foreign authority, and a proxy-hop credential.
fn write_trace_fixture(path: &Path) {
    use eggreplay_core::{
        Flow, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse, Provenance, SCHEMA_VERSION,
        SessionMetadata,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};

    let mut writer =
        SessionWriter::create(path, SessionMetadata::default(), StoreLimits::default()).unwrap();
    let mut sink = writer.begin_blob().unwrap();
    sink.write_all(b"hello").unwrap();
    let request_body = sink.finish().unwrap();
    let mut sink = writer.begin_blob().unwrap();
    sink.write_all(b"ok").unwrap();
    let response_body = sink.finish().unwrap();
    writer
        .append_flow(&Flow {
            schema_version: SCHEMA_VERSION,
            id: "flow-trace".into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: "POST".into(),
                scheme: "http".into(),
                authority: "internal.example".into(),
                path: "/trace".into(),
                query: vec![],
                headers: vec![
                    HeaderEntry {
                        name: "Host".into(),
                        value: "internal.example".into(),
                    },
                    HeaderEntry {
                        name: "Connection".into(),
                        value: "keep-alive".into(),
                    },
                    HeaderEntry {
                        name: "Transfer-Encoding".into(),
                        value: "chunked".into(),
                    },
                    HeaderEntry {
                        name: "Content-Length".into(),
                        value: "999".into(),
                    },
                    HeaderEntry {
                        name: "Proxy-Authorization".into(),
                        value: "Basic SENTINEL".into(),
                    },
                    HeaderEntry {
                        name: "X-Trace".into(),
                        value: "trace-cli".into(),
                    },
                ],
                body: request_body,
                trailers: vec![],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![],
                body: response_body,
                trailers: vec![],
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![],
        })
        .unwrap();
    drop(writer.finish().unwrap());
}

/// Accept one request, capture its raw bytes, and answer `200 ok`.
fn capture_one_request() -> (SocketAddr, std::sync::mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut head_end: Option<usize> = None;
        let mut body_length = 0usize;
        loop {
            if let Some(end) = head_end
                && raw.len() >= end + 4 + body_length
            {
                break;
            }
            let count = stream.read(&mut chunk).unwrap_or(0);
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..count]);
            if head_end.is_none()
                && let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n")
            {
                head_end = Some(index);
                let head = String::from_utf8_lossy(&raw[..index]).to_ascii_lowercase();
                body_length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
            }
        }
        let _ = sender.send(raw);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    });
    (address, receiver)
}

fn request_parts(raw: &[u8]) -> (String, Vec<(String, String)>, Vec<u8>) {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("request must have a head terminator");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_owned();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    (request_line, headers, body)
}

#[test]
fn test_sends_the_target_host_and_never_the_fixture_framing() {
    let dir = temp_dir("headers");
    let fixture = dir.join("trace.eggr");
    write_trace_fixture(&fixture);
    let (address, captured) = capture_one_request();

    let output = run_cli(&[
        "test",
        "--fixture",
        fixture.to_str().unwrap(),
        "--target",
        &format!("http://{address}"),
        "--output",
        "json",
    ]);

    let raw = captured
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("the candidate request must reach the target");
    let (request_line, headers, body) = request_parts(&raw);
    assert_eq!(request_line, "POST /trace HTTP/1.1");
    let expected_host = address.to_string();
    let hosts = headers
        .iter()
        .filter(|(name, _)| name == "host")
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        hosts,
        vec![expected_host.as_str()],
        "exactly one Host, taken from --target: {headers:?}"
    );
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| name == "content-length")
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>(),
        vec![body.len().to_string()],
        "framing comes from the materialized body, not the fixture"
    );
    assert_eq!(body, b"hello");
    assert_eq!(
        headers
            .iter()
            .find(|(name, _)| name == "x-trace")
            .map(|(_, value)| value.as_str()),
        Some("trace-cli"),
        "ordinary end-to-end headers are preserved"
    );
    let wire = String::from_utf8_lossy(&raw);
    for sentinel in [
        "internal.example",
        "SENTINEL",
        "keep-alive",
        "chunked",
        "999",
    ] {
        assert!(
            !wire.contains(sentinel),
            "{sentinel} must not reach the target: {wire}"
        );
    }
    // The command executed a candidate (it reached the target above), so its
    // only acceptable classes are success or a comparison finding.
    let class = stdout_json(&output)["failure_class"].clone();
    assert!(
        class.is_null() || class == "regression",
        "unexpected failure class: {class} (stderr={})",
        stderr_text(&output)
    );
    // Diagnostics must not echo the proxy credential either.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr_text(&output)
    );
    assert!(!combined.contains("SENTINEL"), "{combined}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn ordinary_fixture_headers_still_execute_and_compare() {
    let dir = temp_dir("ordinary");
    let fixture = dir.join("ordinary.eggr");
    write_ordinary_fixture(&fixture);
    let (address, captured) = capture_one_request();

    let output = run_cli(&[
        "test",
        "--fixture",
        fixture.to_str().unwrap(),
        "--target",
        &format!("http://{address}"),
        "--output",
        "json",
    ]);
    let raw = captured
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("the candidate request must reach the target");
    let (_, headers, _) = request_parts(&raw);
    for (name, value) in [
        ("x-trace", "ordinary-1"),
        ("authorization", "Bearer token"),
        ("user-agent", "eggreplay-m021d"),
    ] {
        assert_eq!(
            headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| value.as_str()),
            Some(value),
            "{name} must survive"
        );
    }
    // Header-only findings are acceptable; the command itself must not fail
    // as a configuration/runtime error.
    let class = stdout_json(&output)["failure_class"].clone();
    assert!(
        class.is_null() || class == "regression",
        "unexpected failure class: {class} (stderr={})",
        stderr_text(&output)
    );
    std::fs::remove_dir_all(dir).ok();
}

/// The ordinary fixture used by the end-to-end comparison check.
fn write_ordinary_fixture(path: &Path) {
    use eggreplay_core::{
        BodyRef, Flow, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse, Provenance,
        SCHEMA_VERSION, SessionMetadata,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};

    let mut writer =
        SessionWriter::create(path, SessionMetadata::default(), StoreLimits::default()).unwrap();
    let mut sink = writer.begin_blob().unwrap();
    sink.write_all(b"ok").unwrap();
    let response_body = sink.finish().unwrap();
    writer
        .append_flow(&Flow {
            schema_version: SCHEMA_VERSION,
            id: "flow-ordinary".into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: "GET".into(),
                scheme: "http".into(),
                authority: "internal.example".into(),
                path: "/ordinary".into(),
                query: vec![],
                headers: vec![
                    HeaderEntry {
                        name: "X-Trace".into(),
                        value: "ordinary-1".into(),
                    },
                    HeaderEntry {
                        name: "Authorization".into(),
                        value: "Bearer token".into(),
                    },
                    HeaderEntry {
                        name: "User-Agent".into(),
                        value: "eggreplay-m021d".into(),
                    },
                ],
                body: BodyRef::Empty,
                trailers: vec![],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![HeaderEntry {
                    name: "Content-Length".into(),
                    value: "2".into(),
                }],
                body: response_body,
                trailers: vec![],
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![],
        })
        .unwrap();
    drop(writer.finish().unwrap());
}
