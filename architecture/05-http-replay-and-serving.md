> Deep dive for [overview](overview.md).

# 05 — HTTP Replay and Inbound Serving

`crates/eggreplay-http/src/replay.rs` (~3,205 lines) and
`crates/eggreplay-http/src/inbound.rs` (~602 lines) are the two halves of the
offline replay story. `replay.rs` owns *what* is served: it loads a fixture,
drives the matcher, accounts for consumption, materializes bodies, and renders
responses. `inbound.rs` owns *how the listener is built*: it answers one
question — which inbound protocols may this port accept — and delegates every
byte of framing to EggServe.

Neither file owns transport. Inbound H1 runtime, framing, lifecycle, and tunnel
handoff are EggServe's; the outbound client is EggFetch's. The two files meet
at exactly one call: `replay.rs:498` calls
`inbound::start_inbound_server`.

---

## Module contracts

| Concern | `replay.rs` | `inbound.rs` |
|---|---|---|
| Fixture loading and validation | yes | no |
| Matcher session, consumption, scenarios | yes | no |
| Response rendering (headers, bodies, trailers, faults) | yes | no |
| Runtime selection, TLS identity loading, operator limits | no | yes |
| Protocol name parsing and operator status | no | yes |
| EggServe `Service` implementation | yes (`ReplayTunnelService`, `replay.rs:26-61`) | receives it, never wraps it |

The split is the M015B claim made enforceable. `inbound::start_inbound_server`
takes `S: Service` — the caller's *existing* `eggserve_server::Service` — and
passes it through to whichever runtime the policy selects
(`inbound.rs:295-322`). It "never wraps, rewrites, or substitutes it"
(`inbound.rs:287-289`).

### Feature gates

`inbound.rs` is gated by `eggserve` (`lib.rs:22-23`); the H2 capabilities build
on it (`Cargo.toml:29-34`):

| Feature | Admits | Effect on this module |
|---|---|---|
| `direct` (default) | nothing | `inbound` absent; `replay.rs` compiles to matcher/loading only |
| `eggserve` | `eggserve-primitives`, `eggserve-server` | `inbound` + all `#[cfg(feature = "eggserve")]` serving code in `replay.rs` |
| `h2-inbound` | `+ eggserve-core`, `eggserve-core/http2` | `InboundProtocol::Http2Cleartext`, `start_http2`, `InboundServerHandle::Http2` exist |
| `h2-inbound-tls` | `+ eggserve-core/tls`, `eggnet-tls` | `InboundProtocol::Http2Tls` exists and `start_http2` grows its identity branch |

The `h2` feature is **outbound only** and is a separate capability
(`Cargo.toml:15-20`); it must never imply inbound multiprotocol serving.

The M015B lesson is recorded in `lib.rs:15-21`: leaving `inbound` ungated was a
real regression. The `direct` profile pulls no EggServe at all, so the module
failed to compile. The same discipline applies inside `replay.rs` — the
serving-path tests carry `#[cfg(all(test, feature = "eggserve"))]` with the
same explanation (`replay.rs:1746-1750`), and `render_recorded_headers` is
`#[cfg(feature = "eggserve")]` because `eggserve_primitives` is optional
(`replay.rs:1704-1708`).

### One composition point

`start_inner` (`replay.rs:472-508`) is the only place a replay listener is
built. It builds one `ReplayTunnelService` and hands it to
`start_inbound_server`. `start`, `start_append_new` (`replay.rs:348-432`) are
HTTP/1.1-policy wrappers that unwrap the handle and keep the match exhaustive:
the HTTP/1 branch returns a `Serve` error if a future policy ever produced a
multiprotocol runtime.

---

## `ReplayFixture`

`ReplayFixture` is a `Clone` handle over `Arc<Mutex<ReplayState>>`
(`replay.rs:142-145`). Cloning it shares consumption state; it is how one
fixture backs several listeners.

### `load` is metadata-bounded

`ReplayFixture::load` defaults to `Matcher::strict(8)` and
`StreamTimingMode::Immediate` (`replay.rs:153-160`). Every constructor funnels
into `load_inner` (`replay.rs:171-288`), which retains exactly:

- `Vec<MatchCandidate>` built from flow records (`replay.rs:205-209`) — each
  carries a `CandidateBody` descriptor derived from the flow's `BodyRef`, never
  bytes;
- `candidate_in_append: vec![false; session.manifest().flow_count]`
  (`replay.rs:278`) — an index-aligned provenance vector;
- the `Matcher`, a fresh `MatcherSession`, a cloned `Session`, optional
  `ScenarioRuntime`, the decoded `stream-events` map, the timing mode, and the
  WebSocket transcript map.

No blob is opened. The doc comment states the contract and points at
`docs/architecture.md` (`replay.rs:136-141`). The closure proof is
`lazy_load_does_not_read_blob_bytes` (`replay.rs:2059`), which loads, deletes
an *unselected* blob, and shows load still succeeds.

`candidate_in_append` is the single provenance bit. `false` means "came from
the sealed fixture" (read from `Session`, replay `stream-events`); `true` means
"appended into this process" (read from `RecordingSession`, no recorded timing,
because there is no recorded event history for it — `replay.rs:677-685`).

### Load-time validation

`load_inner` fails closed before serving anything:

| Check | Location |
|---|---|
| Required extension understood: `rules` only if a scenario was selected; `stream-events`; `websocket-messages` needs the feature | `replay.rs:184-204` |
| `stream-events` decodes and passes `validate()` (version, duplicate flow ids, ordering/delay, trailer/window limits) | `replay.rs:211-223` |
| Recorded event byte count equals the flow's recorded response body length | `replay.rs:229-248` |
| WebSocket transcript decodes, validates against `WebSocketLimits`, has no duplicate flow ids | `replay.rs:250-266` |
| Non-`Immediate` timing requires event metadata for every non-empty response | `replay.rs:267-274` |

`required_for_replay` means the reader must *apply* the extension, not that the
operator opted into timing delays (`replay.rs:177-183`). `Immediate` is not a
"skip the extension" mode: it still reproduces trailers and terminal mid-body
errors. There is no generic ignore-required-extension switch.

### `MissLocks`

`MissLocks` (`replay.rs:110-134`) is a `Mutex<HashMap<String, Weak<Mutex<()>>>>`
of per-miss-key async locks, used only by the append-on-miss path. `lock_for`:

1. prunes entries whose `Weak` no longer upgrades (`replay.rs:123`), so the map
   is self-cleaning;
2. returns a lock 503 (`"too many concurrent miss keys"`) if the registry would
   exceed 2,048 entries (`replay.rs:127-129`) — a bounded registry, not
   unbounded key space;
3. registers a downgrade, so a lock lives exactly as long as an in-flight miss.

`append_miss` hashes the canonical request plus body to a SHA-256 key
(`replay.rs:1473-1480`) and holds the lock across the upstream transaction, so
concurrent identical misses collapse into one recording
(`replay.rs:1481-1584`). It is sealed-off by construction: no `MissLocks`
instance exists unless `start_with_protocol_and_append` supplied one
(`replay.rs:464`).

### `ReplayError`

Three variants (`replay.rs:63-76`):

| Variant | Meaning |
|---|---|
| `Store(StoreError)` | fixture loading or body verification failed |
| `State(String)` | poisoned or invalid replay state (extension, decode, validation) |
| `Serve(String)` | EggServe could not start or serve — `eggserve`-gated only |

The `Serve` variant is `#[cfg(feature = "eggserve")]`, so the error taxonomy
itself shrinks with the capability.

---

## Matcher session and consumption

### Request projection

`request_from_eggserve` (`replay.rs:1631-1679`) converts the canonical request
once, and that projection is the *only* request the matcher sees:

- query is parsed into ordered `QueryPair`s (`:1637-1644`);
- authority comes from the head, falling back to `host` (`:1645-1654`);
- headers are lowercased and `host` dropped (`:1724-1733`);
- WebSocket handshakes get `normalize_websocket_handshake_headers`
  (`:1659-1663`) — this is why `h2_inbound_serving`'s `Connection: close`
  finding was a test bug, not a product bug (M015B closure §164-186);
- the body is projected as `BodyRef::Empty` when bytes were read, else
  `BodyRef::Absent` (`:1672-1676`);
- trailers become `HeaderEntry`s (`:1664`, `:1736-1744`).

### Selection

`handle_request` takes the state lock, clones the candidate vector, the
matcher, the `Session`, and the provenance vector, then calls
`select_with_loader` (`replay.rs:624-663`). `ConsumptionMode` is chosen by
server kind, not by fixture (`replay.rs:656-660`):

- `Once` for sealed replay — each recorded flow serves once, then
  `Exhausted`;
- `Unlimited` for append-on-miss, because the fixture is expected to grow.

`MatcherSession` (`matching.rs:491-521`) holds a `BTreeSet<usize>` of consumed
indices and an optional `last` for `RepeatLast`; it is in-memory and never
persisted. It lives inside `ReplayState`, so consumption is per-`ReplayFixture`
and never leaks into the store or across servers (`:280-281`).

The lock is held only for match plus metadata clone. Rendering and streaming
happen after release, which is what makes the served future `Send` and keeps
concurrent requests flowing (`:606-608`).

### A miss is offline

The invariant: **a miss never reaches the network.** With no append context,
`NoMatch` returns a fixed `404` body
(`"eggreplay replay no match\n"`, `replay.rs:701-704`). The sealed server holds
no client, no upstream URI, and no `RecordingSession` — `AppendContext` is
`None` (`replay.rs:477`, `:656`), so there is nothing to reach for. Reaching
EggFetch requires the operator to have called `start_append_new` /
`start_with_protocol_and_append` explicitly.

`Exhausted` is a distinct `409` (`replay.rs:698-700`): the request *did* match,
but its single-use flow was already served. That distinction is preserved
because it is an operational signal, not a miss.

Recorded `FlowOutcome::Error` projects as `502` with the stable
`recorded upstream error: {category}` shape (`:689-695`), matching
`ScenarioFault::TransportError` (`:1399-1401`).

### Near-miss reporting

Near-miss diagnostics exist on the core result — `MatchResult::NoMatch` and
`Exhausted` both carry `Vec<NearMiss>` with candidate index, cost, dimensions,
and a redaction-safe summary (`matching.rs:164-192`) — and the matcher bounds
them at `max_near_misses` (8 by default for `Matcher::strict`,
`replay.rs:154`).

At the serving boundary, `replay.rs` discards them: every match site binds
`{ .. }` rather than the `near_misses` field (`:698`, `:701`, `:1517`,
`:1520`). Diagnostics are therefore available to a caller driving the matcher
directly but are not surfaced by the replay server, which emits only the fixed
`404`/`409` bodies. This is a deliberate-looking simplification, not a
requirement — worth knowing before expecting a 404 to explain itself.

---

## Body materialization

### Request side: descriptor versus bytes

`CandidateBody` (`matching.rs:54-68`) is `Absent` / `Empty` /
`Digest { sha256, length }` / `Inline(Vec<u8>)`. `from_body_ref` builds it
from a `BodyRef` without opening anything; `eggreplay-core` is filesystem-free
and `Inline` exists only for tests and explicitly narrowed loads
(`matching.rs:47-52`).

Two paths in `body_matches_lazy` (`matching.rs:400-472`):

| Path | Trigger | Cost |
|---|---|---|
| Exact | `ExactBytes`/`ExactText` on a `Digest` | `actual_len == length && actual_digest == sha256` — no load (`matching.rs:451-457`) |
| Semantic | `SemanticJson`, or any candidate carrying body redaction markers | loader called, but only after the candidate is *narrowed* — i.e. method, authority, path, query, and headers all matched (`matching.rs:359`, `:418-421`, `:458-471`) |

A candidate that already differs on a non-body dimension never triggers a load
(`matching.rs:298-304`). That is what keeps a large fixture's resident memory
independent of its body count.

**The loader is owned by the adapter, not the matcher.** `replay.rs:634-651`
supplies it as a closure over two things: the sealed `Session` and, for
append-origin candidates, the `RecordingSession`. Its `BodyRef::Absent | Empty`
arm returns the already-resident `candidate.request_body` (or an empty vec)
without touching either store. `eggreplay-core` never learns a path.

### Response side: three paths

Every selected response lands in exactly one of these.

**1. `ResponseBody::Empty` — absent or empty.** `BodyRef::Absent | BodyRef::Empty`
builds a status+headers response with `ResponseBody::Empty`, no allocation and
no open (`replay.rs:781-794`; the append-side equivalents at `:747-759` and
`:1611-1613`). The C001 closure records this as the zero-allocation path.

**2. `ResponseBody::Stream` — the selected blob.** The main path
(`replay.rs:795-1038`):

- `store.open_blob(&blob)` yields a validated `BlobHandle` giving `len` and
  `sha256` without reading the body; the std file is adapted to Tokio
  (`:796-802`);
- reads are capped at 64 KiB per chunk, further capped by the remaining
  recorded segment length when `stream-events` segments are present
  (`:911-915`);
- a `Sha256` hasher is updated with every chunk (`:944-946`), and at EOF the
  final digest *and* the byte count are compared to the handle's values
  (`:919-939`);
- over-reading past the recorded length is caught mid-stream
  (`:948-965`);
- a mid-stream I/O error becomes a `ResponseStreamError` (`:981-996`).

So integrity is verified *incrementally while streaming*, never by a second
pass. `corrupt_replaced_body_fails_integrity` (`replay.rs:2381`) is the
same-length-replacement case.

**3. Narrowed-candidate materialization** — the semantic request path above.
It never touches the response side; the response is always streamed or empty.

Two smaller renderings sit alongside these. Synthetic diagnostic responses
(`404`, `409`, `502`) use `ResponseBody::Bytes` with a fixed short body
(`response_bytes`, `:1316-1332`). Scenario-rendered responses are the third
stream producer: `fault_body_stream` (`:1434-1462`) streams an in-memory `Vec`
in authored chunk sizes with per-chunk delays and an optional terminal error,
declared against a length that deliberately *exceeds* the sent prefix for
truncating faults so truncation is client-visible.

---

## Inbound protocol policy (`inbound.rs`)

The module doc is the clearest statement of the boundary in the codebase
(`inbound.rs:1-30`): "a **policy and composition** boundary, not a second
transport."

### `InboundProtocol`

`#[non_exhaustive]`, `#[default]` on `Http1` (`inbound.rs:44-80`):

| Variant | Runtime | Gate |
|---|---|---|
| `Http1` (default) | `eggserve-server` direct | always |
| `Http2Cleartext` (`h2c`, prior knowledge) | `eggserve-core` | `h2-inbound` |
| `Http2Tls { certificate, private_key }` | `eggserve-core` + `eggnet-tls` | `h2-inbound-tls` |

The accessors are three honest predicates: `token()` (`:87-95`),
`serves_http2()` (`:101-103`, always `false` in a build that cannot express an
H2 policy), `terminates_tls()` (`:106-112`), `is_cleartext()` (`:115-124`).
`describe()` composes them.

### `InboundProtocolDescription`

Four fields — `protocol`, `serves_http2`, `terminates_tls`, `cleartext`
(`inbound.rs:138-152`). It deliberately holds no certificate path, no key path,
and no identity material, so operator status output is "safe to log, diff, and
publish" (`:140-141`). `description_never_carries_key_material`
(`:499-509`) asserts the serialized form mentions no `certificate`,
`private_key`, `key`, or `BEGIN`.

### `H2Limits`

Four optional EggServe-owned limits: concurrent streams, header-list size,
frame size, keep-alive PING interval (`inbound.rs:154-176`). Every field
defaults to `None` = "keep EggServe's own default". The type exists in every
build so the serving signature is stable, but `apply` (`:178-196`) is
`h2-inbound`-gated — in an H1-only build the value is simply never consulted.
EggReplay can only *tighten* what EggServe chose, never substitute its own
guesses.

### `InboundServingError`

Three variants, **all startup refusals** (`inbound.rs:198-217`):
`Unsupported(&'static str)`, `TlsIdentity(String)`, `Runtime(String)`.

### `InboundServerHandle`

`Http1(eggserve_server::ServerHandle)` or `Http2(eggserve_core::server::ServerHandle)`
(`inbound.rs:219-234`). Every caller-visible operation — `local_addr`,
`shutdown`, `wait` — is protocol-neutral, so product code never branches on
which runtime it got. It is not `Debug` on purpose: an invented `Debug` for an
EggServe handle could leak runtime internals into operator output (`:219-226`).

### One service, two runtimes

This is the load-bearing argument (`inbound.rs:9-18`, ADR 0010 §3):

> `eggserve_core::server::Service` **is** `eggserve_server::service::Service`,
> and `eggserve_core::server::Request` **is** `eggserve_primitives::Request`.

Two crates, two runtimes, one service type. Selecting an HTTP/2 variant hands
the *existing* service implementation to a different runtime. It "does not fork
the runtime, and it cannot fork any product authority, because there is only
one service to fork" (`inbound.rs:16-18`).

Concretely, `start_inbound_server` takes `S: Service` once and either calls
`start_http1` (`:307-309`) or `start_http2` (`:310-320`) with the *same* value.
The same `ReplayTunnelService` — therefore the same matcher, consumption,
scenario, redaction, and response-rendering authorities — serves both
(`replay.rs:375-381`).

Both paths set their invariants explicitly rather than relying on defaults, so
"the H1 path is untouched" is checkable, not a comment
(`inbound.rs:324-329`): `Http1RequestTargetMode::OriginOnly` plus
`eggserve_owned()` policy and admission ownership (`inbound.rs:345-347`).
EggServe Core projects HTTP/1.1 connections onto those same settings, so a
client negotiating H1 on the H2 listener gets the same request-target boundary
(`inbound.rs:369-374`).

### Why an enum, not a flag

`inbound.rs:20-23`: cleartext HTTP/2 and TLS+ALPN HTTP/2 "are different trust
and exposure decisions and should not be a single boolean." `Http2Cleartext`
is unencrypted, so it exposes recorded traffic in the clear on that port; it
exists because the replay server and recording gateway are operator-scheduled
local listeners, and "it is not a default, and it is not for untrusted
networks" (`:59-63`). A `--inbound-h2` boolean cannot distinguish those, and a
`tls: true` boolean would let a mistyped flag silently pick the cleartext
variant.

### Prior-knowledge h2c is never an accidental downgrade

`inbound.rs:50-58`: EggServe classifies a cleartext stream as HTTP/2 only when
the complete 24-byte connection preface arrives; a stream diverging at any byte
is HTTP/1.1. "A client therefore selects the protocol by speaking it." The
policy cannot be entered by sniffing, and a client that does not send the
preface is served HTTP/1.1 exactly as before. M015B pins both directions
(`cleartext_policy_serves_h2_with_prior_knowledge`,
`cleartext_policy_does_not_capture_a_plain_h1_client`).

---

## Fail-closed behaviour

An unbuildable protocol policy is a configuration error. Every refusal is at
startup, and there is no path from "asked for H2" to "served H1".

| Refusal | Where |
|---|---|
| Build cannot express the requested protocol | `InboundServingError::Unsupported` (`inbound.rs:206-210`) |
| TLS named without identity material | `InboundServingError::TlsIdentity` (`inbound.rs:448-452`) |
| Identity material fails to load | `InboundServingError::TlsIdentity` (`inbound.rs:408-409`) |
| EggServe rejects the composed runtime | `InboundServingError::Runtime` (`inbound.rs:356`, `:360`, `:363`, `:420`, `:425`, `:429`) |
| Unknown policy name | `Unsupported("unrecognised inbound protocol name")` (`inbound.rs:453-455`) |

The doc says the intent directly: "A listener never degrades to a weaker
protocol than the operator asked for: if the requested policy cannot be built,
the server does not start" (`inbound.rs:200-202`).

**A binary lacking the inbound-H2 features refuses `--inbound http2`.**
`parse_protocol`'s H2 arm is `h2-inbound`-gated, so a build without it falls
through to the catch-all `Unsupported` — "so a build without `h2-inbound` fails
closed on `--inbound http2` instead of quietly serving HTTP/1.1 under an HTTP/2
label" (`inbound.rs:440-442`). `supported_protocol_names`
(`inbound.rs:459-473`) only advertises names the build can act on, and
`advertised_names_match_what_can_be_started` (`inbound.rs:575-590`) pins the
two directions: nothing advertised fails to resolve, nothing resolves that is
not advertised.

TLS is deliberately **not** name-resolvable. `parse_protocol("http2-tls")`
returns `TlsIdentity` even in a TLS-enabled build, because naming the policy
without supplying the identity is a refusal, not a default
(`inbound.rs:432-442`). Construct `InboundProtocol::Http2Tls` directly to
supply paths. M015B mints no CA and never reuses the interception CA as an
implicit server identity (M015B closure §87-97).

### The one protocol-aware rendering rule

`render_recorded_headers` (`replay.rs:1681-1721`) is the *only* place the
product knows the inbound version. Recorded headers are version-neutral facts
about an origin, but `content-length` is a framing fact and framing is
per-connection: H1 EggServe validates the recorded value against the bytes it
writes, whereas over H2 a stale value leaves the peer waiting for bytes that
never arrive. So on HTTP/2 the header is dropped and the transport frames from
the DATA it observes.

It is applied once, before every emission site (`replay.rs:723-725`).

Connection-specific headers are deliberately *not* filtered. `connection`,
`keep-alive`, `proxy-connection`, `transfer-encoding`, `upgrade`, and `te` are
transport-owned on both protocols — Hyper's H2 server strips them — and a
product-side copy "would be a second, weaker copy of a rule the transport
already enforces — and would silently differ from it" (`replay.rs:1696-1702`).

At the CLI, the policy resolves *before* any filesystem precondition, so a
mistyped `--inbound` is reported as a configuration error rather than masked by
a fixture-path error (M015B closure §196-202).

---

## Streaming and SSE replay

The required-when-used `stream-events` extension carries bounded HTTP body
event and terminal semantics. `docs/architecture.md`'s "Streaming semantics
(M010)" states the rule: immediate replay applies terminal semantics with zero
delay; recorded/scaled timing is explicit.

### `StreamTimingMode` as consumed here

`Immediate` / `Recorded` / `Scaled(f64)` (`stream.rs:206-212`). `load_inner`
uses it as a *precondition* as well as a scheduler:

- `Immediate` is the default and imposes no requirement that a
  `stream-events` extension exist (`replay.rs:224-228`);
- any other mode requires the extension (`:224-227`) *and* per-flow event
  metadata for every non-empty response (`:267-274`);
- per event, `delay_ns(delta - previous_delta, accumulated)`
  (`replay.rs:810-822`) enforces per-delay and total-flow ceilings inside
  core; a violation becomes a `500` with `"invalid replay timing"`, not a hang
  (`replay.rs:815-821`).

### Schedule and emission

`replay.rs:803-846` folds the recorded response events into a
`Vec<TimedStreamStep>`: `Data { delay_ns, length }` for chunks,
`Error { delay_ns, message }` for a recorded terminal error, plus separate
trailer and terminal delays. `End` records a delay rather than a step.

The `futures_util::stream::unfold` at `:848-1000` walks that schedule. Its
chunk size is `min(remaining_segment, 64 KiB)` when a segment is active, else
64 KiB (`:911-915`).

**Immediate is zero-delay but not delay-free in a semantic sense.** When the
computed delay is zero, the stream does `tokio::task::yield_now().await`
instead of sleeping, with the reason inline: it is a cooperative yield "so the
transport can flush prior DATA before a terminal Error truncates the connection.
This is not a time delay: Immediate remains zero-delay (microseconds), but
prevents back-to-back DATA+Error from losing buffered DATA on truncation"
(`:881-889`).

### Terminal errors and trailers

A recorded `Error` event drops the file and hasher and yields
`Err(ResponseStreamError::new("recorded stream error: {category}/{phase}"))`
(`:890-907`) — a real truncated stream, never a synthetic status. This is why
`terminal_error` switches the response from
`ResponseStream::with_known_length_and_trailers` to
`ResponseStream::with_trailers` (`:1022-1026`): a declared length that exceeds
what can be delivered would hang the peer.

Trailers are emitted after the trailer-plus-terminal delay, and preserved
verbatim into an `eggserve_primitives::Trailers` block (`:1004-1021`).
`trailer_stream_preserves_recorded_trailers` (`replay.rs:2333`) is the
coverage.

Scenario faults use the same machinery on a rendered in-memory body
(`:1334-1403`), with `CloseBeforeResponse` / `CloseAfterBytes` keeping the
*full* declared length so truncation is explicit rather than silently
successful.

---

## What replay deliberately does not do

| Not done | Why, and where |
|---|---|
| Outbound network on a miss | Sealed replay holds no `AppendContext`, so `NoMatch` can only return `404` (`replay.rs:701-704`). `MissLocks`, the `Client`, and the `RecordingSession` exist only in the append context. |
| A second matcher | One `Matcher` and one `MatcherSession` in `ReplayState`; H2 reuses the same code object (`replay.rs:624-663`, ADR 0010 §Decision). |
| A second renderer | `render_recorded_headers` is the single protocol-aware step, applied once at `replay.rs:725`. M015D's gRPC work is explicitly barred from adding another (M015B closure §282-284). |
| A second service or service wrapper | `start_inbound_server` "never wraps, rewrites, or substitutes it" (`inbound.rs:287-289`). |
| H2 interception (MITM) | `eggreplay-intercept` stays on `eggserve-server`; the boundary lane asserts it never gained `eggserve-core` (ADR 0010 §Decision, `docs/http2-support.md` § What is not supported). |
| HTTP/3 / QUIC | Unsupported, deferred by ADR 0009; `eggserve-core/http3`, `eggress-outbound/quic`, `eggfetch-core/http3` all remain disabled (ADR 0010 §Decision D). |
| WSS and extended-CONNECT WebSockets | The replay handshake requires `HttpVersion::Http11` and `TunnelKind::Http1Upgrade` (`replay.rs:1062-1064`); inbound H2 with a WebSocket upgrade is rejected 400 (`replay.rs:528-533`, `:555`). Stays correct (`docs/architecture.md` M011, M015B closure §285-287). |
| Treating protocol as a matching dimension | Protocol selection is a listener property, never a match dimension (M015B closure §279-281). |
| A generic reverse proxy | Out of scope (`docs/http2-support.md` § What is not supported; `docs/non-goals.md` § Deferred by decision). |

---

## Review checklist

| # | Check | How to verify | Reference |
|---|---|---|---|
| 1 | Metadata-bounded load still holds | Load a fixture, delete an *unselected* blob, load still succeeds; selected candidates are `Digest` | `replay.rs:205-209`, `:136-141`; `lazy_load_does_not_read_blob_bytes` at `:2059` |
| 2 | No byte materialization on the exact fast path | Loader call count is 0 for `ExactBytes`; loader fires at most once, only for a narrowed index | `matching.rs:298-311`, `:451-457`; `replay.rs:634-651` |
| 3 | Loader ownership stays in the adapter | `eggreplay-core` has no filesystem dependency; the loader closure lives in `replay.rs` | `matching.rs:47-52`; `replay.rs:634-651` |
| 4 | Stream chunking and digest verification | Chunks are `<= 64 KiB`; EOF compares both byte count and SHA-256; over-read aborts mid-stream | `replay.rs:911-915`, `:919-939`, `:948-965` |
| 5 | Trailers preserved | Trailer future emits recorded trailers after the recorded delay; known length used when no terminal error | `replay.rs:1004-1026`; `:2333` |
| 6 | Absence stays zero-allocation | `BodyRef::Absent \| Empty` → `ResponseBody::Empty`, no `open_blob` | `replay.rs:781-794` |
| 7 | Fail-closed protocol policy | Unknown name, `--inbound http2` without `h2-inbound`, and TLS-from-a-name all refuse; no H1-under-an-H2-label | `inbound.rs:440-457`; `:522-569` |
| 8 | Exactly one protocol-aware rule | `render_recorded_headers` is the only version branch, and it drops only `content-length` | `replay.rs:723-725`, `:1681-1721` |
| 9 | Advertised names match build capability | Every `supported_protocol_names()` entry resolves, and vice versa | `inbound.rs:575-590` |
| 10 | Defaults remain H1 | `InboundProtocol::default() == Http1` in a build that *can* serve H2 | `inbound.rs:487-495` |
| 11 | Consumption accounting under concurrency | State lock spans match + metadata clone only; streaming happens after release; two concurrent `Once` requests get distinct flows | `replay.rs:606-608`, `:624-706`; `:2423`, `:2550` |
| 12 | Miss is offline | Sealed `404`; `Exhausted` is `409`; only an explicit append context reaches EggFetch | `replay.rs:698-704` |
| 13 | Miss de-duplication holds | Concurrent identical misses hit one upstream call; the re-select under lock (`replay.rs:1483-1522`) closes the fill-then-recheck window | `replay.rs:1465-1584`; `:2742`, `:2857` |
| 14 | Load-time validation still fails closed | Unknown required extension, bad `stream-events`, and byte-count disagreement all reject at load | `replay.rs:184-274`; `:3019`, `:3047` |
| 15 | Terminal semantics without delay | `Immediate` reproduces a recorded terminal error as a stream error, with a yield before truncation | `replay.rs:881-907`; `:3083` |
| 16 | Feature gates intact | The `direct` profile still compiles with `inbound` gated out | `lib.rs:15-23`; `replay.rs:1746-1750` |
