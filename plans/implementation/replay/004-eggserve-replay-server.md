# M004 — EggServe Offline Replay Server

Status: closed
Depends on: M002
Release gate: v0.1

## Objective

Serve recorded flows without upstream access using EggServe's supported inbound HTTP service/runtime.

## Work packages

1. Load/validate fixture metadata before readiness.
2. Materialize response blobs as bounded streams.
3. Preserve status, allowed headers, body bytes, and trailers.
4. Isolate replay-session consumption state per server instance.
5. Return deterministic no-match/fixture-exhausted outcomes without leaking fixture secrets.
6. Use EggServe lifecycle for readiness, graceful shutdown, and cancellation.
7. Retain bounded replay observations for M005 diagnostics without mutating the fixture.
8. Respect runtime authority for HEAD, 1xx, 204, 304, and other body-forbidden semantics.

## Matching dependency

M004 may use a deliberately strict temporary selector to prove serving. It must not grow a second matcher; M005 becomes the authority.

## Tests

Offline replay with upstream disconnected; large streaming body; trailers; repeated response headers; HEAD/204; concurrent clients; fixture exhaustion; corrupt fixture refusal before readiness; graceful shutdown with active streams.

## Acceptance

A recorded interaction can be satisfied entirely from `.eggr` with bounded memory. The pure offline path does not require EggFetch.
