# M011 — WebSocket Semantic Record, Replay, and Regression Closure

Status: closed

## Implementation revisions and dependencies

- Feature implementation: `ec5a6ff678f7278608ca8447cb6b75c2ee574a0a`
- Dependency-boundary guard correction:
  `63d9e6cbd8d3dc6373a1ae84fb64d2aa503c62cf` (regex now matches exact
  crate names; it no longer mistakes EggFetch's `base64ct` PEM dependency for
  the WebSocket `base64` codec helper).
- Transport versions: EggServe 0.2.1, EggFetch 0.2.0, `tokio-tungstenite`
  0.30.0 / `tungstenite` 0.30.0, exact pinned versions in Cargo.lock. Core
  and store remain free of codec/network runtime dependencies. Eggress remains
  limited to `pproxy-compat`.

## Qualification

- Local gates passed: `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features --locked`,
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
  `cargo test --workspace --all-features --locked` (132 passed),
  `cargo audit` (no advisories), and `git diff --check`.
- Hosted Actions run [35880303725](https://github.com/eggstack/eggreplay/actions/runs/35880303725)
  passed all jobs on revision `63d9e6c`: Ubuntu stable, Ubuntu Rust 1.89,
  macOS stable, Windows stable, and dependency-boundary. Each platform ran the
  full workspace test suite successfully (132 tests); check, Clippy, and format
  also passed on each platform.
- Dependency-boundary checks passed for core/store isolation, direct-only
  HTTP, optional WebSocket codec, and narrow Eggress routing.
- Local scripted-peer tests exercise two independent WebSocket sides: raw
  upstream handshake/leading bytes and `tokio-tungstenite` client behavior.
  Codec tests separately characterize fragmentation, control replies, and
  limits. The CI matrix verifies macOS/Linux/Windows and Rust 1.89 finalization
  behavior.

## Supported protocol matrix

| Capability | M011 support |
|---|---|
| RFC 6455 over HTTP/1.1 Upgrade | Supported semantically within configured bounds |
| Inbound `ws://` recording/replay | Supported; recording is opt-in |
| Outbound direct/routed `ws://` | Supported through EggFetch and optional Eggress routing |
| Outbound `wss://` | Unsupported; no caller-owned local trust-root qualification was established |
| Inbound WSS/TLS interception | Unsupported |
| H2 Extended CONNECT / H3 | Deferred to later compatibility plans |
| Negotiated extensions, including permessage-deflate | Declined/unsupported |
| Fragmentation, masking, frame boundaries, packet layout | Not preserved or claimed |
| Text, binary, ping, pong, close semantics | Supported with bounded replay/regression |
| Append-new acquisition | Unsupported; tunnel-aware append-on-miss capture is not implemented |

## Security and resource notes

- Volatile key/accept values are not matching authority. Upstream accept is
  verified; inbound accept is regenerated.
- Header and payload redaction precede durable publication. Persisted and
  report output contains safe markers/digests; live traffic is not mutated by
  storage redaction.
- Upgrades with invalid semantics, request bodies, unsupported extensions, or
  invalid selected subprotocol fail closed.
- Relay paths use direct backpressure and explicit active-tunnel, concurrency,
  message, byte, message-size, duration, transcript, and timing limits. No
  unbounded WebSocket message queue or fixture-wide payload materialization is
  used.
- Shutdown/EOF/reset are represented as abnormal terminal states. Session
  transcript and flow links are validated at finalization/open.

## Known limitations

- WebSocket acquisition in `serve` is supported for once and re-record modes;
  append-new rejects it. Recorded WebSocket flows remain available to sealed
  replay and candidate testing.
- WSS, inbound TLS, H2/H3, compression/extensions, frame-level fidelity, and
  arbitrary conversation scripting are outside this milestone.
- The support claim is RFC 6455 semantic interoperability, not conformance to
  every RFC edge case or preservation of original wire framing.

## Subplan closures and handoff

- M011A — transport/dependency preflight:
  `plans/closure/m011a-transport-dependency-and-upgrade-preflight.md`.
- M011B — semantic model/store/codec:
  `plans/closure/m011b-semantic-model-store-and-codec.md`.
- M011C — gateway:
  `plans/closure/m011c-websocket-recording-gateway.md`.
- M011D — offline replay:
  `plans/closure/m011d-websocket-offline-replay.md`.
- M011E — candidate regression, CLI, and diff:
  `plans/closure/m011e-websocket-candidate-regression-cli-and-diff.md`.
- M011F — hardening and hosted qualification: this record and run 35880303725.

All M011 subplans and the umbrella are closed. M012 is ready; M013 and later
plans remain blocked by their declared dependency order.
