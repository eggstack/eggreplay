# 004 — Research and Compatibility Baseline

Status: canonical research baseline
Date: 2026-09-22

## mitmproxy concepts

Adopt the useful idea that one HTTP flow represents one transaction with request plus optional response or error, and keep client-side and server-side replay as distinct operations. Preserve concurrency metadata rather than permanently inheriting serialized client replay.

References:
- https://docs.mitmproxy.org/stable/api/mitmproxy/http.html
- https://docs.mitmproxy.org/stable/overview/features/

## WireMock concepts

Adopt matching across method/URL/query/headers/cookies/body, semantic JSON comparison, state-machine scenarios, request-derived templates, and strong mismatch diagnostics. v0.1 implements only bounded matcher/consumption essentials; scenarios/templates are M009.

References:
- https://wiremock.org/docs/request-matching/
- https://wiremock.org/docs/stateful-behaviour/
- https://wiremock.org/docs/response-templating/

## VCR.py concepts

Adopt portable cassette lifecycle, configurable matchers, consumption tracking, record-mode distinctions, and pre-persistence filtering. Do not adopt YAML as canonical storage or arbitrary language-stack monkeypatching in the Rust core.

Reference:
- https://vcrpy.readthedocs.io/

## Current Eggstack seams

EggFetch currently documents native body execution, `NativeHttpService`, custom `Dialer`, body DATA/trailers, detailed failures, TLS, pooling, and optional H2/H3.

Eggress documents listener-free `eggress-outbound::OutboundConnector` plus detailed route failures.

EggServe documents canonical request/response/service primitives, embeddable H1 runtime, streaming/trailers/lifecycle, and tunnel handoff; H2/H3 require more qualification.

References:
- https://github.com/eggstack/eggfetch
- https://github.com/eggstack/eggress
- https://github.com/eggstack/eggserve

## Evidence-gated open questions

- exact published EggServe surface to select in M004;
- whether EggFetch needs a generic observer hook after M003 proves the wrapper path;
- whether body-event timing is cheap enough for default capture;
- whether packed `.eggrz` is needed before v0.1;
- whether H2 evidence is affordable for v0.1;
- timing of HAR import/export.

Do not resolve these by speculative abstractions in M001.


## Baseline resolution update — 2026-09-23

Several original open questions are now resolved and should not be re-opened
implicitly by later work:

- EggFetch 0.2.0 is the published outbound baseline and exposes owned
  post-101/CONNECT `UpgradedStream` IO plus caller-owned TLS/dialer seams.
- EggServe's direct downstream surface is published: `eggserve-server 0.2.1`
  is registry-qualified and resolves with `eggserve-primitives 0.2.0`.
  M011A owns EggReplay's migration from the historical Git pin and must prove
  tunnel/read-ahead/lifecycle behavior locally.
- Eggress 1.0.9 is not a safe automatic upgrade target while its upstream
  pooled route-isolation/metadata correctives remain release blockers.
  EggReplay keeps its narrow `pproxy-compat` feature and M011A selects only a
  proven published/pinned line.
- M010 established that body-event timing is optional semantic metadata, not a
  default timing assertion.
- M011 WebSocket architecture is governed by ADR 0006 and must qualify direct
  and routed H1 upgrade ownership before semantic implementation.

The remaining evidence-gated compatibility questions are H2/H3 promotion,
HAR/migration interoperability, interception, and later protocol-specific
extensions. Those stay in M013/M014 rather than expanding M011.
