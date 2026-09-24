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

The synchronous context manager is deferred to the M012D pytest adapter.
Python does not create a runtime or detached server thread per object.
