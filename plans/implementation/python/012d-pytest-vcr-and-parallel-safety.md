# M012D — Pytest/VCR Ergonomics and Parallel Safety

Status: blocked
Depends on: M012C
Parent milestone: M012

## Objective

Add pytest-native ergonomics and VCR-style syntax over the Rust bindings while
making accidental fixture mutation impossible by default.

## A. Pytest plugin registration

Ship `eggreplay.pytest_plugin` and register it through the package's
`pytest11` entry point.

Provide narrowly named fixtures/helpers such as:

- `eggreplay_fixture`;
- `eggreplay_server`;
- `eggreplay_recorder`;
- report/assertion helper.

Exact public names may be refined before closure, but one canonical set must be
documented and typed.

## B. Sealed default

Default behavior is always sealed/read-only.

- missing fixture => test/setup failure;
- mismatch => assertion/report failure;
- no implicit record-on-miss;
- no implicit fixture creation;
- no writes because CI environment variables happen to be present.

Mutation requires an explicit user action such as:

- `--eggreplay-record-mode=once|append-new|re-record`, or
- an explicit marker/decorator argument.

A generic `--update` from another plugin must not enable EggReplay writes.

## C. Fixture path policy

Support explicit fixture paths first.

Optional node-id-derived paths must be:

- deterministic;
- normalized and path-confined;
- insensitive to worker scheduling;
- documented for parametrized tests;
- collision-tested.

Never use current working directory surprises as hidden fixture authority.

## D. VCR-style syntax

Provide decorator/context-manager syntax only as ergonomic sugar, for example:

```python
@eggreplay.use_fixture("fixtures/example.eggr")
def test_example():
    ...
```

and a context-manager equivalent.

These wrappers call the same Rust server/recording lifecycle. They do not
monkeypatch requests/httpx/socket globally and do not implement a Python
cassette format.

## E. Async and sync pytest

Support ordinary synchronous tests and pytest-asyncio tests.

Sync support must use the qualified M012C runtime/lifecycle adapter and must
not leave a server thread/task alive after fixture teardown.

Async fixtures must respect the test's running event loop and propagate
cancellation.

## F. Parallel/xdist safety

Read-only workers may share one fixture.

For writers:

- default to refusing multiple writers to the same fixture path;
- acquire a cross-process sibling lock using atomic create-new semantics in the
  Rust/Python binding layer;
- lock metadata may include PID/worker id for diagnostics but no secrets;
- do not silently break stale locks;
- provide an explicit documented recovery path;
- recommend per-worker/per-test fixture destinations for bulk recording.

A crash must not leave a partially valid fixture; existing store transaction
rules remain authoritative.

## G. Failure/report integration

On mismatch:

- raise/assert with bounded human diagnostics;
- attach the underlying Rust `RegressionReport` to pytest reporting where
  practical;
- do not print secret payloads;
- preserve machine JSON as an explicit artifact/helper rather than
  re-evaluating the result in Python.

## H. Scenario isolation

Every pytest replay fixture gets its own scenario runtime unless explicitly
session-scoped by the user. Parallel tests must not share mutable scenario
state accidentally.

## Required tests

Use pytest itself as the behavioral harness:

- sync sealed replay;
- asyncio sealed replay;
- missing fixture fails without write;
- explicit once recording;
- explicit append-new HTTP update;
- explicit re-record;
- WebSocket recording limitation surfaced;
- VCR decorator/context manager parity;
- xdist/shared read-only;
- two writers same fixture => deterministic refusal;
- per-worker writers succeed on independent paths;
- scenario state isolation;
- report/redaction-safe failure output;
- teardown after failed/cancelled test.

No public Internet.

## Closure

Create `plans/closure/m012d-pytest-vcr-and-parallel-safety.md`.
M012E becomes ready only after the plugin/update contract is stable.
