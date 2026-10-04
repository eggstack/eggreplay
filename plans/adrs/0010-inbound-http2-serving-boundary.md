# ADR 0010 — Inbound HTTP/2 Serving Boundary (M015A)

Status: decided
Date: 2026-10-04
Supersedes: nothing. Extends ADR 0002 (transport ownership) and ADR 0009
(H3 integration boundary) with the opt-in inbound-H2 closure adopted by
M015A and implemented by M015B.

## Context

M014B qualified **outbound** HTTP/2 through EggFetch
(`eggfetch-core/native-http2`, ALPN `h2` over local TLS) and closed with
inbound H2 explicitly unsupported: the then-adopted EggServe closure was the
direct runtime, and no public seam admitted H2 serving. Stage 11 (M015) needs
one coherent HTTP/2 path across acquisition, offline replay, and candidate
regression, which requires an **inbound** serving authority.

M015A is both the adoption milestone and the architecture gate. It requires
proving the boundary from public seams and, if that is impossible, stopping
M015B and recording the blocker rather than forking a server.

The published line surveyed on 2026-10-04 (registry releases, not sibling
`main`):

| Crate | Adopted | Note |
|---|---|---|
| `eggfetch-core` | 0.2.0 → **0.2.2** | adds `Error::transport_failure_kind()` |
| `eggserve-primitives` | 0.2.1 → **0.2.2** | adds head-time `TrailerDeclaration` |
| `eggserve-server` | 0.3.0 → **0.4.0** | additive only (`ResponseMetadataOwnership`) |
| `eggserve-core` | not adopted → **0.4.0 (opt-in)** | multiprotocol composition layer |
| `eggserve-static` | not adopted | arrives transitively with `eggserve-core` |
| `eggress-outbound` | 1.0.8 → **1.0.11** | adds `clear_h2_pool_registries`, connect metadata |
| `eggnet-tls` | **0.2.0** (retained) | already the EggServe identity seam |

### Verified facts about the published seam

1. **`eggserve-server 0.4.0` cannot serve H2.** Its feature table declares
   `http2 = []` and `tls = []` — the features exist for compatibility
   signalling but enable nothing. The direct runtime is H1-only by
   construction, so "just add a flag to the direct runtime" is not available.
2. **`eggserve-core 0.4.0` is the H2 authority.** Its `http2` feature enables
   `hyper/http2` and `hyper-util/http2`; `tls` enables `eggnet-tls`,
   `rustls`, and `tokio-rustls`; `http3` additionally requires `tls` and
   `eggserve-h3`. `RuntimeConfigBuilder::http2(Http2Config)` is public, and
   `Http2Config` carries EggServe-owned limits (concurrent streams, header
   list size, frame size, flow-control windows, reset thresholds, keep-alive
   PING) that are sent to Hyper explicitly.
3. **There is one service contract, not two.**
   `eggserve_core::server::Service` is a compatibility re-export of
   `eggserve_server::service::Service`, and `eggserve_core::server::Request`
   is `eggserve_primitives::Request` — the identical type the existing H1
   replay/gateway service already implements. `eggserve_core::server::server`
   re-exports `service_fn`, `ServiceFuture`, `ServiceError`,
   `RequestBodyPolicy`, and `TunnelCapability` from the same module.
4. **The H1 semantics are preserved by construction.** EggServe Core projects
   its runtime onto the direct H1 connection with
   `http1_request_target_mode: OriginOnly`, `policy_ownership: default()`, and
   `admission_ownership: default()` — the same `eggserve_owned()` policy
   EggReplay configures today for sealed replay. Selecting Core therefore does
   not silently weaken the H1 request-target or ownership boundary.
5. **`eggserve-static` is a non-optional dependency of `eggserve-core`.** Any
   graph that admits Core also admits Static. That closure is accepted *only*
   inside the opt-in H2 graph.
6. **Eggress 1.0.11 is TCP-shaped and stays that way.** `OutboundConnector`
   still exposes TCP as its generic caller-owned route seam with UDP
   association as a separate bounded API. The `quic` feature
   (`eggress-transport-quic`, `eggress-protocol-h3`) remains unadopted, and
   1.0.11's QUIC/H3 hop support does not change that: it is not the QUIC
   endpoint/connection seam EggFetch H3 would need (consistent with ADR 0009).

## Options considered

### A. Fork a Hyper server inside EggReplay for inbound H2

Rejected. It duplicates Hyper server mechanics, H2 framing, and shutdown
semantics inside a product that is explicitly not a transport authority
(ADR 0002). M015A section 2 and the repository instruction "keep transport
ownership in EggFetch, EggServe, and Eggress" both forbid it.

### B. Make `eggserve-core` a default dependency to obtain H2

Rejected. The ordinary H1/direct profile would silently acquire
`eggserve-static`, Hyper `http2`, and the H2 protocol graph, widening every
default build, the Python wheel, and the interception graph for a capability
that must stay opt-in. The plan requires the default graph to be free of the
multiprotocol closure.

### C. Keep the default H1 path on `eggserve-server` and add a separate
opt-in EggServe Core runtime for inbound H2

Accepted. The H1 path is unchanged; the opt-in feature admits Core and
reuses the *same* `Service` implementation, the same canonical request/response
types, and the same matcher/store/redaction/scenario/rendering authorities.
There is no second runtime path inside the product, only a second *feature
selection*.

### D. Adopt `eggserve-h3 0.4.0` now that Core is being added

Rejected. ADR 0009 already deferred H3 on missing upstream seams, and H3 is
not authorized by this dependency adoption. `eggserve-core/http3` is never
enabled, so `eggserve-h3`, `quinn`, and the H3 protocol graph stay out of
every M015-supported graph.

## Decision

**Direct H1 remains owned by `eggserve-server` + `eggserve-primitives`.
Opt-in inbound HTTP/2 is owned by `eggserve-core`, reachable only through the
explicit `eggreplay-http` features `h2-inbound` (which activates
`eggserve-core/http2`) and `h2-inbound-tls` (which additionally activates
`eggserve-core/tls`).**

Concretely:

- `eggreplay-http` gains two non-default features. `h2-inbound` implies the
  existing `eggserve` feature, so the opt-in graph contains both the direct
  runtime and Core. Neither is ever in `default`.
- `eggserve-core` is declared in `[workspace.dependencies]` with
  `default-features = false` and pulled into `eggreplay-http` as an
  `optional = true` dependency. Nothing else in the workspace may depend on
  it.
- **EggReplay does not fork Hyper server mechanics.** The inbound-H2 adapter
  reuses the existing `eggserve_server::Service` implementation; the H2
  capability is a configuration change, not a new code path.
- The `eggserve-static` closure is acceptable inside the opt-in H2 graph
  because it is a non-optional EggServe Core dependency, and it must never
  leak into default H1 builds. CI asserts this in both directions.
- Interception remains on the caller-owned H1 path. `eggreplay-intercept`
  depends on `eggserve-server` only and must never gain `eggserve-core` or any
  `h2-inbound` feature. H2 interception is unsupported.
- TLS/ALPN serving requires explicit operator certificate and key material
  loaded through the approved `eggnet-tls` identity seam with an explicit ALPN
  advertisement. M015B does not create or install a CA, and the interception
  CA is never reused as an implicit server identity.
- H3/QUIC is not authorized by this adoption. `eggserve-core/http3`,
  `eggress-outbound/quic`, and `eggfetch-core/http3` remain disabled.
- `eggfetch-core 0.2.2`'s `Error::transport_failure_kind()` is used only where
  it removes Display/downcast ambiguity. It is diagnostic: it never changes an
  existing public error token, never implies retryability, and never maps a
  timeout, admission, or transport-I/O-inactivity fact. Unknown evidence stays
  unknown. It is a Rust-core surface; no Python, CLI, or FFI classifier claim
  is inherited from it.

## Consequences

- Default, direct, and interception dependency graphs are free of
  `eggserve-core`, `eggserve-static`, and the H2 protocol graph. The `h2`
  library still appears in the default and interception graphs through
  `eggress-outbound` → `eggress-protocol-http`; that is pre-existing Eggress
  routing-hop behavior present since 1.0.8, is not the multiprotocol serving
  closure, and is pinned by the boundary checks so it cannot be mistaken for
  one.
- Enabling `h2-inbound` costs the `eggserve-static` closure. That is a
  deliberate, bounded, opt-in cost.
- Because one `Service` implementation serves both protocols, an H2-specific
  bug cannot be "fixed" by special-casing a parallel matcher or renderer; the
  fix lands in the shared authority.
- The `eggserve-server 0.3.0` → `0.4.0` step is additive
  (`ResponseMetadataOwnership`, `with_request_header_bytes_owner`,
  `with_response_metadata_ownership`) and changes no adopted behavior; no
  EggReplay-visible contract moved.
- Documentation and the support matrix must keep H2 labelled **experimental**
  for as long as EggServe classifies its H2 tier that way, and must keep H2
  MITM, H3/QUIC, WSS, and extended-CONNECT WebSockets labelled unsupported.
