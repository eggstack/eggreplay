# ADR 0006 — WebSocket Semantic Conversations

Status: accepted

## Context

M011 adds WebSocket record/replay/regression after M010 established bounded
HTTP stream-event timing. WebSockets begin as an HTTP/1.1 Upgrade but then
become a bidirectional message protocol. Treating the post-upgrade bytes as an
HTTP body would blur transport ownership, lose message semantics, and overload
M010's request/response-body event model.

## Decision

EggReplay models one WebSocket conversation as a required session extension
attached to the initiating HTTP flow.

- The ordinary flow remains the authority for the HTTP Upgrade request and
  101 response metadata.
- Post-upgrade WebSocket messages live in the required
  `websocket-messages` extension at `websockets.jsonl`.
- A reader that does not understand the extension must reject a fixture that
  uses it rather than treating the 101 as a complete replay.
- Each conversation is keyed by initiating flow id and records the selected
  subprotocol, extension policy, termination state, and an ordered message
  sequence.
- Message records contain a stable sequence number, direction, semantic kind,
  monotonic delta from upgrade completion, payload body reference where
  applicable, and close code/reason where applicable.
- Text, binary, ping, pong, and close are semantic message kinds.
  Fragmentation boundaries, masking keys, frame chunking, and TCP packet
  boundaries are intentionally not canonical.
- Nonempty message payload bytes use the existing content-addressed blob store.
  Extension JSON never duplicates message payload bytes.
- Message timing reuses M010's bounded timing policy and scaling limits, but
  WebSocket message deltas remain in the WebSocket extension. M010
  `stream-events` remains specific to HTTP request/response body frames.
- `Immediate` replay preserves message ordering/terminal semantics with zero
  added delay. Recorded/scaled timing is explicit.

## HTTP handshake authority

EggFetch owns outbound HTTP/TLS and the owned post-101 `UpgradedStream`.
EggServe owns inbound H1 parsing/lifecycle and generic tunnel handoff.
EggReplay owns WebSocket handshake semantics, message orchestration, fixture
state, redaction, matching, and regression. A maintained WebSocket codec owns
RFC 6455 frame/message parsing.

EggReplay must not create a second Hyper client/server stack for WebSockets.

Volatile handshake values are not literal fixture authority:

- `Sec-WebSocket-Key` is per connection and is excluded from ordinary request
  matching/persistence authority.
- `Sec-WebSocket-Accept` is recomputed for replay and is not replayed as a
  recorded literal.
- `Sec-WebSocket-Protocol` is semantic: offered protocols may be persisted in
  normalized form and the selected protocol is part of the conversation
  contract.
- The initial M011 implementation declines WebSocket extensions. In particular,
  it does not negotiate `permessage-deflate`. If an upstream unexpectedly
  negotiates an extension, the acquisition fails closed instead of decoding
  with unknown semantics.

Use codec/library handshake helpers for key/accept validation. Do not hand-roll
SHA-1/base64 framing logic.

## Matching and redaction

The initiating HTTP request uses the ordinary EggReplay matcher plus
WebSocket-specific normalization for volatile handshake values.

Message payload matching is semantic:

- text compares UTF-8 bytes exactly unless structured JSON redaction markers
  establish wildcard paths;
- binary compares bytes exactly unless an explicit whole-message redaction
  marker makes the payload non-authoritative;
- ping/pong payloads are compared exactly unless redacted;
- close compares code and reason after configured redaction.

Persisted WebSocket payloads follow C003's rule: redaction happens before a
durable blob becomes fixture authority.

## Initial support tier

M011 qualifies RFC 6455 over HTTP/1.1 Upgrade only.

Out of scope for M011:

- H2 Extended CONNECT WebSockets;
- HTTP/3 WebSockets;
- negotiated compression/extensions;
- wire-perfect fragmentation/masking reproduction;
- transparent interception;
- arbitrary WebSocket scripting;
- automatic reconnect semantics.

Inbound EggReplay endpoints are plain `ws://` unless TLS is explicitly
composed through a separately qualified EggServe surface. Outbound `wss://`
may be claimed only after local EggFetch TLS qualification with a caller-owned
test trust root.

## Consequences

M011 implementation is decomposed into transport/dependency qualification,
semantic/store authority, recording, offline replay, regression/CLI, and final
hardening. Later Python bindings consume the Rust authority and must not
reimplement this model.
