# M015E — H2 Hardening, Hosted Qualification, and Closure

Status: **ready**
Depends on: M015A, M015B, M015C, M015D closures
Parent: M015

## Objective

Close Stage 11 only after the new dependency line and bidirectional H2 path
are demonstrated safe, bounded, cross-platform, and isolated from default H1
and interception graphs.

## Required verification

Run the repository-standard locked gates:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

Also run feature/topology-specific checks proving:

- default/direct H1 excludes EggServe Core/Static and H2/QUIC/H3 closure;
- interception excludes EggServe Core and stays H1-only;
- outbound-H2-only does not enable inbound multiprotocol serving;
- inbound-H2 feature activation is explicit;
- QUIC/H3 remains absent from every M015-supported graph;
- Python default wheel behavior is unchanged unless a separate Python H2
  surface was explicitly added and qualified by an approved plan.

## Hardening matrix

Exercise hostile/boundary cases for:

- header/count/body limits;
- concurrent stream ceilings;
- slow/stalled request and response bodies;
- reset/cancellation races;
- GOAWAY and shutdown under active streams;
- trailer count/size and illegal H2 headers;
- malformed gRPC envelopes/descriptor sets;
- configured Eggress route failure with no direct fallback;
- TLS verification failures;
- session finalization under concurrent H2 streams.

No deterministic failure may be waived as protocol flakiness.

## Hosted evidence

Require green hosted:

- Linux stable;
- Linux Rust 1.89 MSRV;
- macOS stable;
- Windows stable;
- dependency-boundary/topology lane;
- interception lane;
- Python binding lanes already part of normal CI;
- wheel qualification if package metadata/features affecting wheels changed.

Pin the exact qualifying SHA and workflow IDs.

## Documentation/support reconciliation

Update:

- root README support matrix;
- `plans/001-architecture-and-boundaries.md`;
- `plans/003-qualification-and-release-strategy.md`;
- `plans/004-research-and-compatibility-baseline.md`;
- H2 and gRPC operator/reference docs;
- registry, roadmap, and planning README.

Explicitly retain unsupported/deferred labels for H2 MITM, H3/QUIC, WSS,
extended CONNECT WebSockets, and any unqualified gRPC streaming class.

## Closure

Write:

- `plans/closure/m015a-...` through `m015d-...` as each child closes;
- `plans/closure/m015-bidirectional-http2-and-transport-baseline.md` as the
  umbrella closure.

The umbrella record must contain the final dependency graph, exact test
counts, hosted runs, support matrix, known limitations, and the next-stage
handoff. Only then mark M015/M015E closed.
