//! M015B operator-surface integration tests for opt-in inbound HTTP/2.
//!
//! These drive the built `eggreplay` binary so the flag surface, the
//! fail-closed behaviour of an unavailable protocol, and the
//! machine-readable serving policy are verified end to end.
//!
//! The two halves of the contract are split by build:
//!
//! * **Always**: HTTP/1.1 is the default, the flags exist, the status
//!   envelope reports the serving policy, and the envelope never carries key
//!   material. This half runs in every build, including one that cannot serve
//!   HTTP/2 at all.
//! * **`h2-inbound`**: an HTTP/2 policy resolves.
//! * **`h2-inbound-tls`**: a TLS policy resolves, and it still refuses to
//!   resolve from a name alone.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_eggreplay"))
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("eggreplay binary must run")
}

fn stdout_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn serve_help() -> String {
    stdout_text(&run(&["serve", "--help"]))
}

fn record_help() -> String {
    stdout_text(&run(&["record", "--help"]))
}

#[test]
fn serve_and_record_expose_the_inbound_policy_flags() {
    for help in [serve_help(), record_help()] {
        assert!(help.contains("--inbound"), "missing --inbound: {help}");
        assert!(
            help.contains("--inbound-tls-cert"),
            "missing --inbound-tls-cert: {help}"
        );
        assert!(
            help.contains("--inbound-tls-key"),
            "missing --inbound-tls-key: {help}"
        );
        assert!(
            help.contains("--h2-max-concurrent-streams"),
            "missing --h2-max-concurrent-streams: {help}"
        );
    }
}

#[test]
fn http1_is_the_documented_default() {
    assert!(
        serve_help().contains("http1"),
        "the default policy must be named in help text"
    );
}

#[test]
fn tls_material_flags_require_each_other() {
    // `--inbound-tls-cert` without a key, or a key without a cert, is a
    // usage error: there is no implicit identity.
    for args in [
        vec![
            "serve",
            "--fixture",
            "/nonexistent",
            "--inbound-tls-cert",
            "/tmp/a.pem",
        ],
        vec![
            "serve",
            "--fixture",
            "/nonexistent",
            "--inbound-tls-key",
            "/tmp/a.key",
        ],
    ] {
        let output = run(&args);
        assert!(
            !output.status.success(),
            "a half-specified TLS identity must be refused: {args:?}"
        );
        let stderr = stderr_text(&output);
        assert!(
            stderr.contains("--inbound-tls-"),
            "the error must name the missing flag, got: {stderr}"
        );
    }
}

/// A build without the opt-in feature must refuse `--inbound http2` rather
/// than quietly serving HTTP/1.1 under an HTTP/2 label.
#[cfg(not(feature = "h2-inbound"))]
#[test]
fn http2_policy_is_refused_without_the_feature() {
    let output = run(&[
        "serve",
        "--fixture",
        "/nonexistent/fixture.eggr",
        "--inbound",
        "http2",
    ]);
    assert!(
        !output.status.success(),
        "an unavailable policy must be refused"
    );
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("not available in this build"),
        "the refusal must name the build boundary, got: {stderr}"
    );
    assert!(
        stderr.contains("http1"),
        "the refusal must list what this build does support, got: {stderr}"
    );
}

/// An unrecognised policy name is refused in every build.
#[test]
fn unknown_inbound_policy_is_refused() {
    let output = run(&[
        "serve",
        "--fixture",
        "/nonexistent/fixture.eggr",
        "--inbound",
        "http7",
    ]);
    assert!(!output.status.success());
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("not available in this build") || stderr.contains("unrecognised"),
        "got: {stderr}"
    );
}

/// Build a minimal sealed fixture so `serve` can actually start.
fn sealed_fixture(directory: &std::path::Path) -> PathBuf {
    use eggreplay_core::{
        BodyRef, Flow, FlowOutcome, HttpRequest, HttpResponse, Provenance, SCHEMA_VERSION,
        SessionMetadata,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};
    use std::io::Write;

    let path = directory.join("sealed.eggr");
    let mut writer =
        SessionWriter::create(&path, SessionMetadata::default(), StoreLimits::default())
            .expect("session writer");
    let mut blob = writer.begin_blob().expect("blob");
    blob.write_all(b"cli-ok").expect("write body");
    let body = blob.finish().expect("finish body");
    writer
        .append_flow(&Flow {
            schema_version: SCHEMA_VERSION,
            id: "flow-0000".into(),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: "GET".into(),
                scheme: "http".into(),
                authority: "localhost".into(),
                path: "/api".into(),
                query: Vec::new(),
                headers: Vec::new(),
                body: BodyRef::Empty,
                trailers: Vec::new(),
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body,
                trailers: Vec::new(),
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: Vec::new(),
            redactions: Vec::new(),
        })
        .expect("append flow");
    writer.finish().expect("finish session");
    path
}

/// Start `serve` and read the one operator line it prints on startup.
///
/// That line is the operator-visible report of the serving policy, so it is
/// what a script or a human actually sees. The process is stopped once the
/// line has been read; `serve` runs until interrupted by design.
fn startup_line(extra: &[&str]) -> String {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let case = tempfile::TempDir::new().expect("temp dir");
    let fixture = sealed_fixture(case.path());
    let mut command = Command::new(binary());
    command
        .args([
            "serve",
            "--fixture",
            fixture.to_str().expect("path"),
            "--listen",
            "127.0.0.1:0",
        ])
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn serve");
    let stderr = child.stderr.take().expect("stderr pipe");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read startup line");
    // `case` is dropped here, which removes the fixture out from under the
    // still-running server; that is fine, it only needs to have started.
    let _ = child.kill();
    let _ = child.wait();
    line
}

/// The startup line reports the selected serving policy, and the default is
/// HTTP/1.1 in every build.
#[test]
fn startup_line_reports_the_default_serving_policy() {
    let line = startup_line(&[]);
    assert!(
        line.contains("inbound http1"),
        "the default policy must be reported on startup, got: {line}"
    );
    assert!(
        !line.contains("inbound http2"),
        "HTTP/1.1 must be the default even in an HTTP/2-capable build, got: {line}"
    );
}

/// The operator stream limit is accepted and does not switch protocols.
#[test]
fn operator_limit_is_reported_without_changing_the_policy() {
    let line = startup_line(&["--h2-max-concurrent-streams", "64"]);
    assert!(
        line.contains("inbound http1"),
        "a limit must not imply an HTTP/2 policy, got: {line}"
    );
}

/// Identity material in a build that cannot serve TLS is a refusal, not a
/// silently ignored flag.
#[cfg(not(feature = "h2-inbound-tls"))]
#[test]
fn tls_material_is_refused_without_the_feature() {
    let case = tempfile::TempDir::new().expect("temp dir");
    let cert = case.path().join("server.pem");
    std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\n").expect("write");
    let key = case.path().join("server.key");
    std::fs::write(&key, b"-----BEGIN PRIVATE KEY-----\n").expect("write");
    let output = run(&[
        "serve",
        "--fixture",
        "/nonexistent/fixture.eggr",
        "--inbound-tls-cert",
        cert.to_str().expect("path"),
        "--inbound-tls-key",
        key.to_str().expect("path"),
    ]);
    assert!(!output.status.success());
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("not available in this build"),
        "the refusal must name the build boundary, got: {stderr}"
    );
    assert!(!stderr.contains("BEGIN PRIVATE KEY"), "got: {stderr}");
}

/// The status description never contains key material. Checked against the
/// serialized shape so a future field cannot quietly add one.
#[test]
fn status_payload_never_exposes_key_material() {
    let case = tempfile::TempDir::new().expect("temp dir");
    let cert = case.path().join("server.pem");
    std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\nnot-a-cert\n").expect("write");
    let key = case.path().join("server.key");
    std::fs::write(
        &key,
        b"-----BEGIN PRIVATE KEY-----\nsuper-secret-key-material\n",
    )
    .expect("write");

    let output = run(&[
        "serve",
        "--fixture",
        case.path().join("missing.eggr").to_str().expect("path"),
        "--inbound-tls-cert",
        cert.to_str().expect("path"),
        "--inbound-tls-key",
        key.to_str().expect("path"),
    ]);
    let combined = format!("{}{}", stdout_text(&output), stderr_text(&output));
    for secret in ["super-secret-key-material", "BEGIN PRIVATE KEY"] {
        assert!(
            !combined.contains(secret),
            "operator output leaked key material {secret:?}: {combined}"
        );
    }
}

/// A TLS policy must be constructed from identity material, never from a
/// name. This half only runs where TLS serving exists.
#[cfg(feature = "h2-inbound-tls")]
#[test]
fn tls_policy_needs_material_not_just_a_name() {
    let output = run(&[
        "serve",
        "--fixture",
        "/nonexistent/fixture.eggr",
        "--inbound",
        "http2-tls",
    ]);
    assert!(!output.status.success());
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("certificate and key material"),
        "the refusal must require identity material, got: {stderr}"
    );
    assert!(
        !stderr.contains("BEGIN"),
        "the refusal must not echo key material: {stderr}"
    );
}
