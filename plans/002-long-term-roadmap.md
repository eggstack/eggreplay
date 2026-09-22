# 002 — Long-Term Roadmap

Status: canonical

## Stage 1 — Foundation (M001–M002)

Workspace/contracts and the canonical flow/session + `.eggr` store.

## Stage 2 — Record and replay (M003–M005)

EggFetch recording, EggServe offline replay, deterministic matching,
consumption, and near-miss diagnostics.

## Stage 3 — Regression product (M006–M007)

Candidate replay/diff plus optional Eggress routing and CLI machine contracts.

## Stage 4 — v0.1 hardening (M008 + C001–C006)

Security/redaction, streaming/concurrency corrections, CLI/routing correction,
expanded qualification, and hosted Linux/macOS/Windows/MSRV closure.

Outcome: v0.1 hosted release qualification closed.

## Stage 5 — Stateful/dynamic replay (M009)

Record modes analogous to sealed/offline, once, append-new, and re-record;
explicit pass-through/record-on-miss; authored state-machine scenarios;
variable extraction; deterministic templates; bounded transforms. Arbitrary
scripting remains out of scope.

Executable plan:
`implementation/stateful/009-stateful-dynamic-replay.md`.

## Stage 6 — Streaming semantics (M010)

Optional stream-event timing, immediate/recorded/scaled replay, mid-body
failures, SSE-aware views/diffing, and concurrency timeline replay.

Executable plan:
`implementation/streaming/010-streaming-timing-and-sse.md`.

## Stage 7 — WebSockets (M011)

Use EggFetch upgraded streams and EggServe tunnel handoff. Preserve the
initiating HTTP flow and attach ordered semantic messages. No wire-perfect
claim.

Executable plan:
`implementation/websocket/011-websocket-semantic-record-replay.md`.

## Stage 8 — Python/test ecosystem (M012)

Thin PyO3 bindings over Rust authorities plus pytest/VCR-style fixture helpers.
No second matcher/store/network implementation.

Executable plan:
`implementation/python/012-python-pytest-ecosystem.md`.

## Stage 9 — Optional interception (M013)

Separate explicit-proxy acquisition and opt-in HTTPS MITM boundary with
dedicated CA/key lifecycle. Transparent/TUN/WireGuard remains a future
decision.

Executable plan:
`implementation/interception/013-explicit-proxy-and-optional-mitm.md`.

## Stage 10 — broader compatibility (M014)

Umbrella program split into bounded tracks:

- M014A HAR import/export + fixture migration;
- M014B HTTP/2 qualification;
- M014C HTTP/3 feasibility/qualification;
- M014D gRPC-aware views + bounded fault-model polish.

See `implementation/compatibility/`.

## Roadmap rule

Later stages may not inflate earlier dependency closure. Every support claim
requires EggReplay repository evidence; sibling-project capability or roadmap
language alone is not support evidence.
