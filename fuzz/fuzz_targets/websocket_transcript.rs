#![no_main]

//! Fuzz the WebSocket transcript and payload validation path.
//!
//! Mode 0 replaces the published `websockets.jsonl` transcript with fuzz
//! bytes and reopens the fixture. Mode 1 writes the fuzz bytes as a text
//! message payload blob and publishes through the writer, which validates the
//! transcript and the text payload's UTF-8 before reopening the fixture.

use libfuzzer_sys::fuzz_target;

mod common;

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let fixture = dir.path().join("fixture.eggr");
    let Some((mode, payload)) = data.split_first() else {
        return;
    };
    if mode % 2 == 0 {
        if common::websocket_fixture(&fixture).is_none() {
            return;
        }
        if std::fs::write(fixture.join("websockets.jsonl"), payload).is_err() {
            return;
        }
        let _ = eggreplay_store::Session::open(&fixture, common::bounded_limits());
    } else {
        let _ = common::websocket_payload_fixture(&fixture, payload);
    }
});
