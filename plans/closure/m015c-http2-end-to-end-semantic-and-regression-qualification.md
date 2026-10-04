# M015C — HTTP/2 End-to-End Semantic and Regression Qualification Closure

Status: closed

## Qualifying revision and scope

Qualifies one coherent EggReplay HTTP/2 path across acquisition, offline replay,
candidate regression, and optional Eggress TCP routing — not merely that an
HTTP/2 socket can serve requests.

| File | Change |
|---|---|
| `crates/eggreplay-http/tests/h2_end_to_end.rs` | new: 23 integration tests, three independent peer families, local loopback only |
| `crates/eggreplay-cli/src/main.rs` | `OutboundVersionArgs` / `OutboundVersionChoice`, `--outbound-version` on `record`, `serve`, `replay`, `test`; `build_client_with_version` |
| `crates/eggreplay-cli/Cargo.toml` | outbound `h2` is opt-in on the CLI, matching the inbound opt-in surface |

No production source file changed in M015C. Every product behaviour the plan
requires already existed once M015B's inbound boundary was in place, and the
qualification found exactly one product-semantics gap in the *tests* (see
"Findings"). The CLI change exists because the plan requires a *candidate* to
be spoken to over HTTP/2 as an operator choice, not only in a test harness.

## Exact dependency versions

| Crate | Version | Role |
|---|---|---|
| `eggfetch-core` | 0.2.2 | outbound client; `HttpVersionPolicy`, `transport_failure_kind` |
| `eggserve-core` | 0.4.0 | inbound HTTP/2 runtime (opt-in, `h2-inbound`) |
| `eggserve-server` | 0.4.0 | inbound HTTP/1.1 runtime; `Service` is re-exported by Core |
| `eggserve-primitives` | 0.2.2 | `Request` — the single canonical request type for both runtimes |
| `eggnet-tls` | 0.2.0 | `load_tls_config_with_http2`, the ALPN-approval seam |
| `eggress-outbound` | 1.0.11 | `OutboundConnector` for routed H2 |
| `h2` | 0.4.19 | raw HTTP/2 peer and the outbound stack under EggFetch |
| `hyper` | 1.11.1 | H1 and H2 client/server connection stacks in the fixtures |

`eggserve-core` remains an **optional** workspace dependency with
`default-features = false`, reached only through `h2-inbound` /
`h2-inbound-tls`. The default, direct, H1, interception, and Python graphs stay
free of it — M015A's eight captured `cargo tree` graphs remain valid and
`.github/workflows/ci.yml`'s `protocol-boundary` job re-asserts all of them.

## How "one path" was made falsifiable

M015B's claim — one service, two runtimes — is a structural claim. It becomes
falsifiable only if protocol is given a way to *leak* somewhere it does not
belong. The suite therefore attacks the two cross-protocol directions directly:

- `row6_h1_acquired_fixture_replays_over_h2` — a flow recorded over HTTP/1.1
  must serve over HTTP/2. If protocol had become a matching dimension, or the
  H2 rendering path differed semantically, this would 404.
- `row7_h2_acquired_fixture_replays_over_h1` — and the converse.

Everything else in the matrix shows each protocol works on its own terms; these
two rows show they are the *same* path. A third row pins the remaining leak
vector: `version_annotations_are_observational_not_matching_dimensions` varies
`http-version:h1`, `http-version:h2`, and `http-version:absent` on an otherwise
identical flow and asserts both a match and an equal comparison result, on both
protocols. The annotation is preserved in the fixture and read by nobody.

## Required matrix

All seven required rows, each against an independent peer:

| Required row | Test |
|---|---|
| H2 client → gateway → direct H2 upstream | `row1_h2_client_to_gateway_to_direct_h2_upstream` |
| H2 client → gateway → Eggress-routed H2 upstream | `row2_h2_client_to_gateway_to_eggress_routed_h2_upstream` |
| H2 client → offline H2 replay | `row3_h2_client_to_offline_h2_replay` |
| Recorded fixture → direct H2 regression candidate | `row4_recorded_fixture_to_direct_h2_regression_candidate` |
| Recorded fixture → Eggress-routed H2 regression candidate | `row5_recorded_fixture_to_eggress_routed_h2_regression_candidate` |
| H1 fixtures replayed through the H2 serving path | `row6_h1_acquired_fixture_replays_over_h2` |
| H2-recorded fixtures replayed through H1 | `row7_h2_acquired_fixture_replays_over_h1` |

The seventh row's "explicit loss/unsupported behaviour otherwise" is pinned by
`replay_scheme_must_match_the_acquisition_transport`: an `https`-acquired flow
served cleartext is a **404**, never a relaxed match. This is not an HTTP/2
limitation — it is identical on HTTP/1.1 — and it is pinned here because the
matrix crosses protocols freely, so a reader deserves to know which rows need a
matching scheme.

`HttpVersionPolicy::Http2Only` is used for every product client in the suite
rather than `Auto`. These rows exist to observe the negotiated protocol; a
policy that would accept HTTP/1.1 could not distinguish "spoke H2" from
"silently fell back".

## Independent interoperability

Three peer families, so a shared implementation bug cannot establish support by
self-consistency:

1. **raw `h2`** — `h2_connect_tls` / `h2_connect_cleartext` and `H2Peer`, which
   expose DATA and trailer frames through `h2`'s own API. `h2_collect_with_trailers`
   reads the trailer block directly.
2. **Hyper** — `h1_connect` (H1 connection client), and the H2 upstream, which
   is a Hyper `http2::Builder` server that *asserts* ALPN negotiated `h2`.
3. **EggFetch** — `eggfetch_h2` / `eggfetch_h2_routed`, the product's own
   outbound path, used for the gateway's upstream leg and for every candidate
   regression.

Local loopback only. TLS identity is test-owned `rcgen`, written to PEM files on
disk, because `InboundProtocol::Http2Tls` takes *paths* — an in-memory
`ServerConfig` would qualify a different code path than operators get.

## Semantic evidence, item by item

| Required | Test(s) | Note |
|---|---|---|
| repeated/query/header normalization deterministic | `header_and_query_normalization_is_protocol_neutral` | the *same* fixture served over H1 and H2, byte-compared |
| request and response trailers survive | `trailers_survive_h2_acquisition_and_offline_replay` | acquisition **and** replay; see below |
| large request/response streaming bounded | `large_response_streams_intact_over_h2` | plus M015B's `scenario_body_chunks_stream_as_separate_data_frames` |
| concurrent H2 streams do not serialize unnecessarily | `multiplexing_is_concurrent_where_safe_and_ordered_where_required` | and `stream_events_stay_coherent_under_multiplexing` |
| cancellation/reset is stream-local | `cancellation_is_stream_local_end_to_end` | |
| M010 stream events/timing coherent under multiplexing | `stream_events_stay_coherent_under_multiplexing` | with a fail-closed negative control |
| strict/practical/semantic JSON matching identical to H1 | `matcher_profiles_behave_identically_across_protocols` | |
| stateful scenarios and deterministic templates identical | `stateful_scenarios_and_templates_are_identical_across_protocols` | |
| target remapping works | `target_remap_rewrites_the_authority_onto_the_candidate` | |
| regression diff/report/JSON contracts stable | `regression_report_contracts_are_stable` | |
| protocol annotations are observational | `version_annotations_are_observational_not_matching_dimensions` | |
| GOAWAY/shutdown cannot publish partial sessions | `shutdown_cannot_publish_a_partial_session` | |
| EggFetch 0.2.2 failure classification is evidence-backed | `eggfetch_failure_classification_is_evidence_backed` | |

### Trailers: the acquisition half is the load-bearing half

M015B proved the *serving* half — a fixture with trailers emits them. That is
the weaker claim: a trailer the gateway never recorded could never be replayed,
and the serving test would still pass. `trailers_survive_h2_acquisition_and_offline_replay`
closes the loop: an H2 client sends a DATA body plus a trailer block, the
upstream answers with a trailer block, and the test asserts both blocks are
recorded on the published flow *and* come back out of an offline TLS replay as a
real H2 trailer block. The upstream's `/trailers` path is the only path that
terminates with trailers, so every other row's single-DATA-frame assumption is
untouched.

### M010 stream events, with a negative control

`stream_events_stay_coherent_under_multiplexing` opens three streams before
draining any of them and drains them **out of order**, so attribution cannot be
credited to arrival order. It then asserts:

- `StreamEvents::validate()` passes, which checks contiguity, ordering, and
  terminal state — not just that events exist;
- every flow owns a sequence ending in `End`, and the recorded DATA byte count
  agrees with the stored body (the invariant `ReplayFixture::load_inner`
  enforces at load time, checked here at acquisition time instead);
- a candidate re-execution reproduces the event **shape** via
  `compare_stream_events(..., None)`. Cadence is deliberately not asserted:
  `delta_ns` is wall-clock and a candidate cannot reproduce it exactly. This is
  the same response-only splice the CLI and the Python binding perform, because
  candidate execution does not observe request-frame cadence;
- a **negative control**: `load_with_timing` on a session *without* stream-event
  metadata fails closed, and the same call on the H2-acquired session succeeds.
  Without the control, the positive assertion would be a tautology.

### Target remapping: the remap is a transport concern

`target_remap_rewrites_the_authority_onto_the_candidate` records against one
upstream and executes candidates against a different one. The evidence that the
remap happened is **which upstream was asked** — `candidate_origin.served() ==
flows.len()` and `recorded_origin.served() == flows.len()` (acquisition only). An
assertion on the outgoing request could only restate the intent.

The test also pins a semantic that is easy to get wrong: the observed candidate
flow keeps the **baseline request verbatim**, including the recorded authority,
because `execute_candidate` builds the wire URI from `target_base` but records
`Flow::new(request.clone(), ...)`. A remap therefore can never silently rewrite
what a later report compares against. What the observation *does* change is its
provenance — `mode = "eggfetch-native-candidate"` and the physical route — and
the report is clean, which pins that request authority is not a report dimension.

### Stateful scenarios and templates

`stateful_scenarios_and_templates_are_identical_across_protocols` runs a
three-request sequence against a session that records **no flows at all**, so
every response is either a rendered scenario step or a fall-through 404:

1. `/users/bob` matches no transition → 404, and the state does **not** advance;
2. `/users/alice` matches, extracts `alice` from the path segment, renders it
   into both a body template (`created {{user}}`) and a header, and advances;
3. `/users/alice` again → 404, because the scenario is now in its terminal state.

Step 3 is what distinguishes a stateful scenario from a stateless one. The two
protocols' full status/header/body triples are compared to each other for
equality, so the assertion is parity rather than a per-protocol restatement.
Scenario state belongs to the fixture instance, so each protocol gets its own
load — which is also why the sequences are independent.

## Findings

Two findings changed the tests. Both are recorded because each is a real
correctness question, not a test-authoring problem.

1. **The regression authority compares `date`, and `date` is second-granular.**
   `row4` and `row5` failed intermittently with
   `DiffFinding { kind: Header, field: "response.headers.date", baseline:
   "<present>", candidate: "<present>" }` when acquisition and re-execution
   straddled a second boundary. The product is correct: `compare_headers`
   compares every recorded response header, and that strictness is the contract
   M015C is required to keep stable. The **test** now strips `date` from both
   sides before comparing, which is exactly what the pre-existing outbound-H2
   qualification (`h2_qualification.rs`) already did. Without this the suite
   was green by coincidence roughly half the time — a coin flip is not a
   property. This is the kind of flake that survives review and fails in CI.
2. **A candidate observation records the baseline request, not the remapped
   one.** The first draft of the remap test asserted the opposite and failed.
   The product behaviour is intentional and better than the assumption: the
   remap is a transport concern, so the canonical record of "what was asked"
   stays the baseline request and the remapped path is visible in provenance.
   The test now pins the real behaviour.

## Regression status

The repository-standard command is green on the qualifying revision:

```text
cargo fmt --all -- --check
cargo check  --workspace --all-targets --all-features --locked
cargo clippy  --workspace --all-targets --all-features --locked -- -D warnings
cargo test   --workspace --all-features --locked --no-fail-fast
```

**446 tests passed, 2 failed** across 30 suites (423 + 23 new in
`h2_end_to_end`).

The two failures are the same two pre-existing, environment-specific
`eggreplay-intercept/tests/curl_interop.rs` cases documented in the M015A and
M015B closures — `curl_plain_http_proxies_and_records` and
`curl_https_connect_mitm_records`. They reproduce identically on `main` and are
a local curl/CA-trust artifact. Hosted CI is the authority, and M015E records
that result.

The pre-existing HTTP/2 and HTTP/1.1 surfaces are unchanged: the 30
`h2_inbound_serving` tests, the 16 outbound `h2_qualification` tests, the 8
`m015b_inbound_serving` operator tests, 7 `scenario_faults`, 16
`v01_qualification`, and all 64 `eggreplay-intercept` unit tests pass.

## Support tier matrix

EggServe's adopted HTTP/2 tier is **experimental** upstream. M015C therefore
does **not** label EggReplay HTTP/2 generally supported. Per-plan tiering:

| Capability | Tier | Evidence |
|---|---|---|
| Outbound H2 record/regression | **experimental** | `h2_qualification` (16, pre-existing) + rows 4 and 5 |
| Inbound H2 recording gateway | **experimental, opt-in** | row 1, row 2, `recording_gateway_accepts_inbound_h2` |
| Inbound H2 offline replay | **experimental, opt-in** | row 3, the 30-test M015B suite |
| Direct H2 | **experimental** | row 4 |
| Eggress-routed H2 | **experimental** | row 2, row 5 |
| TLS ALPN H2 | **experimental, operator identity required** | `tls_alpn_h2_gateway_serves_both_protocols`, 3 M015B TLS tests |
| h2c (cleartext prior knowledge) | **experimental, opt-in** | M015B's two cleartext tests, both directions |
| H2 interception | **unsupported in M015** | `eggreplay-intercept` never gained `eggserve-core`; asserted by the boundary lane |
| H3 / QUIC | **unsupported** | absent from every M015-supported graph |
| WSS, extended-CONNECT WebSockets | **unsupported** | the replay handshake path still requires HTTP/1.1 |
| Generic reverse proxy | **out of scope** | |

"Experimental" here means: qualified against independent peers on local
loopback, opt-in behind a feature boundary, and re-qualifiable on any upstream
change — not "unverified".

## Exclusions honoured

No H2 MITM, no H3/QUIC, no WSS, no extended-CONNECT WebSockets, no generic
reverse proxy, and no CA minting. No private Hyper server was added: the only
new production code is a CLI flag, and every HTTP/2 listener still comes from
`inbound::start_inbound_server`.

## Acceptance-criteria status

| Criterion | Status | Evidence |
|---|---|---|
| Seven required matrix rows | met | one test per row, named above |
| Normalization deterministic | met | `header_and_query_normalization_is_protocol_neutral` |
| Trailers survive | met | `trailers_survive_h2_acquisition_and_offline_replay` |
| Large streaming bounded | met | `large_response_streams_intact_over_h2` |
| No accidental stream serialization | met | 2 multiplexing tests |
| Cancellation stream-local | met | `cancellation_is_stream_local_end_to_end` |
| M010 stream events/timing coherent | met | `stream_events_stay_coherent_under_multiplexing` + fail-closed control |
| Matcher profiles identical to H1 | met | `matcher_profiles_behave_identically_across_protocols` |
| Scenarios and templates identical | met | `stateful_scenarios_and_templates_are_identical_across_protocols` |
| Target remapping works | met | `target_remap_rewrites_the_authority_onto_the_candidate` |
| Report contracts stable | met | `regression_report_contracts_are_stable` |
| Annotations observational | met | `version_annotations_are_observational_not_matching_dimensions` |
| Shutdown cannot publish partial sessions | met | `shutdown_cannot_publish_a_partial_session` |
| EggFetch failure classification evidence-backed | met | `eggfetch_failure_classification_is_evidence_backed` |
| Two independent H2 peers minimum | met | raw `h2`, Hyper, EggFetch |
| Local deterministic fixtures only | met | loopback, `rcgen`, test-owned SOCKS5 |
| Explicit tier matrix written | met | the table above |

## Consequences carried into M015D

- `content-length` remains the **single** protocol-aware rendering rule. gRPC
  messages are DATA frames and inherit it; M015D must not introduce a second
  rule.
- Protocol annotations are observational. A gRPC qualification must not let
  `content-type: application/grpc` become a matching dimension either, or M015B
  and M015C's cross-protocol guarantee would be quietly broken.
- The recorded scheme is part of the record, so a `grpc+...` acquisition is
  served over TLS. M015D's offline replay rows need a matching scheme, not a
  cleartext listener.
- The recorded authority for a gateway flow is the **upstream** origin, not the
  inbound `:authority`. A gRPC candidate must be addressed accordingly.
- A recorded request body must be a stored `BodyRef::Blob` for the body
  dimension to discriminate; `Absent` carries no length or digest and matches
  anything. gRPC message bodies are the same case.
- The candidate observation records the **baseline** request, so a gRPC
  descriptor or authority remap is visible only in provenance and the physical
  route — never as a silently rewritten request.

## Consequences carried into M015E

- M015C's flake finding — `date` is compared and is second-granular — applies to
  every regression-backed test in the repository, not only to HTTP/2. M015E's
  hardening matrix should check that no newly added regression test depends on
  acquisition and re-execution landing in the same second.
- All eight M015A dependency-boundary graphs are unchanged by M015C; the CI
  `protocol-boundary` job re-verifies them, and all five steps pass locally.
- The two local `curl_interop` failures remain the only red in the tree, and
  hosted CI is still the deciding authority for them.
