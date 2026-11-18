//! Shared bounded-fixture helpers for the EggReplay fuzz targets.
//!
//! Every helper writes only below a caller-supplied temporary directory and
//! configures small [`StoreLimits`], so a fuzz iteration can never buffer a
//! whole session or reach the network.
#![allow(dead_code)] // each target links a different subset of these helpers

use eggreplay_core::{
    BodyRef, Flow, FlowOutcome, HeaderEntry, HttpRequest, HttpResponse, SessionMetadata,
    WebSocketConversation, WebSocketMessage, WebSocketMessageKind, WebSocketTerminal,
};
use eggreplay_store::{SessionWriter, StoreLimits};
use std::io::Write as _;
use std::path::Path;

/// Small deterministic bounds shared by every target.
pub fn bounded_limits() -> StoreLimits {
    StoreLimits {
        max_line_bytes: 64 * 1024,
        max_blob_bytes: 64 * 1024,
        max_flows: 64,
        max_total_bytes: 1024 * 1024,
    }
}

fn websocket_flow() -> Flow {
    let upgrade = HeaderEntry {
        name: "Upgrade".into(),
        value: "websocket".into(),
    };
    Flow::new(
        HttpRequest {
            method: "GET".into(),
            scheme: "http".into(),
            authority: "fuzz.test".into(),
            path: "/socket".into(),
            query: vec![],
            headers: vec![upgrade.clone()],
            body: BodyRef::Absent,
            trailers: vec![],
        },
        FlowOutcome::Response(HttpResponse {
            status: 101,
            headers: vec![upgrade],
            body: BodyRef::Absent,
            trailers: vec![],
        }),
        1,
    )
}

/// Publish a fixture with one WebSocket 101 flow, empty stream events, and one
/// empty conversation. Returns the initiating flow id.
pub fn websocket_fixture(root: &Path) -> Option<String> {
    let mut writer =
        SessionWriter::create(root, SessionMetadata::default(), bounded_limits()).ok()?;
    let flow = websocket_flow();
    writer.append_flow(&flow).ok()?;
    writer
        .append_stream_events(eggreplay_core::FlowStreamEvents {
            flow_id: flow.id.clone(),
            start_offset_ns: 0,
            request: vec![],
            response: vec![],
        })
        .ok()?;
    writer
        .append_websocket_conversation(WebSocketConversation {
            id: "fuzz-conversation".into(),
            flow_id: flow.id.clone(),
            offered_subprotocols: vec![],
            selected_subprotocol: None,
            messages: vec![],
            terminal: WebSocketTerminal::Abnormal {
                cause: "eof".into(),
            },
        })
        .ok()?;
    writer.finish().ok()?;
    Some(flow.id)
}

/// Publish a fixture whose single WebSocket text message carries `payload`.
///
/// `finish` validates the transcript and the payload's UTF-8, then reopens the
/// published fixture through `Session::open`, so both validation layers run.
pub fn websocket_payload_fixture(root: &Path, payload: &[u8]) -> Option<()> {
    let mut writer =
        SessionWriter::create(root, SessionMetadata::default(), bounded_limits()).ok()?;
    let flow = websocket_flow();
    writer.append_flow(&flow).ok()?;
    let mut body = writer.begin_blob().ok()?;
    body.write_all(payload).ok()?;
    let reference = body.finish().ok()?;
    writer
        .append_websocket_conversation(WebSocketConversation {
            id: "fuzz-conversation".into(),
            flow_id: flow.id,
            offered_subprotocols: vec![],
            selected_subprotocol: None,
            messages: vec![WebSocketMessage {
                sequence: 0,
                direction: eggreplay_core::WebSocketDirection::ClientToServer,
                delta_ns: 0,
                kind: WebSocketMessageKind::Text,
                payload: Some(reference),
                close_code: None,
                close_reason: None,
                redactions: vec![],
            }],
            terminal: WebSocketTerminal::Abnormal {
                cause: "eof".into(),
            },
        })
        .ok()?;
    writer.finish().ok()?;
    Some(())
}
