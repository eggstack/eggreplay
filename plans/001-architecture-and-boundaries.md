# 001 — Architecture and Ownership Boundaries

Status: canonical

## Crate topology

```
eggreplay-core
  canonical flow/session/conversation types
  normalization + matcher/scenario model
  semantic diff/report model
  redaction vocabulary
  no sockets/filesystem/Python/interception runtime

eggreplay-store
  .eggr manifest/JSONL/blob persistence
  schema validation + migrations
  streaming blob reader/writer
  crash-safe finalization
  no Python/interception runtime

eggreplay-http
  EggFetch outbound adapter
  EggServe inbound record/replay adapter
  body/frame/WebSocket orchestration
  optional Eggress-backed Dialer adapter
  no Python runtime

eggreplay-intercept (M013)
  optional explicit forward-proxy acquisition
  CONNECT policy / opaque relay
  dedicated CA + leaf issuance
  client-side TLS termination
  leaf adapter; never depended on by core/store/http/Python

eggreplay-cli
  record / serve / replay / test / diff / inspect / validate
  optional interception commands behind explicit feature

eggreplay-python (M012)
  PyO3 adapter over existing Rust authorities
  Python package/pytest/runtime bridge
  may depend on core/store/http
  must not be depended on by Rust product crates
```

`eggreplay-core`, `eggreplay-store`, and `eggreplay-http` must never gain
PyO3/Python dependencies. The Python crate is a leaf adapter.

`eggreplay-intercept` is a privileged leaf adapter. Existing product crates
must not depend upward on it.

`eggreplay-intercept` is a separate privileged leaf. Existing product crates and
the default Python wheel must not depend upward on it.

## EggFetch boundary

EggFetch is outbound HTTP authority. Current seams include native body
execution, custom Dialer, structured failures, destination TLS, pooling, H1
upgrade ownership, and HTTP framing. M013 intercepted upstream requests use
normal EggFetch verification; installing the EggReplay CA never changes
upstream trust.

## EggServe boundary

EggServe is inbound HTTP service/runtime authority for gateway recording,
replay/mock serving, generic H1 tunnel handoff, and caller-owned H1 execution.
M013 terminates client TLS outside EggServe and passes the decrypted async
stream plus truthful HTTPS/TLS `ConnectionContext` into that driver. Do not
import `eggserve-core` or create a private Hyper server merely for interception.

## Eggress boundary

Use `eggress-outbound::OutboundConnector` for optional listener-free routes.
Eggress owns raw route establishment for CONNECT passthrough and remains the
optional physical route under EggFetch for intercepted semantic requests.
Configured routes never fall back to direct.

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

## Interception boundary

ADR 0008 governs M013.

EggServe owns inbound H1 parsing, CONNECT tunnel handoff, and caller-owned H1
execution after TLS termination. Eggress owns raw CONNECT route establishment.
EggFetch owns semantic upstream HTTP/TLS with ordinary certificate and hostname
verification. EggReplay interception code owns only target policy, CA/leaf
lifecycle, authority coherence, and acquisition orchestration.

Opaque CONNECT passthrough never becomes a semantic HTTP flow. Initial MITM is
HTTP/1.1 only and must not advertise H2. CA/private leaf key material remains
outside `.eggr` and routine diagnostics.

Python remains interception-free in M013 so the qualified default abi3 wheel
does not acquire CA-generation dependencies.

## Interception boundary

ADR 0008 governs M013.

CONNECT passthrough bytes are opaque and never become semantic HTTP flows.
Interception is policy-selected per target. CA/key material is operational
state outside `.eggr`.

Initial MITM is HTTP/1.1 only and advertises only `http/1.1`. H2/H3/WSS/mTLS
interception is not part of M013.

## Flow/conversation authority

EggReplay owns request/response semantics, errors, routes, timestamps,
annotations, redaction markers, stream events, scenarios, and WebSocket
conversation semantics.

ADR 0006 governs WebSockets, ADR 0007 Python binding/runtime ownership, and
ADR 0008 interception security/transport ownership.
ADR 0008 governs interception security and transport ownership.

No plugin ABI or scripting engine is required for v0.1.
