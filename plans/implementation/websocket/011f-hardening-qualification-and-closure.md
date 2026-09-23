# M011F — WebSocket Hardening, Qualification, and Milestone Closure

Status: closed
Depends on: M011E
Parent milestone: M011

## Objective

Re-audit the completed WebSocket surface, close security/resource/portability
gaps, produce hosted evidence, and close M011 only if the declared support
matrix is true.

## A. Support matrix

The closure must state exact support rather than infer it.

Expected initial tier:

- RFC 6455 over HTTP/1.1 Upgrade: supported after qualification;
- inbound EggReplay `ws://`: supported;
- outbound `ws://`: supported;
- outbound `wss://`: supported only if M011A/E local trust-root tests pass;
- inbound `wss://`: unsupported unless a separately qualified EggServe TLS
  composition is deliberately included;
- H2 Extended CONNECT WebSocket: deferred to M014B;
- H3 WebSocket: deferred;
- permessage-deflate/other negotiated extensions: unsupported/declined;
- fragmentation/masking/wire layout fidelity: not claimed;
- semantic text/binary/ping/pong/close fidelity: supported within limits.

## B. Security review

Verify:

- no handshake key/accept literal becomes match authority;
- auth/cookie headers remain C003-redacted;
- configured WebSocket payload redaction occurs before durable blob
  publication;
- no secret payload appears in error/debug/JSON/JUnit output;
- unsupported extensions fail closed;
- invalid UTF-8/control frames/close codes are rejected safely;
- payload/body refs remain confined and symlink-safe;
- no public listener trust-store mutation or TLS interception is introduced.

Run full-directory sentinel tests after representative secret-bearing
conversations.

## C. Resource/backpressure review

Prove explicit bounds for:

- active tunnels/conversations;
- concurrent conversation tasks;
- messages and aggregate payload;
- message size;
- metadata extension size;
- maximum duration;
- timing sleeps;
- diagnostics;
- control-frame flood.

Use direct backpressure; inspect the implementation for unbounded channels,
fixture-wide payload materialization, detached tasks, and lock spans across
network waits.

## D. Lifecycle/portability

Qualify:

- abrupt client disconnect;
- abrupt upstream disconnect;
- server shutdown with active conversation;
- cancellation while blocked on read/write/timing;
- no leaked temporary blobs/extensions;
- Windows rename/finalization behavior;
- macOS/Linux behavior;
- Rust 1.89.

## E. Dependency audit

Final M011 dependency rules:

- `eggreplay-core` and `eggreplay-store` contain semantic/store logic but no
  WebSocket codec/network runtime dependencies;
- WebSocket codec is optional and isolated to `eggreplay-http`;
- no second HTTP client/server;
- EggServe registry migration remains in place;
- EggFetch remains outbound authority;
- Eggress remains narrow `pproxy-compat` only;
- do not upgrade to an Eggress release whose upstream qualification is still
  blocked.

Run cargo tree guards to enforce this.

## F. Interoperability fixtures

Use at least two independent local WebSocket implementations where practical,
not only EggReplay talking to itself. One may be the selected Rust codec test
peer; use another independent implementation/library or raw conformance fixture
without public Internet.

Cover subprotocols, control frames, fragmented inbound messages, leading bytes,
close behavior, and WSS if claimed.

## G. Verification

Run at minimum:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
git diff --check
```

Preserve the existing hosted matrix:

- Ubuntu stable;
- Ubuntu Rust 1.89;
- macOS stable;
- Windows stable;
- dependency-boundary.

Add focused feature jobs only if the ordinary matrix does not exercise the
optional WebSocket feature.

## H. Documentation/closure

Update:

- README support status;
- `docs/architecture.md`;
- `docs/cli.md`;
- `docs/eggr-schema.md`;
- security/non-goals docs;
- roadmap/registry/planning README.

Create `plans/closure/m011-websocket-semantic-record-replay.md` containing:

- implementation SHA(s);
- exact dependency versions/revisions;
- test counts by platform;
- Actions run IDs/URLs;
- support matrix;
- known limitations;
- evidence for all subplan closures.

Only after this record and hosted green evidence:

- mark M011 and M011F closed;
- mark M012 ready;
- leave M013/M014 blocked by dependency order.

## Acceptance

M011 closes only if recorded conversations are crash-safe/redaction-safe,
offline replay is deterministic, candidate regression uses the same semantic
authority, lifecycle/backpressure bounds are proven, and the advertised
cross-platform support matrix is backed by hosted evidence.
