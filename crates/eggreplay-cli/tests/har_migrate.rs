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
fn har_import_refuses_oversized_input_before_reading() {
    let dir = temp_dir("import-oversized");
    let har = dir.join("oversized.har");
    // A sparse file beyond the 32 MiB bound: the metadata check must refuse it
    // without allocating or parsing the document.
    let file = std::fs::File::create(&har).unwrap();
    file.set_len(eggreplay_har::MAX_HAR_FILE_BYTES as u64 + 1)
        .unwrap();
    drop(file);
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "oversized HAR must be configuration"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("configuration"), "stderr={stderr}");
    assert!(
        stderr.contains("HAR document exceeds size limit"),
        "the bounded reader must keep the library diagnostic: {stderr}"
    );
    assert!(!fixture.exists(), "no partial fixture may be published");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_import_truncates_a_multibyte_comment_without_panicking() {
    // Byte 512 falls inside a 3-byte `€`; the pre-fix `input[..512]` panicked
    // and the process exited 101.
    let comment = "€".repeat(200);
    assert_eq!(comment.len(), 600);
    let dir = temp_dir("import-multibyte-comment");
    let har = dir.join("multibyte.har");
    let document = serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": {"name": "test", "version": "0"},
            "entries": [{
                "startedDateTime": "2026-01-01T00:00:00.000Z",
                "time": 1,
                "request": {
                    "method": "GET",
                    "url": "http://example.test/multibyte",
                    "httpVersion": "http/1.1",
                    "cookies": [],
                    "headers": [],
                    "queryString": [],
                    "comment": comment
                },
                "response": {
                    "status": 200,
                    "statusText": "OK",
                    "httpVersion": "http/1.1",
                    "cookies": [],
                    "headers": [],
                    "content": {"size": 0, "mimeType": "text/plain", "text": ""}
                },
                "cache": {},
                "timings": {"send": 0, "wait": 0, "receive": 0}
            }]
        }
    });
    std::fs::write(&har, serde_json::to_vec(&document).unwrap()).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    let code = output.status.code();
    assert_ne!(code, Some(101), "the import must not panic");
    assert_eq!(
        code,
        Some(0),
        "boundary-safe truncation keeps the import successful: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let flows = std::fs::read_to_string(fixture.join("flows.jsonl")).unwrap();
    let flow: serde_json::Value = serde_json::from_str(flows.trim()).unwrap();
    let annotation = flow["annotations"][0][1].as_str().unwrap();
    assert!(annotation.ends_with('…'), "annotation={annotation}");
    let prefix = annotation.strip_suffix('…').unwrap();
    assert_eq!(prefix, "€".repeat(170), "byte 512 rounds down to 510");
    assert!(comment.starts_with(prefix));
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

// ---------------------------------------------------------------------------
// M021A: stream-trailer remediation, HAR confidentiality, warning surface
// ---------------------------------------------------------------------------

fn write_raw_stream_trailer_fixture(dir: &Path, name: &str) -> PathBuf {
    use eggreplay_core::{
        BodyRef, Flow, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse, Provenance,
        SCHEMA_VERSION, SessionMetadata, StreamEvent, StreamEventKind, StreamEvents,
    };
    use eggreplay_store::{SessionWriter, StoreLimits};

    let fixture = dir.join(name);
    let mut writer =
        SessionWriter::create(&fixture, SessionMetadata::default(), StoreLimits::default())
            .unwrap();
    let flow = Flow {
        schema_version: SCHEMA_VERSION,
        id: "flow-raw-trailers".into(),
        started_at_ms: 1,
        completed_at_ms: Some(2),
        request: HttpRequest {
            method: "POST".into(),
            scheme: "http".into(),
            authority: "example.test".into(),
            path: "/upload".into(),
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
    };
    writer.append_flow(&flow).unwrap();
    writer
        .append_stream_events(eggreplay_core::FlowStreamEvents {
            flow_id: flow.id.clone(),
            start_offset_ns: 7,
            request: vec![StreamEvent {
                delta_ns: 11,
                event: StreamEventKind::Trailers {
                    fields: vec![
                        HeaderEntry {
                            name: "x-req-secret".into(),
                            value: "RAW-REQUEST-TRAILER-VALUE".into(),
                        },
                        HeaderEntry {
                            name: "x-req-dup".into(),
                            value: "first".into(),
                        },
                        HeaderEntry {
                            name: "x-req-dup".into(),
                            value: "second".into(),
                        },
                    ],
                },
            }],
            response: vec![
                StreamEvent {
                    delta_ns: 13,
                    event: StreamEventKind::Trailers {
                        fields: vec![
                            HeaderEntry {
                                name: "x-secret".into(),
                                value: "RAW-RESPONSE-TRAILER-VALUE".into(),
                            },
                            HeaderEntry {
                                name: "x-dup".into(),
                                value: "one".into(),
                            },
                            HeaderEntry {
                                name: "x-dup".into(),
                                value: "two".into(),
                            },
                        ],
                    },
                },
                StreamEvent {
                    delta_ns: 17,
                    event: StreamEventKind::End,
                },
            ],
        })
        .unwrap();
    writer
        .write_extension("other", 1, "other.json", false, b"{\"keep\":true}")
        .unwrap();
    let _ = StreamEvents::default();
    let _ = writer.finish().unwrap();
    fixture
}

fn extension_bytes(fixture: &Path, name: &str) -> Vec<u8> {
    let session =
        eggreplay_store::Session::open(fixture, eggreplay_store::StoreLimits::default()).unwrap();
    session.read_extension(name).unwrap().unwrap()
}

#[test]
fn validate_warns_on_raw_stream_trailers_without_naming_the_value() {
    let dir = temp_dir("m021a-validate-warn");
    let fixture = write_raw_stream_trailer_fixture(&dir, "raw.eggr");
    let output = run_cli(&[
        "validate",
        "--fixture",
        fixture.to_str().unwrap(),
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
    let warnings = envelope["warnings"].as_array().unwrap();
    let joined = warnings
        .iter()
        .map(|warning| warning.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("flow-raw-trailers"), "{joined}");
    assert!(joined.contains('6'), "expected the field count: {joined}");
    for value in ["RAW-REQUEST-TRAILER-VALUE", "RAW-RESPONSE-TRAILER-VALUE"] {
        assert!(
            !joined.contains(value),
            "warning must be value-free: {joined}"
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

// One end-to-end remediation sequence: preserve-bytes, scrub, and idempotency
// are assertions about the same fixture, and splitting them would duplicate the
// construction setup.
#[allow(clippy::too_many_lines)]
#[test]
fn migrate_scrub_stream_trailers_is_transactional_and_idempotent() {
    let dir = temp_dir("m021a-migrate-scrub");
    let fixture = write_raw_stream_trailer_fixture(&dir, "raw.eggr");
    let other_before = extension_bytes(&fixture, "other");
    let flows_before = std::fs::read(fixture.join("flows.jsonl")).unwrap();

    // Without the flag, the extension bytes are preserved verbatim.
    let copied = dir.join("copied.eggr");
    let preserved = run_cli(&[
        "migrate",
        "--fixture",
        fixture.to_str().unwrap(),
        "--to",
        copied.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        preserved.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&preserved.stderr)
    );
    let raw_before = extension_bytes(&fixture, "stream-events");
    assert_eq!(
        extension_bytes(&copied, "stream-events"),
        raw_before,
        "migrate without --scrub-stream-trailers must preserve bytes"
    );

    let scrubbed = run_cli(&[
        "migrate",
        "--fixture",
        fixture.to_str().unwrap(),
        "--in-place",
        "--scrub-stream-trailers",
        "--output",
        "json",
    ]);
    assert_eq!(
        scrubbed.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&scrubbed.stderr)
    );
    let scrubbed_bytes = extension_bytes(&fixture, "stream-events");
    let events: eggreplay_core::StreamEvents = serde_json::from_slice(&scrubbed_bytes).unwrap();
    let flow = &events.flows[0];
    let request = match &flow.request[0].event {
        eggreplay_core::StreamEventKind::Trailers { fields } => fields,
        other => panic!("expected request trailers, got {other:?}"),
    };
    assert_eq!(
        request
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["x-req-secret", "x-req-dup", "x-req-dup"]
    );
    assert!(request.iter().all(|field| field.value == "<redacted>"));
    assert_eq!(flow.request[0].delta_ns, 11, "delays are untouched");
    let response = match &flow.response[0].event {
        eggreplay_core::StreamEventKind::Trailers { fields } => fields,
        other => panic!("expected response trailers, got {other:?}"),
    };
    assert_eq!(
        response
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["x-secret", "x-dup", "x-dup"]
    );
    assert!(response.iter().all(|field| field.value == "<redacted>"));
    assert!(matches!(
        &flow.response[1].event,
        eggreplay_core::StreamEventKind::End
    ));
    assert_eq!(flow.response[1].delta_ns, 17, "delays are untouched");
    assert_eq!(
        extension_bytes(&fixture, "other"),
        other_before,
        "unrelated extensions are untouched"
    );
    assert_eq!(
        std::fs::read(fixture.join("flows.jsonl")).unwrap(),
        flows_before,
        "flows are untouched"
    );

    // A second scrub is a byte-for-byte no-op.
    let second = run_cli(&[
        "migrate",
        "--fixture",
        fixture.to_str().unwrap(),
        "--in-place",
        "--scrub-stream-trailers",
        "--output",
        "json",
    ]);
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        extension_bytes(&fixture, "stream-events"),
        scrubbed_bytes,
        "a second scrub must be idempotent"
    );

    // validate no longer warns after remediation.
    let check = run_cli(&[
        "validate",
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(check.status.code(), Some(0));
    assert!(
        stdout_json(&check)["warnings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "remediated fixture must not warn: {}",
        stdout_json(&check)["warnings"]
    );
    std::fs::remove_dir_all(dir).ok();
}

fn har_with_header(header: &str, sentinel: &str) -> String {
    format!(
        r#"{{"log": {{"version": "1.2", "creator": {{"name": "m021a", "version": "1"}}, "entries": [{{
          "startedDateTime": "2026-03-01T12:00:00.000Z", "time": 5,
          "request": {{
            "method": "GET", "url": "http://example.test/path",
            "headers": [{{"name": "{header}", "value": "{sentinel}"}}],
            "queryString": []
          }},
          "response": {{"status": 200, "headers": [], "content": {{"size": 0}}}},
          "cache": {{}}, "timings": {{}}
        }}]}}}}"#
    )
}

fn har_with_credentials(header: &str, query: &str, sentinel: &str) -> String {
    format!(
        r#"{{"log": {{"version": "1.2", "creator": {{"name": "m021a", "version": "1"}}, "entries": [{{
          "startedDateTime": "2026-03-01T12:00:00.000Z", "time": 5,
          "request": {{
            "method": "GET", "url": "http://example.test/path?{query}={sentinel}",
            "headers": [{{"name": "{header}", "value": "{sentinel}"}}],
            "queryString": [{{"name": "{query}", "value": "{sentinel}"}}]
          }},
          "response": {{"status": 200, "headers": [], "content": {{"size": 0}}}},
          "cache": {{}}, "timings": {{}}
        }}]}}}}"#
    )
}

fn fixture_contains(fixture: &Path, needle: &str) -> bool {
    walk(fixture).iter().any(|path| {
        std::fs::read(path).is_ok_and(|bytes| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        })
    })
}

#[test]
fn curated_default_headers_are_redacted_and_absent_from_the_fixture() {
    let dir = temp_dir("m021a-default-api-key");
    let sentinel = "M021A-API-KEY-SENTINEL-31ac";
    let har = har_with_header("X-Api-Key", sentinel);
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
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
    assert!(
        !envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap_or_default().contains("x-api-key")),
        "a default-redacted header must not warn: {envelope}"
    );
    assert!(!fixture_contains(&fixture, sentinel));
    let session =
        eggreplay_store::Session::open(&fixture, eggreplay_store::StoreLimits::default()).unwrap();
    let flow = session.iter_flows().unwrap().next().unwrap().unwrap();
    assert!(flow.request.headers.iter().any(
        |header| header.name.eq_ignore_ascii_case("x-api-key") && header.value == "<redacted>"
    ));
    assert!(flow.redactions.iter().any(|marker| {
        marker
            .field
            .eq_ignore_ascii_case("request.headers.x-api-key")
    }));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unsafe_replace_records_the_sentinel_and_warns_value_free() {
    let dir = temp_dir("m021a-unsafe-replace");
    let sentinel = "M021A-UNSAFE-SENTINEL-b7d2";
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har_with_header("X-Api-Key", sentinel)).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--unsafe-replace-default-redaction",
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
    let warnings = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(warnings.contains("x-api-key"), "{warnings}");
    assert!(!warnings.contains(sentinel), "warnings must be value-free");
    assert!(
        fixture_contains(&fixture, sentinel),
        "the override explicitly replaces the default set"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn csrf_header_warns_without_selector_and_redacts_with_one() {
    let dir = temp_dir("m021a-csrf");
    let sentinel = "M021A-CSRF-SENTINEL-dc48";
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har_with_header("X-Csrf-Token", sentinel)).unwrap();

    let unselected = dir.join("unselected.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        unselected.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let envelope = stdout_json(&output);
    let warnings = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(warnings.contains("x-csrf-token"), "{warnings}");
    assert!(!warnings.contains(sentinel));
    assert!(
        fixture_contains(&unselected, sentinel),
        "documented residual: x-csrf-token is recorded unless selected"
    );

    let selected = dir.join("selected.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        selected.to_str().unwrap(),
        "--redact-header",
        "x-csrf-token",
        "--output",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let envelope = stdout_json(&output);
    assert!(
        !envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("x-csrf-token")),
        "an explicit selector must suppress the warning: {envelope}"
    );
    assert!(!fixture_contains(&selected, sentinel));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn credential_query_warns_without_its_value() {
    let dir = temp_dir("m021a-query-warning");
    let sentinel = "M021A-QUERY-SENTINEL-6c19";
    let har_path = dir.join("input.har");
    std::fs::write(
        &har_path,
        har_with_credentials("X-Ordinary", "access_token", sentinel),
    )
    .unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let envelope = stdout_json(&output);
    let warnings = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(warnings.contains("access_token"), "{warnings}");
    assert!(!warnings.contains(sentinel), "warnings must be value-free");
    // Query defaults are unchanged: the value is recorded.
    assert!(fixture_contains(&fixture, sentinel));
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(unix)]
#[test]
fn har_export_and_loss_reports_are_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("m021a-har-permissions");
    let har_path = dir.join("out.har");
    let loss_path = dir.join("loss.json");
    let fixture = store_fixture("schema-2-stream");
    let output = run_cli(&[
        "har",
        "export",
        "--fixture",
        fixture.to_str().unwrap(),
        "--har",
        har_path.to_str().unwrap(),
        "--loss-report",
        loss_path.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    for path in [&har_path, &loss_path] {
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o600,
            "{} must be private, got {mode:o}",
            path.display()
        );
    }

    // Without --overwrite the existing HAR is refused and left untouched.
    let before = std::fs::read(&har_path).unwrap();
    let refused = run_cli(&[
        "har",
        "export",
        "--fixture",
        fixture.to_str().unwrap(),
        "--har",
        har_path.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(std::fs::read(&har_path).unwrap(), before);

    // With --overwrite it is replaced and remains private.
    let replaced = run_cli(&[
        "har",
        "export",
        "--fixture",
        fixture.to_str().unwrap(),
        "--har",
        har_path.to_str().unwrap(),
        "--overwrite",
        "--output",
        "json",
    ]);
    assert_eq!(replaced.status.code(), Some(0));
    let mode = std::fs::metadata(&har_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn exported_har_preserves_fixture_redaction_markers() {
    let dir = temp_dir("m021a-har-redaction");
    let sentinel = "M021A-HAR-SENTINEL-e4b1";
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har_with_header("Authorization", sentinel)).unwrap();
    let fixture = dir.join("out.eggr");
    let import = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(import.status.code(), Some(0));
    assert!(!fixture_contains(&fixture, sentinel));
    let exported = dir.join("out.har");
    let export = run_cli(&[
        "har",
        "export",
        "--fixture",
        fixture.to_str().unwrap(),
        "--har",
        exported.to_str().unwrap(),
        "--output",
        "json",
    ]);
    assert_eq!(export.status.code(), Some(0));
    let text = std::fs::read_to_string(&exported).unwrap();
    assert!(text.contains("<redacted>"), "markers are preserved");
    assert!(
        !text.contains(sentinel),
        "export must not reintroduce secrets"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn har_comments_and_provenance_are_not_redacted_by_header_or_query_selectors() {
    let dir = temp_dir("m021a-track-e");
    let comment = "TRACK-E-COMMENT-SENTINEL-8a3f";
    let creator = "TRACK-E-CREATOR-2d7c";
    let header_sentinel = "TRACK-E-HEADER-SENTINEL-50b9";
    let query_sentinel = "TRACK-E-QUERY-SENTINEL-f16a";
    let har = format!(
        r#"{{"log": {{"version": "1.2", "creator": {{"name": "{creator}", "version": "9"}}, "entries": [{{
          "startedDateTime": "2026-03-01T12:00:00.000Z", "time": 5,
          "request": {{
            "method": "GET", "url": "http://example.test/path?token={query_sentinel}",
            "headers": [{{"name": "X-Track-E", "value": "{header_sentinel}"}}],
            "queryString": [{{"name": "token", "value": "{query_sentinel}"}}],
            "comment": "{comment}"
          }},
          "response": {{"status": 200, "headers": [], "content": {{"size": 0}}, "comment": "{comment}"}},
          "cache": {{}}, "timings": {{}}
        }}]}}}}"#
    );
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--redact-header",
        "x-track-e",
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
    let session =
        eggreplay_store::Session::open(&fixture, eggreplay_store::StoreLimits::default()).unwrap();
    let flow = session.iter_flows().unwrap().next().unwrap().unwrap();
    // Header/query selectors were applied...
    assert!(flow.request.headers.iter().any(
        |header| header.name.eq_ignore_ascii_case("x-track-e") && header.value == "<redacted>"
    ));
    assert!(
        flow.request
            .query
            .iter()
            .any(|pair| pair.key == "token" && pair.value == "<redacted>")
    );
    // ...but user-supplied HAR metadata is outside that policy.
    assert!(
        flow.annotations
            .iter()
            .any(|(key, value)| key == "har.request.comment" && value.contains(comment))
    );
    assert!(
        flow.annotations
            .iter()
            .any(|(key, value)| key == "har.response.comment" && value.contains(comment))
    );
    let provenance = String::from_utf8(
        session
            .read_extension("interop-provenance")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(provenance.contains(creator), "{provenance}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn human_mode_emits_warnings_on_stderr_without_values() {
    let dir = temp_dir("m021a-human-warnings");
    let sentinel = "M021A-HUMAN-SENTINEL-7c5e";
    let har_path = dir.join("input.har");
    std::fs::write(&har_path, har_with_header("X-Api-Key", sentinel)).unwrap();
    let fixture = dir.join("out.eggr");
    let output = run_cli(&[
        "har",
        "import",
        "--har",
        har_path.to_str().unwrap(),
        "--fixture",
        fixture.to_str().unwrap(),
        "--unsafe-replace-default-redaction",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("har-import: ok"), "{stdout}");
    assert!(stderr.contains("warning:"), "{stderr}");
    assert!(stderr.contains("x-api-key"), "{stderr}");
    assert!(
        !stderr.contains(sentinel),
        "human warnings must be value-free: {stderr}"
    );
    std::fs::remove_dir_all(dir).ok();
}
