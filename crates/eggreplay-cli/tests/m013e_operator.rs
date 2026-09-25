//! M013E operator-surface integration tests.
//!
//! These tests drive the built `eggreplay` binary (`CARGO_BIN_EXE_eggreplay`)
//! so help text, exit codes, and machine output are verified end to end.
//! Interception-capable assertions are gated on the `intercept` feature;
//! capability-failure assertions run only without it.

use std::path::{Path, PathBuf};
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

fn temp_case(_label: &str) -> tempfile::TempDir {
    tempfile::TempDir::new().expect("temp dir")
}

#[test]
fn help_lists_the_interception_namespaces() {
    let _ = temp_case("unused");
    let output = run(&["--help"]);
    assert!(output.status.success());
    let help = stdout_text(&output);
    assert!(help.contains("proxy"), "help must list proxy");
    assert!(help.contains("ca"), "help must list ca");
}

#[test]
fn proxy_help_mentions_record_and_validate() {
    let output = run(&["proxy", "--help"]);
    assert!(output.status.success());
    let help = stdout_text(&output);
    assert!(help.contains("record"));
    assert!(help.contains("validate"));
}

#[test]
fn ca_help_mentions_the_lifecycle_commands() {
    let output = run(&["ca", "--help"]);
    assert!(output.status.success());
    let help = stdout_text(&output);
    for token in ["init", "import", "inspect", "export", "rotate"] {
        assert!(help.contains(token), "ca help must mention {token}");
    }
}

/// Banned trust-store mutation invocations must not appear in Rust source.
///
/// `EggReplay` never installs CA trust automatically; this audit fails the
/// build if OS/browser trust-mutation commands creep into the interception
/// implementation. Documentation may describe manual steps; only `.rs`
/// product source is scanned here.
#[test]
fn source_contains_no_automatic_trust_mutation() {
    let banned = [
        "add-trusted-cert",
        "certutil",
        "update-ca-certificates",
        "update-ca-trust",
        "certmgr",
        "SecTrustSettings",
        "cert9.db",
        "policies.json",
        "HKEY_",
    ];
    let mut files = vec![
        PathBuf::from("src/main.rs"),
        PathBuf::from("src/intercept.rs"),
    ];
    for name in [
        "lib.rs",
        "policy.rs",
        "policy_file.rs",
        "proxy.rs",
        "tunnel.rs",
        "ca.rs",
        "leaf.rs",
        "mitm.rs",
        "headers.rs",
    ] {
        files.push(Path::new("../eggreplay-intercept/src").join(name));
    }
    assert!(!files.is_empty());
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|_| panic!("must read {}", file.display()));
        for token in banned {
            assert!(
                !text.contains(token),
                "{} must not contain trust-mutation invocation {token}",
                file.display()
            );
        }
    }
}

#[cfg(not(feature = "intercept"))]
#[test]
fn proxy_record_without_feature_fails_with_capability_message() {
    let dir = temp_case("capability");
    let fixture = dir.path().join("fixture.eggr");
    let output = run(&["proxy", "record", "--fixture", &fixture.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_text(&output).contains("not compiled"));
}

#[cfg(not(feature = "intercept"))]
#[test]
fn ca_inspect_without_feature_fails_with_capability_message() {
    let output = run(&["ca", "inspect", "--dir", "some-dir"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_text(&output).contains("not compiled"));
}

#[cfg(feature = "intercept")]
#[test]
fn proxy_validate_accepts_a_minimal_policy_as_json() {
    let dir = temp_case("validate-ok");
    let policy = dir.path().join("policy.json");
    std::fs::write(
        &policy,
        br#"{"version": "eggreplay-intercept-policy/v1", "rules": []}"#,
    )
    .unwrap();
    let output = run(&[
        "proxy",
        "validate",
        "--policy-file",
        &policy.to_string_lossy(),
        "--output",
        "json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr_text(&output));
    let stdout = stdout_text(&output);
    assert!(stdout.contains("eggreplay-intercept-policy/v1"));
    assert!(stdout.contains("proxy-validate"));
    for sentinel in ["PRIVATE KEY", "ca-key", "BEGIN"] {
        assert!(
            !stdout.contains(sentinel),
            "machine output leaked {sentinel}"
        );
    }
}

#[cfg(feature = "intercept")]
#[test]
fn proxy_validate_rejects_unknown_versions_with_configuration_exit() {
    let dir = temp_case("validate-bad");
    let policy = dir.path().join("policy.json");
    std::fs::write(
        &policy,
        br#"{"version": "eggreplay-intercept-policy/v99", "rules": []}"#,
    )
    .unwrap();
    let output = run(&[
        "proxy",
        "validate",
        "--policy-file",
        &policy.to_string_lossy(),
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[cfg(feature = "intercept")]
#[test]
fn non_loopback_record_requires_the_explicit_gate() {
    let dir = temp_case("gate");
    let fixture = dir.path().join("fixture.eggr");
    // The bind gate fires before any listener starts, so this returns fast.
    let output = run(&[
        "proxy",
        "record",
        "--listen",
        "0.0.0.0:0",
        "--fixture",
        &fixture.to_string_lossy(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_text(&output).contains("--allow-non-loopback"));
    assert!(!fixture.exists(), "gated bind must not create a fixture");
}

#[cfg(feature = "intercept")]
#[test]
fn ca_lifecycle_enforces_no_overwrite_and_key_safe_output() {
    let dir = temp_case("ca");
    let ca_dir = dir.path().join("ca");
    let export = dir.path().join("ca-cert.pem");

    let init = run(&[
        "ca",
        "init",
        "--dir",
        &ca_dir.to_string_lossy(),
        "--output",
        "json",
    ]);
    assert!(init.status.success(), "stderr: {}", stderr_text(&init));
    let init_stdout = stdout_text(&init);
    assert!(init_stdout.contains("fingerprint_sha256"));
    for sentinel in ["PRIVATE KEY", "ca-key.pem", "BEGIN"] {
        assert!(
            !init_stdout.contains(sentinel),
            "machine output leaked {sentinel}"
        );
    }

    // Second init into the same directory must refuse to overwrite.
    let again = run(&["ca", "init", "--dir", &ca_dir.to_string_lossy()]);
    assert_eq!(again.status.code(), Some(2));
    assert!(stderr_text(&again).contains("refusing to overwrite"));

    let inspect = run(&[
        "ca",
        "inspect",
        "--dir",
        &ca_dir.to_string_lossy(),
        "--output",
        "json",
    ]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        stderr_text(&inspect)
    );
    let inspect_stdout = stdout_text(&inspect);
    assert!(inspect_stdout.contains("fingerprint_sha256"));
    for sentinel in ["PRIVATE KEY", "ca-key.pem"] {
        assert!(!inspect_stdout.contains(sentinel));
    }

    let export_out = run(&[
        "ca",
        "export",
        "--dir",
        &ca_dir.to_string_lossy(),
        "--out",
        &export.to_string_lossy(),
        "--output",
        "json",
    ]);
    assert!(
        export_out.status.success(),
        "stderr: {}",
        stderr_text(&export_out)
    );
    let exported = std::fs::read_to_string(&export).unwrap();
    assert!(exported.contains("BEGIN CERTIFICATE"));
    assert!(!exported.contains("PRIVATE KEY"));

    // Export refuses to overwrite its destination.
    let overwrite = run(&[
        "ca",
        "export",
        "--dir",
        &ca_dir.to_string_lossy(),
        "--out",
        &export.to_string_lossy(),
    ]);
    assert_eq!(overwrite.status.code(), Some(2));
    assert!(stderr_text(&overwrite).contains("refusing to overwrite"));
}
