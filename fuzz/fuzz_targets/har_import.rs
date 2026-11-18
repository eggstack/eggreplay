#![no_main]

//! Fuzz `eggreplay_har::import_har_to_writer` with arbitrary HAR bytes.
//!
//! The writer and its staging directory live in a per-iteration temporary
//! directory with small store limits, so no partial fixture escapes and no
//! allocation is unbounded.

use eggreplay_core::{DEFAULT_MAX_STRUCTURED_REDACTION_BYTES, SessionMetadata};
use eggreplay_store::SessionWriter;
use libfuzzer_sys::fuzz_target;

mod common;

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let limits = common::bounded_limits();
    let fixture = dir.path().join("fixture.eggr");
    let Ok(mut writer) = SessionWriter::create(&fixture, SessionMetadata::default(), limits) else {
        return;
    };
    let redaction = eggreplay_core::RedactionConfig::default_secure();
    let _ = eggreplay_har::import_har_to_writer(
        &mut writer,
        data,
        &redaction,
        "fuzz",
        DEFAULT_MAX_STRUCTURED_REDACTION_BYTES.min(limits.max_blob_bytes),
        limits.max_blob_bytes,
    );
});
