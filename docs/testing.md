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
