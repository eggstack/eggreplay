# 001 — Architecture and Ownership Boundaries

Status: canonical

## Crate topology

```
eggreplay-core
  canonical flow/session/conversation types
  normalization + matcher/scenario model
  semantic diff/report model
  redaction vocabulary
  no sockets/filesystem/Python runtime

eggreplay-store
  .eggr manifest/JSONL/blob persistence
  schema validation + migrations
  streaming blob reader/writer
  crash-safe finalization
  no Python runtime

eggreplay-http
  EggFetch outbound adapter
  EggServe inbound record/replay adapter
  body/frame/WebSocket orchestration
  optional Eggress-backed Dialer adapter
  no Python runtime

eggreplay-cli
  record / serve / replay / test / diff / inspect / validate
  config and presentation

eggreplay-python (M012)
  PyO3 adapter over existing Rust authorities
  Python package/pytest/runtime bridge
  may depend on core/store/http
  must not be depended on by Rust product crates
```

`eggreplay-core`, `eggreplay-store`, and `eggreplay-http` must never gain
PyO3/Python dependencies. The Python crate is a leaf adapter.

## EggFetch boundary

EggFetch is outbound HTTP authority. Current seams include native body
execution, custom Dialer, structured failures, destination TLS, pooling, H1
upgrade ownership, and HTTP framing.

## EggServe boundary

EggServe is inbound HTTP service/runtime authority for gateway recording,
replay/mock serving, and generic H1 tunnel handoff. Do not duplicate listener
lifecycle, parser limits, HTTP framing, keep-alive, or server TLS machinery.

## Eggress boundary

Use `eggress-outbound::OutboundConnector` for optional listener-free routes.
EggReplay reports route metadata but does not implement SOCKS, CONNECT, SSH,
chains, or relays.

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

Python (M012):

```
pytest/application -> eggreplay Python ergonomics -> eggreplay._native
                                              -> existing Rust authorities
```

Python never becomes an alternate path around the Rust store/matcher/network
stack. The plugin may select a fixture path, acquire an exclusive writer
lock, and manage server startup/shutdown. It cannot interpret or publish
fixture data itself. Relative plugin paths are confined to pytest's root;
shared fixtures use an explicit absolute path. Read-only xdist workers may
share a fixture, while writers to one path fail closed.

The package uses a process-wide Tokio bridge. Explicit `aclose()` and context
manager exit own deterministic cleanup. Dropping an unclosed replay object is
covered by a subprocess interpreter-exit qualification; recordings still
require explicit close to publish their completed fixture.

## Flow/conversation authority

EggReplay owns request/response semantics, errors, routes, timestamps,
annotations, redaction markers, stream events, scenarios, and WebSocket
conversation semantics.

ADR 0006 governs WebSockets. ADR 0007 governs Python binding/runtime ownership.

No plugin ABI or scripting engine is required for v0.1.
