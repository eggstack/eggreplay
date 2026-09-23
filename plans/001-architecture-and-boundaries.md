# 001 — Architecture and Ownership Boundaries

Status: canonical

## Initial crate topology

```
eggreplay-core
  canonical flow/session types
  normalization + matcher model
  replay consumption/state model
  semantic diff model
  redaction vocabulary
  no sockets/filesystem runtime

eggreplay-store
  .eggr manifest/JSONL/blob persistence
  schema validation + migrations
  streaming blob reader/writer
  crash-safe finalization

eggreplay-http
  EggFetch outbound adapter
  EggServe inbound record/replay adapter
  body/frame taps and error mapping
  optional Eggress-backed Dialer adapter

eggreplay-cli
  record / serve / replay / test / diff / inspect / validate
  config and presentation
```

Do not create more crates until evidence shows a real ownership boundary. `eggreplay-core` must not depend on EggFetch, EggServe, Eggress, Tokio, Hyper, filesystem APIs, or CLI libraries.

## EggFetch boundary

EggFetch is outbound HTTP authority. Current documented seams include native `execute_http_body()`, DATA/trailer-preserving body APIs, custom `Dialer`, structured failures, destination TLS, pooling, and HTTP framing.

Gateway recording should prefer the native one-shot path so one EggReplay flow corresponds to one actual HTTP transaction. Hidden redirects/retries must not collapse multiple network transactions into one semantic record.

## EggServe boundary

EggServe is inbound HTTP service/runtime authority for gateway recording and replay/mock serving. Prefer the smallest currently supported published surface that provides bounded H1 request bodies, streaming responses, trailers, lifecycle/cancellation, and custom service behavior.

Do not duplicate listener lifecycle, parser limits, HTTP framing, keep-alive, or server TLS machinery. H2/H3 remain qualification-gated.

## Eggress boundary

Use `eggress-outbound::OutboundConnector` for optional listener-free routes. Adapt the returned stream to EggFetch's custom `Dialer`. EggReplay may report route metadata but does not implement SOCKS, CONNECT, SSH, chains, or relays.

## Canonical data flow

Gateway recording:

```
application -> EggServe -> EggReplay recorder -> EggFetch -> target
                                      \-> Eggress Dialer (optional)
```

Offline replay:

```
application -> EggServe -> normalizer -> matcher -> consumption -> recorded response
```

Regression:

```
.eggr -> materializer -> EggFetch -> candidate
                    \-> Eggress Dialer (optional)
candidate flow -> diff -> report
```

## Flow authority

EggReplay owns request head/body/trailers, response head/body/trailers or semantic error, logical origin vs physical route, timestamps, protocol/capture provenance, annotations, and redaction markers.

Application-visible HTTP body events are not TCP packet boundaries. M011 WebSocket messages attach to the initiating HTTP Upgrade flow through the required ADR-0006 conversation extension; EggFetch/EggServe retain transport ownership and the WebSocket codec remains an `eggreplay-http` adapter concern. Future interception feeds the same flow writer rather than creating a second model.

No plugin ABI or scripting engine is required for v0.1.
