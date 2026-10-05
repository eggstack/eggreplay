> Deep dive for [overview](overview.md).

Four opt-in adapter modules live in `eggreplay-http` above the owned
EggFetch / EggServe / Eggress seams: `h2.rs`, `grpc.rs`, `websocket.rs`, and
`eggress.rs`. Together they are about 980 lines, and none of them owns
transport. This document covers what they admit, what tier each capability
reached, and the feature policy that keeps every one of them out of a default
build.

Companions: [04 — HTTP recording](04-http-recording.md),
[05 — replay and serving](05-http-replay-and-serving.md),
[06 — regression](06-regression-and-reporting.md),
[01 — workspace and boundaries](01-workspace-and-boundaries.md),
[`../docs/http2-support.md`](../docs/http2-support.md),
[`../docs/eggress-routing.md`](../docs/eggress-routing.md),
[`../docs/grpc-and-faults.md`](../docs/grpc-and-faults.md).

---

## The tier model

Three words carry the support policy, and they are used consistently across
`README.md`, `docs/http2-support.md`, ADR 0010, and the closure records.

| Tier | Meaning | Example |
|---|---|---|
| **supported** | Qualified against the owned seams, exercised by the repository gate, and a documented behaviour an operator can rely on. | Eggress listener-free routing; WebSocket semantic record/replay/regression. |
| **experimental** | Qualified against **independent peers on loopback**, behind a feature boundary that no default profile enables. Not "unverified" — the opposite. | Outbound H2 record/regression; inbound H2 serving; gRPC views. |
| **unsupported** | Either not implemented, or implemented but deliberately not claimed. Never degrades into a quiet weaker path. | H2 MITM; HTTP/3 / QUIC; outbound `h2c`; WSS. |

The definition that matters for review is the middle one, and both
`README.md:91-93` and `architecture/overview.md:130-132` state it the same
way: *experimental means qualified against independent peers on local loopback
and opt-in behind a feature boundary — not "unverified."* For outbound H2 the
independent peers are concrete: a hyper client doing manual-TLS H2 and a raw
`h2`-crate client, both against the same ALPN-`h2` listener
(`crates/eggreplay-http/tests/h2_qualification.rs:353`,
`crates/eggreplay-http/tests/h2_qualification.rs:383`). For gRPC the peer is
Tonic 0.14.6, and the suite qualifies the oracle against itself before EggReplay
is involved (`plans/closure/m015d-grpc-over-http2-integration-qualification.md`,
§ The harness qualifies itself first).

**No HTTP/2 capability is default in any profile.** `default = ["direct"]` and
`direct = []` are empty of protocol capability
(`crates/eggreplay-http/Cargo.toml:11-12`); the default CLI enables
`eggserve`, `eggress`, and `websocket` but no `h2*` feature
(`crates/eggreplay-cli/Cargo.toml:19`, `:45-53`); the Python wheel requests
`default-features = false` with `direct, eggserve, eggress, websocket`
(`crates/eggreplay-python/Cargo.toml:18`). The `h2` library appearing in a
default tree is not a counter-example: it arrives through
`eggress-outbound` → `eggress-protocol-http` as pre-existing routing-hop
behaviour since 1.0.8, and ADR 0010 § Consequences explicitly distinguishes it
from the multiprotocol serving closure.

### Support matrix

| Capability | Direction | Tier | Feature | Note |
|---|---|---|---|---|
| Outbound H2 record + regression candidate | outbound | experimental | `h2` | ALPN `h2` over TLS; per-client `HttpVersionPolicy` |
| Outbound H2 over an Eggress TCP route | outbound | experimental | `h2` + `eggress` | same tier as direct H2; dialer moves raw TCP only |
| Outbound cleartext prior knowledge (`h2c`) | outbound | unsupported (not exposed) | — | `Http2Only` against cleartext fails closed |
| Inbound H2 gateway + offline replay (`h2c`) | inbound | experimental | `h2-inbound` | admits EggServe Core |
| Inbound H2 over TLS (ALPN) | inbound | experimental | `h2-inbound-tls` | operator identity mandatory |
| H2 interception (MITM) | — | unsupported | — | `eggreplay-intercept` depends on `eggserve-server` only |
| gRPC-over-H2 derived views | both | experimental | `grpc` | caller-side projection; raw body authoritative |
| WebSocket semantic record/replay/regression | both | supported | `websocket` | cleartext RFC 6455; see [04](04-http-recording.md) |
| Eggress listener-free routing | outbound | supported | `eggress` | `pproxy-compat` grammar only |
| HTTP/3 / QUIC | all four | unsupported (deferred) | — | ADR 0009 |

Two rows deserve emphasis because they are the ones most often misread. First,
**inbound `h2c` is supported and outbound `h2c` is not** — that asymmetry is
deliberate and is explained below. Second, **H2 interception is unsupported**
for a structural reason, not a scheduling one: `eggreplay-intercept` depends on
`eggreplay-http` with `features = ["eggserve", "eggress"]` and `eggserve-server`
alone (`crates/eggreplay-intercept/Cargo.toml:14-18`), so the `h2-inbound`
feature is structurally unreachable from interception.

---

## Outbound HTTP/2 (`h2.rs`)

`h2.rs` is a **boundary module of about 145 lines and mostly a header check**.
The H2 capability itself is an EggFetch capability; the module's job is to make
the opt-in explicit, re-export the policy type, and reject request headers that
HTTP/1.1 allows and HTTP/2 forbids.

**Opt-in is per client, twice over.** The `h2` feature is capability only
(`crates/eggreplay-http/Cargo.toml:20`, forwarding `eggfetch-core/native-http2`),
and on top of that every client needs an explicit `HttpVersionPolicy`
(`crates/eggreplay-http/src/h2.rs:7-9`). H1 remains the default policy in every
EggReplay client constructor, including the CLI, the Python bindings, and the
in-tree tests other than the H2 suite
(`plans/closure/m014b-http2-qualification.md`, § Support-tier decision). The
re-exported `HttpVersionPolicy` is `eggfetch_core`'s own type
(`crates/eggreplay-http/src/h2.rs:32`), and a unit test pins the invariant that
matters: the policy must originate from the pinned engine so H2 opt-in can never
desync from it (`crates/eggreplay-http/src/h2.rs:138-144`).

The CLI exposes this as `--outbound-version auto|http1|http2` (the clap
`ValueEnum` spellings, `crates/eggreplay-cli/src/main.rs:362-371`), documented in
`../docs/http2-support.md` § What is supported → Outbound, and one detail is
load-bearing: **`auto` maps to HTTP/1.1, not to EggFetch's `Auto`**. That is a
deliberate stability choice — an upstream release that changed what `Auto`
means could not silently change the protocol of an existing invocation.

**ALPN, not prior knowledge.** H2 is negotiated as `h2` over TLS. Cleartext
prior knowledge is not exposed at all, and `Http2Only` against a cleartext
endpoint **fails closed rather than downgrading** (`h2.rs:17-19`; the test is
`cleartext_prior_knowledge_fails_closed`,
`crates/eggreplay-http/tests/h2_qualification.rs:598`). The reason for refusing
is worth stating, because it is the same reason inbound `h2c` *is* offered:
outbound, EggReplay is the client and has already committed to a transport
shape, so a cleartext endpoint is a configuration error. Inbound, the client
selects the protocol by speaking the 24-byte preface, so cleartext H2 is an
explicit choice rather than an accident. Same wire bytes, opposite verdicts,
different side of the connection.

**Forbidden headers fail fast at the EggReplay boundary.** Per RFC 9113 §8.2.2,
`Connection`, `Keep-Alive`, `Proxy-Connection`, `Transfer-Encoding`, and
`Upgrade` are rejected unconditionally, and `TE` is permitted only when every
comma-separated token is `trailers`
(`crates/eggreplay-http/src/h2.rs:35-70`). EggFetch strips them internally, but
stripping alone would send *altered* semantics; `check_h2_headers` returns
`HttpError::Conversion` naming the offending header so the caller fails fast
instead. The qualified behaviour is that the peer never observes these headers
and the boundary helper independently rejects them
(`h1_connection_headers_do_not_leak`,
`crates/eggreplay-http/tests/h2_qualification.rs:567`).

**The version annotation is observational.** `negotiated_version_annotation`
returns `Some(("transport", "http-version:h2"))` for an H2 response and `None`
for HTTP/1.0 and 1.1 (`crates/eggreplay-http/src/h2.rs:129-136`). The annotation
is preserved in the fixture and read by nothing: replay selection and regression
comparison are version-neutral, which is exactly what makes cross-protocol
replay safe (`../docs/http2-support.md`, § Invariants worth knowing). An H1
fixture replays over H2 and vice versa.

**Routed H2 sits at the same experimental tier.** When `eggress` is enabled too,
H2-over-TLS works through an Eggress TCP route, and the ownership argument is
structural rather than asserted: `EggressDialer` returns raw TCP bytes from
`connect_tcp_detailed` and never touches SNI, ALPN, or the TLS session
(`crates/eggreplay-http/src/eggress.rs:36-47`), so those cannot leave EggFetch.
The routed test pins it through a single-hop SOCKS5 route and asserts
`physical_route.kind == "eggress"`
(`routed_h2_via_eggress_tcp`,
`crates/eggreplay-http/tests/h2_qualification.rs:695`).

What "qualified" meant operationally, from the test names in
`crates/eggreplay-http/tests/h2_qualification.rs`: two independent client
families (`:331`, `:353`, `:383`), concurrency (`:411`), trailers plus stream
events (`:465`), an 8 MiB streaming body (`:511`), per-stream cancellation that
leaves siblings intact (`:522`), target remapping (`:552`), no H1 header leakage
(`:567`), fail-closed cleartext (`:598`), graceful shutdown with an in-flight
record (`:632`), routing (`:695`), a gRPC view over a recorded H2 flow (`:793`),
strict and practical matching (`:830`), a scenario advance (`:888`), and
regression over H2 with scenario replay over H1 (`:935`).

---

## Inbound HTTP/2

The mechanism — `InboundProtocol`, `H2Limits`, `start_http2`,
`InboundServerHandle`, and why one service serves two runtimes — belongs to
[05 — HTTP replay and serving](05-http-replay-and-serving.md) (see its
§ Inbound protocol policy). What belongs here is the boundary and its
isolation.

**Why `h2-inbound` admits EggServe Core.** `eggserve-server 0.4.0` cannot serve
H2: its feature table declares `http2 = []` and `tls = []` — present for
compatibility signalling, enabling nothing — so the direct runtime is H1-only by
construction and "just add a flag" was never available (ADR 0010, § Verified
facts, item 1). `eggserve-core 0.4.0` is the multiprotocol composition layer
and the actual H2 authority. Selecting it is a **feature selection, not a second
code path**, because `eggserve_core::server::Service` *is*
`eggserve_server::service::Service` and `eggserve_core::server::Request` *is*
`eggserve_primitives::Request` (ADR 0010 item 3). That is why an H2-specific bug
cannot be "fixed" by special-casing a parallel matcher or renderer — there is
only one authority to fix.

**Why it must never be default.** `h2-inbound` pulls in `eggserve-core`, whose
`http2` feature enables Hyper `http2` and `hyper-util/http2` (ADR 0010 item 2).
`eggserve-static` is a **non-optional dependency of `eggserve-core`**, so any
graph admitting Core also admits Static (ADR 0010 item 5). That closure is
accepted *only* inside the opt-in H2 graph and must never leak into default H1
builds; CI asserts it in both directions. Making Core a default dependency was
considered and rejected precisely because it would widen every default build,
the Python wheel, and the interception graph (ADR 0010, option B).

The product feature comment says the same thing in the source
(`crates/eggreplay-http/Cargo.toml:21-29`), and the gating has a scar worth
knowing: `inbound` itself sits behind `#[cfg(feature = "eggserve")]`, not
`h2-inbound`, and the comment records that leaving it ungated was a real M015B
regression — the `direct` profile pulls no EggServe at all and stopped compiling
(`crates/eggreplay-http/src/lib.rs:15-23`). The H2 *variants* are gated
separately at `#[cfg(feature = "h2-inbound")]` and
`#[cfg(feature = "h2-inbound-tls")]`
(`crates/eggreplay-http/src/inbound.rs:64`, `:73`), so in a build without those
features the variants do not exist and selecting inbound H2 is a **compile-time
error rather than a runtime surprise**
(`crates/eggreplay-http/src/inbound.rs:28-30`).

**Why `h2-inbound-tls` is separately explicit.** TLS/ALPN serving needs
`eggserve-core/tls`, which in turn requires an actual server identity
(`crates/eggreplay-http/Cargo.toml:30-34`). Three constraints are stated
identically in the manifest, ADR 0010, and the inbound module: operator
certificate and key material are **mandatory**; the feature **never mints or
installs a CA** and there is no insecure mode; and the **interception CA is
never reused as an implicit server identity** (ADR 0010, § Decision). Cleartext
`h2c` and TLS+ALPN are different trust and exposure decisions, which is why the
boundary is an enum with separately gated variants rather than a boolean
(`crates/eggreplay-http/src/inbound.rs:9-24`, `:38-74`).

The known limitations of the inbound H2 surface are recorded rather than hidden
— `max_header_list_size` enforced but not advertised, an oversized body surfacing
as 500 rather than 413, an incomplete request still consuming its single-use
candidate, and `Timeout::from_secs` not setting `total`
(`../docs/http2-support.md`, § Known limitations found during hardening).

---

## gRPC views (`grpc.rs`)

**The load-bearing claim: there is no gRPC branch anywhere in the product.** No
branch in the matcher, the store, the redaction, the scenario engine, or the
renderer. A gRPC call is an HTTP/2 request with a `content-type` and a body, and
nothing in the record/replay path knows gRPC exists
(`../docs/http2-support.md`, § gRPC over H2;
`plans/closure/m015d-grpc-over-http2-integration-qualification.md`, § The
headline). The evidence for that is a real Tonic client calling the replay
server and being asserted to see the recorded envelope **byte for byte** — a
statement about preservation, not about the correctness of a re-derivation.

`grpc.rs` is therefore a **caller-side derived projection** over recorded bytes,
never part of the path. No product code path calls it automatically
(`../docs/grpc-and-faults.md`, opening). It lives in `eggreplay-http` behind
the `grpc` feature rather than in `eggreplay-core` specifically to keep the
protobuf runtime out of core's dependency boundary — `prost-reflect` pulls
`base64`, which the boundary lane forbids in core
(`plans/closure/m014d-grpc-and-fault-polish.md`, § gRPC view).

**Envelope parsing.** Each frame is 1 flag byte plus a 4-byte big-endian
length (`crates/eggreplay-http/src/grpc.rs:125-158`). Message order is
authoritative and exposed as a zero-based `index`; the compression flag is bit
0 of the flags byte, and a flagged payload stays **opaque bytes, never
decompressed** (`grpc.rs:139`, `:70-78`). Bounds fail closed: 16 MiB body,
4096 frames, 1 MiB descriptor (`grpc.rs:24-28`). Malformed input is an error
rather than a reinterpretation — fewer than 5 remaining envelope bytes is
`Truncated`, a declared length past the end is `Overrun`.

**Trailers.** `grpc_status_from_trailers` takes the **last** `grpc-status`
(so a trailers-only HEADERS response and a trailing HEADERS block both work),
rejects a value above 16 as `None`, and percent-decodes `grpc-message` with
`%XX` only — `+` stays literal, malformed escapes stay literal, and the view
never fails on trailer text (`grpc.rs:160-210`). The status is a **diagnostic**,
not a verdict: an absent `grpc-status` is how the fixture says the call never
completed.

**Descriptors are caller-supplied, bounded, and never fetched.**
`decode_grpc_payload` decodes only against an encoded `FileDescriptorSet` the
caller passed in, bounded at 1 MiB
(`grpc.rs:219-239`); `DescriptorPool::decode` is pure parsing, there is no
reflection server and no network descriptor lookup anywhere in the product, and
an unknown message name or an undecodable payload is a typed `GrpcError`. The
strict/lenient split is deliberate: `decode_grpc_payload` is the **strict** API
and returns typed errors, while `grpc_view` is the **lenient** projection — the
same inputs return `Ok` with `decoded: None` (`grpc.rs:248-271`). A caller who
asked for a schema gets an error; a caller who wanted an envelope summary gets a
summary, and never a wrong decode.

Two consequences follow from where the view reads from. It operates on
**caller-provided bytes, in practice already-redacted flow bodies**, so decoded
JSON inherits flow redactions and the view can resurrect nothing
(`grpc.rs:11-13`; `redaction_precedes_persistence_and_the_view_resurrects_nothing`).
And `is_grpc_content_type` is a gate, not a parser: it matches
`application/grpc` exactly or an `application/grpc+` subtype prefix, with no
parameter parsing (`grpc.rs:115-119`). The gate recognises a gRPC **response
framing**, not a gRPC request — a server may answer a non-gRPC request with a
gRPC content type (Tonic's `Unimplemented` does exactly that), and recognition
alone never invents envelopes: an empty body yields a view with zero messages
(`../docs/grpc-and-faults.md`, § Authored faults preamble).

**The un-terminated bidirectional case (M017).** A bidi call whose client half
never closes produces **no terminal `grpc-status`**. The recording gateway
forwards the streaming request body, so the server's replies do reach the
client, and the recorded flow is valid with a whole envelope — but the *missing*
status is precisely the signal that the call never completed
(`../docs/http2-support.md`, § What is not supported). What the fixture also
records is *why* it stopped: the outbound deadline surfaces as a response-body
failure that the recorder turns into a terminal `Error` stream event with
category `timeout`, suppressing the clean `End`
(`../docs/grpc-and-faults.md`, § un-terminated bidi).

M015D originally deferred this class, and M017 found the stated blocker did not
exist — the gateway was **already full-duplex**; hyper's `ResponseFuture`
resolves on response *headers* while the connection task pumps the request body
concurrently. The real defect was one layer down: body-stream errors were
hardcoded to `("other", "body")` with the error discarded, so a deadline
cut-off, a reset, and a protocol violation recorded identically
(`plans/closure/m017-unterminated-bidi-grpc.md`, § The real defect). That is the
same defect M016 had just fixed one layer up, for dial errors.

The asymmetry is recorded rather than smoothed over. M017's own research
**predicted** a clean 200 on replay and therefore `Code::Unknown`; that
prediction was **disproved** by a real client, because replay reproduces the
recorded terminal `Error` event as a `TimedStreamStep::Error`, so the body read
fails and Tonic reports `Code::Internal` (live: 200, partial body, no trailers,
`Unknown`; replay: partial body then a broken stream, `Internal`). The pinned
property is that the call **never replays as success**; whether replay *should*
reproduce the downstream experience rather than the recorded upstream
truncation is recorded as an open question
(`plans/registry.md:208-213`; `plans/closure/m017-unterminated-bidi-grpc.md`,
§ What the tests actually found). The recorder is deliberately not changed to
compensate: no synthesized `grpc-status`, and no cross-direction ordering on one
stream, which would be a genuine schema-v2 milestone.

**Fault injection bounds.** The same module family carries `ScenarioFault`, and
the bounds are validated rather than asserted: `response_head_delay` and
`body_chunk_delay` (delays ≤ 30 s, chunk sizes `1..=16` MiB) complete normally;
`close_before_response` and `close_after_bytes` abort without ever delivering a
clean full body and never invent a synthetic status; `transport_error` projects
the same 502 `recorded upstream error: {Category:?}` shape replay already uses
for a recorded `FlowOutcome::Error`. Arbitrary packet corruption, TCP flag
manipulation, and kernel-level emulation are explicitly out of scope
(`../docs/grpc-and-faults.md`, § Authored faults).

---

## WebSocket codec (`websocket.rs`)

This module is the smallest and the most constrained, and the constraint is
stated in its own first three lines: it **intentionally exposes no connect or
accept network helpers**; the caller must complete HTTP ownership and the
RFC 6455 handshake first
(`crates/eggreplay-http/src/websocket.rs:3-5`).

The reason follows from ownership. A WebSocket begins as an HTTP/1.1 request
with `Upgrade` and a `Sec-WebSocket-Key`, and a response with `101` and a
derived accept key. That exchange belongs to EggFetch's `UpgradedStream` and to
EggServe's tunnel handoff — the layers that already own HTTP. This module sits
strictly *above* them and provides a **codec** over whatever byte stream the
caller has already upgraded.

| Item | Kind | What it is |
|---|---|---|
| `WebSocketStream<S>` | alias | `tokio_tungstenite::WebSocketStream<S>` over a caller-owned async byte stream (`websocket.rs:11`) |
| `Message` | alias | `tungstenite::Message` — the semantic message type (`:14`) |
| `WebSocketConfig` | alias | `tungstenite::protocol::WebSocketConfig` (`:17`) |
| `Error` | alias | `tungstenite::Error` — codec or protocol error, distinct from `recording::HttpError` (`:20`) |

Both role constructors are thin and symmetric: `client(stream, config)` and
`server(stream, config)` each call `WebSocketStream::from_raw_socket` with
`Role::Client` or `Role::Server` over a generic
`AsyncRead + AsyncWrite + Send + Unpin` stream (`websocket.rs:23-36`). The role
must be stated explicitly by the caller because the codec masks and validates
frames according to it.

Handshake helpers are present but minimal: `derive_accept_key` delegates to
tungstenite's maintained implementation rather than re-deriving SHA-1 plus
base64, and `valid_client_key` checks that the key base64-decodes to exactly 16
bytes (`websocket.rs:38-49`). `send` flushes immediately for direct
backpressure, and `next` reads one semantic message (`websocket.rs:51-65`).

The unit tests show what the codec does and — more importantly — what it does
not need: a full text/binary/ping/close round trip and automatic pong queuing
(`websocket.rs:119-166`), continuation-frame reassembly into one semantic
message (`:168-199`), reading leading frame bytes left over on an
already-upgraded stream (`:201-215`), and a configured size bound surfacing as
`Error::Capacity` (`:217-237`). Every one of them runs over
`tokio::io::duplex` or an in-memory prefix adapter. There is no socket, no
handshake exchange, and no HTTP request in this file — which is the point.

tokio-tungstenite is a codec over already-owned streams, never a connection
stack. The record/replay/regression tier for WebSockets is **supported**, and
that tier is carried by the flow semantics in `eggreplay-core` and the gateway
and regression work in [04](04-http-recording.md) and
[06](06-regression-and-reporting.md), not by this module. WSS and
extended-CONNECT WebSockets remain unsupported, and the replay handshake path
still requires HTTP/1.1 (`../docs/http2-support.md`, § What is not supported).

---

## Eggress routing (`eggress.rs`)

Eggress owns listener-free **physical** route establishment. EggFetch still
owns logical Host and SNI, TLS, HTTP framing, pooling, and body semantics
(`../docs/eggress-routing.md`).

| Item | What it does |
|---|---|
| `EggressDialer` | An EggFetch `Dialer` that delegates TCP route establishment; `direct()` and `new(connector)` constructors (`eggress.rs:16-34`) |
| `parse_route` | Parses a `--route` value; `direct` → `Ok(None)` meaning "no dialer, ordinary EggFetch path" |
| `redact_route_credentials` | Scrubs `://userinfo@` per hop across a two-hop `__` chain |
| `physical_route_for` | Builds the redaction-safe `PhysicalRoute` recorded in flows |

`EggressDialer::dial` is the whole integration: it calls
`connect_tcp_detailed(target.host(), target.port())` and returns the stream
boxed as a `DialStream` (`eggress.rs:36-47`). Because the return type is a raw
TCP stream, SNI and ALPN **cannot** leave EggFetch — the ownership boundary is
enforced by the type, not by a comment. That is the same property routed H2
relies on.

**The grammar is deliberately narrow.** Only `direct` or an
`OutboundConnector::from_pproxy_uri` expression, such as
`socks5://127.0.0.1:1080` or a two-hop chain
`socks5://127.0.0.1:1080__http://127.0.0.1:8080`
(`eggress.rs:3-8`, `:66-86`). The `eggress` feature enables exactly
`eggress-outbound` with `pproxy-compat` and the manifest comment says plainly:
do not enable extended, SSH, QUIC, listener, or server surfaces
(`crates/eggreplay-http/Cargo.toml:38-40`).

**Failure is closed and quiet about credentials.** `parse_route` treats
`direct` case-sensitively and returns `None`; any other value goes to
`from_pproxy_uri`, and its errors — which already redact credentials — are
additionally scrubbed of any `@` userinfo that might survive in a wrapper
message (`eggress.rs:74-86`). Malformed input returns `Err` **before any network
execution**; there is no fallback to direct. The unit test pins both halves: a
bad second hop in a chain is an error and its credential sentinel does not
appear in the message, and `bogus://not-a-route` is an error rather than a
silent direct (`eggress.rs:136-150`). `redact_route_credentials` splits on `__`
and rewrites each hop's userinfo to `<redacted>`, so a two-hop chain does not
leak the first hop's credentials through a diagnostic about the second
(`eggress.rs:88-105`).

`physical_route_for` records the fact, redacted: no connector yields
`kind: "direct"`, and a connector yields `kind: "eggress"` with the redacted
route string as the description (`eggress.rs:107-122`). What reaches a fixture
is therefore never the credential.

Error mapping is a translation table from Eggress's typed kinds into EggFetch's
`DialErrorKind` — `Timeout`, `Authentication`, and `Policy` (→ `Rejected`) are
preserved, the connection-shaped kinds collapse to `Connection`, and a
wildcard arm maps anything unlisted to `Other` (`eggress.rs:49-64`). Typed
facts are used rather than parsing display strings
(`../docs/eggress-routing.md`). Worth knowing when reading a failure:
`../docs/http2-support.md` § Known limitations records that a dead Eggress
route fails closed but is categorised `Other` and so is not diagnostic in the
session; M016 separately added a `CustomTransport` arm to `map_fetch_error` so
that a route failure reads the same from either layer
(`plans/closure/m017-unterminated-bidi-grpc.md`, § The real defect).

The TCP adapter makes **no H3 support claim**, and QUIC is never tunneled
through it — see the next section.

---

## Feature isolation

| Feature | Default? | Admits | Deliberately does not admit |
|---|---|---|---|
| `direct` | **yes** | nothing; the empty base | any protocol capability |
| `eggserve` | no | `eggserve-primitives`, `eggserve-server` (H1 direct runtime) | `eggserve-core`, any H2 |
| `h2` | no | `eggfetch-core/native-http2` — **outbound only** | inbound serving, `eggserve-core`, any H2 *serving* |
| `h2-inbound` | no | `eggserve` + `eggserve-core` + `eggserve-core/http2`, and transitively `eggserve-static` | `eggserve-core/http3`, default profile |
| `h2-inbound-tls` | no | `h2-inbound` + `eggserve-core/tls` + `eggnet-tls` | CA minting, reuse of the interception CA |
| `grpc` | no | `prost-reflect` | any transport, Tonic, reflection, descriptor fetching |
| `websocket` | no | `base64`, `tokio-tungstenite` | a connect/accept helper |
| `eggress` | no | `eggress-outbound` with `pproxy-compat` | quic, ssh, extended, listener, server surfaces |

Three isolation properties carry the policy, and all three are structural.

**`h2` is outbound-only and independent of `h2-inbound`.** They are separate
features with no dependency between them in either direction
(`crates/eggreplay-http/Cargo.toml:15-34`), and the CLI forwards them
separately for exactly the stated reason: an operator may want to *record* over
H2 while serving H1, or the reverse (`crates/eggreplay-cli/Cargo.toml:48-53`).
Selecting `h2` does not create a single inbound H2 code path, and selecting
`h2-inbound` does not make an outbound client speak H2.

**`grpc` keeps the protobuf runtime out of direct/H1 builds.** The feature is
`["dep:prost-reflect"]` and nothing else, and it exists off the default
specifically so direct/H1 builds stay codec-free
(`crates/eggreplay-http/Cargo.toml:35-37`); the `direct` boundary check pins
`prost-reflect` to the `grpc` feature. The gRPC **oracle** is the mirror image:
Tonic 0.14.6, `tonic-prost`, `prost`, `prost-types`, and `tower` are
`[dev-dependencies]` of `eggreplay-http` only, pinned to the protobuf versions
already resolved through `prost-reflect` so the oracle and the view share one
runtime (`crates/eggreplay-http/Cargo.toml:77-85`;
`plans/closure/m015d-grpc-over-http2-integration-qualification.md`, § The
oracle). `cargo tree --edges normal` reports zero `tonic` nodes for every
product crate and every feature graph.

**Reachability differs per consumer, and that is worth knowing.**

| Consumer | `eggreplay-http` features | Consequence |
|---|---|---|
| `eggreplay-cli` | `eggserve`, `eggress`, `websocket` unconditional; forwards `h2`, `h2-inbound`, `h2-inbound-tls` | no H2 by default; **does not forward `grpc`**, so the derived view is not reachable from the CLI binary |
| `eggreplay-python` | `default-features = false`, `direct, eggserve, eggress, websocket` | the wheel has no outbound H2, no inbound H2, and no gRPC view |
| `eggreplay-intercept` | `eggserve`, `eggress` | interception can never gain inbound H2 |

---

## HTTP/3 deferral

ADR 0009 (`plans/adrs/0009-http3-integration-boundary.md`) decided to **defer
HTTP/3 on all paths** — direct, routed, replay, and intercept — and classifies
it unsupported. The reason is a seam, not a schedule: QUIC runs over UDP and
negotiates transport parameters, 0-RTT, and connection migration inside the QUIC
handshake, none of which a TCP byte stream can carry, so **tunneling QUIC
through a TCP `Dialer` abstraction is forbidden**.

Four options were considered and all four were rejected — direct EggFetch H3
endpoint ownership, an Eggress QUIC route connector, direct-only H3 with routed
H3 declared unsupported, and EggServe H3 service integration. The three
documented missing seams are:

1. **No Eggress listener-free QUIC route connector on the qualified line** —
   `OutboundConnector` exposes only `connect_tcp*`; the `quic` feature
   (`eggress-transport-quic`, `eggress-protocol-h3`) is unadopted.
2. **No H3 serving seam in the adopted EggServe closure** — `eggserve-h3` is
   upstream-experimental and unqualified, and the direct runtime is H1-only.
3. **EggFetch `http3` (quinn/h3) unconsumed** — no UDP endpoint lifecycle,
   connection migration, or 0-RTT replay-risk review has been done.

Revisit requires all three: a stable Eggress QUIC connector on an adopted line,
a matured H3 serving adapter with EggReplay-local evidence, and a reviewed
EggFetch H3 safety posture. A blocked support decision is the plan-sanctioned
outcome (`plans/closure/m014c-http3-feasibility-and-qualification.md`).

The standing consequences are simple: no QUIC/H3 code path is enabled anywhere;
`eggserve-core/http3`, `eggress-outbound/quic`, and `eggfetch-core/http3` all
stay off (ADR 0010, § Decision); and `EggressDialer` stays TCP-only, so any
future QUIC route must add a **new seam** rather than reuse the TCP dialer.
QUIC/H3 interception is out of scope, so M013's HTTP/1.1 MITM claim does not
expand.

One consequence is worth reading carefully because it is the opposite of the
HTTP/2 rule. Without the `http3` feature, `HttpVersionPolicy::Http3Only`
**silently downgrades to H1** inside EggFetch via `HttpVersionPolicyEnabler`
(ADR 0009, § Context and § Consequences). That is the opposite of `Http2Only`
against a cleartext endpoint, which fails closed. It is a documented property of
the unadopted feature rather than a claim EggReplay makes, but it means "H2
fails closed" must not be generalised into "every version policy fails closed."

---

## Review checklist

| # | Question | Where to look |
|---|---|---|
| 1 | Does every tier label match what the code enforces, and is experimental still qualified against independent peers rather than merely self-tested? | `docs/http2-support.md` § What is supported / not supported; `h2_qualification.rs` peer tests (`:353`, `:383`); `m015d` § The oracle |
| 2 | Could any experimental path be reached from a default, direct, H1, interception, or Python profile? | `Cargo.toml:11-12`; `eggreplay-cli/Cargo.toml:19`; `eggreplay-python/Cargo.toml:18`; `eggreplay-intercept/Cargo.toml:14` |
| 3 | Is `h2` still outbound-only and still independent of `h2-inbound`, and does `h2-inbound` stay non-default? | `Cargo.toml:15-34`; `eggreplay-cli/Cargo.toml:48-53` |
| 4 | Does `prost-reflect` stay behind `grpc`, and does Tonic stay a dev-dependency? | `Cargo.toml:35-37`, `:77-85`; `m015d` § The oracle |
| 5 | Does negotiation fail closed rather than downgrade — `Http2Only` vs cleartext, no H1 headers in H2, no H3 sneaking in? | `h2.rs:17-19`, `:49-70`; `h2_qualification.rs:598`, `:567`; `eggress-outbound/quic` off |
| 6 | Is a malformed route an error rather than a silent direct fallback, and is every diagnostic credential-redacted? | `eggress.rs:74-105`; tests at `:136-160` |
| 7 | Does `h2-inbound-tls` still require operator identity, mint no CA, and never reuse the interception CA? | `Cargo.toml:30-34`; `inbound.rs:66-80`; ADR 0010 § Decision |
| 8 | Are descriptors still caller-supplied, bounded, and never fetched, with no reflection path? | `grpc.rs:212-239`; `m015d` § Descriptors |
| 9 | Do malformed gRPC envelopes stay errors and compressed frames stay opaque? | `grpc.rs:125-158`, `:248-271` |
| 10 | Does the un-terminated bidi case still record rather than synthesize a `grpc-status`, with the live/replay code asymmetry still documented? | `m017` § Non-goals, held; `docs/grpc-and-faults.md` |
| 11 | Is `websocket.rs` still free of connect/accept helpers, and is it still a codec over owned streams? | `websocket.rs:3-5`, `:23-36` |
| 12 | Does the gRPC view stay out of core's dependency boundary, and do views still read already-redacted bytes? | `grpc.rs:11-13`, `:15-17`; `m014d` § gRPC view |

Two code-level details worth re-checking during any of the above, because they
are the kind of thing a reviewer should confirm rather than assume:

- `GrpcError::TrailingBytes` (`crates/eggreplay-http/src/grpc.rs:47-48`) is
  declared and documented, but `parse_grpc_frames` cannot reach it: the loop
  only advances `offset` to a value `<= body.len()`, so it exits at equality and
  the trailing-bytes arm is never taken (`grpc.rs:129-157`). Trailing bytes
  surface as `GrpcError::Truncated`, which is what the unit test asserts
  (`grpc.rs:310-312`). The *behaviour* claim — trailing bytes are an error,
  never reinterpreted — holds; the error token a caller matches on is
  `Truncated`, so a `match` on `TrailingBytes` is dead.
- `is_grpc_content_type` is an exact match or an `application/grpc+` prefix,
  with no parameter parsing and no case folding (`grpc.rs:117-119`). A
  parameterised or differently-cased content type is simply not recognised,
  which degrades to "no view" rather than to a wrong parse.
