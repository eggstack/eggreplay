# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

v0.1 is closed via the corrective workstream (`C001`–`C005`) in
[`plans/registry.md`](plans/registry.md); see
[`plans/closure/v0.1-corrective-closure.md`](plans/closure/v0.1-corrective-closure.md)
which supersedes the original foundation qualification. The core and store
remain usable without a network runtime. Supported transport is direct
HTTP/1.1 acquisition and EggServe inbound HTTP/1.1 replay with optional
listener-free Eggress routing; H2/H3 remain deferred (see `docs/non-goals.md`).

## Quickstart routes

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9000 --fixture demo.eggr --route direct
eggreplay replay --fixture demo.eggr --target http://127.0.0.1:9000 --route direct --output json
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9000 --route socks5://127.0.0.1:1080 --output junit
```

`direct` is the default; non-direct values use listener-free Eggress routing
via the `pproxy-compat` grammar only (see `docs/eggress-routing.md` and
`docs/cli.md` for exit codes and JSON/JUnit contracts).

## Development

Rust 1.89 is the minimum supported version. Run the fast verification command
from [`AGENTS.md`](AGENTS.md) before submitting changes. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for plan closure and evidence rules.
