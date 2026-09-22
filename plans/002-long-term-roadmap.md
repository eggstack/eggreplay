# 002 — Long-Term Roadmap

Status: canonical

## Stage 1 — Foundation (M001–M002)

M001 establishes the Rust workspace, crate boundaries, repository policy, error conventions, configuration skeleton, and deterministic test fixtures.

M002 implements the canonical flow/session model plus `.eggr` storage with schema round trips, content-addressed blobs, streaming writes, integrity validation, and crash-safe finalization.

Outcome: interactions can be represented and persisted without a network runtime.

## Stage 2 — Record and replay (M003–M005)

M003 builds EggFetch-backed outbound observation plus an EggServe-backed gateway recorder. M004 builds the EggServe-backed offline replay endpoint. M005 adds strict/practical normalization and matching, deterministic repeated-call consumption, and near-miss diagnostics.

Outcome: record a real local interaction and replay it offline.

## Stage 3 — Regression product (M006–M007)

M006 adds request replay against a candidate, candidate observation capture, semantic diffing, timing assertions, and report models. M007 adds optional Eggress routing plus the main CLI and JSON/JUnit contracts.

Outcome: the same fixture drives mocks and live network regression tests directly or through a route.

## Stage 4 — v0.1 hardening (M008)

Security/redaction review, schema compatibility fixtures, memory/streaming bounds, MSRV/platform CI, packaging, documentation, and closure evidence.

## Stage 5 — Stateful/dynamic replay (M009)

Record modes analogous to sealed/offline, once, append-new, and re-record; explicit pass-through/record-on-miss; authored state-machine scenarios; variable extraction; deterministic templates; bounded transforms. Arbitrary scripting remains out of scope without a new ADR.

## Stage 6 — Streaming semantics (M010)

Optional event timing, replay timing modes (`immediate`, `recorded`, scaled), mid-body failures, SSE-aware inspection/diffing, and concurrency timeline replay. Recorded timing is never the ordinary-test default.

## Stage 7 — WebSockets (M011)

Use EggFetch upgrade streams and EggServe tunnel handoff. Preserve the initiating HTTP flow and attach ordered messages with direction, message type/opcode, payload reference, and relative time. No wire-perfect claim.

## Stage 8 — Python/test ecosystem (M012)

Thin PyO3 bindings over Rust authorities, then pytest/VCR-style fixture helpers. No second matcher/store/network implementation.

## Stage 9 — Optional interception (M013)

Explicit HTTP proxy acquisition and optional HTTPS MITM with separate CA/key lifecycle, ALPN/SNI handling, certificate-pinning documentation, and a separate feature/crate boundary. Transparent/TUN/WireGuard remains a separate future decision.

## Stage 10 — broader compatibility (M014)

Evaluate HAR import/export, H2/H3 qualification, gRPC-aware views, fixture migration tooling, and additional fault models.

## Roadmap rule

Later stages may not inflate v0.1's dependency closure. Every support claim requires repository evidence; roadmap language is intent, not evidence.
