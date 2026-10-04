# M015D — gRPC over HTTP/2 Integration Qualification Closure

Status: closed

## Qualifying revision and scope

Binds M014D's existing optional gRPC envelope/status projections to real
HTTP/2 traffic from an independent, maintained gRPC implementation.

| File | Change |
|---|---|
| `Cargo.toml` | test-only version sources for `tonic`, `tonic-prost`, `prost`, `prost-types`, `tower`, with a comment stating they are dev-only and why |
| `crates/eggreplay-http/Cargo.toml` | those five as `[dev-dependencies]`; no product dependency changed |
| `crates/eggreplay-http/tests/grpc_integration.rs` | new: 16 integration tests, a real Tonic server and a real Tonic client |
| `Cargo.lock` | regenerated for the new dev-dependencies; committed in the same change, because CI runs `--locked` |

**No product source file changed.** Every gRPC capability M015D required —
envelope parsing, `grpc-status` extraction, descriptor decoding, and
replay/regression of a recorded gRPC call — already existed once M015B's
inbound H2 boundary and M014D's derived view were in place. M015D's job was to
prove that with traffic that did not come from a hand-framed fixture, and the
one thing it added to the product is nothing at all.

## The oracle: Tonic 0.14.6, dev-dependency only

The plan required at least one maintained independent gRPC implementation, and
required that product runtime dependencies not gain it. Both are satisfied
strictly:

- `tonic` 0.14.6, `tonic-prost` 0.14.6, `prost` 0.14.4, `prost-types` 0.14.4,
  `tower` 0.5.3 appear only in `[dev-dependencies]` of
  `crates/eggreplay-http`. `prost` and `prost-types` are pinned to the same
  versions already resolved through `prost-reflect`, so the gRPC oracle and the
  M014D view share one protobuf runtime instead of forking it.
- Verified across the whole workspace: `cargo tree -p <crate> --edges normal
  --all-features` reports **zero** `tonic` nodes for every product crate
  (`eggreplay-core`, `eggreplay-store`, `eggreplay-http`, `eggreplay-cli`,
  `eggreplay-intercept`, `eggreplay-har`, `eggreplay-python`).
- Verified per feature: the `default`, `direct`, `h2`, `h2-inbound`,
  `h2-inbound-tls`, `grpc`, `eggress`, `websocket`, and
  `h2-inbound-tls,grpc,eggress,websocket` product graphs all report zero
  `tonic`. CI's boundary lanes resolve `--edges normal`, so the same property is
  asserted on every push.

What is genuinely Tonic's, and what matters for this qualification, is
`tonic::server::Grpc` and `tonic::client::Grpc`, the `ProstCodec` that writes
and reads the 5-byte envelopes, Tonic's HTTP/2 transport, and its `Status`
trailer handling. What is hand-written is the per-method dispatch glue — which
Tonic's own codegen also hand-writes, and which M015D's non-goals forbid
generating as a *service*. A build script requiring `protoc` in `PATH` would
also have made the crate unbuildable for anyone without it.

TLS is terminated by the test's own acceptor and the decrypted stream is handed
to Tonic, rather than enabling Tonic's TLS feature. EggServe is the only TLS
terminator in the product, and an oracle that quietly added a second one would
have made "TLS ALPN worked" a statement about the harness.

## The harness qualifies itself first

`tonic_oracle_round_trips_before_eggreplay_is_involved` exercises a real Tonic
client against the real Tonic server on the same TLS listener the rest of the
suite uses, across unary, server streaming, client streaming, bidirectional,
and a non-OK status. It runs **before** EggReplay is involved anywhere.

Without it, a bug in the hand-written glue would be indistinguishable from an
EggReplay defect, and the blame would land on the wrong crate. Three real API
mismatches were caught this way while the suite was being built: Tonic's
`Codec` is parameterized `<Encode, Decode>` in opposite roles on the server
(`<response, request>`) and on the client (`<sent, received>`); the client's
streaming methods take `(request, path, codec)`, not `(request, codec, path)`;
and Tonic's `GrpcService` bound is hyper's `Read`/`Write`, so a `tokio-rustls`
stream must be wrapped in `TokioIo` before a custom connector can supply it.

## Qualification: 16 tests

| Plan requirement | Test |
|---|---|
| gRPC content-type recognition over H2 | `unary_grpc_over_h2_records_envelopes_and_status` |
| ordered 5-byte message envelopes | `unary_grpc_over_h2_records_envelopes_and_status` |
| multiple envelopes in one stream | `server_streaming_envelopes_are_ordered_in_one_recorded_stream` |
| compressed-flag reporting, no implicit decompression | `the_compressed_flag_is_reported_and_nothing_is_decompressed` |
| response trailers incl. `grpc-status` and `grpc-message` | `error_status_travels_in_the_recorded_response` |
| caller-supplied descriptor-set decoding within bounds | `caller_supplied_descriptor_decodes_a_recorded_message`, `descriptor_and_envelope_failures_do_not_corrupt_the_fixture` |
| record → offline replay byte/semantic preservation | `recorded_grpc_flow_replays_to_a_real_grpc_client`, `recorded_server_stream_replays_as_ordered_messages`, `client_streaming_is_representable_in_the_canonical_model` |
| candidate regression and derived diagnostics | `grpc_candidate_regression_is_clean_and_derives_the_same_view` |
| cancellation/deadline as observable H2 outcomes | `cancellation_is_an_observable_h2_stream_outcome` |
| redacted secrets not resurrected by the view | `redaction_precedes_persistence_and_the_view_resurrects_nothing` |
| non-gRPC traffic not projected as gRPC | `non_grpc_traffic_is_recorded_normally_and_not_projected_as_grpc` |
| oracle self-qualification | `tonic_oracle_round_trips_before_eggreplay_is_involved` |

### The headline: a real gRPC client calls the replay server

`recorded_grpc_flow_replays_to_a_real_grpc_client` records a unary RPC through
the HTTP/2 gateway, then serves the fixture over TLS HTTP/2 and has a real
Tonic client call it. Tonic is asked the same question it asked the live server
and must get the same answer.

Nothing in the replay path knows what gRPC is. There is no gRPC branch in the
matcher, the store, the redaction, or the renderer — a gRPC call is an HTTP/2
request with a `content-type` and a body. The test then asserts the client saw
the recorded envelope **byte for byte**, not merely an equivalent message, which
is what makes it a statement about preservation rather than about correctness
of a re-derivation.

Because a gateway flow records the *upstream* authority, the replay client is
addressed at the recorded origin and the connector pins the TCP transport. In
production that is DNS or a host mapping; everything above the socket —
`:authority`, `:scheme`, `:path`, headers, body — is produced by Tonic exactly
as it would be if the replay server really were at the recorded origin.

### Trailers: where Tonic puts a status is Tonic's business

`error_status_travels_in_the_recorded_response` records a failing RPC. Tonic may
answer a server-side error as a trailers-only response (`grpc-status` in the
HEADERS) or as headers plus a trailing HEADERS block. The product must preserve
whichever Tonic actually sent rather than normalising it, so the assertion is
that `grpc-status: 5` and the percent-encoded `grpc-message` reach the fixture
and decode to `NotFound` / `no such thing` — not on which frame carried it.

The derived view then reports **zero messages** for a failed call, which is the
correct answer: a call that returned a status sent no envelope.

### Descriptors: the largest coverage hole M015D closes

Before M015D, the descriptor path was unit-tested inside `grpc.rs` and **no
integration test ever passed a descriptor to `grpc_view`**. The wiring between a
stored body and a caller-supplied schema had never been exercised outside the
module. It is now, against a real recorded flow and a `FileDescriptorSet` built
declaratively so a reader can check it against the message struct without a
protobuf tool.

`descriptor_and_envelope_failures_do_not_corrupt_the_fixture` pins the bound
behaviour and the strict/lenient split, which is a deliberate M014D design
decision that was previously untested end to end:

- `decode_grpc_payload` is the **strict** API: an unknown message name, an
  oversized descriptor, and a malformed descriptor each return a typed
  `GrpcError`.
- `grpc_view` is the **lenient** projection: the same inputs return `Ok` with
  `decoded: None`. A caller that asked for a schema gets an error; a caller that
  only wanted an envelope summary gets a summary, and never a wrong decode.
- A descriptor at exactly `GRPC_MAX_DESCRIPTOR_BYTES` is *not* rejected for its
  size, so the bound is a bound and not an accident of padding.
- An envelope past `GRPC_MAX_FRAMES` is rejected.
- Truncated and overrunning envelopes are reported, not guessed at.
- After every one of those failures the stored body is byte-identical, the flow
  still validates, and the session still reads back one flow.

The descriptor is **caller supplied**. Nothing in the product fetches one, and
there is no reflection or network descriptor lookup anywhere in this milestone.

### The compressed flag

`the_compressed_flag_is_reported_and_nothing_is_decompressed` sends a
hand-framed gRPC request from the raw `h2` crate — the compressed bit set, and a
**valid, decodable** `EchoRequest` as the payload.

The payload being valid is what makes the test falsifiable. If EggReplay
decompressed, it would both change the bytes and start decoding; the test
asserts the recorded body is the exact bytes that arrived, that the frame is
reported `compressed`, that the payload is byte-identical to what was sent, and
that the view reports `decoded: None` *even though the payload happens to be a
valid message*. The `h2` crate is used here rather than Tonic precisely because
the claim is about what EggReplay does with a flag it did not set.

### Redaction

`redaction_precedes_persistence_and_the_view_resurrects_nothing` puts
`authorization: Bearer <secret>` in gRPC metadata and asserts the secret
appears in no part of the published flow, that the `authorization` header is
redacted, that the redaction itself is recorded, and — the half that is easy to
get wrong — that a derived view built from the redacted fixture, with a
caller-supplied descriptor, does not contain the secret either.

### Cancellation

`cancellation_is_an_observable_h2_stream_outcome` drops a long server stream
after its first message and asserts three things: a sibling call on the same
connection completes normally, every published flow validates, and no published
body is a partially consumed message. A reset stream is allowed to produce a
flow or no flow; what it must not do is publish half an envelope.

## Streaming classes: supported, supported, supported, deferred

| Class | Status | Evidence |
|---|---|---|
| Unary | **supported (experimental tier)** | `unary_grpc_over_h2_records_envelopes_and_status`, `recorded_grpc_flow_replays_to_a_real_grpc_client` |
| Server streaming | **supported (experimental tier)** | `server_streaming_envelopes_are_ordered_in_one_recorded_stream`, `recorded_server_stream_replays_as_ordered_messages` |
| Client streaming | **supported (experimental tier)** | `client_streaming_is_representable_in_the_canonical_model` — three envelopes in one stored request body, replayed intact |
| Bidirectional, terminated | **supported (experimental tier)** | `a_terminated_bidi_call_records_and_replays_normally` — both directions recorded, terminal status recorded, all of it replayed |
| Bidirectional, un-terminated | **deferred** | `bidi_streaming_is_deferred_with_evidence` |

### Why un-terminated bidi is deferred, with the evidence

The gateway *does* forward a streaming request body, so a bidirectional client
gets somewhere: the server replies to each message as it arrives and the client
sees those replies. What never happens is a terminal status, because the client
never half-closes and the outbound timeout eventually ends the call.

The observable outcome is precise and is the reason for the deferral: a `200`
whose body carries the replies received so far and whose trailers carry **no
`grpc-status` at all**. A gRPC client cannot call that a completed call, and a
replay of that fixture would serve a response with no terminal status — worse
than not replaying it. The recorded flow is valid and its envelope is whole;
the *missing* status is exactly the signal that the call never finished, and the
test asserts that signal is visible in the fixture.

The sibling test is what makes the deferral scoped rather than a blanket
refusal: a bidirectional call that **does** terminate — the client half-closes —
is recorded and replayed normally, terminal status included. The canonical
model has no problem with a bidirectional call that ends. What it cannot
express is a call whose two directions have no shared ending, and the gateway
cannot manufacture one.

Supporting the remaining case needs either a gateway that forwards request DATA
while response DATA is still arriving, or a canonical model that records
cross-direction ordering on one stream — a flow has one request body and one
response body with no shared ordering. Both are new canonical semantics, and
M015D's rule is not to smuggle them in here. That work is left for a future
milestone that explicitly owns it.

## Findings

Three findings changed the tests. Each is a real correctness question, not a
test-authoring problem.

1. **A gRPC regression must supply the recorded request envelope.** The first
   regression test passed `b""` as the request body, following the M015C
   regression helper where no flow had a request body. The candidate then came
   back with an empty response body, a `grpc-status` header, and a
   trailers-only decode error — a legitimate and confusing "mismatch". The
   request body *is* the gRPC envelope; without it there is no candidate for
   this flow. The test now reads the stored request body, asserts it parses as
   one envelope, and passes those bytes. This is the same M015C lesson in a new
   place: a stored `BodyRef::Blob` is what makes the body dimension
   discriminating.
2. **The M014D gate recognises a gRPC *response framing*, not a gRPC request.**
   `non_grpc_traffic_is_recorded_normally_and_not_projected_as_grpc` sends a
   `application/json` POST to a Tonic server, which answers the unknown path
   with `Unimplemented` carrying `content-type: application/grpc`. So a
   non-gRPC call can legitimately come back with a gRPC content-type, and
   `is_grpc_content_type` correctly says `true` about that response. The test
   now asserts the gate on the *request* (false), and separately that
   recognition alone never invents envelopes: an empty body yields a view with
   zero messages, not a fabricated one.
3. **An un-terminated bidi call's missing `grpc-status` is the finding, not a
   defect.** The test was originally written to assert a `504` and saw `200`
   instead. The real behaviour is more interesting: headers arrive as `200`, the
   body carries the replies that did arrive, and the trailers carry no
   `grpc-status`. That is now what the test asserts, and the gateway's outbound
   timeout was made configurable for it so the failure is bounded at two seconds
   rather than thirty.

## Regression status

The repository-standard command is green on the qualifying revision:

```text
cargo fmt --all -- --check
cargo check  --workspace --all-targets --all-features --locked
cargo clippy  --workspace --all-targets --all-features --locked -- -D warnings
cargo test   --workspace --all-features --locked --no-fail-fast
```

**462 tests passed, 2 failed** across 31 suites (446 + 16 new in
`grpc_integration`).

The two failures are the same two pre-existing, environment-specific
`eggreplay-intercept/tests/curl_interop.rs` cases documented in the M015A,
M015B, and M015C closures — `curl_plain_http_proxies_and_records` and
`curl_https_connect_mitm_records`. They reproduce identically on `main` and are
a local curl/CA-trust artifact. Hosted CI is the authority, and M015E records
that result.

### A pre-existing WebSocket flake found while running the gate

`recording::tests::recording_gateway_captures_upgrade_and_leading_post_101_messages`
failed in one of six full `--lib` runs with `Invalid("WebSocket 101 flow is
missing required conversation metadata")`. It passes 8/8 in isolation and is
load-sensitive, not deterministic.

It is **not** a regression from this milestone: `crates/eggreplay-http/src/` is
byte-identical to the M015C commit, and the flake reproduces at that commit
with all M015D changes stashed (2 failures in 8 runs of the full `--lib` suite).
It is a WebSocket test, untouched by M015A–M015D.

It is recorded here rather than left for someone to rediscover, because M015E
states that no deterministic failure may be waived as protocol flakiness — and
this one is *not* deterministic, which is a different thing. M015E's hardening
matrix should either fix it or bound it, and should say which.

The whole `grpc_integration` suite runs in about two seconds on local loopback,
including the bounded un-terminated-bidi case. No test in it waits on an
external resource, a real network, or a wall-clock boundary.

## M014D rules kept

| Rule | Evidence |
|---|---|
| descriptor sets are caller supplied | built in the test, passed as an argument; no fetch anywhere |
| no reflection / network descriptor lookup | no such code path exists or was added |
| bounded descriptor bytes | `GRPC_MAX_DESCRIPTOR_BYTES` asserted at and above the limit |
| bounded message/frame counts | `GRPC_MAX_FRAMES` asserted on real traffic |
| malformed protobuf/envelopes are derived-view errors, not fixture corruption | `descriptor_and_envelope_failures_do_not_corrupt_the_fixture` |
| raw body blobs and trailers are the source of truth | the view is a projection over them; the replay test asserts byte-for-byte equality against the raw blob |
| redaction applies before persistence; derived output must not resurrect secrets | `redaction_precedes_persistence_and_the_view_resurrects_nothing` |

## Exclusions honoured

No generated service implementation, no arbitrary method scripting, no dynamic
protobuf mutation engine, no gRPC-Web, no HTTP/2 MITM, no reflection server. No
canonical store change: the gRPC view remains a caller-side projection, exactly
as M014D left it, and this milestone added no code path that calls it
automatically.

## Acceptance-criteria status

| Criterion | Status | Evidence |
|---|---|---|
| gRPC content-type recognition over H2 | met | `unary_grpc_over_h2_records_envelopes_and_status` |
| ordered 5-byte envelopes | met | same test, payload decoded back to the protobuf message |
| multiple envelopes in one stream | met | `server_streaming_envelopes_are_ordered_in_one_recorded_stream` |
| compressed-flag reporting, no implicit decompression | met | `the_compressed_flag_is_reported_and_nothing_is_decompressed` |
| `grpc-status` / `grpc-message` in recorded trailers | met | `error_status_travels_in_the_recorded_response` |
| caller-supplied descriptor decoding within bounds | met | 2 descriptor tests, including the integration gap |
| record → offline replay preservation | met | 3 replay tests with a real gRPC client |
| candidate regression and derived diagnostics | met | `grpc_candidate_regression_is_clean_and_derives_the_same_view` |
| cancellation/deadline as observable H2 outcomes | met | `cancellation_is_an_observable_h2_stream_outcome` |
| independent maintained gRPC implementation participates | met | Tonic 0.14.6, client and server, oracle-qualified first |
| product runtime gains no Tonic | met | `--edges normal` is tonic-free for every product crate and every feature graph |
| unary and server streaming qualified | met | 3 tests |
| client streaming and bidi evaluated | met | client streaming supported; bidi evaluated and scoped to a deferral with evidence |
| deferred classes recorded with evidence, not smuggled in | met | `bidi_streaming_is_deferred_with_evidence` + the terminated-bidi boundary test |
| M014D descriptor/security rules kept | met | the rules table above |
| non-goals honoured | met | no codegen, no reflection, no gRPC-Web, no MITM |

## Consequences carried into M015E

- **An un-terminated bidirectional gRPC call is a supported recording with an
  unsatisfiable completion.** The fixture is valid and its missing
  `grpc-status` is the signal. M015E's hardening matrix should include it,
  because a hostile or crashed client that never half-closes is exactly the
  case where a recording will have no terminal status.
- **gRPC over H2 needs the recorded scheme to match.** A `grpc+...` acquisition
  is served over TLS; a cleartext replay is a 404, not a relaxed match.
- **A recorded gRPC request body is a `BodyRef::Blob`.** Any regression
  candidate for a gRPC flow must supply those bytes or it is testing the
  server's decode error rather than the flow.
- **The `grpc` feature pulls `prost`/`prost-types` into the product graph**
  (via `prost-reflect`, since M014D) but never `tonic`. M015E's feature and
  topology checks should assert the `tonic`-free property, not just the
  `prost`-free one.
- **Dev-dependencies changed `Cargo.lock`.** Every `--locked` lane depends on
  that file being committed, and M015E re-runs all of them.
- **A pre-existing, load-sensitive WebSocket flake exists in
  `recording_gateway_captures_upgrade_and_leading_post_101_messages`.** It is
  not caused by Stage 11 and is not HTTP/2, but it is in the tree M015E
  inherits and it will be seen by anyone running the full suite under load.
  M015E should fix or bound it.
- M015D is the first milestone whose test suite is codec-aware. The Python
  default wheel is unaffected: `eggreplay-python` never gained `tonic`, and
  `--edges normal --all-features` is tonic-free for it.
