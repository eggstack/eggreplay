//! Untrusted-input bounds exercised through the public store API.
//!
//! The unit tests cover the private bounded readers directly. These tests
//! prove that `Session::open` propagates the same typed failures when a
//! caller configures a small limit, without generating large fixtures.

use eggreplay_core::{
    BodyRef, Flow, FlowOutcome, HttpRequest, HttpResponse, Provenance, SCHEMA_VERSION,
    SessionMetadata,
};
use eggreplay_store::{Session, SessionWriter, StoreError, StoreLimits};
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eggreplay-store-untrusted-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_flow() -> Flow {
    Flow {
        schema_version: SCHEMA_VERSION,
        id: "one".into(),
        started_at_ms: 1,
        completed_at_ms: Some(2),
        request: HttpRequest {
            method: "GET".into(),
            scheme: "http".into(),
            authority: "example.test".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: BodyRef::Absent,
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
    }
}

#[test]
fn open_rejects_a_jsonl_line_over_a_small_configured_limit() {
    let dir = temp_dir("jsonl-line");
    let fixture = dir.join("fixture.eggr");
    let mut writer =
        SessionWriter::create(&fixture, SessionMetadata::default(), StoreLimits::default())
            .unwrap();
    writer.append_flow(&sample_flow()).unwrap();
    drop(writer.finish().unwrap());

    // A real flow record is much longer than 16 bytes, so the bounded reader
    // must reject the line before parsing it.
    let limits = StoreLimits {
        max_line_bytes: 16,
        ..StoreLimits::default()
    };
    let error = Session::open(&fixture, limits).unwrap_err();
    assert!(
        matches!(
            &error,
            StoreError::Invalid(message) if message == "flow JSONL line exceeds configured limit"
        ),
        "unexpected error: {error}"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn open_still_accepts_a_fixture_under_the_default_limits() {
    let dir = temp_dir("defaults");
    let fixture = dir.join("fixture.eggr");
    let mut writer =
        SessionWriter::create(&fixture, SessionMetadata::default(), StoreLimits::default())
            .unwrap();
    writer.append_flow(&sample_flow()).unwrap();
    drop(writer.finish().unwrap());

    let session = Session::open(&fixture, StoreLimits::default()).unwrap();
    assert_eq!(session.manifest().flow_count, 1);

    std::fs::remove_dir_all(dir).unwrap();
}
