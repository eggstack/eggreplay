# M012 — Python Bindings and Pytest/VCR-Style Integration

Status: blocked
Depends on: M011
Roadmap stage: 8

## Objective

Expose EggReplay's existing Rust authorities to Python and pytest without
creating a Python matcher, store, recorder, or HTTP implementation.

## Package topology

Add `crates/eggreplay-python` using PyO3/maturin. Prefer an abi3 build from a
conservative minimum Python version if the selected PyO3 version supports the
required async/runtime interfaces; otherwise publish per-interpreter wheels.

Initial public Python objects should be thin wrappers around Rust concepts:

- Fixture/Session open + validate + inspect;
- Matcher/config profiles;
- replay server lifecycle;
- recorder/gateway lifecycle;
- regression report;
- scenario/record-mode configuration;
- optional WebSocket/timing controls exposed only where the Rust API is already
  stable.

Never deserialize flows into a second Python-only authority for evaluation.

## Runtime model

Use one documented async bridge. Avoid nested ad-hoc Tokio runtimes per call.

Long-running Rust operations release the GIL. Cancellation from asyncio/pytest
must reach Rust lifecycle cancellation and close servers/tasks deterministically.

Provide synchronous convenience only where it can wrap the same authority
without hidden background threads that outlive the Python object.

## Pytest integration

Ship a pytest plugin with explicit fixtures/helpers such as:

- fixture path selection;
- sealed replay server fixture;
- record mode selection matching M009 exactly;
- target/upstream routing configuration;
- update/record-on-miss opt-in;
- assertion/report attachment on failure.

Provide a VCR-style decorator/context manager only as syntax over the same Rust
session/replay machinery.

Default test behavior is sealed/offline. CI must never record/update fixtures
unless the user opts in explicitly.

## Concurrency

Multiple read-only fixture users are safe. Concurrent writers to the same
fixture path must fail clearly or use an explicit single-writer lock; never
silently interleave.

Document pytest-xdist behavior and recommend per-test fixture paths or sealed
shared fixtures.

## Python data contracts

Expose machine reports as typed Python objects plus lossless dict/JSON views.
Preserve ordered duplicate headers/query pairs; do not collapse them into plain
dicts.

Body access must remain explicit and bounded. Large blobs should expose
stream/file-like reads rather than unconditional `bytes`.

## Packaging/CI

Qualify supported CPython versions against current PyO3 support. Target at
minimum:

- Linux x86_64;
- Linux aarch64 when the wheel toolchain is practical;
- macOS arm64 and x86_64/universal strategy as appropriate;
- Windows x86_64.

Do not claim Python 3.15 until the chosen PyO3/maturin toolchain and hosted
tests pass it. Record the exact supported matrix.

Include wheel install smoke tests in clean environments.

## Compatibility tests

Build behavioral tests for common pytest workflows: sync test, asyncio test,
record then sealed replay, append-new explicit update, failure diagnostics,
xdist-safe read-only fixtures, scenario state isolation, and regression report
attachment.

No test should require public Internet.

## Documentation

Add Python quickstart, pytest examples, migration notes from common VCR.py
concepts, and explicit differences (semantic fixture directory, no arbitrary
callbacks/scripts, bounded redaction).

## Closure

Create `plans/closure/m012-python-pytest-ecosystem.md` with wheel matrix,
installation evidence, Python test counts, and proof that Rust remains the
single semantic authority.
