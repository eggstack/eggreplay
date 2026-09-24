# M012C — Python Async Lifecycle and Network Bindings Closure

Status: closed

## Implementation

- Implementation revision: `2998f74f79984e7761186dadeb56c19cc2e8f7f2`.
- Qualification revision: `9c71c367fedb8e3aa45aa68bde10f9d45931bf38`.
- Native runtime remains PyO3 `0.29.2`,
  `pyo3-async-runtimes 0.29.0` with the process-wide Tokio bridge, and
  `abi3-py311`.
- Added async `replay_server`, `recording_gateway`, and `regress_flow`, with an
  owned `Server` handle, `address`, cancellation-safe `aclose`, wait, and async
  context-manager methods. Cleanup runs in a Rust task that survives Python
  waiter cancellation; recording finalization stops admission, drains EggServe,
  and publishes only through `RecordingSession::finish`.
- Replay supports matcher selection, named scenarios, stream timing, and body
  limits. Fixtures requiring WebSocket replay activate the existing Rust
  behavior automatically.
- Recording supports Rust-owned `once`, `append-new`, and `re-record` policy.
  Append-new is HTTP-only and rejects WebSocket acquisition explicitly.
  Re-record uses a sibling staging fixture and transactional replacement.
  Append-new merges per-flow `stream-events` data in `eggreplay-store` while
  preserving fail-closed behavior for unrelated extension conflicts.
- Candidate regression uses EggFetch with direct or Eggress outbound routing,
  Rust comparison policy/report serialization, and the existing semantic
  WebSocket comparison authority. Request and response bodies are bounded.
  Candidate response stream events are compared when requested; candidate
  request cadence is not fabricated because the Rust executor materializes the
  baseline request body.
- Synchronous server helpers are omitted as allowed by this plan. M012D may
  provide a managed synchronous pytest adapter over this shared lifecycle; it
  must not create per-object runtimes or detached server threads.

## Evidence

Local CPython 3.14 x86_64 verification passed:

- maturin installed the `cp311-abi3-macosx_10_12_x86_64` extension;
- Python suite: 19 passed;
- Rust workspace: formatting, check, clippy with warnings denied, and 132 tests
  across 10 suites passed.

Hosted qualification passed on revision
`9c71c367fedb8e3aa45aa68bde10f9d45931bf38`:
[GitHub Actions run 35959113753](https://github.com/eggstack/eggreplay/actions/runs/35959113753).
All jobs passed: Rust stable on Ubuntu, macOS, and Windows; Rust 1.89 on
Ubuntu; dependency-boundary checks; Python bindings on Ubuntu CPython
3.11/Rust 1.89 and CPython 3.14/stable Rust, macOS CPython 3.11, Windows
CPython 3.11; and same-wheel CPython 3.11 to 3.14 ABI reuse. Each Python lane
passed all 19 tests. Wheel and source archive content scans passed.

## Limits and handoff

`regress_flow` compares one stored flow per call. Networked recording requires
an explicit upstream. WebSocket capture is opt-in and is unavailable in
append-new. The synchronous pytest adapter, fixture locking, and pytest/VCR
ergonomics belong to M012D. M012D is ready.
