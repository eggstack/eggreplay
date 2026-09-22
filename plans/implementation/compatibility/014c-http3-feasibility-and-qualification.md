# M014C — HTTP/3 Feasibility and Qualification

Status: blocked
Depends on: M014B
Parent: M014

## Objective

Design the QUIC/H3 integration boundary before enabling HTTP/3. The existing
Eggress TCP Dialer seam is not valid for QUIC.

## Architecture gate

Document an ADR comparing:

- direct EggFetch H3 endpoint ownership;
- an Eggress QUIC/H3 route connector if a stable listener-free seam exists;
- unsupported routed H3 with direct-only H3;
- any required EggServe H3 service integration.

Do not tunnel QUIC through a TCP Dialer abstraction.

## Qualification

If the architecture gate is viable, cover:

- local QUIC/TLS handshake and ALPN;
- semantic headers/body/trailers;
- concurrent streams;
- cancellation;
- large streaming payloads;
- target remap;
- regression;
- scenario/timing extensions;
- network migration/0-RTT only if dependencies expose safe semantics.

Otherwise close the plan with H3 explicitly deferred and the missing upstream
seam documented; a blocked support decision is an acceptable result.

## Interception

QUIC/H3 MITM remains out of scope unless a separate security/transport design
is approved. M013's HTTP/1.1 MITM claim must not expand automatically.

## Closure

Create `plans/closure/m014c-http3-feasibility-and-qualification.md` with the
architecture decision and support classification.
