# Non-goals and unsupported claims

This is the authoritative list of what EggReplay does **not** do. Each entry is
a deliberate boundary with a recorded reason, not a gap awaiting a patch. No
item may be removed without tests that qualify it, and no item may be added
without an ADR or a closure record.

## Deferred by decision

- **HTTP/3 / QUIC** on every path — direct, routed, replay, and intercept.
  Deferred per ADR 0009: no Eggress QUIC route connector, no H3 serving seam,
  and EggFetch's `http3` safety unreviewed. The `protocol-boundary` CI lane
  asserts QUIC/H3 is absent from every supported graph.
- **A generic reverse proxy.** EggReplay replays and compares recorded
  semantics; it is not a forward/reverse proxy product.

## Out of the support claim

- **HTTP/2 interception (MITM).** `eggreplay-intercept` never adopts the
  multiprotocol serving layer; it stays on `eggserve-server` and H1. This is
  asserted in CI, so interception cannot silently gain HTTP/2.
- **WSS** and **extended-CONNECT WebSockets.** Cleartext RFC 6455 over HTTP/1.1
  Upgrade is the qualified WebSocket baseline; the replay handshake path still
  requires HTTP/1.1.
- **Negotiated WebSocket extensions** (`permessage-deflate`, etc.).
- **Wire-frame fidelity.** Frame masking, fragment boundaries, and packet
  layout are not stored, and a message is the unit of authority.
- **Automatic OS/browser CA trust installation.** CA lifecycle is manual and
  operator-driven.
- **Client mTLS interception**, and **certificate-pinned clients**, which are
  expected to fail unless configured for passthrough.
- **Transparent / TUN interception.** Acquisition is explicit-proxy or direct.
- **Inbound HTTP/2 in any default, direct, H1, interception, or Python profile.**
  It is opt-in only, behind `h2-inbound`/`h2-inbound-tls`.

## Redaction limits

- **Opaque binary payloads cannot be semantically redacted.** Redaction covers
  configured headers, query keys, and structured JSON/form bodies with explicit
  selectors. Fixtures remain sensitive data, and access to fixture directories
  must still be restricted.
- **Not implemented, and not planned as features:** entropy scanning, DLP,
  encryption at rest, and arbitrary JSONPath.

## Determinism and scope limits

- **Reactivity is authored, not inferred.** Scenarios are finite state machines
  in a fixture; there is no live scripting against process or host state.
- **`append-new` does not acquire new WebSocket conversations.**
- **Segmented packet behaviour** — arbitrary corruption, TCP flag manipulation,
  and kernel-level emulation — is out of scope. The authored `ScenarioFault`
  model is the supported alternative.
- **HAR is a lossy interchange, never canonical.** Both directions are lossy by
  design and never imply round-trip losslessness; see
  [`har-interchange.md`](har-interchange.md).

## Milestone history

`docs/non-goals.md` previously described the v0.1 horizon, listing Python
bindings, HAR interchange, scenarios, streaming timing, and TLS interception as
unavailable. All of those have since been implemented and qualified through
M017. The current status is in [`../plans/registry.md`](../plans/registry.md);
per-milestone closure records are under `plans/closure/`.
