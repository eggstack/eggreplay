# M003 — EggFetch Recording Path

Status: closed
Depends on: M002
Release gate: v0.1

## Objective

Record real HTTP transactions into schema-1 flows using EggFetch for outbound HTTP and EggServe for the inbound gateway while preserving streaming and one-flow/one-transaction semantics.

## Architecture

```
client -> EggServe H1 service -> recorder -> EggFetch native request -> upstream
```

The recorder owns observation/storage only.

## Work packages

### Conversion layer

Create explicit loss-aware adapters between EggServe request primitives / standard `http` types and EggReplay core types, and between EggReplay materialized requests and EggFetch native execution. Persisted schema structs never contain EggFetch/EggServe concrete types.

### Streaming request capture

Tee request DATA into the store while forwarding with bounded buffering/backpressure. Preserve trailers. Finalize/abort flow state correctly on cancellation or upstream dispatch failure.

### Streaming response capture

Record response head before body streaming. Tee DATA/trailers to storage while returning them to the client. Recording failure defaults to fail-closed fixture integrity rather than silently producing a truncated successful flow.

### Failure mapping

Map EggFetch detailed failure/timeout facts into EggReplay stable semantic error categories. Preserve unknown/other when the route cannot prove a subtype.

### Gateway seam

Add internal service plumbing needed by later `record --listen --upstream`; polished CLI remains M007.

## Critical behavior

Avoid hidden logical redirects/retries on the native recording path unless emitted as distinct flows. Configure EggFetch's public stale-connection retry control where needed so a single recorded transaction is truthful.

## Tests

Local-only GET/POST, binary streaming upload/download, trailers, repeated headers, HEAD/204, connection refusal, local TLS verification failure where available, cancellation mid-body, recorder/store failure propagation, and concurrent overlapping requests.

## Stop condition

If EggFetch's public native-body surface cannot tee frames without semantic loss or unbounded buffering, stop and document the missing reusable seam. Do not build a parallel HTTP client.

## Acceptance

A local gateway recording can later be served from the store with request/response bytes and trailers intact, and large bodies are never fully buffered.
