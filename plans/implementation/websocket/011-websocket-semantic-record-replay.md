# M011 — WebSocket Semantic Record, Replay, and Regression

Status: blocked
Depends on: M010
Roadmap stage: 7

## Objective

Record and replay WebSocket conversations as ordered semantic messages attached
to their initiating HTTP flow. Do not claim wire-perfect reproduction.

Use EggFetch's owned upgraded stream for outbound 101/CONNECT handoff and
EggServe's generic tunnel capability/TunnelIo for inbound server handoff.
EggReplay owns the WebSocket semantic codec/orchestration only.

## Protocol codec

Do not hand-roll WebSocket framing. Use a maintained Rust WebSocket codec
(`tungstenite`/Tokio integration or another reviewed equivalent) over the
EggFetch/EggServe duplex streams.

HTTP handshake transport still belongs to EggFetch/EggServe. The codec may
supply/validate WebSocket-specific handshake values and frame/message parsing,
but must not create a parallel HTTP client/server implementation.

Start with RFC 6455 over HTTP/1.1 Upgrade. H2 Extended CONNECT WebSockets are
M014/H2 qualification work.

## Storage

Reuse ADR 0005. Add `websocket-messages` / `websockets.jsonl`, keyed by
initiating flow id.

Store ordered records containing:

- direction (client->server/server->client);
- semantic kind (text, binary, ping, pong, close);
- payload body ref where applicable;
- close code/reason where applicable;
- relative timing/event reference from M010;
- stable sequence index.

Record reassembled semantic messages, not fragmentation layout. Preserve control
messages needed for meaningful behavior. No masking key/frame-boundary claim.

## Recording gateway

For a validated WebSocket upgrade:

1. accept inbound upgrade through EggServe;
2. establish upstream handshake through EggFetch;
3. obtain both post-upgrade streams;
4. run bounded bidirectional message relay;
5. write message payloads through the existing content-addressed blob store;
6. append the initiating HTTP flow plus WebSocket extension records atomically.

Backpressure must be direct/bounded. No unbounded channel between directions.

Cancellation, peer close, abnormal EOF, and shutdown must finalize a truthful
conversation record without inventing a clean close.

## Offline replay

A recorded WebSocket replay endpoint:

- matches the initiating HTTP request using the ordinary matcher;
- performs the validated WebSocket handshake;
- checks expected client messages in order according to an explicit policy;
- emits recorded server messages;
- uses immediate timing by default and M010 timing modes when requested;
- returns bounded near-miss diagnostics for message mismatches.

Allow a clearly named permissive policy only if it is explicit; strict ordered
message matching is the default.

## Regression

Replay recorded client messages against a candidate WebSocket endpoint and
capture candidate messages through the same semantic model. Compare direction,
message kind, text/binary payload, close semantics, and optional timing.

Reports remain redaction-safe and versioned.

## Security/limits

Enforce caps for handshake headers, messages per conversation, message bytes,
aggregate conversation bytes, duration, ping/pong flood, close reason length,
and diagnostics.

Apply configured redaction to recognized text/JSON messages before durable
publication. Opaque binary messages receive only whole-payload treatment unless
a later explicit decoder exists.

Do not support permessage-deflate initially unless the chosen codec can expose
deterministic decompressed message semantics without compromising limits; if
not, reject/decline the extension explicitly.

## Tests

Local deterministic echo/scripted servers must cover text, binary, ping/pong,
close, abnormal EOF, leading post-101 data, concurrent directions,
backpressure, large bounded messages, cancellation, replay mismatch, candidate
regression, redaction, and exact fixture reopen/validation.

Include at least one test proving the HTTP flow remains usable independently of
the WebSocket extension metadata.

## Closure

Create `plans/closure/m011-websocket-semantic-record-replay.md`. M012 stays
blocked until the Rust surface is stable enough to bind rather than duplicating
it in Python.
