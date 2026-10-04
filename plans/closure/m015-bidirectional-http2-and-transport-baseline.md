# M015 — Bidirectional HTTP/2 and Transport Baseline Closure

Status: closed

Stage 11 executed M015A–M015E in dependency order. This record is the
umbrella: the final dependency graph, exact test counts, hosted runs, support
matrix, known limitations, and the next-stage handoff. The per-milestone
records carry the detail; nothing here replaces them.

## What Stage 11 did

M014 had qualified outbound HTTP/2 as an experimental opt-in tier and left
inbound H2, `h2c`, and gRPC unqualified. Stage 11 refreshed EggReplay onto the
current published Eggstack transport line and extended that to a **coherent
bidirectional HTTP/2 path** — one service, two runtimes — without changing the
supported HTTP/1.1 default.

The load-bearing claim is structural: protocol became a *listener property*, not
a second code path. The same matcher, store, redaction, scenario engine, and
renderer serve both protocols, and the only protocol-aware rendering rule in the
product is `content-length` on HTTP/2. M015C made that falsifiable by proving an
H1-recorded fixture replays over H2 and an H2-recorded fixture replays over H1,
and that a flow differing only in its protocol annotation matches and compares
equal on both.

| Milestone | Delivered | Closure |
|---|---|---|
| M015A | published dependency set; ADR 0010 inbound-H2 ownership; 8 captured dependency graphs; `dependency-boundary` + `protocol-boundary` CI lanes | `m015a-published-dependency-and-h2-boundary-preflight.md` |
| M015B | opt-in inbound H2 recording gateway and offline replay, ALPN TLS and explicit h2c; CLI `--inbound` surface; 38 tests | `m015b-inbound-http2-gateway-and-replay.md` |
| M015C | end-to-end H2 semantic/regression matrix against three independent peer families; 23 tests; support-tier matrix | `m015c-http2-end-to-end-semantic-and-regression-qualification.md` |
| M015D | gRPC over H2 qualified against Tonic 0.14.6 as a dev-only oracle; descriptor integration gap closed; 16 tests; un-terminated bidi deferred | `m015d-grpc-over-http2-integration-qualification.md` |
| M015E | 15-test hardening matrix; a four-commit feature-boundary regression found and fixed; documentation reconciliation; hosted qualification | `m015e-h2-hardening-hosted-qualification-and-closure.md` |

## Final dependency graph

| Crate | Version | Role |
|---|---|---|
| `eggfetch-core` | 0.2.2 | outbound client; `HttpVersionPolicy`, `transport_failure_kind` |
| `eggserve-core` | 0.4.0 | **optional**, `default-features = false`; inbound HTTP/2 runtime |
| `eggserve-server` | 0.4.0 | inbound HTTP/1.1 runtime; `Service` is re-exported by Core |
| `eggserve-primitives` | 0.2.2 | `Request` — one canonical request type for both runtimes |
| `eggnet-tls` | 0.2.0 | `load_tls_config_with_http2`, the ALPN-approval seam |
| `eggress-outbound` | 1.0.11 | optional routed outbound TCP |
| `h2` | 0.4.19 | raw HTTP/2 peer and the outbound stack under EggFetch |
| `hyper` | 1.11.1 | H1 and H2 connection stacks in the fixtures |
| `tonic` / `tonic-prost` | 0.14.6 | **dev-dependency only**; the M015D gRPC oracle |

`eggserve-core` is reachable only through `h2-inbound` and `h2-inbound-tls`:

```text
h2-inbound     = ["eggserve", "dep:eggserve-core", "eggserve-core/http2"]
h2-inbound-tls = ["h2-inbound", "eggserve-core/tls", "dep:eggnet-tls"]
```

Verified by CI on every push, per feature graph, and with `--all-targets`:
`no features`, `direct`, `eggress`, `websocket`, `h2`, `grpc`, `eggserve`,
`h2-inbound`, `h2-inbound-tls`, and all M015 features together. Tonic is
asserted absent from every product graph and present only as a dev edge.

## Exact test counts

| Suite | Tests |
|---|---|
| `eggreplay-http` unit | 76 |
| `h2_qualification` (pre-existing outbound H2) | 16 |
| `h2_inbound_serving` (M015B) | 30 |
| `m015b_inbound_serving` (M015B CLI) | 8 |
| `h2_end_to_end` (M015C) | 23 |
| `grpc_integration` (M015D) | 16 |
| `h2_hardening` (M015E) | 15 |
| `scenario_faults` | 7 |
| `v01_qualification` | 16 |
| `cli_contracts` | 64 |
| `har_migrate` | 11 |
| `m013e_operator` | 7 |
| `m013e_proxy_stats` | 9 |
| `hardening` | 12 |
| `mitm` | 9 |
| `proxy_policy` | 21 |
| `resource_bounds` | 3 |
| `substrate` | 24 |
| `tls_shutdown_isolation` | 3 |
| `ca_leaf` | 7 |
| `curl_interop` | 2 (both failing locally) |
| `eggreplay-core` unit | 76 |
| `eggreplay-store` unit | 11 |
| `eggreplay-intercept` unit | 12 |
| `eggreplay-har` unit | 7 |
| `eggreplay-core-version` | 3 |
| CLI unit | 7 |
| doc-tests | 0 |

**477 passed, 2 failed, across 32 suites** on the repository-standard locked
gate. Stage 11 contributed 92 of those tests (30 + 8 + 23 + 16 + 15).

The two failures are the same pre-existing, environment-specific
`eggreplay-intercept/tests/curl_interop.rs` cases carried since the M015A
closure. **They are confirmed machine-specific**: all four hosted `verify` jobs
run the full workspace suite and all four pass (Linux stable, Linux MSRV 1.89,
macOS, Windows). They are not waived as flakiness — they do not reproduce where
CI runs.

One pre-existing load-sensitive flake is recorded rather than re-run:
`recording_gateway_captures_upgrade_and_leading_post_101_messages` fails about
one run in six under load and passes 8/8 in isolation. It reproduces at the
M015C commit with all Stage 11 changes stashed, so it predates the stage and is
not HTTP/2.

## Hosted runs

Run [`37229585308`](https://github.com/eggstack/eggreplay/actions/runs/37229585308)
on the implementation SHA `874d6de`, branch `stage11-m015-bidirectional-h2`.

- `verify` — ubuntu-latest stable, ubuntu-latest **1.89.0 (MSRV)**, macos-latest,
  windows-latest: **all success**
- `interception` — ubuntu, macos, windows: **all success**
- `python-bindings` — ubuntu 3.11 and 3.14, ubuntu 3.11 at MSRV, macOS 3.11,
  Windows 3.11: **all success**
- `python-abi3-cross-version`: **success**
- `protocol-boundary` (9 steps): **success** — including the two new M015D/M015E
  steps for the gRPC oracle and the feature gates
- `dependency-boundary`: **one failure**, `cargo check -p eggreplay-http
  --no-default-features --features direct`

That failure is the most important result in the stage. The branch's first CI
run revealed that **M015B had broken the `direct` feature profile for four
commits**, and that the repository-standard `--all-features` gate structurally
could not see it: an optional dependency is present in the all-features graph,
so an ungated reference to it compiles. Only the `direct`-only graph exposes
it, and only the `dependency-boundary` lane builds that graph.

Fixed in the commit following M015E, with a permanent CI step that compiles
every feature profile with `--all-targets`. The lesson is recorded in
`plans/closure/m015e-h2-hardening-hosted-qualification-and-closure.md`: an
all-features gate is the wrong instrument for a feature-boundary claim.

## Support matrix

| Capability | Tier | Opt-in feature | Tests |
|---|---|---|---|
| H1 direct / inbound replay | **default** | — | entire pre-existing suite |
| Outbound H2 record / regression | experimental | `h2` | 16 + M015C rows 4–5 |
| Inbound H2 gateway, cleartext h2c | experimental | `h2-inbound` | 30 + M015C row 1 |
| Inbound H2 gateway, ALPN TLS | experimental | `h2-inbound-tls` | M015C TLS ALPN test |
| Inbound H2 offline replay | experimental | `h2-inbound[-tls]` | M015C row 3, rows 6–7 |
| H2 over an Eggress TCP route | experimental | `eggress` | M015C row 2, row 5 |
| gRPC unary / server / client streaming | experimental | `grpc` | 16 in `grpc_integration` |
| gRPC bidirectional, terminated | experimental | `grpc` | `a_terminated_bidi_call_records_and_replays_normally` |
| gRPC bidirectional, un-terminated | **deferred** | — | `bidi_streaming_is_deferred_with_evidence` |
| H2 interception (MITM) | unsupported | — | `protocol-boundary` asserts the absence |
| WSS, extended-CONNECT WebSockets | unsupported | — | replay handshake still requires H1 |
| HTTP/3 / QUIC | deferred (ADR 0009) | — | `protocol-boundary` asserts absence |
| Generic reverse proxy | out of scope | — | — |

"Experimental" means qualified against independent peers on local loopback,
opt-in behind a feature boundary, and re-qualifiable on any upstream change —
**not** "unverified". No HTTP/2 capability is a default in any profile.

## Known limitations

All seven are in `docs/http2-support.md` as well as here, because they are
operator-facing.

1. `H2Limits::max_header_list_size` is enforced inbound but **not advertised**;
   the server still sends EggServe's own 16384.
2. An oversized request body surfaces as **500**, not 413.
3. An **incomplete** request still consumes a single-use candidate. Stream-local,
   not a connection failure.
4. `eggfetch_core::Timeout::from_secs` sets `pool`/`connect`/`write`/`read` but
   **not** `total`, so it does not bound an upstream that never starts
   responding. Use a `total` cap.
5. A dead Eggress route fails closed but is categorised `Other`, so the failure
   is not diagnostic in the session.
6. A TLS peer claiming `:scheme: http` is refused with **400** before matching.
7. The regression authority compares `date`, which is second-granular;
   semantic comparisons should normalize that one field.
8. An un-terminated bidirectional gRPC call records a valid flow with **no
   terminal `grpc-status`**. That absence is the signal the call never
   completed; replaying it as complete would be misleading.

Two further items are inherited rather than introduced: the pre-existing
`curl_interop` local failures (now proven machine-specific) and the pre-existing
load-sensitive WebSocket flake.

## Invariants established

These are the things a future change could plausibly break, and they are each
pinned by a test:

- **Cross-protocol replay is symmetric.** H1-recorded fixtures serve over H2 and
  vice versa.
- **Protocol annotations are observational.** They are preserved and read by
  nothing; replay selection and regression comparison are version-neutral.
- **`content-length` is the only protocol-aware rendering rule**, applied once
  before every emission site.
- **The recorded scheme participates in matching.** A mismatch is a bounded
  refusal, not a relaxed match.
- **A candidate records the baseline request.** Remapping a destination cannot
  silently rewrite what a later report compares against.
- **A configured Eggress route never falls back to direct.**
- **A gRPC descriptor is caller-supplied and never fetched.** Raw body and
  trailers stay authoritative; the view is a projection.
- **No CA is minted and no insecure mode exists** for inbound TLS.

## Next-stage handoff

Carry these forward:

1. **A feature boundary needs a profile-compilation step in the same change.**
   `--all-features` cannot validate an opt-in. This is the one lesson from
   Stage 11 that should change practice immediately.
2. **Un-terminated bidirectional gRPC** needs a terminal-status story before it
   can be qualified. That is new canonical semantics and needs a milestone that
   explicitly owns it — it was deliberately not smuggled into M015D.
3. **Hardening items 1, 2, and 5** are candidate fixes for a future milestone:
   advertise the header-list bound, return 413 for an oversized body, and
   categorise a dead route. Each needs its own evidence; none is a
   correctness/safety failure today.
4. **`H2Limits` has no body bound.** Body size is the
   `start_*_with_protocol` `max_body_bytes` parameter, which is correct but
   split across two surfaces.
5. **The WebSocket flake** should be fixed or bounded by whoever owns
   `eggreplay-http`'s gateway unit tests.
6. **H3/QUIC, H2 MITM, and WSS** remain open and are governed by ADR 0009 and
   the M014C closure.

Stage 11 is closed. Any Stage 12 scope is selected by a new plan, not
pre-selected here.
