//! Optional gRPC-aware views above qualified HTTP flows (M014D).
//!
//! These are pure projections with no transport of their own: the 5-byte
//! gRPC message envelope is parsed, ordered message lengths and
//! compression flags are exposed, `grpc-status`/`grpc-message` trailers
//! surface in diagnostics, and protobuf payloads decode only against an
//! explicit caller-supplied
//! [`FileDescriptorSet`](prost_reflect::prost_types::FileDescriptorSet).
//! Raw body blobs stay authoritative; there is no network descriptor
//! lookup, no decompression of compressed frames, and no canonical-store
//! change. Views operate on caller-provided bytes (in practice,
//! already-redacted flow bodies), so decoded JSON inherits flow
//! redactions; there is no separate view selector language.
//!
//! The module lives in the HTTP adapter crate (behind the `grpc` cargo
//! feature) so `eggreplay-core` keeps its dependency boundary: core
//! never pulls transport or codec runtimes.

use eggreplay_core::HeaderEntry;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum gRPC body accepted for view parsing (16 MiB).
pub const GRPC_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Maximum message frames accepted per body.
pub const GRPC_MAX_FRAMES: usize = 4096;
/// Maximum accepted encoded `FileDescriptorSet` (1 MiB, untrusted input).
pub const GRPC_MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

/// Errors from gRPC view parsing. Bounds fail closed; unknown content is
/// never silently reinterpreted.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GrpcError {
    /// Body exceeds [`GRPC_MAX_BODY_BYTES`].
    #[error("gRPC body exceeds bound")]
    TooLarge,
    /// Frame count exceeds [`GRPC_MAX_FRAMES`].
    #[error("gRPC frame count exceeds bound")]
    TooManyFrames,
    /// Fewer than 5 envelope bytes remain.
    #[error("truncated gRPC envelope")]
    Truncated,
    /// Declared length overruns the remaining body.
    #[error("gRPC frame length overruns body")]
    Overrun,
    /// Trailing bytes after the final frame.
    #[error("trailing bytes after gRPC frames")]
    TrailingBytes,
    /// Descriptor set exceeds [`GRPC_MAX_DESCRIPTOR_BYTES`].
    #[error("descriptor set exceeds bound")]
    DescriptorTooLarge,
    /// Descriptor set is not a valid `FileDescriptorSet`.
    #[error("invalid descriptor set: {0}")]
    DescriptorInvalid(String),
    /// Named message is absent from the descriptor pool.
    #[error("unknown message: {0}")]
    UnknownMessage(String),
    /// Payload does not decode against the named message.
    #[error("protobuf decode failed: {0}")]
    Decode(String),
    /// Decoded message does not project to JSON.
    #[error("JSON projection failed: {0}")]
    Json(String),
}

/// One parsed gRPC length-prefixed message frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcFrame {
    /// Zero-based frame order in the body.
    pub index: usize,
    /// Compression flag (bit 0 of the envelope flags byte). Flagged
    /// payloads are exposed as opaque bytes, never decompressed.
    pub compressed: bool,
    /// Declared payload length in bytes.
    pub length: u32,
    /// Raw payload bytes (authoritative form).
    pub payload: Vec<u8>,
}

/// Parsed `grpc-status` trailer pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcStatus {
    /// Numeric status code (0–16 per the gRPC specification).
    pub code: u32,
    /// Percent-decoded `grpc-message` text (empty when absent).
    pub message: String,
}

/// One message in a derived view, with optional descriptor decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcMessageView {
    /// Zero-based frame order in the body.
    pub index: usize,
    /// Envelope compression flag (opaque when set).
    pub compressed: bool,
    /// Declared payload length in bytes.
    pub length: u32,
    /// Canonical-JSON projection when a descriptor was supplied and the
    /// frame is uncompressed; otherwise `None` (raw payload stays
    /// authoritative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded: Option<serde_json::Value>,
}

/// Optional derived projection of a gRPC body plus its trailers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcView {
    /// Ordered message views.
    pub messages: Vec<GrpcMessageView>,
    /// Trailer status when present and well-formed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<GrpcStatus>,
}

/// Return whether a content type selects the gRPC view.
#[must_use]
pub fn is_grpc_content_type(content_type: Option<&str>) -> bool {
    matches!(content_type, Some(value) if value == "application/grpc" || value.starts_with("application/grpc+"))
}

/// Parse the gRPC length-prefixed envelope over one body.
///
/// Each frame is 1 flag byte plus a 4-byte big-endian length. Bounds fail
/// closed; trailing bytes are an error, never ignored.
pub fn parse_grpc_frames(body: &[u8]) -> Result<Vec<GrpcFrame>, GrpcError> {
    if body.len() > GRPC_MAX_BODY_BYTES {
        return Err(GrpcError::TooLarge);
    }
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < body.len() {
        if frames.len() >= GRPC_MAX_FRAMES {
            return Err(GrpcError::TooManyFrames);
        }
        let rest = &body[offset..];
        if rest.len() < 5 {
            return Err(GrpcError::Truncated);
        }
        let compressed = rest[0] & 0x01 == 0x01;
        let length = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]);
        let payload_start = offset + 5;
        let payload_end = payload_start
            .checked_add(length as usize)
            .filter(|end| *end <= body.len())
            .ok_or(GrpcError::Overrun)?;
        frames.push(GrpcFrame {
            index: frames.len(),
            compressed,
            length,
            payload: body[payload_start..payload_end].to_vec(),
        });
        offset = payload_end;
    }
    if offset != body.len() {
        return Err(GrpcError::TrailingBytes);
    }
    Ok(frames)
}

/// Decode percent-encoded `grpc-message` text (`%XX` only; `+` is literal).
/// Malformed escapes stay literal; the view never fails on trailer text.
fn decode_grpc_message_text(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(pair) = bytes.get(index + 1..index + 3)
            && let (Some(high), Some(low)) = (hex_value(pair[0]), hex_value(pair[1]))
        {
            out.push(high << 4 | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Extract `grpc-status`/`grpc-message` trailers (last `grpc-status` wins).
/// Malformed or out-of-range codes yield `None`.
#[must_use]
pub fn grpc_status_from_trailers(trailers: &[HeaderEntry]) -> Option<GrpcStatus> {
    let code_text = trailers
        .iter()
        .rfind(|entry| entry.name.eq_ignore_ascii_case("grpc-status"))?
        .value
        .trim()
        .to_owned();
    let code: u32 = code_text.parse().ok()?;
    if code > 16 {
        return None;
    }
    let message = trailers
        .iter()
        .rfind(|entry| entry.name.eq_ignore_ascii_case("grpc-message"))
        .map(|entry| decode_grpc_message_text(&entry.value))
        .unwrap_or_default();
    Some(GrpcStatus { code, message })
}

/// Decode one protobuf payload against a caller-supplied descriptor set.
///
/// `descriptor_set` is an encoded `FileDescriptorSet` (bounded by
/// [`GRPC_MAX_DESCRIPTOR_BYTES`], never fetched). `message_name` is the
/// fully qualified message name (for example `grpc.test.Echo`). The
/// result is the canonical JSON mapping. Compressed frames must not be
/// passed here; decode the raw envelope payload only when uncompressed.
pub fn decode_grpc_payload(
    payload: &[u8],
    descriptor_set: &[u8],
    message_name: &str,
) -> Result<serde_json::Value, GrpcError> {
    use prost_reflect::{DescriptorPool, DynamicMessage};
    if descriptor_set.len() > GRPC_MAX_DESCRIPTOR_BYTES {
        return Err(GrpcError::DescriptorTooLarge);
    }
    if payload.len() > GRPC_MAX_BODY_BYTES {
        return Err(GrpcError::TooLarge);
    }
    let pool = DescriptorPool::decode(descriptor_set)
        .map_err(|error| GrpcError::DescriptorInvalid(error.to_string()))?;
    let descriptor = pool
        .get_message_by_name(message_name)
        .ok_or_else(|| GrpcError::UnknownMessage(message_name.to_owned()))?;
    let message = DynamicMessage::decode(descriptor, payload)
        .map_err(|error| GrpcError::Decode(error.to_string()))?;
    serde_json::to_value(&message).map_err(|error| GrpcError::Json(error.to_string()))
}

/// Derive an optional view over one message body plus its trailers.
///
/// When `descriptor` is `Some((set, name))`, uncompressed frames decode
/// against it; compressed frames and decode failures leave `decoded` as
/// `None` while the raw payload stays authoritative (decode failure here
/// is diagnostic absence, not a hard error — use [`decode_grpc_payload`]
/// for strict decoding).
pub fn grpc_view(
    body: &[u8],
    trailers: &[HeaderEntry],
    descriptor: Option<(&[u8], &str)>,
) -> Result<GrpcView, GrpcError> {
    let frames = parse_grpc_frames(body)?;
    let mut messages = Vec::with_capacity(frames.len());
    for frame in frames {
        let decoded = match (descriptor, frame.compressed) {
            (Some((set, name)), false) => decode_grpc_payload(&frame.payload, set, name).ok(),
            _ => None,
        };
        messages.push(GrpcMessageView {
            index: frame.index,
            compressed: frame.compressed,
            length: frame.length,
            decoded,
        });
    }
    Ok(GrpcView {
        messages,
        status: grpc_status_from_trailers(trailers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_reflect::prost::Message as ProstMessage;
    use prost_reflect::prost_types::{
        DescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
    };

    fn frame(compressed: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![u8::from(compressed)];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parses_ordered_frames() {
        let mut body = frame(false, b"\x0a\x02hi");
        body.extend(frame(true, b"opaque"));
        body.extend(frame(false, &[]));
        let frames = parse_grpc_frames(&body).expect("frames");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].index, 0);
        assert!(!frames[0].compressed);
        assert_eq!(frames[0].payload, b"\x0a\x02hi");
        assert!(frames[1].compressed);
        assert_eq!(frames[1].length, 6);
        assert_eq!(frames[2].payload, b"");
    }

    #[test]
    fn rejects_malformed_bodies() {
        assert_eq!(parse_grpc_frames(&[0x00, 0x00]), Err(GrpcError::Truncated));
        let mut overrun = vec![0x00];
        overrun.extend_from_slice(&1000u32.to_be_bytes());
        overrun.extend_from_slice(b"short");
        assert_eq!(parse_grpc_frames(&overrun), Err(GrpcError::Overrun));
        let mut trailing = frame(false, b"ab");
        trailing.push(0xFF);
        assert_eq!(parse_grpc_frames(&trailing), Err(GrpcError::Truncated));
    }

    #[test]
    fn extracts_trailer_status() {
        let trailers = vec![
            HeaderEntry {
                name: "grpc-status".to_string(),
                value: "0".to_string(),
            },
            HeaderEntry {
                name: "grpc-message".to_string(),
                value: "hello%20world%21".to_string(),
            },
        ];
        assert_eq!(
            grpc_status_from_trailers(&trailers),
            Some(GrpcStatus {
                code: 0,
                message: "hello world!".to_string(),
            })
        );
        assert_eq!(grpc_status_from_trailers(&[]), None);
        let bad = vec![HeaderEntry {
            name: "grpc-status".to_string(),
            value: "99".to_string(),
        }];
        assert_eq!(grpc_status_from_trailers(&bad), None);
    }

    fn echo_descriptor_set() -> Vec<u8> {
        let file = FileDescriptorProto {
            name: Some("echo.proto".to_string()),
            package: Some("grpc.test".to_string()),
            syntax: Some("proto3".to_string()),
            message_type: vec![DescriptorProto {
                name: Some("Echo".to_string()),
                field: vec![
                    FieldDescriptorProto {
                        name: Some("text".to_string()),
                        number: Some(1),
                        label: Some(1),
                        r#type: Some(9),
                        json_name: Some("text".to_string()),
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("n".to_string()),
                        number: Some(2),
                        label: Some(1),
                        r#type: Some(5),
                        json_name: Some("n".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let set = FileDescriptorSet { file: vec![file] };
        set.encode_to_vec()
    }

    #[test]
    fn decodes_against_caller_descriptor() {
        let set = echo_descriptor_set();
        // field 1 (string "hi"), field 2 (int32 150).
        let payload = vec![0x0A, 0x02, b'h', b'i', 0x10, 0x96, 0x01];
        let value = decode_grpc_payload(&payload, &set, "grpc.test.Echo").expect("decode");
        assert_eq!(value.get("text").and_then(|v| v.as_str()), Some("hi"));
        assert_eq!(value.get("n").and_then(|v| v.as_i64()), Some(150));
        // Deterministic repeated projection.
        let again = decode_grpc_payload(&payload, &set, "grpc.test.Echo").expect("decode");
        assert_eq!(
            serde_json::to_string(&value).expect("json"),
            serde_json::to_string(&again).expect("json")
        );
    }

    #[test]
    fn descriptor_bounds_fail_closed() {
        let payload = vec![0x0A, 0x01, b'x'];
        assert!(matches!(
            decode_grpc_payload(&payload, &[0xFF; 8], "grpc.test.Echo"),
            Err(GrpcError::DescriptorInvalid(_))
        ));
        let oversized = vec![0u8; GRPC_MAX_DESCRIPTOR_BYTES + 1];
        assert_eq!(
            decode_grpc_payload(&payload, &oversized, "grpc.test.Echo"),
            Err(GrpcError::DescriptorTooLarge)
        );
        let set = echo_descriptor_set();
        assert_eq!(
            decode_grpc_payload(&payload, &set, "grpc.test.Missing"),
            Err(GrpcError::UnknownMessage("grpc.test.Missing".to_string()))
        );
    }

    #[test]
    fn view_marks_compressed_opaque() {
        let mut body = frame(false, &[0x0A, 0x01, b'x']);
        body.extend(frame(true, b"opaque-bytes"));
        let view = grpc_view(&body, &[], None).expect("view");
        assert_eq!(view.messages.len(), 2);
        assert!(view.messages[0].decoded.is_none());
        assert!(view.messages[1].compressed);
        assert!(view.messages[1].decoded.is_none());
        assert_eq!(view.status, None);
    }

    #[test]
    fn content_type_gate() {
        assert!(is_grpc_content_type(Some("application/grpc")));
        assert!(is_grpc_content_type(Some("application/grpc+proto")));
        assert!(!is_grpc_content_type(Some("application/json")));
        assert!(!is_grpc_content_type(None));
    }
}
