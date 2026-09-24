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

## Post-v0.1 milestone discipline

M009 and later milestones use the same evidence rule as v0.1: implementation
presence is not support evidence. A decomposed milestone closes only after its
subplans have closure records and the final gate records one qualifying
implementation SHA, hosted platform/MSRV results, exact dependency versions,
resource/security evidence, and an explicit support/limitation matrix.

M011 is closed by its hosted M011F/umbrella evidence.

For M012, M012F owns final Python support claims. Neither a successful
`maturin develop` nor a built wheel is enough. Closure requires clean-wheel
installation, actual interpreter/platform execution, asyncio/cancellation
tests, pytest behavior, Rust authority boundary checks, and exact wheel/ABI
metadata. Cross-compiled artifacts without runtime execution are build
evidence only.


For M013, ordinary functional green CI is not sufficient. M013F must record:

- interception feature remains absent from default dependency/Python graphs;
- open-proxy/SSRF/authority-coherence policy;
- CA/private-key non-leak sentinel scans;
- upstream TLS verification failure proof;
- direct and Eggress-routed CONNECT/MITM;
- local independent client interoperability;
- Unix private-key permission evidence and truthful Windows behavior;
- resource/cache/tunnel bounds;
- exact protocol exclusions;
- hosted Linux/macOS/Windows/Rust-1.89 results.

No test may mutate the system/browser trust store or require public Internet.
