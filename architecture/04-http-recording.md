> Deep dive for [overview](overview.md).

`crates/eggreplay-http/src/recording.rs` is the acquisition path of EggReplay:
it is where an `http::Request` becomes a durable semantic `Flow`, where bodies
become addressable blobs, and where redaction is applied before anything is
published. At ~4,600 lines including its own test module, it is the largest
file in the workspace, and it is the crate that must never grow a transport.

## Module contract

The file's own one-line description is the contract: "EggFetch-native
streaming observation and schema conversion" (`recording.rs:1`). It owns
conversion, observation, redaction-before-publication, durable flow append, and
session finalization. It owns no sockets, no TLS, no framing, and no listener.

What it owns:

| Responsibility | Site |
|---|---|
| `http::Request` -> `HttpRequest` head conversion | `request_head`, `recording.rs:2289` |
| Body observation (tee into staging, trailers, stream events) | `TeeStream` `recording.rs:1938`, `TeeSessionStream` `recording.rs:2149` |
| Pre-publication redaction of staged bodies | `transform_body` `recording.rs:102`, `finish_sink_redacted` `recording.rs:2009`, `finish_session_sink_redacted` `recording.rs:2065` |
| Flow assembly, provenance, annotations, header reconciliation | `recording.rs:362-387`, `recording.rs:612-643` |
| Gateway composition over EggServe | `GatewayTunnelService` `recording.rs:682`, `start_recording_gateway_with_protocol` `recording.rs:814` |
| Inbound-to-upstream request projection | `gateway_request` `recording.rs:973` |
| WebSocket 101 orchestration and conversation capture | `gateway_websocket_request` `recording.rs:1068`, `relay_websocket` `recording.rs:1551` |
| Conversation finalization barrier | `ConversationCompletion` `recording.rs:1377` |
| Async finalization and blob-drain helpers | `recording.rs:925`, `recording.rs:937`, `recording.rs:961` |

What it delegates, and to whom:

- **Outbound transport** to `eggfetch_core::Client`
  (`client.execute_http_body_default`, `recording.rs:189` and `recording.rs:441`).
  EggReplay never opens an outbound connection.
- **Inbound lifecycle, framing, and tunnel admission** to
  `eggserve_server::Service` / `ServerHandle`, reached through
  `crate::inbound::start_inbound_server` (`recording.rs:898`).
- **Durability** to `eggreplay_store::{SessionWriter, RecordingSession}` and
  their blob writers. This module never computes a digest or a byte length; it
  writes bytes and reads back the `BodyRef` the store produced.
- **Canonical semantics** to `eggreplay_core`: `Flow`, `HttpRequest`,
  `HttpResponse`, `HeaderEntry`, `WebSocketConversation`,
  `redact_flow`, `apply_json_redaction`, `apply_form_redaction`,
  `reconcile_headers_after_body_redaction`, `push_body_markers`.

### Feature gates

`direct` is the crate default (`Cargo.toml:11`) but gates nothing in this file:
both `record_request` and `record_request_with_session` are ungated, which is
why the interception proxy and the outbound-H2 qualification suite can record
without EggServe.

| Feature | Effect on this file |
|---|---|
| `eggserve` | Gates everything inbound: the EggServe imports (`recording.rs:27-41`), `GatewayTunnelService` (`:681`), all three `start_recording_gateway*` entry points (`:735`, `:765`, `:812`), `gateway_request` (`:971`), `eggserve_body_stream` (`:1818`), `file_stream`/`response_stream` (`:1854`, `:1886`). `lib.rs:15-23` records that leaving `inbound` ungated was a real M015B regression that broke the `direct` profile. |
| `websocket` | **Never sufficient alone.** Every WebSocket item is `#[cfg(all(feature = "eggserve", feature = "websocket"))]` (`:1066`, `:1376`, `:1489`, `:1525`, `:1550`, `:1776`). The tunnel capability and the 101 accept handoff are EggServe types; a codec over already-owned streams has nothing to attach to without them. |
| `h2` | Outbound only (`Cargo.toml:15-20`) and inert here. The only recording-side consequence is that a response *can* arrive as `http::Version::HTTP_2`, which `negotiated_version_annotation` (`recording.rs:151`) then annotates. The function itself is ungated, because it is a pure `Version` -> optional annotation mapping. |
| `h2-inbound` | Changes the *return type* of `start_recording_gateway_with_protocol`. `InboundServerHandle` (`inbound.rs:228`) only has an `Http2` variant under this feature, so the `_with_websockets` wrapper can return a plain `ServerHandle` and keeps the impossible H2 arm behind `#[cfg(feature = "h2-inbound")]` (`recording.rs:794-802`). |
| `eggress` | No effect. The Eggress route is not established here; the caller passes a `PhysicalRoute` that recording attaches to the flow as a description of how the client was configured (`recording.rs:369`, `:619`, `:1346`). Routing is `crate::eggress`'s job. |

### The `start_recording_gateway*` family

There are three functions and one implementation
(`recording.rs:737`, `:767`, `:814`):

```
start_recording_gateway                 -> _with_websockets(default options) -> unwrap Http1
start_recording_gateway_with_websockets  -> _with_protocol(Http1, H2Limits::default())
start_recording_gateway_with_protocol    -> the only builder
```

The two wrappers exist to keep existing callers and tests source-identical
across M011C and M015B. M015B's closure states the intent: the protocol-aware
entry point was added and "`ReplayFixture::start` and
`start_recording_gateway_with_websockets` are preserved verbatim as
HTTP/1.1-policy wrappers, so every existing caller and test is unchanged"
(`plans/closure/m015b-inbound-http2-gateway-and-replay.md:41-44`).

## Error model

Two error types, deliberately different in visibility.

`BodyError` (`recording.rs:43-52`) is a private `String` newtype. It exists
because the tee streams must implement `http_body::Body` with a single concrete
`Error` type (`recording.rs:1943`, `:2154`) and because the inbound bridge
`eggserve_body_stream` must produce `Frame<Bytes>` items with one error type
(`recording.rs:1821`). It carries a message, nothing more. The *semantic*
classification of a body failure does not live here; it is taken from the
EggFetch error before it is flattened (see below).

`HttpError` (`recording.rs:56-69`) is the public crate error, re-exported at
`lib.rs:34`:

| Variant | Meaning | Origin |
|---|---|---|
| `Conversion(String)` | Semantic conversion failed: a request URI with no scheme or authority (`recording.rs:2296`, `:2300`), an invalid inbound header value, a zero WebSocket bound (`recording.rs:830`), a listener composition failure (`:907`). | This module. |
| `Fetch(FetchError)` | EggFetch rejected or failed the request. | `#[from]` EggFetch. |
| `Body(String)` | A stream body failed while observed, a sink was poisoned or already closed, a bounded staging read failed, or a structured redaction failed closed. | This module. |
| `Store(StoreError)` | The fixture refused the flow or a blob. | `#[from]` the store. |

### Mapping into `ErrorPhase` / `ErrorCategory`

`eggreplay_core` owns the stable vocabulary; `error_classify.rs` chooses
(category, phase) pairs in three places, because the phase is the only thing that
legitimately differs. The module is shared with the candidate path
(`regression.rs:5`, `:710-718`), so a route failure reads the same from a
recorded run and a candidate run of one request; see
[06](06-regression-and-reporting.md) § Error classification.

`map_fetch_error` (`error_classify.rs:37`) classifies a *request-level* failure:

| EggFetch error | Category | Phase |
|---|---|---|
| `Tls`, `CertificateVerification`, `HostnameVerification`, `TlsConfig`, `CaBundle` | `Tls` | `Tls` |
| `Timeout`, `TransportIoTimeout` | `Timeout` | `Timeout` |
| `Body`, `DecodedBodyTooLarge`, `Decompression`, `DecompressionRatioExceeded` | `Other` | `Body` |
| `Protocol`, `Hyper`, `HyperClient` | `Protocol` | `Headers` |
| `Connect`, `Io`, `Pool` | `Unreachable` | `Connect` |
| `CustomTransport` | delegated to `classify_dial_error` | delegated |
| anything else | `Other` | `Other` |

`classify_body_error` (`error_classify.rs:107`) is the body-phase counterpart over
the *same* `FetchError` type. The phase becomes `Body` because that is where the
failure happened, with three deliberate exceptions: an expired deadline stays
`Timeout`/`Timeout` (`:112-114`), and `CustomTransport` reuses
`classify_dial_error`'s category with the phase forced to `Body` (`:122-126`).
`classify_dial_error` (`error_classify.rs:82`) is the shared dial vocabulary -
`Connection -> Unreachable/Connect`, `Timeout -> Timeout/Timeout`,
`Authentication -> Policy/Connect`, `Rejected -> Policy/Policy`,
`Other -> Other/Other` - and its doc comment records why one specific mapping
was refused: EggFetch's typed evidence collapses all connection-establishment
failures to one kind, so inferring `ConnectionRefused` "would be a guess. An
honest general category beats a specific wrong one"
(`error_classify.rs:62-66`).

Inbound body errors are *not* classified, on purpose. When the EggServe-side
request body fails, the event records `Other`/`Body` with a comment explaining
that the inbound error type "carries no EggFetch category to consult", and that
"claiming a category here would be a guess, and a wrong guess in a recording is
worse than an honest `Other`" (`recording.rs:2183-2195`).

The gateway translates `HttpError` into `ServiceError` at its boundary: an
internal fault becomes `ServiceError::internal` (e.g. `recording.rs:1031`), and
a recorded upstream failure becomes a 502 carrying the category
(`recording.rs:1051-1054`). WebSocket rejections use explicit status codes:
400 for a malformed upgrade (`recording.rs:1097`, `:1125`, `:1133`), 403 for a
disabled-admission denial (`recording.rs:1059-1064`), 502 for a failed or
dishonest upstream handshake (`recording.rs:1193`, `:1218`, `:1230`), and 504
for the 30-second handshake timeout (`recording.rs:1192`).

## Single-request recording

`record_request` (`recording.rs:166`) takes a `&mut SessionWriter`;
`record_request_with_session` (`recording.rs:407`) takes a `&RecordingSession`.
The bodies are near-duplicates by construction, and the session variant is the
one the product uses: the CLI, the Python lifecycle, `eggreplay-intercept`, and
`replay.rs:1563` all call `record_request_with_session`. `record_request`
survives as a public API exercised by `h2_qualification.rs` and the unit tests
at `recording.rs:3889`. The doc comment on the session variant states the
distinguishing property: "Body sinks are independent of the flow-log lock:
`begin_blob` never holds the append mutex while bytes stream, and `append_flow`
serializes only the final bounded metadata write. No network await holds the
session lock" (`recording.rs:400-402`).

### The pipeline

1. **Head conversion.** `request_head` (`recording.rs:2289`) produces an
   `HttpRequest` with method, scheme, authority, path, parsed query pairs, and
   lowercased headers. Two things are decided here. Userinfo is stripped from
   the logical authority - "Userinfo must never persist in logical authority"
   (`recording.rs:2297-2306`) - and any header value that is not valid UTF-8 is
   hex-escaped while `lossy_header_values` is set (`recording.rs:2332-2355`),
   which later becomes the `conversion / opaque-header-values-escaped`
   annotation (`recording.rs:383-386`). Query values are kept as an ordered
   `Vec<QueryPair>`, not a map, so repeated keys survive.

2. **Stage, then observe.** A staging blob is opened *before* the request is
   sent (`recording.rs:426-427`), wrapped in `Arc<Mutex<Option<RecordingBodyWriter>>>`.
   `tee_body_with_session` (`recording.rs:2117`) wraps the caller's body so that
   every DATA frame is written to staging *and* forwarded unchanged to EggFetch
   (`recording.rs:2209-2248`), while trailers are copied to a side channel
   (`recording.rs:2258-2260`) and request-side stream events are pushed with a
   running `offset` (`:2225-2246`). The point is that nothing is buffered in
   memory: without structured selectors, DATA streams straight to disk.

3. **Execute.** One `client.execute_http_body_default` (`recording.rs:441`).

4. **Publish the request body.** `finish_session_sink_redacted`
   (`recording.rs:2065`) takes the writer out of the `Option` and decides how it
   ends. This is the redaction-before-publication step, and its ordering is the
   single most important property in the file.

5. **Observe the response body.** A second staging blob is opened
   (`recording.rs:464-465`) and the response body is pulled frame by frame
   (`:471`). Each DATA frame is appended to staging and recorded as a
   `StreamEventKind::Data { offset, length }` with a `elapsed_ns` delta; trailers
   are recorded and also kept for the flow; a body error is classified by
   `classify_body_error` and recorded as a `StreamEventKind::Error`, after which
   the loop breaks (`recording.rs:472-490`); a clean end appends
   `StreamEventKind::End` (`:530-538`).

6. **Publish the response body**, again through
   `finish_session_sink_redacted` semantics, inline at `recording.rs:540-587`.

7. **Assemble the flow.** `Flow::new` (`:612`), `completed_at_ms`, the transport
   annotation, `provenance.mode = "eggfetch-native"` and
   `provenance.observer = "eggreplay-http"` (`:617-618`), the caller's
   `physical_route` (`:619`), then `eggreplay_core::redact_flow` for header and
   query redaction (`:620`), then the body-redaction markers (`:621`).

8. **Reconcile framing.** If the request body was redacted, request headers are
   fixed up with the *new* length and the resulting notes become annotations
   (`recording.rs:628-638`); the same happens for the response at `:588-601`.
   Then stream-event lengths are reconciled (`:643`) and the flow plus its
   events are appended (`:644-650`).

### Redaction before blob finalization

The ordering, exactly as the code performs it in
`finish_session_sink_redacted` (`recording.rs:2065-2115`):

1. Empty body -> `staged.finish()` immediately (`:2079-2082`).
2. No structured selectors configured -> `staged.finish()` immediately
   (`:2083-2087`). The raw bytes *are* the intended content here.
3. Selectors configured but this media type is neither JSON nor form ->
   `drop(staged)` and return `HttpError::Body("body redaction requested but
   media type unsupported; failing closed")` (`:2090-2096`). Dropping the writer
   aborts the staging file, so raw sensitive bytes never become a finalized,
   addressable blob.
4. Transform required ->
   `staged.read_staging_bounded(max_structured_bytes)` (`:2100-2102`) is the
   bounded read, then **`drop(staged)` (`:2103`)**, then `transform_body`
   (`:2104-2105`), then a *fresh* blob is opened, written, and finalized
   (`:2109-2113`).

The critical property is that `drop(staged)` at `recording.rs:2103` runs
*before* the redacted blob is published. The raw staging file is closed and
removed (the store's `Drop` for `RecordingBodyWriter`,
`eggreplay-store/src/lib.rs:885-891`) before any finalized blob containing
transformed bytes exists. The response side does the same inline, with the drop
at `recording.rs:565` ahead of the fresh `begin_blob()` at `:576`. The function
doc says the same thing in prose: "Staging bytes transform before any finalized
blob exists; raw staging is dropped (Drop cleans) on transform or fail-closed
paths, so sensitive bytes never become addressable blobs"
(`recording.rs:2004-2008`), and the public entry point repeats it: "Raw bytes
never become finalized blobs. An aborted stream leaves no referenced blob"
(`recording.rs:164`).

`transform_body` (`recording.rs:102`) is the selector dispatcher. JSON pointer
redaction applies when the media type is `application/json` or `*+json`
(`recording.rs:94-96`); form redaction applies to
`application/x-www-form-urlencoded` (`:98-100`). Each branch emits
`RedactionMarker`s that are later appended to the flow (`:621`), with
direction-qualified field names such as `request.body.form:{key}`
(`recording.rs:122-132`). A body that is empty returns early with no markers
(`recording.rs:109-111`).

### `negotiated_version_annotation`

`negotiated_version_annotation` (`recording.rs:151-156`) is a total function
from `http::Version` to `Option<(String, String)>`: `Some(("transport",
"http-version:h2"))` for HTTP_2, `None` for everything else. It is called on the
*response* parts (`recording.rs:209`, `:462`) and pushed as a flow annotation
(`:364-366`, `:614-616`).

Three properties are load-bearing and all are stated in the code or its tests:
it records the **upstream** negotiated version, not the inbound listener
protocol, because the gateway rewrites the request onto the configured upstream
origin (`recording.rs:1002-1010`); it is "diagnostic only and never a match
dimension" (`recording.rs:148`); and it is not a fixture-format change, because
H1 flows keep their existing annotation shape by returning `None`
(`recording.rs:146-147`).

### Digest and length accounting

This module never hashes. `RecordingBodyWriter::finish`
(`eggreplay-store/src/lib.rs:842`) owns the SHA-256 and the `BlobRef`; all
recording does is write bytes and read `blob.length` back out to reconcile
headers (`recording.rs:592-596`) and to reconcile stream events.

Length accounting happens in two places. Offsets advance with
`offset.saturating_add(length)` as DATA frames are recorded
(`recording.rs:513`, `:2246`), and the *published* length is then reconciled
against the *observed* total by `reconcile_stream_event_body_length`
(`recording.rs:2530`), called at `:387` and `:643`. That function is what keeps
the `stream-events` extension honest after a body was transformed: if the sum of
recorded DATA lengths no longer equals `response.body.len()`, all DATA events are
dropped and a single synthetic `Data { offset: 0, length: expected }` is
re-inserted at the first DATA event's delta, and any `Error` event offsets are
re-anchored to the new end (`recording.rs:2547-2569`). Without it, a redacted
body would leave events describing bytes that were never published.

Event volume is bounded twice. Zero-length DATA events are dropped
(`recording.rs:2498-2503`); the per-direction cap is
`MAX_STREAM_EVENTS_PER_FLOW / 2` (`recording.rs:2497`), and overflow first tries
to coalesce with an adjacent contiguous DATA run (`:2508-2518`) before failing
with `"stream event count exceeds configured limit"` (`:2519`).

## The recording gateway

`start_recording_gateway_with_protocol` (`recording.rs:814-908`) is the only
function that starts a listener.

Bounds are validated before anything is bound: with WebSocket acquisition
enabled, `max_active_tunnels == 0` or a zero `max_duration` is a configuration
error (`recording.rs:828-833`).

The handler is a closure that clones per request - `upstream_base`, `client`,
`session`, `redaction`, `profile_id`, `physical_route`
(`recording.rs:836-841`) - so no mutex spans the awaited upstream transaction.
Inside it:

- **Upgrade intent is detected from the raw head** by scanning for an
  `upgrade: websocket` token across comma-separated values
  (`recording.rs:843-850`).
- **A tunnel capability is decisive.** If `call_with_tunnel` delivered one, the
  request goes to `gateway_websocket_request` when WebSocket recording is
  enabled, and otherwise the capability is dropped and the client gets a 403
  (`recording.rs:851-872`, `denied_upgrade_response` at `:1059-1064`). The drop
  is the admission decision: a tunnel that is not taken is not recorded.
- **Upgrade intent without a tunnel is a 400**, not a fallback
  (`recording.rs:873-878`). A request that asked to be a WebSocket is never
  quietly proxied as ordinary HTTP.
- **Everything else** goes to `gateway_request` (`recording.rs:879-889`).

`GatewayTunnelService` (`recording.rs:682-719`) is the adapter onto EggServe. Its
`request_body_policy` returns `RequestBodyPolicy::Reject` for any request
carrying `upgrade: websocket` (`:696-706`) so EggServe never buffers a body for
a 101; everything else uses the caller's
`RequestBodyPolicy::Stream { max_bytes }` (`:892-896`). The tunnel capability
arrives only through `call_with_tunnel`; plain `call` always passes `None`
(`:708-718`).

**How the protocol policy is threaded in.** The service and the two policy
values are handed to `crate::inbound::start_inbound_server`
(`recording.rs:898-905`). The tunnel-admission cap is passed conditionally:
`websocket.enabled.then_some(websocket.max_active_tunnels)` (`:903`), so a
gateway with WebSocket acquisition off has no tunnel budget to hand out. The
same `GatewayTunnelService` value serves H1 and H2; only the listening protocol
differs. That is the M015B central claim restated at the call site: the service
implementations are the same objects and no H2-specific matcher, store,
redaction, scenario, or renderer exists
(`plans/closure/m015b-inbound-http2-gateway-and-replay.md:26-44`).

**Return type.** `start_recording_gateway_with_protocol` returns
`InboundServerHandle` (`inbound.rs:228`), which is `Http1(ServerHandle)` or,
under `h2-inbound`, an `Http2` variant. The `_with_websockets` wrapper unwraps
the Http1 case and returns the plain `ServerHandle` the CLI and Python
lifecycles hold, keeping the impossible other arm behind the feature gate
(`recording.rs:794-802`).

**`gateway_request` (`recording.rs:973-1056`)** is where inbound semantics
become outbound semantics:

- The `Host` header is dropped (`:986-988`) and replaced by authority derived
  from `upstream_base`; the target is path plus query only (`:995-1001`);
  scheme and authority come from `upstream_base` (`:1002-1010`). This is why the
  recorded authority for a gateway flow is the **upstream** origin, not the
  inbound `:authority` - a behaviour the M015B suite asserts deliberately rather
  than works around (`m015b-...-and-replay.md:147-151`).
- The inbound body is bridged into a `StreamBody` via `eggserve_body_stream`
  (`:1011`, `:1819`), which yields DATA frames and then at most one trailer
  frame before ending.
- The recorded flow is then *served back* from the fixture: `session.body_path`
  resolves the blob (`:1034-1036`) and `response_stream` streams it in 64 KiB
  chunks (`recording.rs:1855-1912`, buffer at `:1871`) with the recorded length
  and trailers attached through
  `ResponseStream::with_known_length_and_trailers` (`:1898`).

**Shutdown is a caller contract, not something the gateway performs.** The
policy is documented at `recording.rs:729-734` and repeated at
`recording.rs:918-924`: `ServerHandle::shutdown` stops admission, `wait` drains
in-flight gateway tasks including tracked tunnel tasks, then the owner calls
`RecordingSession::shutdown` (idempotent), drains active blobs, and finalizes via
`finish_recording_session` on the blocking pool. The store enforces the middle of
that: `begin_blob` and `append_flow` both fail after shutdown
(`eggreplay-store/src/lib.rs:1141-1144`, `:1189-1191`), and `finish` fails if
any sink is still active (`eggreplay-store/src/lib.rs:1283-1289`).

## WebSocket capture

**Options.** `WebSocketRecordingOptions` (`recording.rs:656-667`) is a `Copy`
struct with five fields and a `Default` that is entirely conservative
(`recording.rs:669-679`): `enabled: false` (acquisition is opt-in),
`redact_text: false`, `redact_binary: false`, `max_duration: 1 hour`,
`max_active_tunnels: 16`.

**The 101 handoff.** `gateway_websocket_request`
(`recording.rs:1068-1359`) is a strict, ordered negotiation:

1. Preconditions: HTTP/1.1, GET, `TunnelKind::Http1Upgrade`, and a `websocket`
   protocol on the tunnel (`recording.rs:1089-1101`). Anything else is a 400.
   This check is why extended-CONNECT WebSockets cannot enter here.
2. Inbound validation: `validate_websocket_handshake_headers` under default
   limits (`:1115-1117`), a well-formed key, `Sec-WebSocket-Version: 13`,
   `Connection: upgrade` and `Upgrade: websocket` tokens (`:1120-1126`), and no
   request body - `content-length` must be 0 or absent and
   `transfer-encoding` must be absent (`:1127-1137`).
3. Upstream acquisition: a **fresh** EggFetch `GET` with a **newly generated**
   `Sec-WebSocket-Key` (`:1153-1154`), hop-by-hop and handshake headers stripped
   from the inbound set (`:1156-1175`), a 30-second timeout (`:1183-1192`), and
   a required 101 (`:1194-1199`). The upstream `Sec-WebSocket-Accept` must equal
   `derive_accept_key(upstream_key)` (`:1216-1221`), and any selected
   subprotocol must be singular and one the client actually offered
   (`:1222-1237`).
4. The post-101 stream is **EggFetch's**: `NetworkStream::Upgraded(upstream_stream)`
   is destructured at `recording.rs:1238-1246`. No second connection is created
   here. The inbound side is **EggServe's** tunnel, accepted at
   `recording.rs:1280-1283`.
5. The inbound accept value is derived from the *inbound* key (`:1252`), so the
   downstream client sees a correct handshake even though the upstream key
   differed. Only the selected subprotocol is mirrored (`:1255-1259`).
6. The finalizer is registered **before** the tunnel is accepted
   (`recording.rs:1268-1279`), with the comment: "Register a session-owned
   completion barrier for the conversation task BEFORE accepting the tunnel, so
   the request never becomes detached without a registered finalizer."

**Bounded messages into a conversation.** The accepted-tunnel closure
(`recording.rs:1283-1315`) wraps the two already-owned streams in codecs -
`crate::websocket::server` and `::client` - and runs `relay_websocket`. That
module is "Raw WebSocket codec adapters over streams already owned by
EggFetch/EggServe" and "intentionally exposes no connect/accept network
helpers" (`websocket.rs:1-4`). Codec bounds are pinned at 16 MiB for both
message and frame size (`recording.rs:1526-1530`).

`relay_websocket` (`recording.rs:1551-1774`) selects over the duration deadline,
`lifecycle.cancelled()`, and both directions (`:1576-1582`), and derives every
terminal from evidence: `Abnormal { cause: "duration-limit" }`,
`"shutdown"`, `"eof"`, `"reset"`, `"protocol-error"`
(`websocket_error_cause` at `:1533-1548`), `"write-error"`, `"storage-error"`,
and `"capture-limit"`. `CleanClose` is only reached when both sides have closed
(`:1751-1756`). Raw `Message::Frame` is skipped outright (`:1634`) - wire frames
are never persisted.

Pings are recorded and matched pong payloads are suppressed from forwarding
(`recording.rs:1638-1661`), because the codec answers them itself. Every
non-empty payload becomes one blob via `session.begin_blob()`
(`:1693-1711`); a storage failure ends the conversation with
`"storage-error"` rather than silently dropping the message. Per-message bounds
are re-checked against `WebSocketLimits::default()` (`:1712-1721`).

Redaction applies to the **stored payload only**. The bytes written to the blob
are the redacted `payload_bytes` (`:1698`), but the frame forwarded to the peer
is the original `message` (`:1742`, `:1762-1765`) - live traffic passes through
unchanged, matching the M011C closure's "Redaction is applied to stored payloads
only; live payloads pass unchanged"
(`plans/closure/m011c-websocket-recording-gateway.md:19-21`). Whole-message
text/binary redaction and configured JSON-pointer redaction are distinct
recorded shapes (`WebSocketRedaction::WholeMessage` versus `::JsonPointers`,
`:1667-1685`), plus a dedicated `CloseReason` marker (`:1686-1689`).

The result is a `WebSocketConversation` keyed to the 101 flow's id
(`recording.rs:1299-1306`), appended at `:1307`. The 101 flow itself is staged
afterwards with `BodyRef::Absent`, normalized handshake headers, and
`provenance.mode = "eggfetch-native-websocket"`
(`recording.rs:1339-1357`).

**Finalization, including the abandoned case.**
`ConversationCompletion` / `ConversationCompleter` (`recording.rs:1377-1523`) are
a `Mutex<bool>` plus a `Condvar`. `complete()` sets the flag and notifies
(`:1499-1507`); `Drop` does the same when `complete()` never ran (`:1511-1522`),
which is the whole point: an aborted, cancelled, or panicked conversation task
still releases the barrier, so `RecordingSession::finish` cannot deadlock waiting
for a conversation that no longer exists. Because the conversation is appended
*before* the completer signals (`recording.rs:1307-1313`), the recorded state
already reflects the terminal the relay observed - typically
`Abnormal { cause: "shutdown" }` (`recording.rs:1370-1375`).

The store drives these barriers with `finalizer.drive()`
(`eggreplay-store/src/lib.rs:1295-1303`) before publishing the manifest. The
deadline variant `drive_with_deadline` (`recording.rs:1425-1479`) is not called
by the store; it exists as a bounded alternative and its loop deliberately
re-reads the clock instead of trusting `timed_out()`, because Windows can report
a timeout slightly early (`recording.rs:1463-1477`).

**Two drain helpers, different jobs.** `await_websocket_conversations`
(`recording.rs:961-969`) polls `websocket_conversation_count()` up to 10,000
times at 1 ms and returns a bool. Its doc comment explains the race it exists
for: `ServerHandle::shutdown` is a *hard* stop that aborts tracked tunnel tasks,
so a caller that shuts down immediately after a clean Close can win the race
against the relay's final append - and then `finish` correctly refuses to
publish ("missing required conversation metadata"). The rule it states is
"Wait for the append to land, *then* shut down"
(`recording.rs:946-960`). `drain_active_blobs` is the separate, weaker helper
described next.

## Session finalization and shutdown

`finish_recording_session` (`recording.rs:925-931`) is the async-context
wrapper: it moves the session into `tokio::task::spawn_blocking` and calls
`session.finish()`. The reason is blocking isolation - `finish` may block on a
std `Condvar` while a WebSocket finalizer is pending, and running it on the
blocking pool keeps the executor free to drive the conversation task to its
terminal signal, which is safe on both multi-thread and current-thread runtimes
(`recording.rs:910-917`). A join failure is surfaced as
`StoreError::Invalid("recording finalization join: ...")`.

`drain_active_blobs` (`recording.rs:937-944`) spins up to 1,000 times calling
`tokio::task::yield_now()` until `session.active_blobs() == 0`. It returns
`()` - it cannot report that it gave up. It is a courtesy spin covering blob
accounting only; its doc comment is explicit that WebSocket conversation tasks
are drained by `ServerHandle::wait` before it runs
(`recording.rs:933-936`). **The authoritative rule lives in the store:**
`finish` refuses to publish while `active_blobs != 0`, with
`StoreError::Invalid("cannot finalize with active transactions")`
(`eggreplay-store/src/lib.rs:1283-1289`). A caller that skips the drain does not
get a corrupt fixture; it gets an error.

Ordering guarantees the caller must respect, as stated at
`recording.rs:729-734` and `recording.rs:918-924`:

1. `ServerHandle::shutdown` - stop admission. New `begin_blob` and
   `append_flow` calls fail from this moment
   (`eggreplay-store/src/lib.rs:1141-1144`, `:1189-1191`).
2. `ServerHandle::wait` - drain in-flight gateway tasks. Tracked WebSocket
   tunnel tasks are drained or **aborted** here, which is why step 2 must be
   preceded by `await_websocket_conversations` when a clean-close conversation
   is expected to be published.
3. `RecordingSession::shutdown` - idempotent; the same flag step 1 sets.
4. `drain_active_blobs` - best-effort spin.
5. `finish_recording_session` - blocking-pool finalization.

Underneath, `finish` drives every registered conversation finalizer to
completion before publishing (`eggreplay-store/src/lib.rs:1290-1303`), closes
the flow-log handle before the staging rename so Windows can complete the
publication (`:1308-1324`), and cross-validates the result: a recorded 101 flow
without the `websocket-messages` extension is refused rather than published as a
fixture that cannot be replayed (`eggreplay-store/src/lib.rs:2224-2234`).

The CLI and the Python lifecycle follow exactly this order
(`crates/eggreplay-cli/src/main.rs:865-867`, `:1058-1060`, `:1245-1247`;
`crates/eggreplay-python/src/lifecycle.rs:563-564`).

## What recording deliberately does not do

| Not done | Why / where |
|---|---|
| No second HTTP stack | The gateway composes through `crate::inbound::start_inbound_server` and hands it the *same* `GatewayTunnelService` value (`recording.rs:892-905`); M015B records that this adds "**no** product transport and **no** second authority" (`m015b-...-and-replay.md:26-44`). ADR 0002 forbids copying a sibling protocol implementation to avoid an integration dependency (`plans/adrs/0002-transport-ownership.md:11`). |
| No WebSocket handshake ownership in the codec | `websocket.rs:1-4`: the module "intentionally exposes no connect/accept network helpers. The caller must complete HTTP ownership and the RFC 6455 handshake first." The handshake in `recording.rs:1139-1259` is performed by the gateway over EggFetch/EggServe types. |
| No WSS, inbound TLS interception, H2 Extended CONNECT, H3, or negotiated WebSocket extensions | Stated in `docs/architecture.md:76-78` and in the M015B exclusions (`m015b-...-and-replay.md:247-250`). Concretely, `sec-websocket-extensions` is stripped from the outgoing upstream request (`recording.rs:1168`) and no accept is ever mirrored, so extensions are declined rather than negotiated. |
| No wire-frame fidelity | `Message::Frame(_) => continue` (`recording.rs:1634`); bodies become semantic DATA/trailer/end stream events, and the stored schema is headers plus blobs, not bytes on the wire. `docs/architecture.md:76-78` disclaims "frame-layout fidelity". |
| No H2 MITM | `h2-inbound` is serving-only (`Cargo.toml:21-29`), and M015B asserts `eggreplay-intercept` never gained `eggserve-core` (`m015b-...-and-replay.md:247-249`). |
| Inbound protocol is not a record dimension | Protocol selection is a listener property; the only trace of a non-H1 upstream is the diagnostic annotation, which is "diagnostic only and never a match dimension" (`recording.rs:148`). |
| No `append-new` WebSocket acquisition | `plans/closure/m011c-websocket-recording-gateway.md:36-40`: append-on-miss has no tunnel-aware upstream capture path, so the CLI rejects `serve --record-mode append-new --websockets` rather than adding a fallback transport. |
| No inbound-authority recording | The gateway records the upstream origin by construction (`recording.rs:1002-1010`); M015B pins this as existing acquisition policy that later work must preserve (`m015b-...-and-replay.md:288-290`). |

## Review checklist

1. **Redaction precedes publication.** In any new sink path, confirm the raw
   staging writer is dropped before the transformed blob is opened, and that
   the fail-closed branch drops rather than finishes. Compare
   `recording.rs:2100-2113` and `:562-581`; both keep the drop ahead of
   `begin_blob()`.
2. **Staging cleanup on abort.** An aborted transaction must leave no
   referenced blob. The store's `Drop` for `RecordingBodyWriter`
   (`eggreplay-store/src/lib.rs:885-891`) is what makes this true, so a review
   question is whether any new path can `take()` a writer and then return
   without either `finish()` or an explicit `drop`.
3. **Bounded structured reads.** Any transform must go through
   `read_staging_bounded(max_structured_bytes)` (`recording.rs:2046`, `:2100`),
   never an unbounded read of a staging file.
4. **Digest and length correctness.** This module must not hash; verify lengths
   are read from the returned `BodyRef` (`:592-596`) and that
   `reconcile_stream_event_body_length` (`:2530`) still runs after every
   redaction path, so published DATA events never describe unpublished bytes.
5. **Shutdown drain completeness.** Confirm the finalizer is registered before
   `tunnel.accept` (`:1274-1279`) and that `complete()` is called after the
   conversation append, never before (`:1307-1313`).
6. **Lock discipline across the store boundary.** No lock may span an await.
   Check that body sinks are `Arc<Mutex<Option<..>>>` around independent
   `begin_blob` writers (`:426`), that `append_flow` is the only serialized
   write (`:644`), and that no `MutexGuard` is held across
   `client.execute_http_body_default` (`:441`).
7. **Fail closed on a residual sink.** `finish` must still refuse when
   `active_blobs != 0` (`eggreplay-store/src/lib.rs:1283-1289`), and the 101
   cross-validation must still refuse a flow without its
   `websocket-messages` extension (`:2224-2234`). Do not let a new "best effort"
   path convert either refusal into a partial publish.
8. **Admission is not a fallback.** A WebSocket-intent request must never be
   served by the ordinary path: with a tunnel and acquisition disabled it is a
   403 (`:851-872`), and without a tunnel it is a 400 (`:873-878`).
