# M014D — gRPC-Aware Views and Bounded Fault-Model Polish Closure

Status: closed

## Qualifying revision and hosted evidence

Implementation commits qualify with Actions run
[36777792500](https://github.com/eggstack/eggreplay/actions/runs/36777792500)
on the standard matrix (all thirteen jobs: Ubuntu stable, Ubuntu Rust
1.89, macOS stable, Windows stable, interception, dependency-boundary,
Python bindings, and the Python abi3 cross-version lanes).
An earlier D-impl push (run `36776162645`) failed for two diagnosed,
fixed causes, recorded for auditability: the Windows H2 regression
compared the volatile auto-`date` header across a second boundary
(fixed by normalizing `date` out of both sides — trailers still compare
exactly), and `prost-reflect` (via `base64`) violated the
`eggreplay-core` dependency boundary (fixed by moving the views to
`eggreplay-http` behind the `grpc` feature; core keeps only the
dependency-free `ScenarioFault`). A lockfile-sync follow-up rode with
the fix (same substance).

Local verification green with the repository-standard command:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Workspace suite at D-impl: 372 tests passed, 0 failed (355 at M014B
plus 7 `grpc` + 1 scenario-fault core tests, 7 `scenario_faults`
replay tests, 1 H2/gRPC view integration test, and 1 `grpc`-feature
lifecycle integration test). The earlier `371+` lower-bound count was
conservative because the lifecycle test had not yet landed under the
`grpc` feature at the time the count was first quoted; the count
above is exact. The M014 umbrella closure run
[36778923619](https://github.com/eggstack/eggreplay/actions/runs/36778923619)
on `c71ffd7` is the final qualifying matrix evidence for the M014
program.

## gRPC view

New `eggreplay-http::grpc` behind the `grpc` cargo feature (pure
projections; raw blobs stay authoritative, no canonical-store change).
It lives in the HTTP adapter crate — not core — so `eggreplay-core`
keeps its dependency boundary (`prost-reflect` pulls `base64`, which
the boundary lane forbids in core; the `grpc` feature also keeps
direct/H1 builds codec-free per the `direct` boundary check):

- 5-byte envelope parsing with closed bounds (16 MiB body, 4096
  frames); truncated/overrun/trailing inputs are errors.
- Ordered `index`/`compressed`/`length` exposure; compressed frames
  never decompressed.
- `grpc-status` (0–16, last wins) with percent-decoded `grpc-message`.
- `decode_grpc_payload` against caller-supplied `FileDescriptorSet`
  only (1 MiB bound, `prost-reflect` canonical JSON, deterministic
  repeated projections); unknown messages and bad payloads fail
  closed. New dependency: `prost-reflect 0.16` (+ `prost`/`prost-types`
  0.14, pure parsing, no sockets — core stays transport-neutral).
- `grpc_view` optional projection with stable JSON; decoded output
  inherits flow redactions.

Covered by core unit tests (ordered/compressed/malformed frames,
trailer status, descriptor decode + determinism + bounds, content-type
gate) and an H2 integration test recording a framed gRPC body with
trailers over experimental H2 (`grpc_view_over_h2_recorded_flow`).

## Fault models

`ScenarioResponse.fault` (`None` for pre-M014D fixtures) with
`ScenarioRules::validate` bounds, applied in replay serving without
holding the state lock across sleeps:

- head delay, chunked body delay (complete normally);
- close-before-response (headers + full declared length, then abort,
  nothing delivered);
- close-after-N (prefix offered under full declared length, then
  abort; never clean);
- transport error (502 `recorded upstream error` projection matching
  recorded-error shape).

Covered by 7 replay integration tests (delays, truncation wire truth
via frame loop, 502 shape, cancellation safety, determinism). No
arbitrary packet/TCP/kernel emulation.

## Reports

Views and fault projections are optional diagnostics over canonical
flow/stream-event data with stable JSON fields; redaction rules apply
via the underlying flows. See `docs/grpc-and-faults.md`.

## Handoff

M014D is closed. With M014A–M014D all closed, the M014 umbrella closes
in `plans/closure/m014-compatibility-program.md`.
