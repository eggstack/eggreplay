//! Transport-neutral WebSocket conversation semantics and validation.

use crate::{BlobRef, BodyRef, HeaderEntry};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashSet;

/// Current schema for the `websocket-messages` session extension.
pub const WEBSOCKET_SCHEMA_VERSION: u16 = 1;

/// Bounds for one WebSocket session/transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketLimits {
    /// Maximum conversations in a session.
    pub max_conversations: usize,
    /// Maximum messages in a single conversation.
    pub max_messages_per_conversation: usize,
    /// Maximum messages across a session.
    pub max_messages_per_session: usize,
    /// Maximum payload length for one message.
    pub max_message_bytes: u64,
    /// Maximum payload bytes across the session.
    pub max_total_payload_bytes: u64,
    /// Maximum total duration of a conversation.
    pub max_duration_ns: u64,
    /// Maximum encoded transcript/extension bytes.
    pub max_extension_metadata_bytes: usize,
    /// Maximum handshake header count.
    pub max_handshake_header_count: usize,
    /// Maximum aggregate handshake header bytes.
    pub max_handshake_header_bytes: usize,
    /// Maximum diagnostic and near-miss bytes.
    pub max_diagnostic_bytes: usize,
    /// Maximum ping/pong/close data bytes.
    pub max_control_payload_bytes: usize,
    /// Maximum close reason length in UTF-8 bytes.
    pub max_close_reason_bytes: usize,
    /// Maximum subprotocol count.
    pub max_subprotocols: usize,
    /// Maximum subprotocol name length.
    pub max_subprotocol_bytes: usize,
}

impl Default for WebSocketLimits {
    fn default() -> Self {
        Self {
            max_conversations: 1024,
            max_messages_per_conversation: 100_000,
            max_messages_per_session: 1_000_000,
            max_message_bytes: 16 * 1024 * 1024,
            max_total_payload_bytes: 1024 * 1024 * 1024,
            max_duration_ns: 24 * 60 * 60 * 1_000_000_000,
            max_extension_metadata_bytes: 16 * 1024 * 1024,
            max_handshake_header_count: 128,
            max_handshake_header_bytes: 64 * 1024,
            max_diagnostic_bytes: 4096,
            max_control_payload_bytes: 125,
            max_close_reason_bytes: 123,
            max_subprotocols: 32,
            max_subprotocol_bytes: 128,
        }
    }
}

/// Direction of one semantic message relative to the recording endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketDirection {
    /// Message sent by the client toward the server.
    ClientToServer,
    /// Message sent by the server toward the client.
    ServerToClient,
}

/// Semantic message kind. Frame layout, masking, and fragmentation are omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketMessageKind {
    /// UTF-8 text message.
    Text,
    /// Binary message.
    Binary,
    /// RFC 6455 ping control message.
    Ping,
    /// RFC 6455 pong control message.
    Pong,
    /// RFC 6455 close control message.
    Close,
}

/// Payload redaction authority attached to a semantic message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebSocketRedaction {
    /// The payload has JSON pointer replacements. Those paths are wildcards
    /// when comparing client messages.
    JsonPointers {
        /// Canonical JSON pointer paths which were replaced.
        pointers: Vec<String>,
    },
    /// Payload bytes are intentionally non-authoritative.
    WholeMessage,
    /// The close reason is intentionally non-authoritative.
    CloseReason,
}

/// Terminal result observed for the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebSocketTerminal {
    /// A valid close handshake completed.
    CleanClose {
        /// Close code, when the peer supplied one.
        code: Option<u16>,
        /// Close reason, when nonempty.
        reason: Option<String>,
    },
    /// The stream ended without a valid clean close.
    Abnormal {
        /// Bounded stable classification such as `eof`, `reset`, or `shutdown`.
        cause: String,
    },
}

/// One canonical message in the conversation transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketMessage {
    /// Zero-based contiguous sequence number shared across both directions.
    pub sequence: u64,
    /// Sender direction.
    pub direction: WebSocketDirection,
    /// Monotonic nanoseconds after successful upgrade completion.
    pub delta_ns: u64,
    /// Semantic message type.
    pub kind: WebSocketMessageKind,
    /// Content-addressed payload for nonempty text, binary, and control data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<BodyRef>,
    /// Close code, present only for close messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    /// Close reason, present only for close messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    /// Explicit redaction metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<WebSocketRedaction>,
}

/// A WebSocket conversation associated with exactly one initiating HTTP flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketConversation {
    /// Stable identifier for this conversation.
    pub id: String,
    /// Initiating HTTP Upgrade flow identifier.
    pub flow_id: String,
    /// Offered subprotocols, in client order.
    #[serde(default)]
    pub offered_subprotocols: Vec<String>,
    /// Selected subprotocol, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_subprotocol: Option<String>,
    /// Strict global semantic transcript.
    pub messages: Vec<WebSocketMessage>,
    /// Conversation terminal state.
    pub terminal: WebSocketTerminal,
}

/// Required session-level transcript extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketTranscript {
    /// Schema version of this transcript.
    pub schema_version: u16,
    /// All captured WebSocket conversations.
    pub conversations: Vec<WebSocketConversation>,
}

impl WebSocketTranscript {
    /// Validate bounded sizes, stable ordering, references, and terminal rules.
    pub fn validate(&self, limits: WebSocketLimits) -> Result<(), String> {
        if self.schema_version != WEBSOCKET_SCHEMA_VERSION {
            return Err("unsupported WebSocket extension schema".into());
        }
        if self.conversations.len() > limits.max_conversations {
            return Err("WebSocket conversation count exceeds limit".into());
        }
        let mut ids = HashSet::new();
        let mut flow_ids = HashSet::new();
        let mut total_messages = 0usize;
        let mut total_payload = 0u64;
        for conversation in &self.conversations {
            if conversation.id.is_empty() || conversation.id.len() > 128 {
                return Err("invalid WebSocket conversation id".into());
            }
            if conversation.flow_id.is_empty() || conversation.flow_id.len() > 128 {
                return Err("invalid WebSocket initiating flow id".into());
            }
            if !ids.insert(&conversation.id) {
                return Err("duplicate WebSocket conversation id".into());
            }
            if !flow_ids.insert(&conversation.flow_id) {
                return Err("duplicate WebSocket initiating flow id".into());
            }
            if conversation.offered_subprotocols.len() > limits.max_subprotocols {
                return Err("too many offered WebSocket subprotocols".into());
            }
            for protocol in &conversation.offered_subprotocols {
                if protocol.is_empty()
                    || protocol.len() > limits.max_subprotocol_bytes
                    || !protocol.bytes().all(is_token_byte)
                {
                    return Err("invalid WebSocket subprotocol".into());
                }
            }
            if let Some(selected) = &conversation.selected_subprotocol
                && (!conversation.offered_subprotocols.contains(selected)
                    || selected.len() > limits.max_subprotocol_bytes)
            {
                return Err("selected WebSocket subprotocol was not offered".into());
            }
            if conversation.messages.len() > limits.max_messages_per_conversation {
                return Err("WebSocket message count exceeds conversation limit".into());
            }
            total_messages = total_messages
                .checked_add(conversation.messages.len())
                .ok_or_else(|| "WebSocket message count overflow".to_owned())?;
            if total_messages > limits.max_messages_per_session {
                return Err("WebSocket message count exceeds session limit".into());
            }
            let mut prior_delta = 0;
            let mut close_directions = HashSet::new();
            let mut terminated = false;
            for (index, message) in conversation.messages.iter().enumerate() {
                if message.sequence != index as u64 {
                    return Err("WebSocket sequence numbers must be contiguous".into());
                }
                if index > 0 && message.delta_ns < prior_delta {
                    return Err("WebSocket message timing must be nondecreasing".into());
                }
                if message.delta_ns > limits.max_duration_ns {
                    return Err("WebSocket conversation duration exceeds limit".into());
                }
                prior_delta = message.delta_ns;
                if terminated {
                    return Err("WebSocket messages follow a terminal event".into());
                }
                if close_directions.contains(&message.direction)
                    && message.kind != WebSocketMessageKind::Close
                {
                    return Err("WebSocket message follows close in the same direction".into());
                }
                let length = match message.payload.as_ref() {
                    None => 0,
                    Some(body) => body
                        .len()
                        .ok_or_else(|| "WebSocket payload body length is invalid".to_owned())?,
                };
                if length > limits.max_message_bytes {
                    return Err("WebSocket message payload exceeds limit".into());
                }
                total_payload = total_payload
                    .checked_add(length)
                    .ok_or_else(|| "WebSocket payload length overflow".to_owned())?;
                if total_payload > limits.max_total_payload_bytes {
                    return Err("WebSocket payload bytes exceed session limit".into());
                }
                let is_control = matches!(
                    message.kind,
                    WebSocketMessageKind::Ping
                        | WebSocketMessageKind::Pong
                        | WebSocketMessageKind::Close
                );
                if is_control && length > limits.max_control_payload_bytes.min(125) as u64 {
                    return Err("WebSocket control payload exceeds configured limit".into());
                }
                if message.kind == WebSocketMessageKind::Close {
                    if message.payload.is_some() {
                        return Err("WebSocket close data must use close metadata".into());
                    }
                    if message
                        .close_code
                        .is_some_and(|code| !valid_close_code(code))
                    {
                        return Err("invalid WebSocket close code".into());
                    }
                    if message.close_reason.as_ref().is_some_and(|reason| {
                        reason.len() > limits.max_close_reason_bytes.min(123)
                            || message.close_code.is_none()
                    }) {
                        return Err("invalid WebSocket close reason".into());
                    }
                    if !close_directions.insert(message.direction) {
                        return Err("duplicate WebSocket close message in one direction".into());
                    }
                    terminated = close_directions.len() == 2;
                } else if message.close_code.is_some() || message.close_reason.is_some() {
                    return Err("close metadata on a non-close WebSocket message".into());
                }
                validate_redactions(&message.redactions, message.kind)?;
            }
            match &conversation.terminal {
                WebSocketTerminal::CleanClose { code, reason } => {
                    if close_directions.len() != 2 {
                        return Err("clean WebSocket terminal requires both close messages".into());
                    }
                    let Some(last) = conversation.messages.last() else {
                        return Err("clean WebSocket terminal requires a close message".into());
                    };
                    if last.kind != WebSocketMessageKind::Close
                        || last.close_code != *code
                        || last.close_reason != *reason
                    {
                        return Err("clean WebSocket terminal requires a close message".into());
                    }
                    if code.is_some_and(|value| !valid_close_code(value))
                        || reason.as_ref().is_some_and(|value| {
                            value.len() > limits.max_close_reason_bytes.min(123) || code.is_none()
                        })
                    {
                        return Err("invalid WebSocket terminal close metadata".into());
                    }
                }
                WebSocketTerminal::Abnormal { cause } => {
                    if !matches!(
                        cause.as_str(),
                        "eof"
                            | "reset"
                            | "shutdown"
                            | "duration-limit"
                            | "protocol-error"
                            | "read-error"
                            | "write-error"
                            | "timeout"
                            | "cancelled"
                            | "upstream-error"
                            | "downstream-error"
                            | "other"
                    ) {
                        return Err("invalid WebSocket abnormal terminal cause".into());
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_redactions(
    redactions: &[WebSocketRedaction],
    kind: WebSocketMessageKind,
) -> Result<(), String> {
    for marker in redactions {
        match marker {
            WebSocketRedaction::JsonPointers { pointers } => {
                if kind != WebSocketMessageKind::Text
                    || pointers.len() > 256
                    || pointers
                        .iter()
                        .any(|pointer| pointer.len() > 1024 || !pointer.starts_with('/'))
                {
                    return Err("invalid WebSocket JSON redaction metadata".into());
                }
            }
            WebSocketRedaction::WholeMessage => {}
            WebSocketRedaction::CloseReason if kind != WebSocketMessageKind::Close => {
                return Err("close reason redaction on a non-close message".into());
            }
            WebSocketRedaction::CloseReason => {}
        }
    }
    Ok(())
}

fn valid_close_code(code: u16) -> bool {
    (1000..=1014).contains(&code) && !matches!(code, 1004..=1006) || (3000..=4999).contains(&code)
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// Construct a payload reference from bytes for the semantic validation seam.
/// Store code still owns durable publication and digest verification.
pub fn payload_ref_for_bytes(bytes: &[u8]) -> Result<BodyRef, String> {
    if bytes.is_empty() {
        return Ok(BodyRef::Empty);
    }
    let digest = format!("{:x}", sha2::Sha256::digest(bytes));
    let blob = BlobRef::new(digest, bytes.len() as u64).map_err(|error| error.to_string())?;
    Ok(BodyRef::Blob(blob))
}

/// Remove volatile handshake fields without changing generic HTTP header rules.
///
/// Request normalization removes `Sec-WebSocket-Key`; response normalization
/// removes `Sec-WebSocket-Accept`. All other ordered headers remain semantic.
pub fn normalize_websocket_handshake_headers(
    headers: &[HeaderEntry],
    response: bool,
) -> Vec<HeaderEntry> {
    let volatile = if response {
        "sec-websocket-accept"
    } else {
        "sec-websocket-key"
    };
    headers
        .iter()
        .filter(|header| !header.name.eq_ignore_ascii_case(volatile))
        .cloned()
        .collect()
}

/// Validate WebSocket handshake header bounds and reject negotiated extensions.
///
/// An offered extension may be present in a client request, but the initial
/// EggReplay semantic protocol does not accept an extension in a response.
pub fn validate_websocket_handshake_headers(
    headers: &[HeaderEntry],
    limits: WebSocketLimits,
    response: bool,
) -> Result<(), String> {
    if headers.len() > limits.max_handshake_header_count {
        return Err("WebSocket handshake header count exceeds limit".into());
    }
    let bytes = headers.iter().try_fold(0usize, |total, header| {
        total
            .checked_add(header.name.len())
            .and_then(|total| total.checked_add(header.value.len()))
            .and_then(|total| total.checked_add(4))
            .ok_or_else(|| "WebSocket handshake header length overflow".to_owned())
    })?;
    if bytes > limits.max_handshake_header_bytes {
        return Err("WebSocket handshake headers exceed byte limit".into());
    }
    if response
        && headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("sec-websocket-extensions")
                && !header.value.trim().is_empty()
        })
    {
        return Err("negotiated WebSocket extensions are unsupported".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(
        messages: Vec<WebSocketMessage>,
        terminal: WebSocketTerminal,
    ) -> WebSocketTranscript {
        WebSocketTranscript {
            schema_version: WEBSOCKET_SCHEMA_VERSION,
            conversations: vec![WebSocketConversation {
                id: "ws-1".into(),
                flow_id: "flow-1".into(),
                offered_subprotocols: vec!["chat".into()],
                selected_subprotocol: Some("chat".into()),
                messages,
                terminal,
            }],
        }
    }

    fn close(sequence: u64, delta_ns: u64) -> WebSocketMessage {
        WebSocketMessage {
            sequence,
            direction: WebSocketDirection::ServerToClient,
            delta_ns,
            kind: WebSocketMessageKind::Close,
            payload: None,
            close_code: Some(1000),
            close_reason: None,
            redactions: vec![],
        }
    }

    #[test]
    fn transcript_roundtrips_and_accepts_clean_close() {
        let transcript = conversation(
            vec![
                close(0, 0),
                WebSocketMessage {
                    direction: WebSocketDirection::ClientToServer,
                    sequence: 1,
                    ..close(1, 1)
                },
            ],
            WebSocketTerminal::CleanClose {
                code: Some(1000),
                reason: None,
            },
        );
        transcript.validate(WebSocketLimits::default()).unwrap();
        let encoded = serde_json::to_vec(&transcript).unwrap();
        assert_eq!(
            serde_json::from_slice::<WebSocketTranscript>(&encoded).unwrap(),
            transcript
        );
    }

    #[test]
    fn rejects_bad_sequence_timing_close_and_duplicate_flow() {
        let limits = WebSocketLimits::default();
        let mut invalid = conversation(
            vec![close(1, 0)],
            WebSocketTerminal::CleanClose {
                code: Some(1000),
                reason: None,
            },
        );
        assert!(invalid.validate(limits).is_err());
        invalid.conversations[0].messages[0].sequence = 0;
        assert!(invalid.validate(limits).is_err());
        invalid.conversations[0].messages.push(close(1, 0));
        assert!(invalid.validate(limits).is_err());
        invalid.conversations[0].messages.pop();
        invalid.conversations[0].messages[0].close_code = Some(1005);
        assert!(invalid.validate(limits).is_err());
        invalid.conversations[0].messages[0].close_code = Some(1000);
        invalid.conversations[0].messages[0].redactions = vec![WebSocketRedaction::JsonPointers {
            pointers: vec!["/secret".into()],
        }];
        assert!(invalid.validate(limits).is_err());
        invalid.conversations[0].messages[0].redactions.clear();
        invalid.conversations.push(invalid.conversations[0].clone());
        assert!(invalid.validate(limits).is_err());
    }

    #[test]
    fn rejects_decreasing_time_bad_subprotocol_and_control_overflow() {
        let limits = WebSocketLimits::default();
        let text = WebSocketMessage {
            sequence: 0,
            direction: WebSocketDirection::ClientToServer,
            delta_ns: 10,
            kind: WebSocketMessageKind::Text,
            payload: None,
            close_code: None,
            close_reason: None,
            redactions: vec![],
        };
        let mut transcript = conversation(
            vec![text, close(1, 9)],
            WebSocketTerminal::CleanClose {
                code: Some(1000),
                reason: None,
            },
        );
        assert!(transcript.validate(limits).is_err());
        transcript.conversations[0].messages[0].delta_ns = 0;
        transcript.conversations[0].selected_subprotocol = Some("other".into());
        assert!(transcript.validate(limits).is_err());
        transcript.conversations[0].selected_subprotocol = Some("chat".into());
        transcript.conversations[0].messages.insert(
            0,
            WebSocketMessage {
                sequence: 0,
                direction: WebSocketDirection::ClientToServer,
                delta_ns: 0,
                kind: WebSocketMessageKind::Ping,
                payload: Some(BodyRef::Blob(BlobRef::new("0".repeat(64), 126).unwrap())),
                close_code: None,
                close_reason: None,
                redactions: vec![],
            },
        );
        transcript.conversations[0].messages[1].sequence = 1;
        transcript.conversations[0].messages[2].sequence = 2;
        assert!(transcript.validate(limits).is_err());
    }

    #[test]
    fn rejects_messages_after_bilateral_close_terminal() {
        let peer_close = WebSocketMessage {
            sequence: 1,
            direction: WebSocketDirection::ClientToServer,
            ..close(1, 1)
        };
        let transcript = conversation(
            vec![
                close(0, 0),
                peer_close,
                WebSocketMessage {
                    sequence: 2,
                    direction: WebSocketDirection::ServerToClient,
                    delta_ns: 2,
                    kind: WebSocketMessageKind::Text,
                    payload: None,
                    close_code: None,
                    close_reason: None,
                    redactions: vec![],
                },
            ],
            WebSocketTerminal::CleanClose {
                code: Some(1000),
                reason: None,
            },
        );
        assert!(transcript.validate(WebSocketLimits::default()).is_err());
    }

    #[test]
    fn handshake_normalization_only_removes_volatile_websocket_value() {
        let headers = vec![
            HeaderEntry {
                name: "Sec-WebSocket-Key".into(),
                value: "secret-key".into(),
            },
            HeaderEntry {
                name: "Origin".into(),
                value: "https://example.test".into(),
            },
            HeaderEntry {
                name: "Cookie".into(),
                value: "session=secret".into(),
            },
            HeaderEntry {
                name: "Sec-WebSocket-Protocol".into(),
                value: "chat".into(),
            },
        ];
        let normalized = normalize_websocket_handshake_headers(&headers, false);
        assert_eq!(normalized.len(), 3);
        assert!(normalized.iter().any(|header| header.name == "Origin"));
        assert!(normalized.iter().any(|header| header.name == "Cookie"));
        assert!(
            normalized
                .iter()
                .any(|header| header.name == "Sec-WebSocket-Protocol")
        );
        assert!(
            normalize_websocket_handshake_headers(&headers, true)
                .iter()
                .any(|header| header.name == "Sec-WebSocket-Key")
        );
    }

    #[test]
    fn handshake_limits_and_extension_decline_are_enforced() {
        let limits = WebSocketLimits::default();
        let extension = HeaderEntry {
            name: "Sec-WebSocket-Extensions".into(),
            value: "permessage-deflate".into(),
        };
        assert!(
            validate_websocket_handshake_headers(std::slice::from_ref(&extension), limits, false)
                .is_ok()
        );
        assert!(validate_websocket_handshake_headers(&[extension], limits, true).is_err());
        let constrained = WebSocketLimits {
            max_handshake_header_count: 0,
            ..limits
        };
        assert!(
            validate_websocket_handshake_headers(
                &[HeaderEntry {
                    name: "host".into(),
                    value: "x".into()
                }],
                constrained,
                false,
            )
            .is_err()
        );
    }
}
