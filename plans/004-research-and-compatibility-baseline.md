# 004 — Research and Compatibility Baseline

Status: canonical research baseline
Date: 2026-09-22

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
EggServe's qualified downstream set is `eggserve-server 0.2.1` with
`eggserve-primitives 0.2.0`. EggReplay currently uses
`eggress-outbound 1.0.8` with only `pproxy-compat`.

M011 qualified direct/routed cleartext H1 WebSocket upgrade semantics and is
closed. WSS/H2/H3 remain later compatibility work.

References:
- https://github.com/eggstack/eggfetch
- https://github.com/eggstack/eggress
- https://github.com/eggstack/eggserve

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

Remaining evidence-gated questions are Python 3.15/free-threaded promotion,
M013 interception qualification, H2/H3 promotion, HAR/migration
interoperability, and later protocol-specific extensions. These belong to
their owning later milestones rather than speculative changes to closed work.


## Interception baseline — 2026-09-24

M013 planning is based on current sibling evidence:

- `eggserve-server 0.2.1` already publishes generic caller-owned
  `serve_http1_connection`, so a decrypted rustls stream can feed the
  canonical H1 runtime without a private Hyper server;
- `ConnectionContext::for_tcp` accepts truthful TLS metadata and produces
  HTTPS scheme when TLS is present;
- EggServe source is versioned 0.2.2, but the newly published 0.2.2 registry
  patch is `eggserve-core` for Tower/HTTP interop; unchanged direct server
  crates were intentionally not republished;
- EggServe's neutral `eggnet-tls` source exposes bounded identity/trust
  parsing and rustls server-configuration helpers. M013A must query its exact
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

The remaining M013 evidence gates are the exact published `eggnet-tls` and
certificate-generation versions, cross-platform CA file protection,
independent local client interoperability, and final release-binary feature
policy.


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

EggReplay itself still pins server 0.2.1/primitives 0.2.0 until M013B0
migrates and qualifies 0.3.0. Do not describe the repository as already
running on the new line before that closure.

M013B0 intentionally keeps ordinary EggReplay services on EggServe-owned
policy/admission defaults. M013B will use `OriginOrAbsolute` explicitly for
the proxy listener after the migration is proven.
