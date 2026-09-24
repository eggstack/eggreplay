# M012C — Async Lifecycle and Network Bindings

Status: implemented (hosted qualification pending)
Depends on: M012B
Parent milestone: M012

## Objective

Expose replay servers, recording gateways, and candidate regression through one
Tokio/asyncio bridge without hidden alternate networking or lifecycle rules.

## A. Replay server binding

Expose a Rust-backed replay server handle with:

- explicit async start;
- bound address;
- `aclose()`;
- async context manager;
- cancellation-safe wait;
- scenario state isolated per server instance;
- stream timing controls;
- WebSocket replay automatically active for fixtures that require it.

The Python wrapper must call the existing Rust replay authority.

Implementation in `crates/eggreplay-python/src/lifecycle.rs` exports async
`replay_server`, `recording_gateway`, `regress_flow`, and an owned `Server`
handle. Replay supports matcher profile, scenario, stream timing, and bounded
request bodies. Recording supports Rust policy modes once, append-new, and
re-record; WebSocket capture is opt-in and append-new rejects it. Candidate
regression routes through EggFetch/Eggress and returns M012B's report wrapper,
including stream/SSE policy and semantic WebSocket regression. See the closure
record for local evidence and the deliberate synchronous-adapter handoff.

## B. Recording gateway binding

Expose recording lifecycle with existing M009/M011 policies:

- sealed is not a recording mode;
- once;
- append-new for supported HTTP flows;
- re-record;
- upstream/route;
- redaction;
- WebSocket acquisition opt-in;
- WebSocket append-new limitation surfaced explicitly rather than hidden.

Finalize fixtures only through the Rust recording/session authority.

Cancellation or exception exit must shut down admission, drain/cancel
in-flight work according to Rust policy, and either finalize a valid fixture or
fail without publishing incomplete authority.

## C. Candidate regression/test binding

Expose async candidate replay/regression over the existing Rust
`RegressionReport`.

Support:

- target remap;
- direct/Eggress route;
- HTTP stream comparison;
- SSE policy;
- WebSocket regression;
- timing/cadence options.

Return the same report object from M012B. Python never recomputes findings.

## D. Runtime/cancellation

Use the M012A process-wide Tokio bridge.

Required behavior:

- asyncio task cancellation reaches Rust futures promptly;
- closing one server does not stop unrelated servers;
- Python interpreter shutdown does not leave non-daemon process blockers;
- synchronous cleanup in `__del__` is best-effort only;
- explicit close/context-manager APIs are authoritative;
- GIL is not held while Rust waits on network IO, sleep, shutdown, or fixture
  finalization.

## E. Synchronous lifecycle convenience

Provide synchronous context managers only if they can use the same shared Tokio
runtime without per-object threads/runtimes.

A supported sync test should be able to:

```python
with eggreplay.replay_server("fixture.eggr") as server:
    ...
```

The context must stop/join the Rust lifecycle before exit. If this cannot be
implemented without unsafe re-entrancy/deadlock, omit it here and make the
pytest plugin own an explicitly managed sync adapter in M012D. Do not fake sync
support with detached threads.

## F. Concurrency

Qualify multiple concurrent read-only replay servers and independent recording
sessions. Python object locks must not span awaited Rust/network work.

## Required tests

- asyncio replay lifecycle;
- asyncio recording lifecycle;
- candidate regression;
- cancellation during body/timing/WebSocket wait;
- multiple simultaneous servers;
- exception inside async context manager;
- finalization after cancellation;
- no GIL starvation using a Python-side heartbeat task/thread;
- sync context manager if implemented;
- direct and one routed local network fixture;
- WebSocket replay/regression binding.

## Closure

Create `plans/closure/m012c-python-async-lifecycle-and-network-bindings.md`.
M012D remains blocked until lifecycle behavior is stable.
