# M012B — Python Fixture, Report, and Data Bindings Closure

Status: closed

## Implementation

- Qualifying revision: `486de4be8f93b283576eb5b3cc8298a845178a4f`
- Native package/runtime/toolchain: `eggreplay._native`, PyO3 `0.29.2`,
  `pyo3-async-runtimes 0.29.0` (Tokio), maturin `1.14.1`, pytest `8.4.2`,
  pytest-asyncio `1.2.0`, uv `0.8.22`; GIL-enabled `abi3-py311`.
- Added Rust-backed fixture/session, flow/request/response/error and extension
  summaries, bounded streaming `BodyReader`, typed regression report and
  findings, stable exception mapping, and configuration wrappers. Duplicate
  headers and query pairs preserve order. Route validation uses the existing
  Eggress route parser. Child views retain the session owner.
- Body reads are bounded per operation; `read_all` has a default cap. SHA-256
  verification and path/symlink validation remain in `eggreplay-store`.
- Store test fixtures are excluded from packaged source artifacts.

## Evidence

Hosted qualification passed on revision
`486de4be8f93b283576eb5b3cc8298a845178a4f`:
[GitHub Actions run 35956198913](https://github.com/eggstack/eggreplay/actions/runs/35956198913).
All jobs passed: Rust stable on Ubuntu, macOS, and Windows; Rust 1.89 on
Ubuntu; Python bindings on Ubuntu CPython 3.11/Rust 1.89 and CPython
3.14/stable Rust, macOS CPython 3.11, and Windows CPython 3.11; dependency
boundary checks; and same-wheel CPython 3.11 to 3.14 ABI reuse. The Python
pytest step passed on all four binding lanes. Wheel and source archive scans
passed on the binding jobs.

The workspace matrix passed formatting, locked check, clippy with warnings
denied, and all workspace tests. A separate local CPython 3.14 run passed 13
tests and skipped one symlink test because symlink creation is unavailable in
that local environment; hosted Unix and Windows binding jobs passed the suite.

## Contract and limits

Python projects Rust-owned fixture parsing, validation, hashing, report
serialization, and configuration validation. Body materialization stays
explicit and capped. Extension data is exposed as bounded summaries. No
network lifecycle bindings are part of this milestone; those belong to M012C.

M012C is ready.
