# M011B — WebSocket Semantic Model, Store, and Codec Closure

Status: closed

## Implementation

- Commit: `349c72b4cb427d119329693338b4bdb9cecdcc56`
- Codec: `tokio-tungstenite 0.30.0` / `tungstenite 0.30.0`, exact pin,
  Rust 1.85 minimum, default features disabled, only `handshake` enabled.
- The optional codec is exposed only by `eggreplay-http/websocket`; it wraps
  caller-owned `AsyncRead + AsyncWrite + Send + Unpin` streams with raw
  client/server roles. No `connect_async`, `accept_async`, connector, TLS
  backend, or compression feature is used.
- Core/store have no WebSocket codec dependency. The CLI does not enable the
  codec yet; the direct non-WebSocket adapter build remains available.

## Implemented authority

- Added transport-free conversation, message, direction, kind, redaction,
  terminal, transcript, schema version, and resource-limit types in
  `eggreplay-core`.
- Validation covers sequence continuity, monotonic time, bilateral clean
  close, post-terminal messages, supported close codes/reasons, UTF-8 text,
  subprotocol offers, control sizes, payload/session/message limits,
  duration, duplicate flow/conversation IDs, and bounded abnormal causes.
- WebSocket-specific handshake normalization removes request keys and
  response accept values while preserving all other ordered HTTP headers.
  Header limits and response extension rejection are explicit.
- Added a single required `websocket-messages` extension at
  `websockets.jsonl`. Session schema 2 is required. `SessionWriter`,
  `RecordingSession`, and `Session::open` validate the transcript before
  publication/loading, cross-check every conversation against a WebSocket 101
  flow, reject 101 flows without conversations, and verify confined payload
  blob length/digest and UTF-8 text.
- Codec tests characterize text, binary, fragmented text reassembly, ping with
  automatic queued Pong, close echo, caller-provided leading bytes, and
  message-size rejection. Key validation and accept derivation use maintained
  codec/library helpers.

## Verification

All required local gates passed on the implementation commit:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo check -p eggreplay-http --no-default-features --features websocket --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked  # 125 passed
git diff --check
```

Focused tests passed: 6 core WebSocket tests, 4 store extension tests, and 5
codec/helper tests. Dependency-tree inspection confirmed core/store and direct
HTTP builds contain no `tungstenite` or `base64` dependency. The optional
WebSocket feature adds `tokio-tungstenite` without enabling its connector
feature.

M011C is ready; no semantic/store/codec blocker remains.
