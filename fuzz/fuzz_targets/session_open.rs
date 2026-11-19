#![no_main]

//! Fuzz `Session::open` across every untrusted fixture document.
//!
//! A valid skeleton fixture is published first, then one file is replaced
//! with fuzz bytes: `manifest.json`, `flows.jsonl`, `stream-events.json`, or
//! `websockets.jsonl`. Every path is inside a per-iteration temporary
//! directory and `Session::open` runs with small limits.

use eggreplay_store::Session;
use libfuzzer_sys::fuzz_target;

mod common;

const FILES: [&str; 4] = [
    "manifest.json",
    "flows.jsonl",
    "stream-events.json",
    "websockets.jsonl",
];

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let fixture = dir.path().join("fixture.eggr");
    if common::websocket_fixture(&fixture).is_none() {
        return;
    }
    let (mode, payload) = match data.split_first() {
        Some((mode, rest)) => (usize::from(*mode) % FILES.len(), rest),
        None => (0, &[][..]),
    };
    if std::fs::write(fixture.join(FILES[mode]), payload).is_err() {
        return;
    }
    let _ = Session::open(&fixture, common::bounded_limits());
});
