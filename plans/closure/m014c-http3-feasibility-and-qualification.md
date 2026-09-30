# M014C — HTTP/3 Feasibility and Qualification Closure

Status: closed (H3 deferred with documented missing seams)

## Architecture decision

ADR `plans/adrs/0009-http3-integration-boundary.md` compares the four
plan-mandated options and defers HTTP/3 on all paths:

- **Direct EggFetch H3 endpoint ownership**: possible via
  `eggfetch-core/http3` (quinn/h3/h3-quinn, already published), but
  rejected — direct-only H3 with no routed, replay, or intercept story
  is incoherent, and QUIC UDP/migration/0-RTT safety is unreviewed.
- **Eggress QUIC/H3 route connector**: rejected — no stable
  listener-free seam exists on qualified `eggress-outbound 1.0.8`
  (TCP-only `connect_tcp*`; `quic` feature unadopted).
- **Direct-only H3 with routed H3 unsupported**: rejected — clears
  nothing (still no replay path, still unreviewed QUIC semantics).
- **EggServe H3 service integration** (`eggserve-h3 0.4.0`,
  upstream-experimental): rejected — requires leaving the qualified
  0.3.0/0.2.1 direct-runtime line with no serving evidence.

QUIC is never tunneled through the TCP `Dialer` abstraction
(`EggressDialer` stays TCP-only by construction). QUIC/H3 MITM remains
out of scope; M013's HTTP/1.1 MITM claim does not expand.

## Support classification

H3 is **unsupported** on every path: direct, routed, replay, and
intercept. No QUIC/H3 code paths are enabled; `HttpVersionPolicy`
H3 variants safely downgrade to H1 inside EggFetch while the `http3`
feature stays off. A blocked support decision is the plan-sanctioned
acceptable result.

## Change surface

Docs-only milestone. No dependency moves, no product code, no new
tests: `plans/adrs/0009-http3-integration-boundary.md` (decision),
this closure record, registry/roadmap/baseline/README status updates.

Hosted CI run:
[36773818886](https://github.com/eggstack/eggreplay/actions/runs/36773818886)
(docs-only push shared with the M014B closure; standard matrix must stay
green — number filled after green, substance unchanged).

## Handoff

M014C is closed (deferred). M014D is ready (M014B and M010 closed).
The M014 umbrella remains open until M014D closes.
