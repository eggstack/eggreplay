# M011A — Transport Dependency and Upgrade Preflight Closure

Status: closed

## Implementation

- Commit: `e9fb093a61707edf3cf6846d03ad4654b17ae505`
- EggFetch: crates.io `eggfetch-core 0.2.0`; enabled `native-http1`,
  `high-level-url`, `tls-rustls`, and `tls-native-roots` without default
  features. The high-level URL feature is required for the API that returns
  owned post-101 IO; HTTP/1 is selected explicitly for WebSocket handshakes.
- EggServe: crates.io `eggserve-server 0.2.1` and
  `eggserve-primitives 0.2.0`, pinned exactly.
- Eggress: crates.io `eggress-outbound 1.0.8`, pinned exactly with defaults
  disabled; the workspace enables only `pproxy-compat` through the
  `eggreplay-http/eggress` feature. Eggress 1.0.9 remains excluded because of
  the upstream route-isolation/metadata qualification blockers.
- No `eggserve-core` or parallel HTTP client was introduced.

## Evidence

Local deterministic loopback qualification in
`crates/eggreplay-http/tests/v01_qualification.rs` proves:

- EggFetch H1 returns a 101 response with owned `NetworkStream::Upgraded`
  IO, preserves leading post-101 bytes, supports writes, and closes the owned
  stream.
- Registry EggServe `service_fn_with_tunnel` grants H1 Upgrade capability,
  preserves read-ahead through `TunnelIo`, echoes duplex bytes, and accepts
  the connection-total-timeout disable control while retaining runtime
  shutdown.
- Eggress 1.0.8 routes CONNECT and the H1 upgrade through the configured local
  HTTP proxy, returns the owned upgraded stream, and preserves leading bytes.
- An unavailable configured proxy fails and a reachable origin observes no
  direct-fallback connection.
- The configured EggFetch profile has only native HTTP/1 transport enabled;
  WebSocket handshakes explicitly request `HttpVersionPolicy::Http1Only`.
- The dependency-boundary CI job confirms core/store remain free of Eggstack
  transports and the Eggress feature stays narrow.

Local verification passed:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked  # 110 passed
git diff --check
```

Hosted matrix passed on commit `e9fb093a61707edf3cf6846d03ad4654b17ae505`:
[GitHub Actions run 35869629107](https://github.com/eggstack/eggreplay/actions/runs/35869629107)
(Ubuntu stable, Ubuntu Rust 1.89, macOS stable, Windows stable, and
dependency-boundary).

## Qualified support boundary

- HTTP/1.1 `ws://` upgrade substrate: qualified for direct and Eggress-routed
  acquisition.
- Outbound `wss://`: not claimed; local caller-owned trust-root TLS handshake
  has not been qualified.
- Inbound `ws://`: generic EggServe H1 tunnel handoff is qualified.
- Inbound `wss://`, H2 Extended CONNECT, H3, and negotiated WebSocket
  extensions remain outside this preflight.

No blocking substrate gap remains. M011B is ready.
