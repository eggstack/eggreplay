# M015A — Published Dependency Refresh and H2 Serving Boundary Preflight

Status: closed
Depends on: M014-R2 closure, M014B closure
Parent: M015

## Objective

Move EggReplay's sibling dependency baseline to the current published lines
needed for Stage 11 and prove the inbound-H2 ownership boundary before
product behavior is added.

This is both an adoption milestone and an architecture gate. Do not begin
M015B until the dependency graph, public seams, and default-feature isolation
are demonstrated in repository evidence.

## Published versions to evaluate/adopt

Target the currently published releases:

- `eggfetch-core 0.2.2`;
- `eggserve-primitives 0.2.2`;
- `eggserve-server 0.4.0`;
- `eggserve-core 0.4.0` only behind the new inbound-H2 feature boundary;
- `eggress-outbound 1.0.11`;
- retain `eggnet-tls 0.2.0` unless direct evidence requires another
  published version.

Use registry releases, not sibling `main`, as product dependencies.

## 1. Preserve the lean default graph

The ordinary H1/default profile must continue to use
`eggserve-server + eggserve-primitives` and must not pull:

- `eggserve-core`;
- `eggserve-static`;
- H2 protocol dependencies;
- QUIC/H3 dependencies.

Introduce a separate opt-in H2-serving feature boundary in
`eggreplay-http`. The precise feature spelling may be finalized during this
plan, but it must make the broader EggServe Core graph explicit.

Recommended shape:

- existing outbound `h2` capability remains opt-in;
- an inbound/server H2 feature activates `eggserve-core/http2`;
- TLS/ALPN serving is separately explicit if EggServe's feature graph requires
  `eggserve-core/tls`;
- default/direct/H1 builds remain byte-for-byte feature-equivalent except for
  adopted patch/minor dependency behavior.

Do not make `eggserve-core` a default dependency merely to obtain H2.

## 2. Record the architecture decision

Create ADR 0010 covering:

- direct H1 remains owned by `eggserve-server`;
- opt-in inbound H2 is owned by `eggserve-core`;
- EggReplay does not fork Hyper server mechanics;
- the optional H2 graph may contain `eggserve-static` because it is a
  non-optional EggServe Core dependency, but that closure must not leak into
  default H1 builds;
- interception remains on the caller-owned H1 path and does not import the
  multiprotocol layer;
- H3 is not authorized by this dependency adoption.

If the published EggServe API cannot satisfy this boundary without private
modules or duplicated protocol runtime, stop M015B and record the blocker
rather than creating a private server.

## 3. Adopt EggFetch 0.2.2 conservatively

Re-run all M014B outbound-H2 behavior on 0.2.2.

Evaluate the new
`Error::transport_failure_kind() -> Option<TransportFailureKind>` API for
EggReplay's structured failure projection. Use it only where it removes
Display/downcast ambiguity without changing existing public error tokens or
retry policy. Unknown evidence must remain unknown.

No Python/CLI/FFI classifier claim is inherited from EggFetch; its 0.2.2
classifier is a Rust-core surface.

## 4. Adopt Eggress 1.0.11 narrowly

Keep EggReplay's ordinary route feature on the listener-free
`pproxy-compat`/TCP path used by M014B.

Do not enable Eggress `quic` merely because 1.0.11 contains QUIC/H3 hop
support. Current `OutboundConnector` still exposes TCP as its generic
caller-owned route seam, with UDP association as a separate bounded API; that
is not the QUIC endpoint/connection seam EggFetch H3 would require.

Re-run routed H1 and routed H2 tests after adoption and prove no configured
route silently falls back to direct.

## 5. Compatibility and graph verification

Capture `cargo tree` evidence for at least:

- workspace/default;
- `eggreplay-http` direct/default;
- EggServe H1 feature;
- outbound H2 only;
- inbound H2 only;
- inbound + outbound H2;
- interception;
- all-features.

Add/extend dependency-boundary checks so future changes cannot accidentally
pull EggServe Core/Static or QUIC/H3 into the default H1 or interception
profiles.

Re-run the full existing H1, WebSocket, Python, interception, H2-outbound,
HAR, gRPC-view, and scenario suites.

## Acceptance

M015A closes only when:

- exact published versions and lockfile resolution are recorded;
- default H1 behavior is green with the adopted sibling versions;
- the optional EggServe Core H2 boundary is proven public and usable;
- default/direct/interception graphs remain free of the new multiprotocol
  closure;
- outbound H2 remains green on EggFetch 0.2.2;
- direct and routed H1/H2 remain green on Eggress 1.0.11;
- ADR 0010 records the final ownership decision;
- hosted Linux/macOS/Windows/MSRV CI is green;
- a closure record makes M015B ready or explicitly blocks it with evidence.
