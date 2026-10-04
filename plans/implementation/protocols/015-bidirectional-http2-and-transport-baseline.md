# M015 — Bidirectional HTTP/2 and Transport Baseline

Status: closed (umbrella; M015A–M015E all closed)
Depends on: M014-R2 closure, M014B closure, M014D closure
Roadmap stage: 11

## Purpose

M015 promotes EggReplay from the M014 outbound-only HTTP/2 experiment to a
coherent, opt-in bidirectional HTTP/2 product tier while refreshing the
published Eggstack transport baseline.

The milestone must preserve the existing supported H1/default path. It does
not replace EggServe's direct H1 authority, make multiprotocol dependencies
default, or expand interception merely because H2 serving becomes available.

Current published sibling baseline researched for this plan:

- `eggfetch-core 0.2.2` (published 2026-10-02);
- `eggserve-primitives 0.2.2`, `eggserve-server 0.4.0`,
  `eggserve-core 0.4.0` (published 0.4.0 line);
- `eggress-outbound 1.0.11`;
- `eggnet-tls 0.2.0` remains the neutral TLS seam where applicable.

EggServe 0.4 deliberately keeps the direct `eggserve-server` crate H1-only;
H2 ownership lives in the compatibility/multiprotocol `eggserve-core`
layer. EggReplay therefore treats EggServe Core as an optional H2-serving
dependency rather than moving the default H1 graph onto it.

## Tracks

- **M015A** — adopt and qualify the current published dependency line; define
  the optional inbound-H2 ownership/feature boundary.
- **M015B** — implement opt-in inbound H2 gateway and replay serving through
  EggServe's H2 authority.
- **M015C** — qualify end-to-end H2 recording, replay, candidate regression,
  routing, streaming, trailers, multiplexing, and failure semantics.
- **M015D** — exercise M014D gRPC-aware views against real H2/gRPC traffic and
  record a bounded interoperability/support decision.
- **M015E** — hardening, dependency-boundary proof, hosted qualification,
  documentation, and umbrella closure.

M015A is the only executable child at registration. Later children become
ready only when their declared dependencies close.

## Explicit exclusions

M015 does not add:

- HTTP/2 MITM/interception;
- HTTP/3/QUIC support;
- a caller-facing Eggress QUIC route abstraction;
- WSS acquisition/replay;
- WebSocket-over-H2 extended CONNECT;
- arbitrary protobuf service emulation or network descriptor lookup;
- packet/TCP/kernel fault injection;
- automatic certificate trust installation.

Those require separate ownership and qualification.

## Support-tier rule

EggServe 0.4 classifies H2 as opt-in/experimental. EggReplay must not claim a
stronger tier than its adopted dependency and local evidence justify.

The expected successful M015 outcome is therefore **experimental,
bidirectional H2** unless the dependency tier changes before closure and
EggReplay requalifies against that changed line. H1 remains the supported
default.

## Closure

M015 closes only after M015A–M015E have explicit closure records, a final
support/limitation matrix is written, and hosted Linux/macOS/Windows/MSRV
qualification is green on the qualifying source revision.
