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
