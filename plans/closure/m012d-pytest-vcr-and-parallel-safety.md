# M012D — Pytest/VCR Ergonomics and Parallel Safety Closure

Status: closed

## Implementation

- Implementation revisions: `bbe152cff52d2d333485ba5281d885d7fc068cad`,
  `a427cdf`, and qualifying revision
  `21aa5aefc6fb3f5b6d4c1b71142fd084e0caa00d`.
- Registered `eggreplay.pytest_plugin` using the `pytest11` entry point. The
  canonical fixtures are `eggreplay_fixture`, `eggreplay_server`,
  `eggreplay_async_server`, `eggreplay_recorder`,
  `eggreplay_async_recorder`, and `eggreplay_report`.
- Replay is sealed by default. A missing fixture fails setup without creating
  files. Mutation requires an explicit `once`, `append-new`, or `re-record`
  choice plus explicit upstream configuration for networked modes. Generic
  pytest `--update` does not grant write access. WebSocket append-new fails
  with the Rust policy's explicit limitation.
- Relative fixture paths resolve from pytest's root. Decorator and sync/async
  context-manager ergonomics use the Rust server lifecycle; they do not patch
  Python clients or introduce a cassette format. Synchronous teardown uses one
  Python `asyncio.Runner` over the process Rust runtime bridge.
- Writers acquire an atomic sibling lock with PID/worker metadata. Conflicting
  writers fail deterministically, stale locks require explicit operator
  recovery, independent fixture paths can be written independently, and
  teardown releases owned locks. Read-only fixtures support simultaneous
  replay servers.
- Regression assertion output is bounded and omits baseline/candidate values;
  the structured Rust `RegressionReport` remains attached to the assertion.
  Each replay server has its own scenario runtime.
- An intermediate subprocess-based parallel reader check failed in the
  Windows hosted lane. It was replaced with a portable simultaneous
  read-only server check. The cancellation finalization test also replaced a
  100 ms timing assumption with a bounded wait for published fixture state.

## Evidence

Local CPython 3.14 x86_64 verification passed:

- `maturin develop --target x86_64-apple-darwin` installed the
  `cp311-abi3-macosx_10_12_x86_64` extension;
- Python suite: 29 passed;
- Rust workspace: formatting, check, clippy with warnings denied, and 132
  tests across 10 suites passed;
- `uv lock --check` and `git diff --check` passed.

Hosted qualification passed on revision
`21aa5aefc6fb3f5b6d4c1b71142fd084e0caa00d`:
[GitHub Actions run 35963105100](https://github.com/eggstack/eggreplay/actions/runs/35963105100).
All jobs passed: Ubuntu stable Rust, Ubuntu Rust 1.89, macOS stable Rust,
Windows stable Rust, dependency-boundary checks, Ubuntu CPython 3.11 and 3.14,
macOS CPython 3.11, Windows CPython 3.11, and same-wheel CPython 3.11-to-3.14
ABI reuse. Python lanes passed all 29 tests. The hosted Windows lane includes
the explicit record-mode, teardown, lock, reporting, and concurrent replay
checks.

## Limits and handoff

Node-id fixture derivation is intentionally not provided; users supply fixture
paths explicitly. Read-only parallel safety is qualified using simultaneous
managed server instances; no xdist-specific dependency or hidden path naming
convention is introduced. M012E is ready for wheel, typing, and clean-install
qualification.
