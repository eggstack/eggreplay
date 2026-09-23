# M011 — WebSocket Semantic Record, Replay, and Regression

Status: closed
Depends on: M010 + M010-C1 closure
Roadmap stage: 7
Architecture: ADR 0006

## Objective

Record, replay, and regress RFC 6455 WebSocket conversations as ordered
semantic messages attached to their initiating HTTP Upgrade flow, without
claiming wire-perfect reproduction or creating another HTTP stack.

EggFetch remains outbound HTTP/TLS + owned post-101 stream authority.
EggServe remains inbound H1 runtime/lifecycle + generic tunnel authority.
Eggress remains optional listener-free route establishment. EggReplay owns the
conversation model, fixture storage, handshake semantics, matching, redaction,
replay state, regression, and CLI orchestration.

## Execution decomposition

M011 is intentionally split into dependency-ordered handoff plans:

| ID | Plan | Result |
|---|---|---|
| M011A | `011a-transport-dependency-and-upgrade-preflight.md` | registry/dependency baseline + direct/routed upgrade proof |
| M011B | `011b-semantic-model-store-and-codec.md` | canonical conversation/store/codec authority |
| M011C | `011c-recording-gateway.md` | bounded redaction-safe recording |
| M011D | `011d-offline-replay.md` | deterministic offline replay |
| M011E | `011e-candidate-regression-cli-and-diff.md` | candidate regression, reports, CLI/diff |
| M011F | `011f-hardening-qualification-and-closure.md` | security/resource/portable qualification + milestone closure |

Only M011A is ready initially. Do not begin a later subplan before its
dependency closes; several plans intentionally touch the same HTTP/store/CLI
surfaces and are sequenced to avoid conflicting ownership.

## Canonical semantics

ADR 0006 controls M011's storage/matching boundary:

- initiating HTTP Upgrade remains a normal EggReplay flow;
- post-upgrade messages live in required `websocket-messages` /
  `websockets.jsonl`;
- payload bytes use content-addressed blobs;
- text/binary/ping/pong/close are semantic message kinds;
- fragmentation, masking keys, frame chunking, and packet layout are not
  canonical;
- message timing is monotonic and stored in the WebSocket extension while
  reusing M010 timing policy/limits;
- volatile key/accept handshake values are regenerated/ignored as literal match
  authority;
- negotiated WebSocket extensions, including permessage-deflate, are not
  supported in initial M011.

## Initial protocol scope

Target support after M011F qualification:

- RFC 6455 over HTTP/1.1 Upgrade;
- inbound EggReplay `ws://`;
- outbound direct/routed `ws://`;
- outbound `wss://` only if local EggFetch trust-root qualification succeeds;
- semantic message fidelity, not wire fidelity.

Deferred:

- H2 Extended CONNECT WebSockets;
- H3 WebSockets;
- inbound WSS unless separately composed/qualified;
- compression extensions;
- arbitrary scripting/reconnect policy.

## Closure

M011 closes only through M011F after all subplans have closure evidence and one
qualifying hosted matrix supports the declared protocol/support tier.

Final closure:
`plans/closure/m011-websocket-semantic-record-replay.md`.

M012 is ready now that M011's Rust authority is stable.
