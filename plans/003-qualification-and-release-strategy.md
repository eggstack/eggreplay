# 003 — Qualification and Release Strategy

Status: canonical

## Toolchain

- Rust MSRV 1.89.
- Resolver 3 unless an integration proves a blocker.
- `unsafe_code = "forbid"` in EggReplay-owned crates absent a separate ADR.
- Linux, macOS, and Windows release targets; Linux aarch64/SBC targeted when binary releases begin.

## Routine CI

Every change: fmt, workspace check, clippy/all-targets/all-features, unit/integration tests, minimum-feature checks, schema/golden tests, and local-only network tests.

## Release integration evidence

- EggFetch local-origin recording and failure mapping.
- EggServe gateway/replay lifecycle.
- Eggress direct + at least one routed integration when enabled.
- Large streamed request/response with bounded memory.
- Request/response trailers.
- Cancellation and partial-body behavior.
- Fixture migration/version handling.
- CLI JSON snapshots and exit-code contract.

External tools such as VCR.py, WireMock, and mitmproxy may be used as behavior oracles, never runtime dependencies.

## Required v0.1 scenarios

GET; binary POST; repeated headers/query keys; multiple Set-Cookie; trailers; HEAD/204 suppression; unknown-length streaming; large upload/download; connection refusal; classifiable DNS failure; TLS verification failure; response-head timeout; mid-body cancellation; repeated request consumption; strict mismatch; practical volatile-header match; semantic JSON match; sensitive-header redaction; configured query/JSON redaction; direct replay; Eggress-routed replay; target remap; status/body/header regression; corrupt blob detection.

## Performance policy

Benchmarks are decision evidence, not universal claims. Explicit limits are required for body bytes, metadata counts, matcher candidates, diagnostic output, and concurrency. Large-body tests must demonstrate streaming rather than only completion.

## Release gate

M008 must create a closure record with implementation commits, exact verification results, protocol/platform support matrix, schema version, dependency versions, known limitations, and deferred work. Source presence alone never establishes support.
