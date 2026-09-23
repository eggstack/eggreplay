# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

v0.1 is fully qualified through the corrective workstream (`C001`–`C006`).
The hosted release gate passed Linux stable, Linux Rust 1.89 MSRV, macOS stable,
Windows stable, and the dependency-boundary lane; see
[`plans/closure/c006-windows-hosted-ci-qualification.md`](plans/closure/c006-windows-hosted-ci-qualification.md).

M009 stateful/dynamic replay is closed. M010 streaming timing/SSE is closed
with its M010-C1 corrective (required-extension semantics, candidate response
stream observation, and opt-in stream/cadence/SSE regression); see
[`plans/closure/m010-streaming-timing-and-sse.md`](plans/closure/m010-streaming-timing-and-sse.md).
M011 WebSocket semantic record/replay is ready; M012–M014D remain blocked.

The core and store remain usable without a network runtime. Supported transport
for v0.1 is direct HTTP/1.1 acquisition and EggServe inbound HTTP/1.1 replay
with optional listener-free Eggress routing; H2/H3 remain outside the v0.1
support claim.

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
