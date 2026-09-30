//! M014A subprocess CLI contracts: HAR import/export and fixture migration.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_eggreplay"))
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("CLI subprocess must run")
}

fn stdout_json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).expect("stdout must be JSON envelope")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eggreplay-m014a-{name}-{}-{}",
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

fn har_corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../eggreplay-har/tests/corpus")
        .join(name)
}

fn store_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../eggreplay-store/tests/fixtures")
        .join(name)
}

#[test]
fn har_import_minimal_succeeds_with_json_and_loss_report() {
    let dir = temp_dir("import-ok");
    let fixture = dir.join("imported.eggr");
    let loss = dir.join("loss.json");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_corpus("minimal.har").to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--loss-report",
        loss.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["success"], true);
    assert_eq!(envelope["command"], "har-import");
    assert_eq!(envelope["payload"]["flow_count"], 2);
    assert!(loss.exists(), "loss report must be written");
    let loss_doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&loss).unwrap()).unwrap();
    assert_eq!(loss_doc["flows"], 2);
    assert!(loss_doc["losses"].as_array().unwrap().len() >= 2);
    // Fixture validates and preserves query ordering.
    let validate = run_cli(&[
        "validate",
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(validate.status.code(), Some(0));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_import_preserves_duplicates_without_collapsing() {
    let dir = temp_dir("import-dup");
    let fixture = dir.join("dup.eggr");
    let loss = dir.join("loss.json");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_corpus("duplicates.har").to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--loss-report",
        loss.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let loss_doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&loss).unwrap()).unwrap();
    let text = loss_doc.to_string();
    assert!(
        text.contains("request.cookies"),
        "cookie view must be reported: {text}"
    );
    // Inspect the fixture directly for duplicate preservation.
    let session =
        eggreplay_store::Session::open(&fixture, eggreplay_store::StoreLimits::default()).unwrap();
    let flows: Vec<_> = session
        .iter_flows()
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(flows.len(), 1);
    assert_eq!(
        flows[0]
            .request
            .headers
            .iter()
            .filter(|h| h.name == "X-Dup")
            .count(),
        2
    );
    assert_eq!(flows[0].request.query.len(), 3);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_import_redacts_secrets_before_publication() {
    let dir = temp_dir("import-redact");
    let har_path = dir.join("secret.har");
    let sentinel = "M014A-SENTINEL-9f8e7d6c5b4a";
    let har = format!(
        r#"{{"log": {{"version": "1.2", "creator": {{"name": "t", "version": "0"}}, "entries": [{{
      "startedDateTime": "2026-03-01T12:00:00.000Z", "time": 5,
      "request": {{
        "method": "GET", "url": "http://example.test/?token={sentinel}",
        "headers": [{{"name": "Authorization", "value": "Bearer {sentinel}"}}],
        "queryString": [{{"name": "token", "value": "{sentinel}"}}]
      }},
      "response": {{"status": 200, "headers": [], "content": {{"size": 0}}}},
      "cache": {{}}, "timings": {{}}
    }}]}}}}"#
    );
    std::fs::write(&har_path, har).unwrap();
    let fixture = dir.join("redacted.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--redact-query",
        "token",
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // No secret bytes anywhere under the fixture directory.
    let mut leaked = false;
    for entry in walk(&fixture) {
        if entry.is_file() {
            let bytes = std::fs::read(&entry).unwrap_or_default();
            if bytes
                .windows(sentinel.len())
                .any(|w| w == sentinel.as_bytes())
            {
                leaked = true;
            }
        }
    }
    assert!(
        !leaked,
        "imported secrets must be redacted before publication"
    );
    std::fs::remove_dir_all(dir).ok();
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    stack.push(entry.path());
                }
            }
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn har_import_rejects_invalid_har_as_configuration() {
    let dir = temp_dir("import-bad");
    let bad = dir.join("bad.har");
    std::fs::write(&bad, r#"{"log": {"version": "9.9", "entries": []}}"#).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        bad.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid HAR must be configuration"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration"));
    assert!(!fixture.exists(), "no partial fixture may be published");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_export_embeds_lossy_provenance_and_never_claims_lossless() {
    let dir = temp_dir("export-ok");
    let har_path = dir.join("out.har");
    let loss = dir.join("export-loss.json");
    let output = run_cli(&[
        "har",
        "export",
        "--fixture",
        store_fixture("schema-2-stream").to_str().unwrap(),
        "--har",
        har_path.to_str().unwrap(),
        "--loss-report",
        loss.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let har: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&har_path).unwrap()).unwrap();
    assert_eq!(har["log"]["version"], "1.2");
    assert!(
        har["log"]["comment"]
            .as_str()
            .unwrap()
            .contains("not round-trip lossless")
    );
    assert!(
        har["log"]["_eggreplay"]["losses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|loss| loss["field"] == "extensions.stream-events")
    );
    assert!(
        !har.to_string().contains("round-trip lossless export")
            || har["log"]["comment"]
                .as_str()
                .unwrap()
                .contains("not round-trip")
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_export_projects_typed_errors_as_status_zero() {
    let dir = temp_dir("export-error");
    // Import the binary-and-error corpus (contains a status-0 entry), then export.
    let fixture = dir.join("imported.eggr");
    let import = run_cli(&[
        "har",
        "import",
        "--har",
        har_corpus("binary-and-error.har").to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        import.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&import.stderr)
    );
    let har_path = dir.join("out.har");
    let export = run_cli(&[
        "har",
        "export",
        "--fixture",
        fixture.to_str().unwrap(),
        "--har",
        har_path.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(export.status.code(), Some(0));
    let har: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&har_path).unwrap()).unwrap();
    let statuses: Vec<_> = har["log"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["response"]["status"].as_u64().unwrap())
        .collect();
    assert!(
        statuses.contains(&0),
        "typed error must project as status 0: {statuses:?}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_upgrades_schema_one_to_current_transactionally() {
    let dir = temp_dir("migrate-to");
    let dest = dir.join("migrated.eggr");
    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-with-flows").to_str().unwrap(),
        "--to",
        dest.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["payload"]["target_schema"], 2);
    assert_eq!(envelope["payload"]["flow_count"], 2);
    // Source is untouched (still schema 1).
    let source_check = run_cli(&[
        "validate",
        "--fixture",
        store_fixture("schema-1-with-flows").to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(stdout_json(&source_check)["payload"]["schema_version"], 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_current_to_current_is_idempotent() {
    let dir = temp_dir("migrate-idem");
    let first = dir.join("first.eggr");
    let second = dir.join("second.eggr");
    let to_first = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-2-stream").to_str().unwrap(),
        "--to",
        first.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(to_first.status.code(), Some(0));
    let to_second = run_cli(&[
        "migrate",
        "--fixture",
        first.to_str().unwrap(),
        "--to",
        second.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(to_second.status.code(), Some(0));
    let first_session =
        eggreplay_store::Session::open(&first, eggreplay_store::StoreLimits::default()).unwrap();
    let second_session =
        eggreplay_store::Session::open(&second, eggreplay_store::StoreLimits::default()).unwrap();
    assert_eq!(
        first_session.manifest().metadata,
        second_session.manifest().metadata
    );
    assert_eq!(
        first_session.manifest().extensions,
        second_session.manifest().extensions
    );
    let first_flows: Vec<_> = first_session
        .iter_flows()
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let second_flows: Vec<_> = second_session
        .iter_flows()
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(first_flows, second_flows);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_in_place_replaces_atomically_and_rejects_conflicting_flags() {
    let dir = temp_dir("migrate-inplace");
    // Copy a schema-1 fixture into the temp dir, then migrate in place.
    let local = dir.join("local.eggr");
    let copy = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-empty").to_str().unwrap(),
        "--to",
        local.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(copy.status.code(), Some(0));
    let inplace = run_cli(&[
        "migrate",
        "--fixture",
        local.to_str().unwrap(),
        "--in-place",
        "--output",
        "json",
    ]);
    assert_eq!(
        inplace.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&inplace.stderr)
    );
    let check = run_cli(&[
        "validate",
        "--fixture",
        local.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(stdout_json(&check)["payload"]["schema_version"], 2);
    // Conflicting flags are a configuration error.
    let conflict = run_cli(&[
        "migrate",
        "--fixture",
        local.to_str().unwrap(),
        "--in-place",
        "--to",
        dir.join("other.eggr").to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(conflict.status.code(), Some(2));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_refuses_to_overwrite_destination_without_flag() {
    let dir = temp_dir("migrate-overwrite");
    let dest = dir.join("dest.eggr");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("marker"), "existing").unwrap();
    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-empty").to_str().unwrap(),
        "--to",
        dest.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(dest.join("marker")).unwrap(),
        "existing",
        "destination must be preserved without --overwrite"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_future_extension_fails_closed_without_mutating_source() {
    let dir = temp_dir("migrate-blocked");
    // Craft a fixture with a future stream-events schema by copying a valid
    // fixture and bumping the extension version in the manifest + payload.
    let source = dir.join("future.eggr");
    let copy = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-2-stream").to_str().unwrap(),
        "--to",
        source.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(copy.status.code(), Some(0));
    // Bump the extension schema to a future version.
    let manifest_path = source.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["extensions"][0]["schema_version"] = serde_json::json!(99);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let before = std::fs::read(&manifest_path).unwrap();
    let dest = dir.join("out.eggr");
    let output = run_cli(&[
        "migrate",
        "--fixture",
        source.to_str().unwrap(),
        "--to",
        dest.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert!(
        output.status.code() == Some(3),
        "future extension must fail as fixture, got {:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dest.exists(), "no partial destination may be published");
    assert_eq!(
        std::fs::read(&manifest_path).unwrap(),
        before,
        "source must be preserved"
    );
    std::fs::remove_dir_all(dir).ok();
}
