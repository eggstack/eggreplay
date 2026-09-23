# M011C — WebSocket Recording Gateway

Status: blocked
Depends on: M011B
Parent milestone: M011

## Objective

Record RFC 6455 conversations through the existing EggServe inbound and
EggFetch outbound authorities while preserving bounded backpressure, redaction,
and truthful termination.

## A. Admission and inbound handshake

Extend the recording gateway through EggServe's tunnel-capable service path.

A WebSocket acquisition is accepted only when:

- HTTP/1.1 Upgrade intent is valid;
- method/target/header bounds pass;
- `Upgrade: websocket` and version 13 are valid;
- no request body crosses the upgrade boundary;
- M011 WebSocket acquisition is explicitly enabled by CLI/config.

Use codec/library helpers to validate the client key and derive
`Sec-WebSocket-Accept`; do not hand-roll cryptographic handshake logic.

Keep ordinary non-WebSocket requests on the existing recording path.

## B. Upstream handshake through EggFetch

Construct an H1 WebSocket handshake through the EggFetch 0.2.0 path qualified
by M011A.

- preserve target, cookies/auth, Origin, and subprotocol offers subject to the
  existing route/redaction policy;
- generate an independent upstream `Sec-WebSocket-Key`;
- require status 101 and valid Upgrade/Connection/Accept response semantics;
- require selected subprotocol to have been offered;
- omit `Sec-WebSocket-Extensions` from the upstream offer in initial M011;
- fail closed if upstream nevertheless returns a negotiated extension;
- extract the owned `UpgradedStream`;
- preserve leading data;
- use the same Eggress Dialer path for routed acquisition with no direct
  fallback.

The inbound 101 is generated from the client's key and mirrors only semantic
upstream results such as selected subprotocol. Never forward the upstream
`Sec-WebSocket-Accept` literal to the client.

## C. Long-lived lifecycle

EggServe 0.2.1's default total connection lifetime must not silently truncate
WebSockets. Disable only that generic total lifetime for the upgraded service
and enforce EggReplay's explicit bounded maximum conversation duration.

Retain independent admission/shutdown controls. Server shutdown cancels active
conversation tasks and produces truthful abnormal termination instead of an
invented clean close.

## D. Bidirectional relay

After both handshakes succeed:

1. wrap inbound `TunnelIo` in server-role WebSocket codec;
2. wrap outbound EggFetch `UpgradedStream` in client-role codec;
3. relay both directions concurrently;
4. await each sink write/flush for direct backpressure;
5. assign one atomic/serialized global sequence across both directions;
6. timestamp observations from one monotonic upgrade-complete origin;
7. persist payloads through independent bounded staging body writers;
8. accumulate only bounded metadata in memory;
9. record ping, pong, and close as semantic messages.

No unbounded message channel is permitted.

## E. Redaction-before-publication

Handshake headers continue through C003.

For WebSocket payloads:

- valid UTF-8 text that parses as JSON may apply existing configured JSON
  Pointer redaction before blob publication;
- non-JSON text is unchanged unless an explicit whole-text redaction option is
  enabled;
- binary payloads are unchanged unless an explicit whole-binary redaction
  option is enabled;
- ping/pong/close reasons follow the same explicit whole-message policy where
  applicable;
- whole-message redaction stores a typed marker and never publishes the raw
  staging payload.

Add CLI/config names consistent with existing redaction options, e.g.
`--redact-websocket-text` and `--redact-websocket-binary`. Exact spelling may
follow established clap conventions.

Redaction markers must later drive wildcard message matching; never compare a
literal placeholder as secret authority.

## F. Finalization and atomicity

The initiating HTTP flow and WebSocket conversation must become valid fixture
authority together at session finalization.

- do not publish a valid manifest with a WebSocket 101 flow lacking required
  conversation metadata;
- do not leave conversation metadata pointing at a missing flow/blob;
- cancelled/failed handshakes append neither a successful 101 conversation nor
  incomplete payload refs;
- abnormal post-upgrade termination is recorded as abnormal terminal state;
- clean close is recorded only after valid close semantics.

## G. CLI

Extend `record`/network-capable `serve` with an explicit WebSocket acquisition
toggle/policy. Default should not unexpectedly expand the existing HTTP-only
security surface.

Machine-readable output reports whether WebSocket acquisition was enabled and
conversation counts, never payload contents/secrets.

## Required tests

Local deterministic scripted peers must cover:

- ordinary HTTP path unchanged;
- successful text/binary relay;
- client and server ping/pong;
- normal close from either side;
- abnormal EOF/reset;
- concurrent bidirectional messages;
- leading post-101 bytes;
- selected subprotocol;
- rejected invalid key/version/subprotocol;
- extension offer is declined and negotiated extension fails closed;
- routed acquisition through Eggress;
- no-direct-fallback route failure;
- backpressure with bounded large messages;
- cancellation/server shutdown;
- maximum duration;
- JSON redaction before blob publication;
- whole text/binary redaction sentinel scans;
- final fixture reopen/cross-validation.

## Closure

Create `plans/closure/m011c-websocket-recording-gateway.md`.
M011D remains blocked until recording produces stable canonical fixtures.
