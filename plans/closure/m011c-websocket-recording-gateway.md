# M011C — WebSocket Recording Gateway Closure

Status: closed

## Implementation

- Implementation commit: `ec5a6ff678f7278608ca8447cb6b75c2ee574a0a`
- Uses EggServe 0.2.1 tunnel ownership, EggFetch 0.2.0 outbound upgrade and
  owned stream, Eggress narrow `pproxy-compat` routing, and the optional
  `tokio-tungstenite` 0.30.0 codec from M011B.
- Acquisition is opt-in. HTTP/1.1 websocket requests are validated before
  upstream acquisition; ordinary requests retain the existing streaming path.
- A successful upstream upgrade is validated, the inbound accept value is
  derived from the inbound key, and only selected subprotocol is mirrored.
  Extensions are declined and unexpected negotiation fails closed.
- Relay backpressure is direct, with bounded message/frame size, active
  tunnels, duration, message/byte totals, and transcript metadata. Close,
  ping/pong, EOF, and reset are captured as semantic terminal events.
- Redaction is applied to stored payloads only; live payloads pass unchanged.
  The end-to-end gateway test verifies that the secret is transmitted but not
  present in persisted blobs.
- Session finalization publishes and cross-validates the required transcript
  together with its HTTP upgrade flow.

## Verification

The full local workspace gates passed on `ec5a6ff`: format, all-target/all-
feature check, Clippy with warnings denied, and 132 tests. Focused local tests
cover bidirectional text/binary and ping/pong relay, clean close from either
side, abnormal EOF, standard client handshake, selected subprotocol, leading
post-101 bytes, persisted redaction, and fixture reopen. Hosted matrix evidence
is recorded in the M011 umbrella closure.

## Scope note

`record` and `serve` once/re-record support opt-in WebSocket acquisition.
`serve --record-mode append-new --websockets` is rejected because append-on-
miss currently has no tunnel-aware upstream capture path. Existing recorded
WebSocket flows can still be replayed in sealed mode. This limitation is
documented in the CLI and does not introduce a fallback transport.

M011D is closed and its implementation is recorded in the next subplan
closure.
