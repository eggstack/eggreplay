# ADR 0007 — Python Binding Authority and Runtime

Status: accepted

## Context

M012 adds a Python package and pytest integration over the Rust implementation
closed through M011. The main risk is semantic duplication: a convenient
Python surface could accidentally become a second matcher, fixture parser,
network client, scenario evaluator, or report authority.

The Python package also needs a clear async/runtime contract. EggReplay already
uses Tokio internally for long-running server, recording, replay, and
regression operations; Python asyncio must adapt to that authority rather than
creating one runtime per call or hiding detached worker threads.

## Decision

The distribution package is named `eggreplay`. Its compiled extension module
is private, `eggreplay._native`, and is implemented by
`crates/eggreplay-python`.

The pure-Python package may provide:

- ergonomic constructors/configuration;
- pytest fixtures/markers;
- context managers/decorators;
- typing stubs and small presentation adapters.

It must not implement independent matching, fixture validation, redaction,
scenario evaluation, HTTP/WebSocket transport, or regression decisions.

Rust remains the only semantic authority.

## Native dependency baseline

M012A qualifies the current compatible line before exposing product APIs:

- PyO3 0.29.x, initially pinning 0.29.2;
- `pyo3-async-runtimes` 0.29.x, initially pinning 0.29.0 with Tokio support;
- maturin 1.14.x, initially pinning the CI/build tool to 1.14.1.

These versions fit EggReplay's Rust 1.89 floor, but support is established only
by EggReplay's own build/import/lifecycle tests.

## Python support tier

Initial product target:

- CPython 3.11–3.14, GIL-enabled builds;
- `abi3-py311` is preferred if all required APIs and async behavior qualify;
- Python 3.15 and free-threaded builds are not claimed until hosted import and
  behavioral tests pass on final supported interpreters;
- PyPy/GraalPy are not part of M012.

If the async bridge or required PyO3 APIs cannot qualify under `abi3-py311`,
M012A may select per-interpreter wheels, but it must record the reason and keep
the same behavior/test contract. Do not silently drop interpreter coverage to
make packaging pass.

## Runtime contract

Use one process-wide Tokio runtime authority through the selected
`pyo3-async-runtimes` Tokio bridge.

- no Tokio runtime per Python method call;
- no background thread per Python object;
- async methods return Python awaitables backed by Rust futures;
- synchronous lifecycle helpers, if provided, use the same runtime authority
  and block with the GIL released;
- Python cancellation must propagate to Rust task/server cancellation;
- object destruction is a fallback, not the normal lifecycle API;
- explicit `close()` / async `aclose()` and context-manager support are
  required for long-lived resources.

## Data contract

Python wrappers must preserve Rust semantics losslessly.

- ordered duplicate headers/query values are sequences of pairs, not dicts;
- reports expose typed objects plus JSON/dict projections derived from the
  Rust report authority;
- large fixture blobs use bounded reader/iterator APIs instead of automatic
  `bytes` materialization;
- redaction markers remain typed and never become recovered secret values;
- WebSocket transcripts/timing/scenarios are exposed only through stable Rust
  types already closed by their milestones.

## Exceptions

Expose a bounded Python exception hierarchy mapped from stable Rust error
categories. Preserve machine-readable category/phase/context fields where
available. Exception messages remain redaction-safe.

Do not expose Rust implementation type names or arbitrary debug strings as the
public exception contract.

## Pytest/update safety

Pytest integration defaults to sealed/offline operation. Missing fixtures,
record-on-miss, append-new, and re-record never become implicit writes.

Any fixture mutation requires explicit test/user configuration and a
single-writer policy. Parallel pytest workers may share read-only fixtures but
must not silently write the same fixture concurrently.

## Consequences

M012 is decomposed into toolchain/scaffold, data bindings, async lifecycle,
pytest ergonomics, wheel packaging, and final qualification. M013 remains
blocked until this Python surface is stable and closed.
