# Testing and qualification

The supported local gate is the command in [`../AGENTS.md`](../AGENTS.md):

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI adds `--locked` and runs tests with `--no-fail-fast`. See
[verification and qualification](../.skills/verification-qualification.md) for
the boundary lanes and milestone evidence rules.

Tests use local loopback fixtures and do not depend on the public Internet.
`eggreplay-core` and `eggreplay-store` need no network runtime. CI also checks
their dependency trees for transport runtimes.

## CI coverage

The `verify` jobs build and test all workspace targets and features on Linux,
macOS, and Windows, plus the Rust 1.89 MSRV on Linux. Separate lanes check
dependency boundaries, protocol boundaries, Python bindings, Python abi3
compatibility, and interception behavior. These checks catch architectural
regressions that ordinary tests may not detect.

## Support matrix

| Capability | Status |
|---|---|
| Direct HTTP/1.1 acquisition | Qualified |
| EggServe inbound HTTP/1.1 replay | Qualified |
| WebSocket over cleartext RFC 6455 and H1 Upgrade | Qualified |
| Eggress listener-free routing | Qualified, opt-in |
| Outbound HTTP/2 | Qualified, experimental, opt-in |
| Inbound HTTP/2 (`h2c` and ALPN TLS) | Qualified, experimental, opt-in |
| gRPC over HTTP/2 | Qualified, experimental, opt-in |
| HTTP/1.1 proxy and CONNECT policy | Qualified, opt-in, policy-gated |
| HTTPS MITM over HTTP/1.1 | Qualified, opt-in, policy-gated |
| HTTP/2 interception | Unsupported |
| WSS and extended-CONNECT WebSockets | Unsupported |
| HTTP/3 / QUIC | Deferred by ADR 0009 |

Closure records in `plans/closure/` contain the exact evidence. The current
execution gate is in [`../plans/registry.md`](../plans/registry.md).

## Known local failures

Two `curl_interop` tests may fail with some local `curl` builds:

- `curl_plain_http_proxies_and_records` may record no flow.
- `curl_https_connect_mitm_records` may fail to trust the test CA, for example
  with `curl: (60) SSL certificate problem: self signed certificate`.

These failures were reproduced on the stock macOS SecureTransport `curl` and
were absent from four hosted `verify` jobs. Treat them as a local environment
limitation, not as a green test result. The suite is not clean when they fail.

## Fixture testing

Redaction applies before body publication and fails closed on malformed,
oversized, or unsupported structured transformations. Opaque binary secret
discovery is unsupported. Fixture readers reject future schemas, malformed or
oversized JSONL, invalid blob names, hash or length mismatches, symlinked blobs,
and incomplete manifests.

Golden fixtures are in `crates/eggreplay-store/tests/fixtures/` and HAR corpus
files are in `crates/eggreplay-har/tests/corpus/`. Both directories are excluded
from the published package.

## Parser fuzzing (M021C, nightly, local)

`fuzz/` is a standalone `cargo-fuzz` workspace with an empty `[workspace]`
table. It is deliberately **not** a member of the product workspace, so the
locked gate never requires nightly and no fuzz crate enters a shipped graph.
Every target writes only below a per-iteration temporary directory, uses small
`StoreLimits`, and never touches the network or a non-temp path.

Targets:

| Target | Surface |
|---|---|
| `har_import` | arbitrary bytes into `eggreplay_har::import_har_to_writer` |
| `session_open` | `manifest.json`, `flows.jsonl`, `stream-events.json`, `websockets.jsonl` under `Session::open` |
| `websocket_transcript` | transcript bytes and text-payload UTF-8 through `Session::open`/`SessionWriter::finish` |

Local acceptance is a 60-second run per target:

```sh
cargo install cargo-fuzz            # once, if `cargo fuzz` is absent
cd fuzz
cargo +nightly fuzz build
cargo +nightly fuzz run har_import -- -max_total_time=60 -max_len=4096
cargo +nightly fuzz run session_open -- -max_total_time=60 -max_len=4096
cargo +nightly fuzz run websocket_transcript -- -max_total_time=60 -max_len=4096
```

A ten-second run per target is enough to prove execution during development.
`cargo +nightly fuzz build` is the compile check. Any crash writes an artifact
under `fuzz/artifacts/<target>/`; replay it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<artifact>`. The
`.github/workflows/fuzz.yml` lane runs each target for 60 seconds weekly and on
manual dispatch, uploads the corpus and any crash artifact, and is deliberately
not a required pull-request gate. A bounded smoke does not prove the absence of
bugs.
