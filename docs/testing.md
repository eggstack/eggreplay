# Testing and qualification

The supported local gate is the command in
[`../AGENTS.md`](../AGENTS.md):

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI runs the same four steps with `--locked` and, for tests, `--no-fail-fast` so
one failing suite never hides the rest. Full command semantics, the boundary
lanes, and the closure model are in
[`.skills/verification-qualification.md`](../.skills/verification-qualification.md).

Tests are local-only and use loopback fixtures; no routine test depends on the
public Internet. Network behavior is qualified through the delegated
EggFetch/EggServe/Eggress surfaces, and `eggreplay-core`/`eggreplay-store` tests
need no network runtime at all. That is asserted, not assumed: the
`dependency-boundary` lane checks those two crates' resolved trees for any
transport runtime.

## CI coverage

`verify` compiles and tests the whole workspace under `--all-features` across
three platforms (Linux, macOS, Windows on stable) plus Linux on the Rust 1.89
MSRV. Four further single-runner lanes assert *architecture* rather than
behavior — `dependency-boundary`, `protocol-boundary`, `python-bindings`, and
`python-abi3-cross-version` — and the `interception` lane pins the proxy/CA
qualification evidence to its own OS matrix. Fourteen hosted jobs in total.

A change can pass every test and still be a boundary break, so run the relevant
boundary lane locally before submitting a change to a feature, a dependency, or
a crate edge.

## Supported matrix

| Capability | Status |
|---|---|
| Direct HTTP/1.1 acquisition | qualified |
| EggServe inbound HTTP/1.1 replay | qualified |
| WebSocket (cleartext RFC 6455 over H1 Upgrade) | qualified |
| Eggress listener-free routing | qualified, opt-in |
| Outbound HTTP/2 | qualified experimental, opt-in |
| Inbound HTTP/2 (`h2c` / ALPN TLS) | qualified experimental, opt-in |
| gRPC over HTTP/2 (all four streaming classes) | qualified experimental, opt-in |
| Explicit HTTP/1.1 proxy + CONNECT policy | qualified, opt-in, policy-gated |
| HTTPS MITM HTTP/1.1 | qualified, opt-in, policy-gated |
| HTTP/2 interception | unsupported |
| WSS / extended-CONNECT WebSockets | unsupported |
| HTTP/3 / QUIC | deferred (ADR 0009) |

The exact-SHA evidence, per-suite counts, and run links for each milestone live
in `plans/closure/`; the live execution gate is
[`../plans/registry.md`](../plans/registry.md). Docs do not hardcode a live
qualifying SHA — they reference the ledger.

## Known local failures

Two pre-existing `curl_interop` tests fail on some local machines and are
proven machine-specific by four green hosted `verify` jobs. They gate on the
locally installed `curl`. Report them as pre-existing rather than claiming a
clean suite.

## Redaction and fixture testing

Redaction is persistence-safe for configured headers, query keys, and JSON/form
bodies: transformation happens before any finalized blob exists, and
malformed, oversized, or unsupported transformations fail closed. Opaque binary
secret discovery is explicitly unsupported, and fixture readers reject future
schemas, malformed or oversized JSONL, invalid blob names, hash/length
mismatches, symlinked blobs, and incomplete manifests.

The checked-in golden corpus lives in
`crates/eggreplay-store/tests/fixtures/` (schema-1, schema-1-migrated, and
schema-2 fixtures for each registered extension) and
`crates/eggreplay-har/tests/corpus/`. Both directories are excluded from the
published package.
