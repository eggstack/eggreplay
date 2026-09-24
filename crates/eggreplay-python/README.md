# eggreplay Python bindings

The package is built from the Rust workspace with maturin. Fixture parsing,
validation, reports, configuration checks, and body integrity remain Rust
authorities.

```python
import eggreplay

fixture = eggreplay.Fixture("fixtures/example.eggr")
for flow in fixture.iter_flows():
    print(flow.id, flow.request.method, flow.request.headers)

with fixture.open_body("flow-id", "response") as body:
    for chunk in body:
        consume(chunk)
```

Header and query fields are ordered pair sequences and preserve duplicates.
Body reads are bounded; `read_all(max_bytes=...)` requires an explicit cap
above its 1 MiB default. Report JSON projections use the Rust serde schema.

Async server and candidate APIs use the process-wide Tokio bridge. Servers
bind to loopback on an ephemeral port by default and must be stopped with
`aclose()` or an async context manager:

```python
import eggreplay

fixture = eggreplay.Fixture("fixtures/example.eggr")
server = await eggreplay.replay_server(fixture)
try:
    print(server.address)
finally:
    await server.aclose()
```

`replay_server` accepts `scenario_id` and the Rust stream `timing_mode`.
`recording_gateway` requires an explicit upstream and supports `once`,
`append-new`, and `re-record`; WebSocket acquisition is opt-in, and
append-new rejects it because the Rust authority does not support that mode.
Recordings are finalized on `aclose()` after admission stops and in-flight
requests drain. `regress_flow` executes one fixture request against a
candidate target and returns the Rust `RegressionReport`; `route` accepts
`direct` or Eggress' supported outbound route grammar.

The package registers the `eggreplay.pytest_plugin` pytest plugin. Its fixtures
are `eggreplay_fixture` (validated read-only fixture data), `eggreplay_server`
and `eggreplay_async_server` (managed sync and pytest-asyncio servers),
`eggreplay_recorder` and `eggreplay_async_recorder` (aliases using the same
explicit mode), and `eggreplay_report` (bounded candidate-regression
assertions). Set a fixture path with `--eggreplay-fixture=PATH` or
`@eggreplay.use_fixture(PATH)`. Relative paths resolve from pytest's root.
The `eggreplay_fixture` and `eggreplay_async_fixture` markers also accept a
path and optional `record_mode`, `upstream`, `route`, and `websockets` values.

All plugin use is sealed/read-only by default. Missing fixtures fail setup
without creating a directory. Writes require
`--eggreplay-record-mode=once|append-new|re-record` plus an upstream for
networked modes, or the corresponding explicit marker/decorator options. A
generic pytest `--update` option has no effect on EggReplay. Append-new remains
HTTP-only. A sibling create-new lock refuses concurrent writers; if a process
crashes, verify its PID/worker is no longer active and remove the named stale
lock file explicitly. For bulk recording, give each worker a separate fixture
path. Read-only workers can share a fixture. Each server owns its own scenario
runtime.

The VCR-style `fixture_context(path)` context manager and
`@use_fixture(path)` decorator call the same Rust server lifecycle. They do not
patch Python HTTP clients or use a Python cassette format. Synchronous use
manages one Python `asyncio.Runner` around the shared process Tokio bridge; it
does not create a Rust runtime or detached server thread per object.
