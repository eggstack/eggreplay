# M011B — WebSocket Semantic Model, Store, and Codec Boundary

Status: blocked
Depends on: M011A
Parent milestone: M011
ADR: 0006

## Objective

Introduce the transport-neutral WebSocket conversation authority, bounded
fixture storage, and a codec adapter over already-owned duplex streams. Do not
add recording/replay orchestration yet.

## A. Core semantic types

Add transport-free types in `eggreplay-core`:

- `WebSocketConversation`;
- `WebSocketMessage`;
- `WebSocketDirection`;
- `WebSocketMessageKind`;
- close code/reason representation;
- selected subprotocol;
- terminal state;
- schema/version constants;
- validation and resource limits.

Each message contains:

- contiguous stable sequence index;
- direction;
- monotonic `delta_ns` from upgrade completion;
- kind: text/binary/ping/pong/close;
- `BodyRef` for nonempty payload where applicable;
- close code/reason for close messages;
- redaction markers/annotations needed to make payload authority explicit.

Do not store frame FIN/opcode fragmentation layout or masking keys.

## B. Limits

Define explicit bounds for at least:

- conversations per session;
- messages per conversation/session;
- message payload bytes;
- aggregate WebSocket payload bytes;
- extension metadata bytes;
- close reason bytes;
- subprotocol count/name length;
- handshake header count/bytes;
- ping/pong payload size;
- maximum conversation duration;
- diagnostic/near-miss output.

Validation must reject noncontiguous sequence numbers, decreasing timing,
messages after terminal close/error, invalid UTF-8 text, invalid close
codes/reasons, oversized control payloads, duplicate conversation flow ids,
and unsupported extension schema.

## C. Required fixture extension

Add `websocket-messages` at `websockets.jsonl` through ADR 0005's extension
registry.

When any WebSocket conversation exists:

- descriptor is `required_for_replay=true`;
- session schema is current schema 2;
- every conversation references an existing initiating flow;
- every payload `BodyRef` is confined/validated like ordinary bodies;
- a WebSocket initiating flow cannot be duplicated across conversation
  records;
- session finalization cross-validates conversations and initiating 101 flows;
- a required extension that is missing/malformed/inconsistent fails closed.

Add bounded aggregate support to both `SessionWriter` and
`RecordingSession`; do not create one extension file per conversation.

## D. Handshake normalization

Define WebSocket-specific canonicalization without mutating generic HTTP rules.

- `Sec-WebSocket-Key` is volatile and excluded from request match authority.
- `Sec-WebSocket-Accept` is volatile and excluded from response replay
  authority.
- `Connection`/`Upgrade`, version 13, offered subprotocols, selected
  subprotocol, Origin, cookies/auth, and ordinary headers remain semantic
  according to existing matcher/redaction rules.
- Initial M011 never persists an accepted
  `Sec-WebSocket-Extensions` value because negotiated extensions are not
  supported.

Persist annotations/typed metadata sufficient for inspect/debugging without
storing volatile handshake values as literal match keys.

## E. Codec feature

Add a narrow optional `websocket` feature to `eggreplay-http`.

Use a maintained `tungstenite`/`tokio-tungstenite` line compatible with
Rust 1.89. Prefer the minimal feature set:

- RFC 6455 frame/message codec;
- handshake helper functions needed for key/accept/subprotocol validation;
- no built-in network connector;
- no TLS backend owned by the codec;
- no compression extension.

Wrap caller-owned `AsyncRead + AsyncWrite + Send + Unpin` streams with raw
client/server roles after EggFetch/EggServe complete HTTP ownership.

Do not use `connect_async`, `accept_async`, or any API that creates a
parallel HTTP/TCP/TLS stack.

Characterize codec ping/pong/close behavior so automatic control responses do
not hide or duplicate semantic records.

## F. Timing

Store WebSocket `delta_ns` in the WebSocket extension. Reuse
`StreamTimingMode` scaling/bounds where appropriate, but do not add WebSocket
directions to M010 `StreamEvents`.

## Required tests

- type/schema roundtrips;
- all invalid sequence/timing/terminal cases;
- payload BodyRef confinement/integrity;
- required extension flag;
- extension/session cross-validation;
- schema-1/session-2 compatibility;
- volatile handshake normalization;
- raw codec text/binary/control/close roundtrips over `tokio::io::duplex`;
- fragmented input reassembled to semantic messages;
- leading bytes through an EggFetch-like stream wrapper;
- resource-limit rejection;
- core/store dependency boundaries contain no codec.

## Closure

Create `plans/closure/m011b-websocket-semantic-model-store-and-codec.md`.
M011C becomes ready only after the store and codec authority are stable.
