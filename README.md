# EggReplay

Record real HTTP interactions once, then replay them **offline with no origin
running** — deterministically, in CI, with no network flakiness. Or point the
same fixture at a live service to diff a candidate against a recorded baseline.

EggReplay owns HTTP *semantics* — flows, matching, scenarios, redaction,
comparison, reporting. Transport (HTTP, TLS, framing, inbound serving, route
establishment) is delegated to [EggFetch](https://crates.io/crates/eggfetch-core),
[EggServe](https://crates.io/crates/eggserve-server), and
[Eggress](https://crates.io/crates/eggress-outbound). There is no parallel HTTP
stack inside this project.

## Install

Not published to crates.io yet. Build from source (Rust 1.89+):

```sh
git clone https://github.com/eggstack/eggreplay.git
cd eggreplay
cargo install --path crates/eggreplay-cli
eggreplay --version
```

## Quickstart

This walks the whole loop with a throwaway origin. Every command below is
verified as written.

**1. Start something to record.** Any real service works; this needs no setup:

```sh
mkdir qs && cd qs && echo '{"greeting":"hello"}' > api.json
python3 -m http.server 9100 --bind 127.0.0.1 &
```

**2. Record through the gateway.** Your client talks to the gateway, not the
origin. Traffic flows through unmodified, so record in any environment you like:

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9100 --fixture demo.eggr
```

```sh
curl http://127.0.0.1:8080/api.json      # {"greeting":"hello"}
```

Press `Ctrl-C` to finalize the fixture. The result is a directory — not a
single file:

```
demo.eggr/
├── manifest.json          # written last; the publication marker
├── flows.jsonl            # one JSON flow per line
├── blobs/<sha256>         # content-addressed bodies
└── stream-events.json     # required extension
```

**3. Kill the origin and replay offline.** This is the whole point:

```sh
# the http.server from step 1 is gone
eggreplay serve --fixture demo.eggr --listen 127.0.0.1:9100
```

```sh
curl http://127.0.0.1:9100/api.json      # {"greeting":"hello"} — no network, no origin
```

> Serve on the **authority the flows recorded** (here `127.0.0.1:9100`, the
> upstream). Replay is origin-only: a gateway recording stores the *upstream*
> authority it reached, so a request with a different `Host` is a clean `404`
> "no match" rather than a relaxed match. Recorded flows are consumed once, so
> a second identical request returns `409`.

**4. Check a fixture, or diff two of them:**

```sh
eggreplay validate --fixture demo.eggr --output json
eggreplay inspect  --fixture demo.eggr --output json
eggreplay diff --baseline before.eggr --candidate after.eggr --output json
```

**5. Regress a live service against a recording.** `test` enforces and exits
non-zero on a difference; `replay` reports the same thing but always exits `0`:

```sh
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9100 --output junit
```

```xml
<testsuite name="test" tests="1" failures="1" errors="0">
  <testcase name="flow-…" classname="test">
    <failure message="mismatch">Header response.headers.date …</failure>
  </testcase>
</testsuite>
```

That `date` finding is expected and is the one thing to know about step 5:
`Date` is origin-generated and second-granular, so it differs on every run. The
`practical` matcher profile ignores `date` when *matching*, but the regression
*comparison* authority has no header-ignore, so it will surface. Treat it as
noise until comparison-level normalization ships.

## What it does

| Capability | Status |
|---|---|
| Gateway recording of HTTP/1.1 into a `.eggr` fixture | default |
| Offline replay server, zero network | default |
| Regression compare with a versioned report (JSON / JUnit) | default |
| Structural redaction (headers, query, JSON/form bodies) | default |
| Authored scenarios, near-miss reporting, WebSocket codec | default |
| HAR 1.2 import / export (lossy, with a loss report) | default |
| Explicit HTTP/1.1 proxy, CONNECT policy, HTTPS MITM, CA lifecycle | opt-in `--features intercept` |
| Outbound HTTP/2, inbound HTTP/2 (h2c / ALPN), gRPC over H2 | opt-in cargo features, experimental |
| Eggress listener-free outbound routing | opt-in cargo feature |

**No HTTP/2 capability is default in any profile.** A protocol you did not ask
for is a protocol you cannot silently get. Full matrices, including what is
explicitly *not* supported, live in
[`docs/http2-support.md`](docs/http2-support.md) and
[`docs/non-goals.md`](docs/non-goals.md).

## Documentation

Start here, then go deeper:

| Guide | Covers |
|---|---|
| [Quickstart](docs/quickstart.md) | The full walkthrough above, plus scenarios, redaction, and regression |
| [CLI and exit contracts](docs/cli.md) | Every command, flag, exit code, and the JSON envelope |
| [Configuration](docs/configuration.md) | Matcher profiles, timing, route policy, recording modes |
| [Regression cookbook](docs/regression-cookbook.md) | `replay` vs `test` vs `diff`, schedulers, comparison policy |
| [`.eggr` schema](docs/eggr-schema.md) | On-disk format, extension registry, bounds, integrity |
| [HAR interchange](docs/har-interchange.md) | What HAR cannot carry, in both directions |
| [HTTP/2 support](docs/http2-support.md) | H2/gRPC/WebSocket/Eggress tiers and known limitations |
| [Interception threat model](docs/interception-threat-model.md) | The proxy, CONNECT policy, CA, and MITM boundaries |
| [CA and trust](docs/interception-ca-trust.md) | CA lifecycle and trust operations |
| [Testing](docs/testing.md) | The verification gate and the CI lanes |
| [Release process](docs/release-process.md) | Manual release procedure and dependency pins |
| [Architecture](docs/architecture.md) | Crate ownership, transport delegation, dependency state |

For the reasoning behind the design, see
[`architecture/overview.md`](architecture/overview.md) and the deep dives it
indexes. For agents working in this repository, [`AGENTS.md`](AGENTS.md) is the
entry point.

## Python

PyO3 bindings ship as a separate abi3 wheel, deliberately multiprotocol-free
and without the interception capability:

```sh
cd crates/eggreplay-python && uv sync --extra dev && maturin develop
```

See [`crates/eggreplay-python/README.md`](crates/eggreplay-python/README.md).

## Development

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI adds `--locked` and `--no-fail-fast`, plus four single-runner lanes that
assert *architecture* rather than behavior. A change can pass every test and
still break a boundary — see [`AGENTS.md`](AGENTS.md) and
[`plans/registry.md`](plans/registry.md) for the live execution gate.

Contributing and security reports: [CONTRIBUTING.md](CONTRIBUTING.md),
[SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE-MIT](LICENSE-MIT).
