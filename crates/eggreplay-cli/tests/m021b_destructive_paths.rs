//! M021B: destructive replacement targets are validated and staged.
//!
//! Every command that advertises `--overwrite` (or implicitly replaces its own
//! fixture, as `serve --record-mode re-record` does) must refuse a path that is
//! not an `EggReplay` fixture, an empty directory, or absent, and must leave the
//! old path byte-identical on every failure.

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
    serde_json::from_str(&stdout).expect("stdout must be a JSON envelope")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eggreplay-m021b-{name}-{}-{}",
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

fn store_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../eggreplay-store/tests/fixtures")
        .join(name)
}

fn har_corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../eggreplay-har/tests/corpus")
        .join(name)
}

/// Publish a valid fixture with a known flow count at `path` (must be absent).
fn write_fixture(path: &Path, schema_version: u16, flows: usize) {
    use eggreplay_core::{
        BodyRef, Flow, FlowOutcome, HttpRequest, HttpResponse, Provenance, SCHEMA_VERSION,
        SessionMetadata,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};

    let metadata = SessionMetadata {
        schema_version,
        ..SessionMetadata::default()
    };
    let mut writer = SessionWriter::create(path, metadata, StoreLimits::default()).unwrap();
    for index in 0..flows {
        writer
            .append_flow(&Flow {
                schema_version: SCHEMA_VERSION,
                id: format!("flow-{index}"),
                started_at_ms: 1,
                completed_at_ms: Some(2),
                request: HttpRequest {
                    method: "GET".into(),
                    scheme: "http".into(),
                    authority: "example.test".into(),
                    path: format!("/item-{index}"),
                    query: vec![],
                    headers: vec![],
                    body: BodyRef::Empty,
                    trailers: vec![],
                },
                outcome: FlowOutcome::Response(HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: BodyRef::Empty,
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
    }
    drop(writer.finish().unwrap());
}

fn open_flow_count(path: &Path) -> usize {
    eggreplay_store::Session::open(path, eggreplay_store::StoreLimits::default())
        .expect("fixture must still open")
        .manifest()
        .flow_count
}

/// A deterministic byte snapshot of a directory tree.
fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        let mut children = std::fs::read_dir(dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                entries.push((format!("{relative}/"), Vec::new()));
                walk(root, &path, entries);
            } else {
                entries.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }

    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries
}

/// No hidden `.<target>.*` staging/backup sibling may remain after an
/// operation. A store writer nests its own incomplete directory under the
/// hidden staging name (`..<target>.<label>-...incomplete-...`), so leading
/// dots are stripped before matching.
fn assert_no_transaction_residue(parent: &Path, target_name: &str) {
    let prefix = format!("{target_name}.");
    let residue = std::fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.') && name.trim_start_matches('.').starts_with(&prefix))
        .collect::<Vec<_>>();
    assert!(
        residue.is_empty(),
        "transaction residue must be cleaned up: {residue:?}"
    );
}

#[test]
fn record_refuses_non_fixture_target_without_deleting_it() {
    let dir = temp_dir("record-victim");
    let victim = dir.join("victim.eggr");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"keep me byte-for-byte").unwrap();
    let before = snapshot_tree(&victim);

    let output = run_cli(&[
        "record",
        "--listen",
        "127.0.0.1:0",
        "--upstream",
        "http://127.0.0.1:9",
        "--fixture",
        victim.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["failure_class"], "configuration");
    assert_eq!(
        std::fs::read(victim.join("keep.txt")).unwrap(),
        b"keep me byte-for-byte"
    );
    assert_eq!(snapshot_tree(&victim), before);
    assert_no_transaction_residue(&dir, "victim.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn record_validates_route_before_touching_a_valid_fixture() {
    let dir = temp_dir("record-route");
    let fixture = dir.join("existing.eggr");
    write_fixture(&fixture, 2, 2);
    let before = snapshot_tree(&fixture);

    let output = run_cli(&[
        "record",
        "--listen",
        "127.0.0.1:0",
        "--upstream",
        "http://127.0.0.1:9",
        "--fixture",
        fixture.to_str().unwrap(),
        "--overwrite",
        "--route",
        "bogus://not-a-route",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot_tree(&fixture), before);
    assert_eq!(open_flow_count(&fixture), 2);
    assert_no_transaction_residue(&dir, "existing.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_import_malformed_har_preserves_the_existing_fixture() {
    let dir = temp_dir("har-malformed");
    let fixture = dir.join("import.eggr");
    write_fixture(&fixture, 2, 2);
    let manifest_before = std::fs::read(fixture.join("manifest.json")).unwrap();
    let before = snapshot_tree(&fixture);
    let har = dir.join("malformed.har");
    std::fs::write(&har, b"{}").unwrap();

    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Parsing fails inside the import writer, before any publish; the CLI
    // reports the configuration class on stderr and emits no success envelope.
    assert!(
        String::from_utf8_lossy(&output.stderr).starts_with("configuration:"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(open_flow_count(&fixture), 2);
    assert_eq!(
        std::fs::read(fixture.join("manifest.json")).unwrap(),
        manifest_before
    );
    assert_eq!(snapshot_tree(&fixture), before);
    assert_no_transaction_residue(&dir, "import.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_import_replaces_an_empty_directory() {
    let dir = temp_dir("har-empty");
    let fixture = dir.join("empty.eggr");
    std::fs::create_dir(&fixture).unwrap();

    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_corpus("minimal.har").to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout_json(&output)["payload"]["flow_count"], 2);
    assert_eq!(open_flow_count(&fixture), 2);
    assert_no_transaction_residue(&dir, "empty.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_refuses_non_fixture_destination_with_overwrite() {
    let dir = temp_dir("migrate-refuse");
    let destination = dir.join("destination.eggr");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), b"keep").unwrap();
    let before = snapshot_tree(&destination);

    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-empty").to_str().unwrap(),
        "--to",
        destination.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot_tree(&destination), before);
    assert_no_transaction_residue(&dir, "destination.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_replaces_an_empty_destination_directory() {
    let dir = temp_dir("migrate-empty");
    let destination = dir.join("empty.eggr");
    std::fs::create_dir(&destination).unwrap();

    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-empty").to_str().unwrap(),
        "--to",
        destination.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(open_flow_count(&destination), 0);
    assert_no_transaction_residue(&dir, "empty.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_replaces_a_valid_fixture_atomically_without_backup() {
    let dir = temp_dir("migrate-atomic");
    let destination = dir.join("destination.eggr");
    write_fixture(&destination, 2, 0);

    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-with-flows").to_str().unwrap(),
        "--to",
        destination.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout_json(&output)["payload"]["flow_count"], 2);
    let session =
        eggreplay_store::Session::open(&destination, eggreplay_store::StoreLimits::default())
            .expect("replacement must open");
    assert_eq!(session.manifest().flow_count, 2);
    assert_eq!(
        session.manifest().metadata.schema_version,
        eggreplay_core::SESSION_SCHEMA_VERSION
    );
    assert_no_transaction_residue(&dir, "destination.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn migrate_refuses_to_replace_an_existing_fixture_without_overwrite() {
    let dir = temp_dir("migrate-no-flag");
    let destination = dir.join("destination.eggr");
    write_fixture(&destination, 2, 0);
    let before = snapshot_tree(&destination);

    let output = run_cli(&[
        "migrate",
        "--fixture",
        store_fixture("schema-1-with-flows").to_str().unwrap(),
        "--to",
        destination.to_str().unwrap(),
        "--output",
        "json",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(snapshot_tree(&destination), before);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn serve_re_record_refuses_non_fixture_before_binding_a_listener() {
    let dir = temp_dir("serve-rerecord");
    let victim = dir.join("victim.eggr");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"keep").unwrap();
    let before = snapshot_tree(&victim);

    let output = run_cli(&[
        "serve",
        "--fixture",
        victim.to_str().unwrap(),
        "--record-mode",
        "re-record",
        "--upstream",
        "http://127.0.0.1:9",
        "--listen",
        "127.0.0.1:0",
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot replace"),
        "refusal must name the replacement target: {stderr}"
    );
    assert_eq!(snapshot_tree(&victim), before);
    assert_no_transaction_residue(&dir, "victim.eggr");
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(feature = "intercept")]
#[test]
fn proxy_record_validates_policy_before_touching_the_fixture() {
    let dir = temp_dir("proxy-policy");
    let fixture = dir.join("proxy.eggr");
    write_fixture(&fixture, 2, 1);
    let before = snapshot_tree(&fixture);
    let policy = dir.join("invalid-policy.json");
    std::fs::write(&policy, b"not a policy").unwrap();

    let output = run_cli(&[
        "proxy",
        "record",
        "--listen",
        "127.0.0.1:0",
        "--fixture",
        fixture.to_str().unwrap(),
        "--overwrite",
        "--policy-file",
        policy.to_str().unwrap(),
        "--output",
        "json",
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot_tree(&fixture), before);
    assert_no_transaction_residue(&dir, "proxy.eggr");
    std::fs::remove_dir_all(dir).ok();
}
