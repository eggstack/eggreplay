# EggReplay

Record HTTP interactions in versioned `.eggr` fixtures. Replay them without an
origin, or send the recorded requests to a live service and compare responses.

EggReplay owns the semantic layer: flows, matching, scenarios, redaction,
comparison, and reporting. HTTP, TLS, framing, inbound serving, and route
establishment are delegated to [EggFetch](https://crates.io/crates/eggfetch-core),
[EggServe](https://crates.io/crates/eggserve-server), and
[Eggress](https://crates.io/crates/eggress-outbound).

## Install

The CLI is not yet published on crates.io. Install it from source with Rust
1.89 or newer:

```sh
git clone https://github.com/eggstack/eggreplay.git
cd eggreplay
cargo install --path crates/eggreplay-cli
eggreplay --version
```

To include the optional HTTP/1.1 proxy and HTTPS interception commands, add
`--features intercept` to `cargo install`.

## Quickstart

This records a request, stops the origin, then serves the fixture offline. It
requires Python 3 and `curl`. Use two terminals. Keep the first terminal in the
same shell so its `ORIGIN_PID` variable remains available.

In **terminal 1**, start the temporary origin and recorder:

```sh
mkdir -p eggr-quickstart
cd eggr-quickstart
printf '%s\n' '{"greeting":"hello"}' > api.json
python3 -m http.server 9100 --bind 127.0.0.1 &
ORIGIN_PID=$!
eggreplay record --listen 127.0.0.1:8080 \
  --upstream http://127.0.0.1:9100 --fixture demo.eggr
```

While `record` is running, use **terminal 2** to make a request through the
gateway:

```sh
curl http://127.0.0.1:8080/api.json
```

Press `Ctrl-C` in terminal 1 to finalize the fixture. Then stop the origin and
start offline replay in terminal 1:

```sh
kill "$ORIGIN_PID"
wait "$ORIGIN_PID" 2>/dev/null || true
eggreplay serve --fixture demo.eggr --listen 127.0.0.1:9100
```

In terminal 2, request the recorded origin again:

```sh
curl http://127.0.0.1:9100/api.json
```

The response comes from `demo.eggr`; no origin is running. Press `Ctrl-C` in
terminal 1 to stop the replay server. Replay matches the recorded upstream
authority, here `127.0.0.1:9100`, and consumes each flow once.

For a full walkthrough, including validation, live regression, and redaction,
see the [Quickstart guide](docs/quickstart.md).

## Capabilities

| Capability | Availability |
|---|---|
| HTTP/1.1 recording, offline replay, and live regression | CLI default |
| HAR 1.2 import and export | CLI default; lossy, with a loss report |
| WebSocket conversation recording and replay | Supported; recording requires `--websockets` |
| Eggress routing | Available in the CLI; direct is the default |
| Outbound and inbound HTTP/2 | Optional CLI features; experimental |
| gRPC-derived views | Optional `eggreplay-http/grpc` feature; experimental |
| HTTP/1.1 proxy, CONNECT policy, and HTTPS MITM | Optional `intercept` feature; policy-gated |

HTTP/2 is never enabled by default. See the [protocol support matrix](docs/http2-support.md)
and [unsupported capabilities](docs/non-goals.md) for details and limitations.

## Documentation

| Guide | Covers |
|---|---|
| [Quickstart](docs/quickstart.md) | Record, inspect, replay, and regress a fixture |
| [CLI and exit contracts](docs/cli.md) | Commands, flags, exit codes, and output formats |
| [Configuration](docs/configuration.md) | Matcher, timing, recording, and route settings |
| [Matcher profiles](docs/matcher-profiles.md) | Matching behavior and flow consumption |
| [Regression cookbook](docs/regression-cookbook.md) | `replay`, `test`, and `diff` |
| [`.eggr` schema](docs/eggr-schema.md) | Fixture layout, extensions, bounds, and integrity |
| [HAR interchange](docs/har-interchange.md) | Import/export behavior and known data loss |
| [HTTP/2 support](docs/http2-support.md) | H2 and gRPC tiers and limitations |
| [Interception threat model](docs/interception-threat-model.md) | Proxy, CONNECT policy, and MITM boundaries |
| [CA and trust](docs/interception-ca-trust.md) | CA lifecycle and manual trust setup |
| [Non-goals](docs/non-goals.md) | Deliberate exclusions and redaction limits |
| [Python bindings](crates/eggreplay-python/README.md) | Python package and pytest integration |
| [Testing](docs/testing.md) | Local checks and CI qualification |

For the design and crate boundaries, see [Architecture](docs/architecture.md) and
[the architecture index](architecture/overview.md). For repository contribution
rules, start with [`AGENTS.md`](AGENTS.md).

## Python

Python bindings are distributed separately as an abi3 wheel. The default wheel
excludes interception and multiprotocol serving. To develop locally:

```sh
cd crates/eggreplay-python
uv sync --extra dev
maturin develop
```

See the [Python package guide](crates/eggreplay-python/README.md).

## Development

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI also uses `--locked` and `--no-fail-fast`, plus architecture boundary lanes.
A passing test suite alone does not prove the dependency and protocol boundaries
are intact. See [`AGENTS.md`](AGENTS.md) and the
[planning registry](plans/registry.md).

Contributing and security reports: [CONTRIBUTING.md](CONTRIBUTING.md),
[SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE-MIT](LICENSE-MIT).
