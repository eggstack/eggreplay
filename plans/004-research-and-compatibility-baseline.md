# 004 — Research and Compatibility Baseline

Status: canonical research baseline
Date: 2026-10-02

## mitmproxy concepts

Adopt the useful idea that one HTTP flow represents one transaction with request plus optional response or error, and keep client-side and server-side replay as distinct operations. Preserve concurrency metadata rather than permanently inheriting serialized client replay.

References:
- https://docs.mitmproxy.org/stable/api/mitmproxy/http.html
- https://docs.mitmproxy.org/stable/overview/features/

## WireMock concepts

Adopt matching across method/URL/query/headers/cookies/body, semantic JSON comparison, state-machine scenarios, request-derived templates, and strong mismatch diagnostics.

References:
- https://wiremock.org/docs/request-matching/
- https://wiremock.org/docs/stateful-behaviour/
- https://wiremock.org/docs/response-templating/

## VCR.py concepts

Adopt portable cassette lifecycle, configurable matchers, consumption tracking,
record-mode distinctions, and pre-persistence filtering. Do not adopt YAML as
canonical storage or arbitrary language-stack monkeypatching in the Rust core.

Reference:
- https://vcrpy.readthedocs.io/

## Current Eggstack seams

EggFetch 0.2.0 is the published outbound authority used by EggReplay.
EggServe's qualified downstream set is `eggserve-server 0.3.0` with
`eggserve-primitives 0.2.1`. EggReplay uses `eggress-outbound 1.0.8` with
only `pproxy-compat`.

M011 qualified direct/routed cleartext H1 WebSocket upgrade semantics and is
closed. WSS/H2/H3 remain later compatibility work.

References:
- https://github.com/eggstack/eggfetch
- https://github.com/eggstack/eggress
- https://github.com/eggstack/eggserve

## Stage 11 transport research refresh — 2026-10-02

The published sibling baseline has advanced since M014:

- EggFetch 0.2.2 is published. In addition to the existing H1/H2 and
  experimental H3 transport surfaces, it adds the Rust-core-only
  `TransportFailureKind` classifier used to distinguish evidence-backed
  connect/TLS/protocol/cancelled failures without Display parsing.
- EggServe's 0.4.0 release line publishes `eggserve-primitives 0.2.2`,
  `eggserve-server 0.4.0`, `eggserve-core 0.4.0`, and
  `eggserve-h3 0.4.0`. H1 remains the supported transport; H2/H3 are
  opt-in/experimental. The direct server crate remains H1-only by design;
  H2 ownership lives in the compatibility/multiprotocol core layer.
- Eggress 1.0.11 is published. `eggress-outbound` has optional UDP and
  QUIC/H3 protocol/transport features, but its generic caller-facing
  `OutboundConnector` connection seam remains TCP; UDP association is a
  separate bounded API. There is no generic `connect_quic`-style seam that
  can be substituted for EggFetch's QUIC endpoint ownership.

These changes make bidirectional H2 feasible without an upstream prerequisite
if EggServe Core is kept behind an explicit optional feature so the ordinary
H1/default graph remains lean. They also improve the H3 position: direct H3
record plus H3 replay may now be technically coherent, but routed H3 still
lacks the required Eggress caller-owned QUIC boundary. Stage 11 therefore
selects H2 promotion and dependency adoption (M015) while leaving H3 for a
later, separately researched stage.

## Python baseline — 2026-09-23

M012 planning uses the current upstream-compatible line:

- PyO3 0.29.2; the 0.29 line supports current CPython through the 3.15
  transition and has an MSRV below EggReplay's Rust 1.89 floor;
- `pyo3-async-runtimes 0.29.0` provides the Tokio/asyncio bridge and supports
  Rust 1.83+;
- maturin 1.14.1 provides current wheel/abi3 tooling.

EggReplay does not inherit upstream support claims automatically. M012A must
qualify its own CPython/ABI/runtime set. The planned initial product target is
CPython 3.11–3.14 GIL builds with `abi3-py311` preferred. Python 3.15 and
free-threaded support remain evidence-gated.

## Post-M012 resolution

M012 qualified `abi3-py311` for CPython 3.11–3.14 and qualified the declared
Linux x86_64/aarch64, macOS arm64/x86_64, and Windows x86_64 wheel matrix.

M013 interception qualification is closed under ADR 0008 on the qualified
dependency line `eggfetch-core 0.2.0`, `eggserve-primitives 0.2.1`,
`eggserve-server 0.3.0`, `eggress-outbound 1.0.8`, `eggnet-tls 0.2.0`
(see `closure/m013-explicit-proxy-and-optional-mitm.md`).

M014A HAR interchange/migration is closed with lossy, redacted, transactional
tooling and a golden corpus (see `closure/m014a-har-and-migration.md`); HAR
remains explicit interchange, never canonical.

M014B qualifies experimental outbound H2 record/regression (see
`closure/m014b-http2-qualification.md` and `docs/http2-support.md`). Stage 11
extends that to a coherent bidirectional H2 path on a refreshed dependency
line: M015A adopted `eggfetch-core 0.2.2`, `eggserve-server 0.4.0`,
`eggserve-core 0.4.0`, `eggserve-primitives 0.2.2`, and `eggress-outbound
1.0.11`, and M015B–M015E qualified inbound H2 serving, `h2c`, and gRPC over H2
as experimental opt-in tiers (see `adrs/0010` and
`closure/m015b-…`, `closure/m015c-…`, `closure/m015d-…`). H2 MITM remains
unsupported: `eggreplay-intercept` never adopts the multiprotocol serving
layer.

M014C defers HTTP/3 on all paths: no Eggress QUIC route connector on the
qualified line, no H3 serving seam in the adopted EggServe closure, and
EggFetch `http3` safety unreviewed (see `adrs/0009` and
`closure/m014c-http3-feasibility-and-qualification.md`).

M014D closed with gRPC envelope/status views, bounded caller-descriptor
decode, and five replay-serving fault models (see
`closure/m014d-grpc-and-fault-polish.md` and `docs/grpc-and-faults.md`);
raw blobs stay authoritative and arbitrary fault emulation stays out of
scope.

Remaining evidence-gated questions are Python 3.15/free-threaded promotion
and later protocol-specific extensions. These belong to their owning later milestones rather than
speculative changes to closed work. No sibling EggFetch/EggServe/Eggress
capability is an EggReplay support claim without EggReplay-local end-to-end
evidence.


## Interception baseline — 2026-09-24

M013 planning was based on the M013A-era sibling evidence:

- `eggserve-server 0.2.1` already publishes generic caller-owned
  `serve_http1_connection`, so a decrypted rustls stream can feed the
  canonical H1 runtime without a private Hyper server;
- `ConnectionContext::for_tcp` accepts truthful TLS metadata and produces
  HTTPS scheme when TLS is present;
- EggServe source is versioned 0.2.2, but the newly published 0.2.2 registry
  patch is `eggserve-core` for Tower/HTTP interop; unchanged direct server
  crates were intentionally not republished;
- EggServe's neutral `eggnet-tls` source exposes bounded identity/trust
  parsing and rustls server-configuration helpers. M013A queried its exact
  published version before depending on it;
- EggReplay remains on `eggress-outbound 1.0.8`. Eggress main is preparing
  1.0.10, but that release is currently blocked on an H2 TLS-override
  correctness corrective, so M013 must not opportunistically adopt it;
- EggFetch already owns custom-CA/SNI/client TLS policy and remains the secure
  upstream HTTPS authority for intercepted requests.

ADR 0008 therefore keeps interception in a separate leaf crate, uses Eggress
for opaque CONNECT route establishment, uses EggFetch for intercepted semantic
HTTP, and keeps CA generation/private-key state outside fixtures/default
Python packaging.

The M013A evidence gates were the exact published `eggnet-tls` and
certificate-generation versions, cross-platform CA file protection,
independent local client interoperability, and final release-binary feature
policy. M013B0 now qualifies the direct EggServe dependency line; CA
lifecycle, independent client interoperability, and release-binary feature
policy remain later interception work.


## EggServe embedding update — 2026-09-24

EggServe Plan 286 supersedes the M013A-era direct-server baseline for new
interception work:

- `eggserve-primitives 0.2.1` is published;
- `eggserve-server 0.3.0` is published;
- 0.3.0 includes opt-in
  `Http1RequestTargetMode::OriginOrAbsolute`, transport-neutral
  `RequestTargetForm::Absolute` metadata, projected `H1ConnectionPolicy`,
  and explicit policy/admission ownership;
- exact crates.io-only upstream consumers qualified default origin-only
  behavior, absolute-form metadata/bounds, duplicate headers, streaming
  bodies/trailers, caller-owned TLS H1, tunnels, shutdown, and Tower
  composition;
- `eggnet-tls` remains 0.2.0.

EggReplay has adopted and qualified server 0.3.0/primitives 0.2.1. The
M013B0 closure records the exact dependency graph, local verification, and
hosted cross-platform evidence.

M013B0 intentionally keeps ordinary EggReplay services on EggServe-owned
policy/admission defaults. Historical pre-implementation note (resolved by
M013B closure): M013B used `OriginOrAbsolute` explicitly for the proxy
listener; see `closure/m013b-explicit-http-proxy-and-connect-policy.md`.
