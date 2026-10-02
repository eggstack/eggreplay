# M015B — Inbound HTTP/2 Gateway and Replay

Status: blocked
Depends on: M015A closure
Parent: M015

## Objective

Add opt-in inbound HTTP/2 to EggReplay's recording gateway and offline replay
server using the public EggServe H2 authority qualified by M015A.

Do not implement a private Hyper server and do not replace the default H1
runtime.

## Serving boundary

Use EggServe Core only through the M015A-approved optional feature boundary.
The existing H1 path remains on `eggserve-server`.

The adapter must translate between EggServe canonical request/response
semantics and existing EggReplay flow/matcher/store authorities; protocol
selection must not create a second matcher, store, redaction, scenario, or
response-rendering implementation.

## Required behavior

Implement and qualify:

- inbound H2 requests into the normal recording gateway;
- inbound H2 requests into sealed/offline replay;
- correct request method/authority/path/header/body projection;
- response headers, streaming body, and trailers;
- request trailers where EggServe exposes them;
- concurrent streams over one connection;
- stream-local cancellation/reset without corrupting sibling streams;
- graceful shutdown/GOAWAY behavior;
- truthful HTTP-version annotation/provenance;
- existing redaction before durable publication;
- existing matching, consumption, scenario, and response rendering
  authorities.

HTTP/2 connection-specific headers and pseudo-header state must never leak
into canonical stored headers.

## TLS and h2c policy

Qualify normal ALPN `h2` over local TLS with test-owned certificates and
explicit trust.

Evaluate cleartext prior-knowledge/h2c only if EggServe exposes it as a public,
bounded, explicit policy. It must never be enabled by accidental sniffing or a
silent downgrade. If h2c is not cleanly supportable, keep it unsupported and
record that decision.

Product TLS configuration must require explicit operator certificate/key
material or another already-approved EggServe identity seam. M015B does not
create or install a CA and must not reuse the interception CA as an implicit
server identity.

## CLI/library surface

Expose H2 serving through an explicit opt-in configuration that composes with
existing record/serve commands and APIs. Defaults remain H1.

Machine-readable status/config output must report the selected serving
protocol policy without exposing key material.

## Exclusions

No H2 MITM, extended CONNECT WebSockets, H3, WSS, or generic reverse proxy
features.

## Acceptance

M015B closes only with deterministic local tests covering TLS ALPN H2,
multiplexing, trailers, streaming, cancellation, shutdown, mismatch response,
scenario response, and an H1 regression matrix.

Create `plans/closure/m015b-inbound-http2-gateway-and-replay.md`. M015C
becomes ready only after B closes.
