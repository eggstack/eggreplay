# Protocol and Interop Roadmap

Status: active roadmap

## v0.1

HTTP/1.1 semantic recording/replay/regression is the required contract, including trailers, streaming bodies, HEAD/body-forbidden semantics, cancellation, and transport errors.

## HTTP/2

EggFetch and EggServe have relevant H2 capability, but EggReplay claims H2 only after end-to-end multiplexing, concurrency, trailer, cancellation, and target-remap evidence.

## HTTP/3

Deferred. QUIC does not fit the TCP custom-Dialer path and needs a separate route/integration design.

## SSE

Authoritative data remains raw HTTP body bytes. M010 may add a derived event view and timing comparisons.

## WebSocket

M011 uses EggFetch upgrade streams plus EggServe tunnel handoff and attaches ordered message records to the initiating flow.

## gRPC

Transport-level H2 may eventually work without semantic decoding. Protobuf/gRPC matchers are optional future work.

## Imports

HAR and third-party fixture import/export are lossy adapters, never the canonical store. Loss must be reported.
