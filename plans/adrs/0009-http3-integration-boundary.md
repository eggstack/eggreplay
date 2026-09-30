# ADR 0009 — HTTP/3 Integration Boundary (M014C)

Status: decided (M014C closes with H3 deferred)
Date: 2026-09-30

## Context

M014C must design the QUIC/H3 integration boundary before enabling
HTTP/3. The existing Eggress TCP `Dialer` seam is not valid for QUIC:
QUIC runs over UDP and negotiates transport parameters, 0-RTT, and
connection migration inside the QUIC handshake, none of which a TCP byte
stream can carry. Tunneling QUIC through a TCP Dialer abstraction is
forbidden. M014B closed with experimental outbound H2 on the unchanged
dependency line (`eggfetch-core 0.2.0`, `eggserve-server 0.3.0`,
`eggserve-primitives 0.2.1`, `eggress-outbound 1.0.8`, `eggnet-tls 0.2.0`).

Sibling capabilities surveyed on 2026-09-30:

- `eggfetch-core` publishes an `http3` feature (`http1`, `tls-rustls`,
  `quinn`, `h3`, `h3-quinn`, `high-level-url`) with `HttpVersionPolicy`
  support (`Http3Only`, `Auto { allow_http3 }`). Without the feature,
  H3 policies silently downgrade to H1 (`HttpVersionPolicyEnabler`).
  EggReplay does not enable it.
- `eggress-outbound 1.0.8` exposes only `connect_tcp`,
  `connect_tcp_detailed`, and timeout variants. There is no
  `connect_quic`/`connect_udp` seam on the qualified line; QUIC-capable
  transport lives behind the unadopted `quic` feature
  (`eggress-transport-quic`, `eggress-protocol-h3`).
- `eggserve-h3 0.4.0` exists as an "Experimental EggServe HTTP/3 and
  QUIC transport adapter". The adopted EggServe closure is the direct
  H1-only runtime (`eggserve-server 0.3.0`); even at 0.4.0 EggServe
  states H1 is the supported transport with H2/H3 opt-in experimental.
- QUIC/H3 interception (MITM) is out of scope unless a separate
  security/transport design is approved; M013's HTTP/1.1 MITM claim must
  not expand automatically.

## Options considered

### A. Direct EggFetch H3 endpoint ownership

Enable `eggfetch-core/http3` and let EggFetch own the QUIC endpoint
(UDP socket, TLS, ALPN `h3`) the way it owns TCP/TLS/ALPN for H2.
Rejected for now: it yields direct-only H3 with no routed path (no
QUIC connector seam, option B missing), no inbound replay path
(EggServe direct runtime is H1-only), and unverified safety semantics
for UDP endpoint lifecycle, network migration, and 0-RTT replay risk.
A record-only H3 with no route/replay/intercept story is incoherent.

### B. Eggress QUIC/H3 route connector

Adopt a stable listener-free Eggress QUIC connector analogous to
`OutboundConnector::connect_tcp_detailed`, keeping SNI/ALPN ownership
in EggFetch as M014B proved for TCP. Rejected: no such seam exists on
the qualified 1.0.8 line. The `quic` feature is unadopted and
unqualified; adopting it would also expand the dependency closure
(transport-quic, protocol-h3) without evidence.

### C. Unsupported routed H3 with direct-only H3

Ship direct-only H3 (option A) while declaring routed H3 unsupported.
Rejected: it still requires the missing inbound replay story and the
unreviewed QUIC safety semantics of option A, so it clears nothing at
the architecture gate. A tier that can record but never route, replay,
or intercept is not a support claim.

### D. EggServe H3 service integration

Adopt `eggserve-h3 0.4.0` for inbound replay/gateway. Rejected for now:
the adapter is upstream-experimental, it requires moving off the
qualified 0.3.0/0.2.1 direct-runtime line that M014B deliberately kept,
and it needs its own service/body/trailer/lifecycle qualification that
M014C has no evidence for.

## Decision

**Defer HTTP/3 on all paths (direct, routed, replay, intercept).**
H3 is classified **unsupported**. The missing upstream seams are:

1. No Eggress listener-free QUIC route connector on the qualified line
   (TCP-only `connect_tcp*`; `quic` feature unadopted).
2. No H3 serving seam in the adopted EggServe closure (`eggserve-h3`
   experimental and unqualified; direct runtime H1-only).
3. EggFetch `http3` (quinn/h3) unconsumed: no UDP endpoint, migration,
   or 0-RTT safety review.

Revisit when all three hold: Eggress publishes a stable QUIC connector
on an adopted line, `eggserve-h3` (or successor) matures with
EggReplay-local serving evidence, and EggFetch H3 safety semantics are
reviewed. A blocked support decision is the plan-sanctioned outcome.

## Consequences

- EggReplay enables no QUIC/H3 code paths; `HttpVersionPolicy::Http3Only`
  without the `http3` feature safely downgrades to H1 inside EggFetch.
- `EggressDialer` stays TCP-only; any future QUIC route must add a new
  seam, never reuse the TCP Dialer.
- Docs and the support matrix record H3 as unsupported with this ADR as
  evidence (see `plans/closure/m014c-http3-feasibility-and-qualification.md`).
