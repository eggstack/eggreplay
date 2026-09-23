# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

v0.1 is fully qualified through C001–C006. M009 stateful/dynamic replay and
M010 streaming timing/SSE (including M010-C1) are closed with hosted
cross-platform evidence.

M011 WebSocket semantic record/replay is decomposed into M011A–M011F under ADR
0006. M011A transport/dependency/upgrade preflight is the only ready task;
later WebSocket subplans and M012–M014D remain blocked by dependency order.

The v0.1 support baseline remains direct HTTP/1.1 acquisition and EggServe
inbound HTTP/1.1 replay with optional listener-free Eggress routing. WebSocket,
H2/H3, and interception support are not part of the v0.1 claim until their
later milestones close.

## Quickstart routes

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9000 --fixture demo.eggr --route direct
eggreplay replay --fixture demo.eggr --target http://127.0.0.1:9000 --route direct --output json
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9000 --route socks5://127.0.0.1:1080 --output junit
```

`direct` is the default; non-direct values use listener-free Eggress routing
via the `pproxy-compat` grammar only.

## Development

Rust 1.89 is the minimum supported version. Run the verification command from
[`AGENTS.md`](AGENTS.md) before submitting changes. See
[`plans/registry.md`](plans/registry.md) for the current execution gate.
