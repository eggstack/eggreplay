# M015B — Inbound HTTP/2 Gateway and Replay Closure

Status: closed

## Qualifying revision and scope

Adds opt-in inbound HTTP/2 to the recording gateway and the offline replay
server, served by EggServe Core behind the M015A feature boundary, reusing
the *same* service implementations the HTTP/1.1 path already uses.

| File | Change |
|---|---|
| `crates/eggreplay-http/src/inbound.rs` | new: `InboundProtocol`, `H2Limits`, `InboundServerHandle`, `start_inbound_server`, `parse_protocol`, `supported_protocol_names`, `InboundProtocolDescription`; 8 unit tests |
| `crates/eggreplay-http/src/lib.rs` | `pub mod inbound;` plus the re-exports |
| `crates/eggreplay-http/src/replay.rs` | `start_with_protocol` / `start_with_protocol_and_append`; `start_inner` now composes through `inbound::start_inbound_server`; one protocol-aware rule (`render_recorded_headers`) |
| `crates/eggreplay-http/src/recording.rs` | `start_recording_gateway_with_protocol`; the gateway composes through the same `inbound::start_inbound_server` |
| `crates/eggreplay-http/tests/h2_inbound_serving.rs` | new: 30 integration tests, all against independent peers |
| `crates/eggreplay-cli/Cargo.toml` | non-default features `h2-inbound`, `h2-inbound-tls` forwarding to `eggreplay-http` |
| `crates/eggreplay-cli/src/main.rs` | `InboundServingArgs` (`--inbound`, `--inbound-tls-cert`, `--inbound-tls-key`, `--h2-max-concurrent-streams`) on both `serve` and `record`; `"serving"` added to every serving status payload; sealed serve extracted to `serve_sealed` |
| `crates/eggreplay-cli/tests/m015b_inbound_serving.rs` | new: 8 operator-surface tests |
| `.github/workflows/ci.yml` | `protocol-boundary` step 2 extended to assert the CLI's opt-in features forward the capability |

## One service, two runtimes — the central design claim

`eggreplay_core::server::Service` *is* `eggserve_server::service::Service`
(`eggserve-core-0.4.0/src/server/service.rs` is a 22-line file whose entire
body is `pub use eggserve_server::service::*;`), and
`eggrecore_core::server::Request` is `eggserve_primitives::Request` — the
identical type the existing replay and gateway services already implement.
So M015B adds **no** product transport and **no** second authority:

- `ReplayFixture::start_with_protocol` and
  `start_recording_gateway_with_protocol` construct the caller's existing
  `ReplayTunnelService` / `GatewayTunnelService` and hand it to
  `inbound::start_inbound_server`, which passes it to whichever EggServe
  runtime the policy selects.
- The matcher, consumption, scenario, redaction, durable-publication, and
  response-rendering paths are the same code objects in both cases. There is
  no H2-specific matcher, store, redaction, scenario, or renderer.

`inbound::start_inbound_server` is the single composition point for both
product listeners. `ReplayFixture::start` and
`start_recording_gateway_with_websockets` are preserved verbatim as
HTTP/1.1-policy wrappers, so every existing caller and test is unchanged.

## The one protocol-aware rule

Recorded response headers are version-neutral facts about an origin, but
`content-length` is a *framing* fact and framing is per-connection. HTTP/1.1
EggServe validates the recorded value against the bytes it writes. HTTP/2 has
no such requirement, and a stale value would leave the peer waiting for bytes
that never arrive. So `render_recorded_headers` drops the recorded
`content-length` on HTTP/2 only, applied once, at a single point, before all
four response-emission sites.

The test for it is deliberately stronger than "the header is absent": the
fixture records `content-length: 99` for a 5-byte body, and the suite asserts
the emitted header is **`5`**. A header of `99` would mean the record was
trusted; no header at all would mean framing was not derived. Only `5` proves
the rule does its job — the runtime derives the value from the DATA it
actually streams.

Connection-specific headers are deliberately **not** filtered by the product.
Hyper's HTTP/2 server calls `strip_connection_headers` on every service
response, removing `connection` (and every header it names), `keep-alive`,
`proxy-connection`, `transfer-encoding`, `upgrade`, and `te`. A product-side
copy would be a second, weaker version of a rule the transport already
enforces, and would silently differ from it. `connection_specific_recorded_headers_never_reach_h2_wire`
proves the transport rule holds for a record carrying all three.

## TLS and h2c policy

**h2c is supported, as its own explicit policy.** M015B said to evaluate
cleartext prior knowledge only if EggServe exposes it as a public, bounded,
explicit policy. It does: EggServe's cleartext classification requires the
*complete* 24-byte HTTP/2 preface, and a stream that diverging at any byte is
HTTP/1.1. So a client selects the protocol by speaking it, and the policy
cannot be entered by accidental sniffing.

Two tests pin that, in both directions:

- `cleartext_policy_serves_h2_with_prior_knowledge` — h2c serves H2.
- `cleartext_policy_does_not_capture_a_plain_h1_client` — on the *same*
  cleartext listener, a plain HTTP/1.1 client is served HTTP/1.1. This is the
  "never a silent downgrade" half: EggReplay does not decide the protocol.

**TLS requires operator identity material.** `InboundProtocol::Http2Tls`
carries a certificate path and a key path; `parse_protocol` deliberately
refuses to resolve `http2-tls`/`h2-tls` from a name alone
(`tls_policy_is_not_name_resolvable`). The material is loaded through the
approved `eggnet_tls::load_tls_config_with_http2(cert, key, http2: bool)`
seam, which takes the ALPN advertisement as an explicit argument rather than
reading a crate feature. M015B mints no CA, installs nothing, and never
reuses the interception CA as an implicit server identity.

`tls_policy_without_usable_identity_fails_closed` proves a missing identity
file is a startup refusal naming the identity failure, not a fallback.

**ALPN is the operator's, in both directions.**

- `tls_alpn_h2_serves_a_sealed_replay` — a client offering `h2` gets `h2`.
- `tls_listener_serves_h1_to_a_client_that_does_not_offer_h2` — a client
  offering only `http/1.1` gets `http/1.1`, asserted on the negotiated
  `alpn_protocol()` *and* on raw HTTP/1.1 wire bytes. Raw bytes are used
  deliberately: no client library can normalise a request the server would
  have rejected.

## Qualification suite — 30 tests, independent peers

`crates/eggreplay-http/tests/h2_inbound_serving.rs`. Every client is an
independent peer — the raw `h2` crate, Hyper's H1 and H2 stacks, and raw
HTTP/1.1 bytes over TLS — never EggReplay's own client, so the suite
qualifies served bytes rather than a shared assumption about them. Local
loopback only; test-owned `rcgen` identity written to PEM files on disk,
because `InboundProtocol::Http2Tls` takes *paths* and an in-memory
`ServerConfig` would qualify a different code path than operators get.

M015B acceptance coverage, item by item:

| Required | Tests |
|---|---|
| TLS ALPN H2 | `tls_alpn_h2_serves_a_sealed_replay`, `tls_listener_serves_h1_to_a_client_that_does_not_offer_h2`, `tls_policy_without_usable_identity_fails_closed` |
| Multiplexing | `concurrent_streams_share_one_connection` (3 streams opened before any is awaited), `consumption_policy_applies_per_stream_over_h2` |
| Trailers | `response_trailers_arrive_after_the_body`, `request_trailers_project_into_the_matcher` |
| Streaming | `scenario_body_chunks_stream_as_separate_data_frames` (asserts ≥ 2 DATA frames), `stale_recorded_content_length_never_reaches_h2` |
| Cancellation | `stream_reset_does_not_disturb_siblings`, `dropped_response_future_leaves_the_connection_usable` |
| Shutdown | `graceful_shutdown_completes_in_flight_then_stops_accepting` |
| Mismatch response | `mismatch_response_matches_the_h1_shape_over_h2`, `recorded_upstream_error_projects_as_502_over_h2` |
| Scenario response | `scenario_response_applies_over_h2`, `terminal_fault_surfaces_as_a_stream_error` |
| H1 regression matrix | `h1_regression_matrix_on_the_h2_enabled_graph`, `h1_no_match_body_is_identical_on_both_runtimes`, `recorded_content_length_is_preserved_over_h1`, `http1_policy_never_speaks_http2`, `cleartext_policy_does_not_capture_a_plain_h1_client` |

Plus the plan's other required behaviour:

- **Request projection** — `request_method_authority_and_path_project_into_the_matcher`
  (`:path` and `:method` must both discriminate, or both would match),
  `request_body_participates_in_matching_over_h2` (an empty body must *not*
  satisfy a candidate recorded with a stored request body),
  `recorded_request_body_matches_over_h2`.
- **Pseudo-header containment** — `pseudo_header_state_never_reaches_canonical_headers`
  asserts no `:`-prefixed name on the response and **no `Host` header** (H2
  carries authority in `:authority`), and that a real header
  (`x-trace: abc`) survives. `recording_gateway_accepts_inbound_h2` asserts the
  same on the *published* recorded flow.
- **Redaction before durable publication** — the same gateway test reads the
  finalized fixture and asserts the `authorization` secret is gone and a
  `request.headers.authorization` redaction marker was recorded.
- **Truthful version annotation** — the gateway records the *upstream*
  negotiated version, unchanged. The recorded authority in that test is the
  upstream's (`127.0.0.1:<port>`), not the inbound `:authority`: the gateway
  rewrites onto the configured upstream origin, which is existing acquisition
  policy, and the test asserts what the product actually does and why.
- **Operator limits** — `operator_concurrent_stream_limit_is_advertised`
  opens exactly the advertised number of concurrent streams.

### The shutdown test is genuinely in-flight

`graceful_shutdown_completes_in_flight_then_stops_accepting` serves a
`ResponseHeadDelay` fault so the request is *accepted and in flight* when
shutdown arrives, then asserts the stream still completes with its full body
and that a fresh connection is refused afterwards. An earlier version of this
test shut down immediately after sending and observed `ConnectionReset`; that
proved nothing about GOAWAY semantics, so the head delay is load-bearing.

### What the suite found, and what it changed

Three findings during qualification changed the implementation or the tests,
and are recorded because each was a real correctness question rather than a
test-authoring problem:

1. **`:scheme` is validated against the transport.** EggServe rejects an
   HTTP/2 request whose `:scheme` disagrees with the negotiated transport.
   That is a correct guard, and it means a client is not free to claim `https`
   on a cleartext connection. `H2Peer` therefore carries the scheme *with* the
   connection so no call site can pair a path with the wrong one.
2. **HTTP/1.1 origin-form is mandatory, and a client library will not give it
   to you by default.** Hyper's connection client emits **absolute-form**
   whenever the request URI carries a scheme and authority, which
   `Http1RequestTargetMode::OriginOnly` correctly rejects. `h1_request`
   therefore sends a path-only target with an explicit `Host`, and the ALPN
   test uses raw bytes. With a path-only URI Hyper sends no `Host` of its
   own, so the header must be explicit.
3. **The matcher compares request headers, including hop-by-hop ones.** An
   early version of the ALPN test sent `Connection: close`, which the
   recorded fixture had no counterpart for, producing a legitimate 404. The
   fix was to make the test client behave like an ordinary keep-alive client
   — the product behaviour is correct and was not changed.

## Library and CLI surface

`InboundProtocol` is the explicit opt-in, with `Http1` as `#[default]`.
`InboundProtocolDescription` is the machine-readable, secret-free status
shape (`protocol`, `serves_http2`, `terminates_tls`, `cleartext`), and
`description_never_carries_key_material` asserts its serialized form mentions
no certificate, key, or PEM marker.

CLI: `--inbound`, `--inbound-tls-cert`, `--inbound-tls-key`, and
`--h2-max-concurrent-streams` are flattened onto both `serve` and `record`.
Defaults remain H1. The policy is resolved **before** any filesystem
precondition, so a mistyped `--inbound` is reported as a configuration error
rather than masked by a fixture-path error. The startup line reports the
selected policy, and every serving status payload gained a `"serving"`
object.

`crates/eggreplay-cli/tests/m015b_inbound_serving.rs` (8 tests) drives the
built binary: the flags are present on both commands, HTTP/1.1 is the
documented default, the TLS flags require each other, an unknown policy is
refused, a real `serve` process reports `inbound http1` on its startup line
even in an HTTP/2-capable build, a stream limit does not change the policy,
and no operator output contains key material. Two halves are build-gated: an
HTTP/2 policy resolves only under `h2-inbound`, and TLS-from-a-name is
refused under `h2-inbound-tls` while identity material in a build without it
is refused outright rather than silently ignored.

## Regression status

The repository-standard command is green on the qualifying revision:

```text
cargo fmt --all -- --check
cargo check  --workspace --all-targets --all-features --locked
cargo clippy  --workspace --all-targets --all-features --locked -- -D warnings
cargo test   --workspace --all-features --locked --no-fail-fast
```

**423 tests passed, 2 failed** across 29 suites (377 + 46 new: 30
`h2_inbound_serving`, 8 `m015b_inbound_serving`, 8 `inbound` unit tests).

The two failures are the same two pre-existing, environment-specific
`eggreplay-intercept/tests/curl_interop.rs` cases documented in the M015A
closure — `curl_plain_http_proxies_and_records` and
`curl_https_connect_mitm_records`. They reproduce identically on `main` and
are a local curl/CA-trust artifact. Hosted CI is the authority, and M015E
records that result.

The entire pre-existing H1 surface is unchanged: `eggreplay-http` 68 unit
tests plus 16 `h2_qualification` (outbound), 7 `scenario_faults`, and 16
`v01_qualification` all pass, and every one of the 64
`eggreplay-intercept` unit tests passes.

All five `protocol-boundary` CI steps pass locally, including the two
extended by this milestone: the CLI's `h2-inbound` and `h2-inbound-tls`
features each activate `eggserve-core`, and the CLI's **default** graph stays
free of the multiprotocol closure.

## Exclusions honoured

No H2 MITM (`eggreplay-intercept` never gained `eggserve-core`; the boundary
lane asserts it), no extended-CONNECT WebSockets (the replay handshake path
still requires HTTP/1.1 and rejects anything else), no H3 or QUIC (asserted
absent from every M015-supported graph), no WSS, no generic reverse proxy.

## Acceptance-criteria status

| Criterion | Status | Evidence |
|---|---|---|
| Inbound H2 into the recording gateway | met | `recording_gateway_accepts_inbound_h2` |
| Inbound H2 into sealed/offline replay | met | the H2 suite's 30 tests |
| Correct method/authority/path/header/body projection | met | 4 projection tests |
| Response headers, streaming body, trailers | met | `stale_recorded_content_length_never_reaches_h2`, `scenario_body_chunks_stream_as_separate_data_frames`, `response_trailers_arrive_after_the_body` |
| Request trailers | met | `request_trailers_project_into_the_matcher` |
| Concurrent streams over one connection | met | `concurrent_streams_share_one_connection` |
| Stream-local cancellation without corrupting siblings | met | 2 cancellation tests |
| Graceful shutdown / GOAWAY | met | `graceful_shutdown_completes_in_flight_then_stops_accepting` |
| Truthful HTTP-version annotation/provenance | met | gateway test asserts recorded authority and upstream-version behaviour |
| Existing redaction before durable publication | met | gateway test reads the finalized fixture |
| Existing matching/consumption/scenario/rendering authorities | met | one service, two runtimes; 409/404/502/scenario tests |
| No H2 state in canonical stored headers | met | pseudo-header tests on the wire and in the published fixture |
| ALPN `h2` over local TLS, explicit trust | met | 3 TLS tests |
| h2c explicit, never accidental or a silent downgrade | met | 2 cleartext tests, both directions |
| Operator identity material required, no CA minted | met | `tls_policy_without_usable_identity_fails_closed`, `tls_policy_is_not_name_resolvable` |
| Explicit opt-in config; defaults remain H1 | met | 8 CLI tests + 8 `inbound` unit tests |
| Machine-readable status without key material | met | `description_never_carries_key_material`, CLI output tests |
| No private Hyper server | met | no Hyper server construction in `crates/*/src` |
| Deterministic local test matrix | met | 30 + 8 tests, loopback only, `rcgen` identity |
| H1 regression matrix | met | 5 explicit H1 tests on the H2-enabled graph |

## Consequences carried into M015C

- The supported inbound matrix is now: H1 (default, direct runtime), and
  opt-in H2 over cleartext or TLS/ALPN (EggServe Core runtime). Protocol
  selection is a listener property, never a matching dimension.
- `content-length` is the single documented protocol-aware rendering rule.
  M015D's gRPC framing work must not introduce a second one; gRPC messages
  are DATA frames and inherit this rule.
- Inbound H2 with the `websocket` feature still rejects a WebSocket
  handshake that is not HTTP/1.1. That is correct and stays: extended-CONNECT
  WebSockets are out of scope.
- The recorded authority for a gateway flow is the upstream origin. Any M015C
  work on authority handling must preserve that.
