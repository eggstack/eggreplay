# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

v0.1 is fully qualified through C001–C006. M009 stateful/dynamic replay, M010
streaming timing/SSE, and M011 semantic WebSocket record/replay/regression are
closed with hosted cross-platform evidence.

M012 Python/pytest integration is closed under ADR 0007 with hosted
Rust/Python and wheel qualification. M013 explicit-proxy/optional HTTPS
interception is decomposed under ADR 0008. M013A substrate/dependency/threat
preflight is closed. M013B0 has adopted and locally qualified EggServe
server 0.3.0/primitives 0.2.1 and is awaiting hosted closure evidence before
M013B proxy work resumes. Interception is not yet part of the supported
product baseline. M014 remains downstream of M013.

The support baseline includes direct HTTP/1.1 acquisition and EggServe inbound
HTTP/1.1 replay with optional listener-free Eggress routing. WebSocket support
covers RFC 6455 over cleartext HTTP/1.1 Upgrade (`ws://`) with bounded semantic
text, binary, ping, pong, and close messages. WSS, interception, inbound TLS,
H2/H3, negotiated extensions, and wire-frame fidelity remain outside the claim.

## Quickstart routes

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9000 --fixture demo.eggr --route direct
eggreplay replay --fixture demo.eggr --target http://127.0.0.1:9000 --route direct --output json
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9000 --route socks5://127.0.0.1:1080 --output junit
```

`direct` is the default; non-direct values use listener-free Eggress routing
via the `pproxy-compat` grammar only.

## Python quickstart

The Python adapter builds from this repository and uses the same Rust fixture,
transport, matching, and regression authorities as the CLI:

```sh
cd crates/eggreplay-python
uv sync --extra dev
maturin develop
python -c 'import eggreplay; print(eggreplay.__version__)'
```

Python exposes `.eggr` directory fixtures and managed replay/recording
lifecycles. The qualified default wheel intentionally does not include the
future M013 interception/CA feature. See
[`crates/eggreplay-python/README.md`](crates/eggreplay-python/README.md) for
pytest fixtures, explicit record modes, and migration notes for VCR.py users.
It is not a drop-in VCR.py replacement.

## Development

Rust 1.89 is the minimum supported version. Run the verification command from
[`AGENTS.md`](AGENTS.md) before submitting changes. See
[`plans/registry.md`](plans/registry.md) for the current execution gate.
