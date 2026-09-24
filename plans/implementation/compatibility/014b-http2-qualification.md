# M014B — HTTP/2 End-to-End Qualification

Status: blocked
Depends on: M013 closure, M010
Parent: M014

## Objective

Determine and, if evidence permits, promote EggReplay HTTP/2 recording,
replay, and regression using EggFetch/EggServe's existing H2 capabilities.

EggServe currently classifies its own H2 support as experimental; EggReplay
must not claim more than the weakest dependency/integration tier.

## Dependency work

Update/pin EggServe/EggFetch versions only after verifying the required public
H2 service/body/trailer/lifecycle seams. Do not fork protocol stacks.

Eggress TCP custom dialing can support H2-over-TLS only if logical SNI/ALPN
ownership remains in EggFetch; verify this explicitly.

## Required behavior

Qualify:

- ALPN h2 over local TLS;
- cleartext prior-knowledge only if intentionally exposed;
- multiple concurrent streams on one connection;
- request/response trailers;
- large streaming bodies and backpressure;
- per-stream cancellation/reset without corrupting siblings;
- target remapping;
- strict/practical matching;
- scenario behavior;
- M010 event/timing behavior under multiplexing;
- regression findings;
- graceful shutdown/GOAWAY behavior;
- routed H2 through an Eggress TCP route.

No HTTP/1 connection-specific headers may leak into H2 semantics.

## Interception

Do not enable H2 MITM merely because direct H2 passes. Intercepted H2 needs its
own ALPN/caller-owned-connection evidence and may remain unsupported.

## Evidence

Use at least two independent H2 client families where practical, plus
EggFetch. Tests must be local/deterministic. Record Linux/macOS/Windows
availability separately.

If EggServe remains experimental, EggReplay may expose an experimental feature
but must not label it generally supported.

## Closure

Create `plans/closure/m014b-http2-qualification.md` with a support-tier
decision and exact dependency revisions.
