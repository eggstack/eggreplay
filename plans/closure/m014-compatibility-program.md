# M014 — Broader Compatibility Program Closure

Status: closed

## Track decisions (each with its own evidence record)

| Track | Decision | Evidence |
|---|---|---|
| M014-R1 planning reconciliation | Closed | `closure/m014r1-planning-and-documentation-reconciliation.md` (run `36731867169`) |
| M014A HAR + migration | Closed, supported tooling (lossy interchange, never canonical) | `closure/m014a-har-and-migration.md` (run `36736273433`) |
| M014B HTTP/2 | Closed, **experimental outbound** (record + regression via EggFetch H2); inbound H2, H2 MITM, `h2c` unsupported | `closure/m014b-http2-qualification.md` (run `36771106385`), `docs/http2-support.md` |
| M014C HTTP/3 | Closed, **unsupported** (deferred with documented missing seams) | `closure/m014c-http3-feasibility-and-qualification.md`, ADR `adrs/0009-http3-integration-boundary.md` |
| M014D gRPC + faults | Closed, supported helpers within stated bounds | `closure/m014d-grpc-and-fault-polish.md`, `docs/grpc-and-faults.md` |

## Consolidated support matrix (M014 end state)

| Capability | Tier | Notes |
|---|---|---|
| HTTP/1.1 record/replay/regression (+ routed, scenarios, M010 timing, WebSocket, MITM) | Supported | Pre-M014 baseline, unchanged |
| HAR import/export + fixture migration | Supported tooling | Lossy, redacted, transactional; corpus-pinned |
| H2 record + regression (direct + Eggress-TCP-routed) | Experimental | Opt-in `eggreplay-http/h2` + explicit version policy; `http-version:h2` annotation, never a match dimension |
| H2 inbound serving / H2 MITM / `h2c` | Unsupported | No EggServe seam on qualified line; needs own evidence |
| H3 direct/routed/replay/intercept | Unsupported | Deferred per ADR 0009: no Eggress QUIC connector, no adopted H3 serving seam, QUIC safety unreviewed |
| gRPC envelope/status views + bounded descriptor decode | Supported helpers | Raw blobs authoritative; bounded, deterministic, redaction-inheriting |
| Authored scenario faults (delays, close-before/after-N, transport error) | Supported | Replay-serving only, validated bounds, existing lifecycle controls |
| Arbitrary packet/TCP/kernel fault emulation | Out of scope | EggChaos/EggBench territory |

## Dependency line (unchanged through M014)

`eggfetch-core 0.2.0` (+ `native-http2` under the `h2` feature),
`eggserve-primitives 0.2.1`, `eggserve-server 0.3.0`,
`eggress-outbound 1.0.8`, `eggnet-tls 0.2.0`, plus `prost-reflect 0.16`
(pure parsing for M014D descriptor decode). No sibling capability was
adopted as support without EggReplay-local end-to-end evidence, per the
roadmap rule.

## Umbrella closure

M014A–M014D each have an explicit closure/support decision above, so
the umbrella closes. The umbrella closure commit `c71ffd7` was verified
green on the standard matrix by Actions run
[36778923619](https://github.com/eggstack/eggreplay/actions/runs/36778923619)
(all thirteen jobs, hosted Linux stable + Rust 1.89, macOS, Windows,
interception, dependency-boundary, Python bindings, and the Python
abi3 cross-version lane); the closure commit itself is `c71ffd7` and
the workspace test count recorded for that run is the
`36778923619`-anchored matrix number consumed by the M014 closure
record. Per-track qualification runs are listed in the table above.

Qualification note (recorded failure, not a regression): run `36773818886`
first showed a macOS-only failure in
`recording_gateway_captures_upgrade_and_leading_post_101_messages`
(M011 websocket-conversation finalization racing session finish under
full-suite load; also observed once intermittently in a local
full-workspace run). No M014 code path is involved — the failing
assertion predates M014 and the M014B/M014C push carries no gateway or
store-sequencing changes. Rerunning the failed lane went green, and the
M014B implementation run `36771106385` was green on all lanes including
macOS on first pass.

The recorded failure was repaired in M014-C1 by introducing a
session-owned WebSocket conversation finalizer barrier in
`RecordingSession` (`crates/eggreplay-store`) and a std-only
`ConversationCompletion` in `crates/eggreplay-http`; see
`closure/m014c1-post-m014-closure-and-websocket-finalization.md` for the
ownership invariant, deterministic race regression tests, and the
qualifying green matrix.
