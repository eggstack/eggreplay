> Deep dive for [overview](overview.md).

# Python bindings

`eggreplay-python` is a ~1.6k-line PyO3 `cdylib` leaf crate plus a small
pure-Python package. It adds no semantics: fixture parsing, matching, record
policy, redaction, transport, and regression decisions all stay in the Rust
authorities described in [03](03-store-persistence.md),
[05](05-http-replay-and-serving.md), and
[06](06-regression-and-reporting.md). What the binding adds is a second *call
surface* onto those authorities, plus the pytest ergonomics that make them
usable from a Python test suite.

The governing document is
[ADR 0007 — Python Binding Authority and Runtime](../plans/adrs/0007-python-binding-authority-and-runtime.md),
which names the risk as "semantic duplication" (`Context`) and states the rule
directly: the pure-Python layer "must not implement independent matching,
fixture validation, redaction, scenario evaluation, HTTP/WebSocket transport, or
regression decisions" (`Decision`).

## Crate contract and leaf status

| Property | Value | Source |
| --- | --- | --- |
| Lib name | `_native` | `crates/eggreplay-python/Cargo.toml:11` |
| Crate type | `["cdylib"]` | `Cargo.toml:12` |
| Cargo tests | disabled (`test = false`) | `Cargo.toml:13` |
| Doctests | disabled (`doctest = false`) | `Cargo.toml:14` |
| PyO3 | `=0.29.2`, `extension-module` + `abi3-py311` | `Cargo.toml:23` |
| Async bridge | `pyo3-async-runtimes =0.29.0`, `tokio-runtime` | `Cargo.toml:24` |
| HTTP features | `default-features = false`, `direct`, `eggserve`, `eggress`, `websocket` | `Cargo.toml:18` |

`cdylib` plus `test = false` is the leaf marker: there is no Rust rlib for a
downstream Rust crate to link, and the crate's behaviour is not exercised by
`cargo test`. Its contract is instead the Python suite
(`crates/eggreplay-python/tests/test_preflight.py`) and hosted wheel smokes. The
Python binding lane therefore runs `maturin develop` and
`python -m pytest tests -q` with pinned tooling rather than the workspace test
command (`.github/workflows/ci.yml:61-62`), across Ubuntu 3.11 / 3.14, macOS
3.11, and Windows 3.11 (`.github/workflows/ci.yml:36-48`).

The leaf status is enforced in both directions by one CI step, "Ensure semantic
crates remain Python-free" (`.github/workflows/ci.yml:63-75`):

| Direction | Check | Failure message |
| --- | --- | --- |
| Product crates must not pull PyO3 | `cargo tree -p <core\|store\|http>` contains no `pyo3`/`pyo3-async-runtimes` | "depends on Python" |
| The Python crate must not be depended on | `cargo tree --workspace --invert eggreplay-python` lists no `eggreplay-core\|store\|http\|cli` | "Rust product crates must not depend on the Python extension" |

The second check is the one that keeps `eggreplay-python` a leaf: if the CLI or
any adapter ever depended on it, the inverted tree would name it and the lane
fails. `eggreplay-python` is also enumerated in the interception-exclusion loop
(`.github/workflows/ci.yml:147`) alongside core/store/http/cli.

The ABI is `abi3-py311`, so one binary serves CPython 3.11 and every later
GIL-enabled 3.x. The `python-abi3-cross-version` lane builds the wheel under
3.11 and installs *the same file* under 3.14, then runs the Python suite against
it (`.github/workflows/ci.yml:105-124`). ADR 0007 (`Python support tier`) is
explicit that dropping interpreter coverage to make packaging pass is not
allowed, and that 3.15/free-threaded/PyPy remain unclaimed.

## Two-layer structure

| Layer | Files | Owns |
| --- | --- | --- |
| Native extension `eggreplay._native` | `src/lib.rs`, `config.rs`, `errors.rs`, `fixture.rs`, `lifecycle.rs`, `report.rs` | All Rust authority calls, GIL handling, tokio bridge, exception construction |
| Pure Python `eggreplay` | `python/eggreplay/__init__.py`, `_lifecycle.py` | Enum/typing ergonomics, sync/async context managers, mode dispatch, `__all__` |
| Pure Python `eggreplay.pytest_plugin` | `python/eggreplay/pytest_plugin.py` | CLI options, markers, fixtures, writer lock, assertion presentation |

The split is what lets the Rust side stay small. `__init__.py` does no
semantics: it re-exports native names, builds four `str`-backed `Enum`s from
the Rust-serialized member values (`__init__.py:41-48`), and wraps
`validate_record_mode` in a typed `record_policy` helper
(`__init__.py:56-62`). `_lifecycle.py` owns the sync/async decision and the
"managed lifecycle" plumbing; `pytest_plugin.py` owns pytest's option and marker
surface plus the single-writer lock. Neither re-derives a policy — each calls
back into `record_policy`, which calls `eggreplay_core::RecordMode::resolve`.

Typing is declared, not inferred from the extension. `py.typed` marks the
package as typed for downstream type checkers, and `__init__.pyi` /
`pytest_plugin.pyi` describe the runtime surface (including the native classes
and Rust enum member names). `test_stub_manifest_matches_runtime_exports_and_rust_enum_names`
parses both stubs with `ast` and asserts that `__all__` names are declared and
that stub enum members equal the runtime `__members__`
(`tests/test_preflight.py:21-67`), so a Rust enum rename cannot silently drift
from the stub.

## Module registration

`#[pymodule] fn _native` (`src/lib.rs:34-71`) registers, in order:

- Functions: `version()` (`lib.rs:13`, `env!("CARGO_PKG_VERSION")`),
  `async_value()`, `async_sleep()` (`lib.rs:19-31`).
- `config::register` (`src/config.rs:328-339`): `enum_values`,
  `validate_matcher_profile`, `validate_consumption_mode`, `validate_record_mode`,
  `validate_stream_timing`, plus classes `RedactionConfig`, `StreamTimingMode`,
  `ComparisonPolicy`, `RouteSpecification`, `WebSocketOptions`.
- `lifecycle::register` (`src/lifecycle.rs:613-618`): class `Server`, functions
  `replay_server`, `recording_gateway`, `regress_flow`.
- Fixture views (`src/fixture.rs`): `PyFixture`→`Fixture`, `FlowView`→`Flow`,
  `RequestView`→`Request`, `ResponseView`→`Response`,
  `FlowErrorView`→`FlowErrorInfo`, `PyFlowIterator`→`FlowIterator`,
  `BodyReader`.
- Exceptions: `EggReplayError`, `FixtureError`, `MatchError`, `NetworkError`,
  `ConfigurationError` (`lib.rs:52-68`), plus `RegressionError` which
  `report::register` adds alongside the report classes (`src/report.rs:98-102`).
- `report::register`: classes `DiffFinding` and `RegressionReport`.

Two registration details matter. The exceptions form a hierarchy rooted at
`pyo3::exceptions::PyException` with `EggReplayError` as the only intermediate
node (`src/errors.rs:3-8`), so a caller can catch one class. And `MatchError` is
registered but never raised anywhere in this crate; it exists so a Python
`except` clause for a match failure has a stable type rather than borrowing a
transport or fixture class.

`FlowIterator` is a native class but is not re-exported from `__init__.py`; it
is reachable as `eggreplay._native.FlowIterator` and is declared in
`__init__.pyi:80-82` as the return type of `Fixture.iter_flows`.

## Fixture views (fixture.rs)

`fixture.rs` is a read model, not a second parser. `Fixture::new` opens the
`.eggr` directory through the same `eggreplay_store::Session::open` with
`StoreLimits::default()` that the Rust replay path uses
(`src/fixture.rs:353-362`), and every projection reuses the core types
(`Flow`, `HttpRequest`, `HttpResponse`, `FlowError`) rather than reinterpreting
JSON.

| View | Surface | Notes |
| --- | --- | --- |
| `Fixture` | `manifest`, `extension_metadata`, `websocket_summary()`, `scenario_summary()`, `stream_summary()`, `iter_flows()`, `flow(id)`, `open_body(flow_id, side)` | `src/fixture.rs:351-477` |
| `Flow` | `id`, `request`, `response`, `error`, `outcome`, `to_dict()` | `fixture.rs:128-174` |
| `Request` | `method`, `scheme`, `authority`, `path`, `query`, `headers`, `to_dict()` | `fixture.rs:33-70` |
| `Response` | `status`, `headers`, `to_dict()` | `fixture.rs:78-95` |
| `FlowErrorInfo` | `category`, `phase`, `message`, `to_dict()` | `fixture.rs:103-126` |
| `BodyReader` | `length`, `sha256`, `read(size)`, `read_all(max_bytes)`, `close()`, iteration | `fixture.rs:239-323` |

Laziness follows ADR 0007's data contract ("large fixture blobs use bounded
reader/iterator APIs instead of automatic `bytes` materialization"). Flow
iteration is a single ordered pass: `iter_flows` returns a `FlowIterator` holding
the store's `FlowIter` plus its own session reference, and each `__next__`
advances one flow under `py.detach` (`fixture.rs:326-343`). Bodies are never
materialized by iteration — `open_body` is a separate call.

`BodyReader` is the bounded, integrity-checking reader. It holds the `Arc<Session>`
and a file handle, hashes incrementally with SHA-256, and refuses to return the
final chunk until the running digest matches the content-addressed digest
(`fixture.rs:205-236`); a mismatch surfaces as `StoreError::Integrity`. Bounds:
`MAX_BODY_CHUNK = 8 MiB` per read (`fixture.rs:13`, enforced at
`fixture.rs:209-213`), `read_all` requires an unread body and defaults to a 1 MiB
cap (`fixture.rs:261-267`), and iteration yields 64 KiB chunks
(`fixture.rs:306`). Because the reader owns an `Arc` clone, it outlives its
parent `Fixture`; `test_repeated_fixture_children_survive_python_gc` and
`test_repeated_async_server_create_close_is_bounded` pin that
(`tests/test_preflight.py:314-324`, `992-1003`).

Stored bodies are already redacted. The binding does not redact on read; it
never gets unredacted bytes to redact. `FixtureError`'s message is the fixed
string "fixture is invalid or unreadable" (`src/errors.rs:10-12`) and ignores
the `Display` of the underlying `StoreError`, which
`test_corrupt_fixture_errors_are_redacted_and_body_digest_is_checked` asserts by
planting sentinel bytes and digest material and requiring neither to appear
(`tests/test_preflight.py:252-262`). `open_body` also requires a real
`BodyRef::Blob` (an inline/absent body is `FixtureError`) and rejects a
response body on an error outcome (`fixture.rs:454-468`).

Ordered duplicates are preserved as pair sequences, never dicts, per ADR 0007's
data contract; `test_fixture_preserves_ordered_duplicates_and_child_lifetime`
asserts both the tuple projections and the `to_dict()` JSON form
(`tests/test_preflight.py:175-196`).

## Async lifecycle (lifecycle.rs, _lifecycle.py)

Every async entry point returns a Python awaitable backed by a Rust future via
`pyo3_async_runtimes::tokio::future_into_py` — the process-wide Tokio bridge
required by ADR 0007's `Runtime contract` ("no Tokio runtime per Python method
call; no background thread per Python object"). `replay_server`,
`recording_gateway`, and `regress_flow` each end in one
`future_into_py(py, async move { ... })` (`src/lifecycle.rs:197`, `260`, `400`).
There is no `Runtime::new()` anywhere in the crate, and the only explicit spawn
uses the shared handle: `pyo3_async_runtimes::tokio::get_runtime().spawn(...)`
(`lifecycle.rs:92`).

The sync/async split lives in the Python layer. The native surface is async only
(M012C recorded "Synchronous server helpers are omitted as allowed by this
plan"), and `_lifecycle.py` / `pytest_plugin.py` adapt with one Python
`asyncio.Runner` per call site around the *same* process runtime:
`_FixtureContext.__enter__` creates a `Runner`, runs `open_server`, and closes it
in `__exit__` (`_lifecycle.py:77-97`); `eggreplay_server` does the same inside a
`with asyncio.Runner()` block (`pytest_plugin.py:235-246`). The README states
the rule plainly: sync use "manages one Python `asyncio.Runner` around the shared
process Tokio bridge; it does not create a Rust runtime or detached server
thread per object".

`Server` is the managed lifecycle object. It owns the EggServe `ServerHandle`,
the `Finalization` recipe, a `closing` flag, and a `watch` channel that
publishes the cleanup result (`lifecycle.rs:42-49`). `is_closing()` reports the
flag; `wait()` awaits the channel without stopping anything
(`lifecycle.rs:141-143`); `aclose()` is idempotent via an `AcqRel` swap on that
flag (`lifecycle.rs:78`). `__aenter__`/`__aexit__` map to the same two
operations (`lifecycle.rs:145-158`).

Cancellation semantics are deliberately asymmetric. If the *Python waiter* is
cancelled, the Rust cleanup task is not: `aclose` spawns it on the runtime and
observes it only through a `watch::Receiver` (`lifecycle.rs:91-137`), so the
`Result` in the channel is retained for a later `wait()`. The opposite holds for
ordinary futures — dropping the awaitable drops the Rust future, which is what
`test_async_cancellation` requires of `async_sleep` and
`test_candidate_cancellation_closes_pending_network_request` requires of a stalled
candidate request (`tests/test_preflight.py:77-88`, `774-808`).

Recording finalization is where that asymmetry is load-bearing. `Finalization`
encodes three recipes (`lifecycle.rs:23-38`): `Once` finishes the session;
`ReRecord` finishes then transactionally replaces the target from a staging
fixture; `Append` finishes, merges source + additional into a combined fixture
via `Session::merge_to`, replaces, and removes the temporary
(`lifecycle.rs:98-130`). Replacement is rename-based with a rollback rename
(`lifecycle.rs:581-592`), and staging paths are nonce- and PID-suffixed
siblings (`lifecycle.rs:569-579`) so a crashed run leaves debris rather than a
half-published target. Ordering is explicit: `handle.shutdown()` then
`handle.wait()` *before* `finish_session`, because finalization must not race
EggServe's tracked tunnel tasks (the comment at `lifecycle.rs:554-561`).

The GIL is never held across blocking work. Fixture and blob reads use
`py.detach` (`fixture.rs:357`, `470`, `252`), and `regress_flow` moves the whole
baseline read — flow lookup, bounded blob reads, `stream-events` and
`websocket-messages` extension parsing — onto `tokio::task::spawn_blocking`
(`lifecycle.rs:400-479`). Cancellation of the Python waiter at that point still
tears down the in-flight HTTP request, because the executor work is an ordinary
Rust future.

`open_server` in `_lifecycle.py` is the mode dispatcher, and it is where the
"no accidental writes" rule is expressed in Python: no `record_mode` and no
WebSockets means `replay_server` only; a sealed policy also means replay only;
any other resolved mode requires an explicit `upstream` or it raises
`ValueError` (`_lifecycle.py:39-59`).

## Error mapping (errors.rs)

`errors.rs` is twelve lines and deliberately contains no Rust error translation
table. It creates the hierarchy and one redacting constructor:

```text
PyException
└── EggReplayError
    ├── FixtureError
    ├── MatchError
    ├── NetworkError
    ├── ConfigurationError
    └── RegressionError
```

The mapping discipline is *one exception per failure source, message
redaction by omission*. `fixture_error(_: impl Display)` accepts the underlying
`StoreError` and discards its text entirely
(`src/errors.rs:10-12`), so the same fixed message covers "unreadable",
"digest mismatch", and "closed reader" without ever formatting Rust internals
into Python. This satisfies ADR 0007's `Exceptions` rule that messages stay
redaction-safe and that Rust type/debug strings stay out of the public contract.

The category taxonomy in `eggreplay-core` is a separate axis and is exposed as
*data*, not as exception types. `ErrorPhase` (`request`, `connect`, `tls`,
`headers`, `body`, `timeout`, `cancelled`, `policy`, `other`) and `ErrorCategory`
(`dns`, `connection-refused`, `unreachable`, `tls`, `protocol`, `policy`,
`timeout`, `cancelled`, `other`) are stable snake_case enums
(`crates/eggreplay-core/src/error.rs:9-52`), and `FlowError` is the persistable
pair with a sanitized message (`error.rs:57-64`). `FlowErrorInfo` surfaces those
three fields as strings built by the same serde path that wrote them
(`fixture.rs:106-122`), and the round trip is asserted end-to-end in
`test_flow_error_keeps_rust_category_phase_and_provenance`
(`tests/test_preflight.py:199-204`).

So the selection rule works like this: the *category* of a failure decides which
exception type a call raises — configuration-shaped input is always
`ConfigurationError` (unknown enum, bad route, unparseable bind/timing, invalid
comparison policy), anything touching the store or fixture bytes is always
`FixtureError`, socket/accept failures are `NetworkError`, and the regression
path is `RegressionError`. The `ErrorPhase` × `ErrorCategory` pair is what
crosses the boundary *inside* a recorded flow, where no exception is involved.
The same two axes are reused by the CLI, which maps a failure class to a process
exit code (`crates/eggreplay-cli/src/main.rs:662-670`); the Python layer has no
exit code, and asserting a value is `RegressionAssertions`' job.

## Configuration and reports (config.rs, report.rs)

`config.rs` exposes Rust validation as *round-trippable* helpers rather than
hand-maintained Python tables. Each `validate_*` parses the string through
`serde_json` into the real `eggreplay_core` type, so the accepted set is the
Rust enum, not a Python list that can drift
(`config.rs:9-51`). `enum_values("matcher_profile")` and friends serialize the
Rust variants to read back the `kind` tag
(`config.rs:54-90`) — that serialization is what `__init__.py:41-48` turns into
the runtime `Enum` members, so adding a Rust variant surfaces in Python without a
stub edit. `validate_record_mode` returns the *resolved* policy, not the input
string, by calling `mode.resolve(fixture_exists, upstream_configured)`
(`config.rs:27-40`).

`StreamTimingMode` normalizes `Scaled(factor)` to `scaled:{factor}` in both
directions (`config.rs:43-51`, `108-114`). `RedactionConfig` starts from
`RedactionConfig::default_secure()` and only ever *narrows* the selector sets
(`config.rs:132-144`), so omitting arguments cannot weaken redaction; the test
asserts `authorization` is in the default header set
(`tests/test_preflight.py:353`). `ComparisonPolicy` runs `validate()` inside its
constructor (`config.rs:187`) and `WebSocketOptions` rejects a zero cadence
tolerance (`config.rs:298-302`).

`RouteSpecification` enforces mutually exclusive fields — direct takes no
target, Eggress requires a non-blank one that `eggreplay_http::parse_route`
accepts (`config.rs:224-253`) — and redacts credentials on read through
`redact_route_credentials` (`config.rs:261-265`). The test asserts a
`user:password@host` route leaks neither the password nor a repr/str trace
(`tests/test_preflight.py:363-368`).

`report.rs` is the same policy for regression data. `RegressionReport` owns a
`RegressionReport` and adds typed getters (`success`, `finding_count`,
`baseline_flow_ids`, `findings`) plus `to_json()` and `to_dict()`
(`report.rs:51-96`). The projections are not re-implementations: `to_dict()`
literally calls `to_json()` and runs `json.loads`
(`report.rs:90-95`), and `to_json()` is the serde encoding of the same Rust
struct the CLI serializes. `DiffFinding` exposes `kind`/`field`/`baseline`/
`candidate` and is `Clone` + `skip_from_py_object`, so findings are copied out
of the Rust report rather than borrowed with an unsafe lifetime
(`report.rs:7-37`).

That makes the Python report schema-identical to the CLI envelopes: the CLI
wraps payloads in an `Envelope` with `command`, `schema_version`, `success`,
`failure_class`, `warnings`, and a payload that is the report itself
(`main.rs:651-659`), and the human/json/junit renderer is documented as a
projection rather than a re-evaluation. `test_regression_report_uses_rust_json_shape`
asserts `json.loads(report.to_json()) == report.to_dict() == payload` for a
hand-written report document (`tests/test_preflight.py:371-392`). One difference
is deliberate: `regress_flow` builds its report with
`ReportScheduler::Sequential` because it compares exactly one flow
(`lifecycle.rs:493-498`, `511-518`).

## pytest plugin

`pyproject.toml:32-33` registers `eggreplay.pytest_plugin` through the
`pytest11` entry point, so the fixtures exist as soon as `eggreplay` is
installed.

| Fixture | Behaviour | Source |
| --- | --- | --- |
| `eggreplay_fixture` | Validated read-only `Fixture`; fails with `pytest.fail` if absent or invalid | `pytest_plugin.py:212-223` |
| `eggreplay_server` | Managed sync server over one `asyncio.Runner` | `pytest_plugin.py:226-246` |
| `eggreplay_async_server` | Same for pytest-asyncio, or a clear failure when pytest-asyncio is absent | `pytest_plugin.py:255-282` |
| `eggreplay_recorder` / `eggreplay_async_recorder` | Aliases over the same explicit mode | `pytest_plugin.py:285-296` |
| `eggreplay_report` | `RegressionAssertions` with `compare` / `acompare` | `pytest_plugin.py:299-334` |

Options are `--eggreplay-fixture`, `--eggreplay-record-mode`
(`once|append-new|re-record`), `--eggreplay-upstream`, `--eggreplay-route`,
`--eggreplay-bind`, and `--eggreplay-websockets`
(`pytest_plugin.py:20-61`). Markers `eggreplay_fixture` and
`eggreplay_async_fixture` carry the same values, and `@eggreplay.use_fixture(P)`
attaches them plus the matching `usefixtures`
(`_lifecycle.py:134-165`).

Sealed-by-default is enforced at four points. No path and no marker means
`pytest.fail` with setup instructions (`pytest_plugin.py:89-95`); `record_mode`
is omitted by default so `_open_options` never even passes a record option
(`pytest_plugin.py:201-209`); a resolved `sealed` policy returns `None` from
`_writer_lock` and no write path is taken (`pytest_plugin.py:195-196`); and
`--eggreplay-websockets` without a record mode fails rather than silently
upgrading (`pytest_plugin.py:207-208`). The Rust side independently refuses
`sealed` for the recording gateway
(`lifecycle.rs:245-249`). A missing fixture errors without creating a directory
or a lock file, and generic `--update` is simply unrecognized
(`tests/test_preflight.py:840-855`).

Parallel safety (M012D) is a single-writer lock, not a merge. `_WriterLock`
creates `.{name}.eggreplay.lock` with `O_CREAT|O_EXCL` and writes
`{"pid":…,"worker_id":…}` (`pytest_plugin.py:119-171`). A conflicting writer
raises with the recorded owner and *does not* remove the lock — stale recovery
is an explicit operator step — which two tests pin
(`tests/test_preflight.py:957-981`). Read-only workers take no lock at all, so
xdist workers may share a fixture (`tests/test_preflight.py:882-897`).

Relative fixture paths resolve from pytest's root and are rejected if they
escape it; absolute paths are allowed for shared fixture locations
(`pytest_plugin.py:96-106`). Failure output is bounded: `_message` prints at most
20 `kind field` lines with no baseline/candidate values
(`pytest_plugin.py:303-308`), and the structured report rides on the raised
`AssertionError.report` (`pytest_plugin.py:311-316`). A nested pytest run
asserts the values never reach terminal output
(`tests/test_preflight.py:423-457`).

On VCR.py: the README is explicit that these are "not drop-in VCR.py
compatibility" — fixtures are semantic `.eggr` directories, recording uses
explicit modes, and "arbitrary callbacks or scripts are not supported. Python
does not patch HTTP clients or implement a cassette format. Rust owns matching,
redaction, fixture persistence, and transport." The M012 closure agrees:
migration is "conceptual rather than compatible"
(`plans/closure/m012-python-pytest-ecosystem.md`, `Implementation`).

## Packaging and distribution

`pyproject.toml` uses maturin as the build backend with `python-source = "python"`
and `module-name = "eggreplay._native"`, so the native module lands *inside* the
package (`pyproject.toml:45-49`). Metadata is `name = "eggreplay"`, `version =
"0.1.0"`, `requires-python = ">=3.11"`, MIT license expression, and
`dependencies = []` — the extension carries no runtime Python requirements
(`pyproject.toml:5-14`). Dev tools are pinned under the `dev` extra:
`pytest==8.4.2`, `pytest-asyncio==1.2.0`, `pytest-xdist==3.8.0`
(`pyproject.toml:35-40`); the build backend itself is `maturin==1.14.1`
(`pyproject.toml:1-3`).

The abi3 strategy is one wheel per platform, built once. The wheel matrix in
`.github/workflows/python-wheels.yml:19-40` covers `manylinux_2_34_x86_64`,
`manylinux_2_34_aarch64`, `macosx_11_0_arm64`, `macosx_10_12_x86_64`, and
`win_amd64`, each on a native runner with a `platform.machine()` assertion
(`python-wheels.yml:51-52`). Every job builds under CPython 3.11 with
`--compatibility pypi --locked` (`python-wheels.yml:53-54`), then the
`abi3-interpreter-smoke` job downloads the Linux x86_64 artifact and re-runs the
full clean-install smoke under 3.11, 3.12, 3.13, and 3.14
(`python-wheels.yml:94-111`) — the "build under 3.11, install under 3.14"
contract, widened.

`tools/python/` holds the qualification scripts:

| Script | Asserts |
| --- | --- |
| `inspect_wheel.py` | filename matches `cp311-abi3-<platform_tag>`; contains `py.typed`, `__init__.pyi`, `pytest_plugin.pyi`, `pytest_plugin.py`; METADATA name/version/`Requires-Python`/license/repository; no `tests`/`fixtures`/`target`/`.venv`/`__pycache__`/bytecode; exactly one native binary named `eggreplay/_native.*` |
| `clean_wheel_smoke.py` | creates a throwaway venv in a temp dir, `pip install "<wheel>[dev]"`, and runs the smoke with `PYTHONPATH`/`PYTHONHOME` stripped from an `outside-checkout` cwd |
| `wheel_smoke.py` | asserts the imported package is *not* from the checkout, then exercises fixture load, bounded body read, replay server, `regress_flow`, and a generated pytest run using the installed plugin |
| `inspect_sdist.py` | single root, required members (workspace manifests, `Cargo.lock`, Python sources, stubs, `py.typed`), PKG-INFO fields, no forbidden parts |
| `write_artifact_manifest.py` | writes `SHA256SUMS.txt` beside the artifacts |

The wheel-content invariants in `inspect_wheel.py:33-59` are the packaging
contract: typing marker and both stubs ship, and exactly one native object
exists, so a future accidental second extension fails the lane. Standard CI
adds a lighter content check on the wheel plus sdist
(`.github/workflows/ci.yml:81-103`) and a same-wheel cross-version job
(`ci.yml:105-124`). Artifact hashes from the qualifying run are recorded in
`plans/closure/m012e-python-wheel-stubs-and-distribution-qualification.md`
(`Qualified artifacts`); nothing has been published to PyPI.

## Feature isolation

The qualified default wheel is interception-free. `eggreplay-intercept` and the
CA generator `rcgen` are checked out of every ordinary product graph, and
`eggreplay-python` is in that loop (`.github/workflows/ci.yml:146-152`). The
M013 interception/CA capability reaches users only through the CLI's opt-in
`--features intercept` build, where `eggreplay-cli`'s
`intercept = ["dep:eggreplay-intercept"]` is a non-default feature
(`crates/eggreplay-cli/Cargo.toml:45-47`). Since the binding's Cargo manifest
never enables it, the wheel cannot intercept traffic or issue certificates.

The binding's own transport selection is narrow and pinned in
`crates/eggreplay-python/Cargo.toml:18`: `default-features = false` plus exactly
`direct`, `eggserve`, `eggress`, and `websocket`. The inbound multiprotocol
closure stays out — the "Python binding lane remains free of the multiprotocol
closure" step fails if `eggserve-core`, `eggserve-static`, or `eggserve-h3`
appear in the tree (`.github/workflows/ci.yml:255-264`). `WebSocketOptions` is
therefore capability *configuration* (capture, cadence tolerance, redaction
mode) evaluated by Rust, not a bundled WebSocket serving stack. The dedicated
inbound H2 lane also asserts the CLI's `h2-inbound` features are never defaults
(`ci.yml:250-253`), which is the same reasoning applied one level up.

## Development workflow

From `AGENTS.md` and the crate README, the loop is an isolated environment in
`crates/eggreplay-python`:

```sh
cd crates/eggreplay-python
uv sync --extra dev      # pinned pytest, pytest-asyncio, pytest-xdist
maturin develop          # builds and installs eggreplay._native in place
python -m pytest tests
```

The crate README uses `uv sync --extra dev` before `maturin develop`
(`crates/eggreplay-python/README.md`, install section), and CI runs the same two
steps non-interactively with explicit `--with` pins rather than relying on the
lockfile alone (`.github/workflows/ci.yml:61-62`). `maturin develop` is required
before running the tests: the suite imports `eggreplay._native` directly
(`tests/test_preflight.py:11-12`), so there is no pure-Python fallback. The Rust
side is unchanged by this loop — `test = false` means `cargo test` does not
exercise the binding, and the supported local verification command in
`AGENTS.md` does not include it.

## Review checklist

| Check | Where to look |
| --- | --- |
| No PyO3 in product crates, no product-crate dependency on the binding | `.github/workflows/ci.yml:63-75`; keep both greps passing |
| GIL released across every blocking read | `py.detach` / `spawn_blocking` in `src/fixture.rs` and `src/lifecycle.rs:400-479` |
| One process-wide runtime; no per-call runtime, no per-object thread | `future_into_py` only; the single `get_runtime().spawn` at `src/lifecycle.rs:92` |
| Cancellation reaches Rust futures, but recording cleanup survives waiter cancellation | `Finalization` + watch channel, `src/lifecycle.rs:23-38`, `91-137` |
| Finalization order preserved: stop admission, drain, then publish | `finish_session` comment contract, `src/lifecycle.rs:553-567` |
| Fixture immutability: no Python write to an existing fixture, no staging read-back | `_WriterLock`; `open_blob`/`read_blob` only |
| Bounded body reads and enforced digest | `MAX_BODY_CHUNK`, `read_all` cap, `fixture.rs:205-236` |
| Redaction cannot be widened from Python | `default_secure()` base in `config.rs:132`; fixed `fixture_error` message |
| No interception, CA, or inbound-H2 closure in the default wheel | `ci.yml:146-152` and `ci.yml:255-264` |
| ABI compatibility: one `cp311-abi3` artifact serves 3.11–3.14 | `python-wheels.yml:94-111`, `ci.yml:105-124` |
| Error mapping complete and message-redacting | `src/errors.rs`; new failure sites must reuse `fixture_error` or an existing class |
| Stubs and `__all__` still agree with the runtime | `test_stub_manifest_matches_runtime_exports_and_rust_enum_names` |
| Wheel content invariants hold | `tools/python/inspect_wheel.py:33-59` |

One surface to watch for leakage: any new Python-facing constructor that accepts
raw redaction selectors, comparison tolerances, or route strings should reuse
`config.rs`'s validating types rather than a plain dict, because those are the
only places Rust re-checks policy before a socket is opened.
