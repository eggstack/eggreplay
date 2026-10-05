> Deep dive for [overview](overview.md).

# Regression and reporting

EggReplay has three traffic paths: record into `.eggr`, replay offline from the
fixture, and execute the fixture against a live target and diff the result. The
third is regression. It spans exactly two authorities — a transport-side module
that executes candidates, and a core module that decides what "different" means
— plus a thin CLI that renders whatever those two produce.

## Module contracts

| Concern | Owner | Module |
| --- | --- | --- |
| Execute one baseline request against a live target | `eggreplay-http` | `crates/eggreplay-http/src/regression.rs` |
| Classify a candidate transport failure | `eggreplay-http` | `regression.rs` (`map_error`, `regression.rs:693`) |
| Compare two flows into a versioned report | `eggreplay-core` | `crates/eggreplay-core/src/report.rs` |
| Produce machine-readable findings | `eggreplay-core` | `report.rs` (`DiffFinding`, `report.rs:48`) |
| Schedule candidates and render output | `eggreplay-cli` | `crates/eggreplay-cli/src/main.rs` |

The split is deliberate and load-bearing. `regression.rs` is the only place that
opens a socket for a comparison; it produces an *observed* `Flow` plus bounded
body bytes and response stream events, and knows nothing about report schema.
`report.rs` is the only place that decides a difference; it is pure, takes two
flows and two body buffers, and knows nothing about HTTP or networks. The CLI
never re-evaluates: `compare_flows_with_timing_and_policy` is documented as "the
single flow-diff authority; callers must not fork separate evaluators for
JSON/JUnit projections" (`report.rs:204-206`), and `emit_reports` states that
"JUnit is a projection of the report authority, not a re-evaluation"
(`main.rs:2752`).

Feature gates:

- `eggreplay-http`'s `websocket` feature gates `compare_websocket_candidate`
  and its helpers (`regression.rs:36`, `regression.rs:317-470`). The feature is
  off by default (`crates/eggreplay-http/Cargo.toml:11-14`); with it off, the
  101 branch in the CLI still compiles because the call is a plain module
  function, but a build without the feature has no candidate WebSocket path.
- `h2` gates outbound HTTP/2 policy, `h2-inbound`/`h2-inbound-tls` gate inbound
  serving, and `eggress` gates route parsing. None of them change the comparison
  contract: comparison operates on the same semantic `Flow` either way.

## Candidate execution

`execute_candidate` (`regression.rs:490`) is the whole of candidate execution for
non-WebSocket flows. Its path:

1. **Target URI.** `target_uri` (`regression.rs:668`) takes the target base's
   scheme and authority and the *baseline* request's path, re-encoding the query
   with `form_urlencoded` so the candidate sees the recorded parameters rather
   than the recorded raw string. Only the base is remapped; path, query, and
   headers stay baseline-derived.
2. **Materialise `http::Request`.** Method, header names, and header values are
   each parsed, and any parse failure becomes `RegressionError::Conversion`
   (`regression.rs:499-508`) rather than a transport error — a baseline header
   that cannot be materialised is a configuration defect, not an origin
   behaviour. The body is materialised as a single `Full` buffer
   (`regression.rs:509-513`). That is why request cadence is not observable on
   the candidate side; the doc comment on `CandidateObservation` says so
   explicitly and warns against fabricating request timing from baseline events
   (`regression.rs:472-477`).
3. **Execute through EggFetch.** The call is
   `client.execute_http_body_default(outgoing)` (`regression.rs:516`) — the same
   native body path recording uses. The client itself is built in
   `build_client_with_version` (`main.rs:743`): `retry_canceled_requests(false)`
   so a cancelled request is never silently retried, an explicit
   `http_version_policy`, an optional `timeout`, and — for a non-direct route —
   `EggressDialer` through EggFetch's custom-dialer seam (`main.rs:766-781`).
   Route parsing failures return `("configuration", ..)`, i.e. exit 2, before
   any socket is opened (`main.rs:782`, `plans/closure/c004-cli-eggress-contracts.md:10`).
4. **Observe.** Wall-clock start is taken before the call and elapsed after
   (`regression.rs:515-517`); the body is drained frame by frame, emitting a
   `Data` event per delivered chunk, a `Trailers` event when trailers arrive,
   and a terminal `End` — or a terminal `Error` on a mid-body failure
   (`regression.rs:528-633`).
5. **Shape the observation.** The outcome becomes a `FlowOutcome::Response`
   with real status/headers/trailers, or a `FlowOutcome::Error` via `map_error`.
   `Flow::new` is stamped with the start time, `completed_at_ms` is filled in
   from the measured elapsed, `provenance.mode` is set to
   `"eggfetch-native-candidate"`, and the `PhysicalRoute` is attached
   (`regression.rs:657-660`). Provenance is what lets a stored candidate be told
   apart from a recorded one.

`CandidateObservation` (`regression.rs:479-486`) carries three things: the
semantic `flow`, the body bytes up to the observation bound, and the bounded
response stream events. The body bound is enforced *during* the drain — exceeding
`max_body_bytes` returns `RegressionError::BodyLimit`
(`regression.rs:575-577`), so a huge candidate response cannot exhaust memory
before the limit is noticed. The CLI passes the store's own
`StoreLimits::default().max_blob_bytes` (`main.rs:1641`), so the candidate bound
is the same bound the fixture uses.

A mid-body transport failure is deliberately *not* an error return. Status,
headers, and partial bytes are kept, a terminal `StreamEventKind::Error` is
appended, and the observation is returned (`regression.rs:529-551`). The
regression test `candidate_mid_body_error_returns_partial_observation`
(`regression.rs:992`) pins this: a `Content-Length: 100` response truncated
after 5 bytes must yield status 200, the bytes `hello`, and a terminal error
event. This is the M010 decision that an observable mid-body condition is a
*result to compare*, not a command-level runtime failure.

## Error classification

`RegressionError` (`regression.rs:16-32`) is the module's own error type:
`Conversion`, `Fetch`, `BodyLimit`, `Body`, `WebSocket`. Only the first four are
plumbing failures of the harness; none of them is a comparison result.

A candidate transport failure is mapped into the shared taxonomy rather than
flattened, by `map_error` (`regression.rs:693-715`):

| `eggfetch_core::Error` | `ErrorCategory` | `ErrorPhase` |
| --- | --- | --- |
| `Timeout` | `Timeout` | `Timeout` |
| `Tls`, `CertificateVerification`, `HostnameVerification` | `Tls` | `Tls` |
| `Connect`, `Io` | `Unreachable` | `Connect` |
| `Body` | `Other` | `Body` |
| anything else | `Other` | `Other` |

`ErrorCategory` and `ErrorPhase` are defined in
`crates/eggreplay-core/src/error.rs:33` and `error.rs:9`. `FlowError::new`
truncates the diagnostic message to 512 bytes (`error.rs:110-112`) so a
transport string can never carry a body into the report.

The taxonomy only earns its keep if a deadline cut-off, a connection reset, and
a protocol violation are *distinguishable*. Two closed milestones established
that they were not, and the registry records it plainly
(`plans/registry.md:180-215`):

- **M016**: every Eggress route failure was categorised `Other`, because
  `map_fetch_error` had no `CustomTransport` arm. `ErrorCategory::ConnectionRefused`
  was unreachable in the whole product. Fixed by the arm at
  `crates/eggreplay-http/src/recording.rs:2387-2390`, which delegates to
  `classify_dial_error` (`recording.rs:2403-2412`): `Connection` →
  `(Unreachable, Connect)`, `Timeout` → `(Timeout, Timeout)`, `Authentication` →
  `(Policy, Connect)`, `Rejected` → `(Policy, Policy)`. M016 also found the CLI
  never called `ClientBuilder::timeout` at all — every network-capable command
  was unbounded — and added `--timeout-secs` populating `pool`, `connect`,
  `write`, `read`, and `total` (`plans/closure/m016-post-m015-corrective.md:18-30`).
  The comment at `recording.rs:2383-2386` explains why the arm says `Unreachable`
  and not `ConnectionRefused`: EggFetch's typed evidence collapses every
  connection-establishment failure into one kind, so naming "refused" would be a
  guess, and an honest general category beats a specific wrong one.
- **M017**: the same defect one layer down. Body-stream errors were hardcoded to
  `("other", "body")` with the error discarded, so a deadline cut-off, a reset,
  and a protocol violation all recorded identically
  (`plans/closure/m017-unterminated-bidi-grpc.md:39-68`). Fixed by
  `classify_body_error` (`recording.rs:2427-2456`), which forces phase `Body`
  because that is where the failure happened, *except* a deadline, which reports
  `Timeout`/`Timeout` "so that a call cut off by the outbound timeout is
  distinguishable from one killed by a reset" (`recording.rs:2418-2420`).
  Protocol-shaped failures map to `Protocol`/`Body` here where
  `map_fetch_error` uses `Protocol`/`Headers`, because this site is past the
  headers by construction (`recording.rs:2451-2454`).

`ErrorCategory::as_str` / `ErrorPhase::as_str` (`error.rs:73`, `error.rs:91`)
exist so a surface that records a category as a bounded *string* uses the same
spelling the serialiser emits, and they are pinned against serde in a unit test
(`error.rs:129-165`) so a wire-name drift fails the build rather than producing
a stream event the JSON decoder would not read back.

**The candidate path does not yet share the recording path's table.** The
`("other", "body")` pair M017 removed from recording is still hardcoded at three
mid-body sites in `regression.rs:542-546`, `regression.rs:561-565`, and
`regression.rs:596-600`, and the comment there says so as a convention
("Category/phase use the same stable `other`/`body` convention as recording",
`regression.rs:533-537`). Likewise `regression.rs`'s `map_error`
(`regression.rs:693`) has no `CustomTransport` arm, so a dead Eggress route
observed *by a candidate* collapses to `(Other, Other)` even though recording
attributes it correctly. `classify_dial_error` and `classify_body_error` are
module-private `fn`s in `recording.rs` (not `pub(crate)`), so sharing them would
be a deliberate refactor, not a visibility tweak. Effect: a candidate body cut
off by `--timeout-secs` and one killed by a connection reset produce byte-identical
terminal events, and a candidate that differs from its baseline only in *how* it
failed mid-body can pass silently.

## Comparison engine

`compare_flows_with_timing_and_policy` (`report.rs:207`) is the single evaluator.
The three other entry points are delegations: `compare_flows` (`report.rs:145`)
calls `compare_flows_with_timing` with no timing, which calls the authority with
`ComparisonPolicy::default()`; `compare_flows_with_policy`
(`report.rs:185`) supplies a policy and no timing.

Findings are produced per dimension, dispatched on the outcome pair
(`report.rs:217-308`):

| Dimension | Kind | Field path | How it is decided |
| --- | --- | --- | --- |
| Status | `Status` | `response.status` | integer inequality (`report.rs:219`) |
| Response headers | `Header` | `response.headers.<name>` | per-name value-vector inequality, lowercased names (`report.rs:228`, `report.rs:464`) |
| Response trailers | `Trailer` | `response.trailers.<name>` | same helper, separate prefix (`report.rs:234`) |
| Body | `Body` | `response.body` | SHA-256 *or* length inequality; reports `sha256:<hex> length:<n>` only (`report.rs:241-258`) |
| Outcome class | `Outcome` | `outcome.kind` | response-vs-error mismatch (`report.rs:302`) |
| Error class | `Outcome` | `outcome.error` | `category` or `phase` inequality, reported as `Category/Phase` (`report.rs:292-301`) |
| Elapsed time | `Timing` | `timing.elapsed_ms` | only when an assertion is supplied (`report.rs:309-321`) |
| SSE semantics | `Sse` | `response.sse` | opt-in, derived (`report.rs:263-290`) |
| Ordered events | `StreamEvent` / `Timing` | `stream.<dir>.events`, `stream.<dir>.cadence[i]` | opt-in (`report.rs:335-383`) |
| WebSocket | `WebSocket` | handshake/message/cadence fields | `websocket` feature (`regression.rs:318`) |

`TimingAssertion` (`report.rs:61-64`) holds only `max_elapsed_ms`, and its doc
comment states the rule: "no implicit timing comparisons are made". A timing
finding fires only when an assertion exists *and* the candidate has
`completed_at_ms` (`report.rs:310`). No caller in the workspace currently
supplies one — `compare_flows_with_timing` is reached only through
`compare_flows`, and the CLI calls `compare_flows_with_policy`
(`main.rs:1652`, `main.rs:1796`) — so elapsed time is an available, unwired
authority rather than an active check.

`DiffFinding` (`report.rs:48`) is the stable wire shape: `kind`, `field`,
`baseline`, `candidate`, all strings, all redaction-safe by construction. Header
findings are especially terse: a header present on both sides with different
values reports `<present>` on both sides (`report.rs:449-458`), because the
authority records presence-set divergence, not values. A `Date` header minted
fresh by the candidate therefore produces a real finding that an operator cannot
distinguish from a value drift — the M014D qualification suite normalises `date`
out of both sides in the *test*, not in the product
(`crates/eggreplay-http/tests/h2_qualification.rs:1008-1014`).

Findings are sorted by `(kind, field)` before the report is built
(`report.rs:322-324`), which is what makes report JSON deterministic across
runs and across schedulers. `MatchDimension` (`crates/eggreplay-core/src/matching.rs:149`)
is a different vocabulary: it belongs to *baseline selection*, naming the
dimensions that made a near miss near (`Method`, `Authority`, `Path`, `Query`,
`Headers`, `Body`, `matching.rs:166-175`). It is not a diff kind and never
appears in a report.

### Versioned authority

`REPORT_SCHEMA_VERSION` is `2` (`crates/eggreplay-core/src/lib.rs:69`) and is
stamped into every `RegressionReport` (`report.rs:326`). A stored report is only
meaningful against the version that produced it: the version says which
vocabularies were in play — whether `DiffKind` had an SSE variant, whether the
header summary was `<present>` or a value, whether cadence findings were
`Timing`-kind or their own kind. A consumer must check the version before
interpreting findings, and a bump is a compatibility event, not a cosmetic one.

## Report and reproducibility

`RegressionReport` (`report.rs:122-131`) is four fields: `schema_version`,
`scheduler`, `baseline_flow_ids`, `findings`. `is_success` is exactly
`findings.is_empty()` (`report.rs:135-137`) — there is no partial-credit state,
and a run either produced findings or it did not.

`ReportScheduler` (`report.rs:13-20`) has three variants: `Sequential`,
`RecordedStartOrder`, and `Timeline`. It is recorded in the report because the
scheduler changes what a passing run *means*. Sequential executes one baseline
flow at a time; timeline starts candidates at their recorded monotonic offsets
with bounded concurrency. The same fixture and the same candidate can produce
different outcomes under the two, because concurrent candidates observe each
other's timing and the target's connection pool. Recording the scheduler makes
a diff reproducible: a report is only re-derivable if you know how the
candidates were driven.

The CLI wires two of the three. `SchedulerChoice` (`main.rs:525-529`) is
`Sequential` (default) or `Timeline`, and `SchedulerOptions`
(`main.rs:531-534`) adds `max_concurrency`. Sequential is a plain loop
(`main.rs:1435-1452`). Timeline requires the `stream-events` extension, requires
a start offset for *every* flow, and fails closed with a `fixture` error when one
is missing (`main.rs:1460-1494`); concurrency is bounded to `1..=1024`
(`main.rs:1454-1458`); each task sleeps until `origin + offset`
(`main.rs:1516`) and writes into a pre-sized slot keyed by fixture index
(`main.rs:1433`), so *reports* come back in fixture order regardless of
completion order. A scheduler that dropped a flow is an explicit `runtime`
error, not a short report (`main.rs:1543-1546`).

One wrinkle worth knowing: `diff` compares two fixtures, opens no socket, and
still records `ReportScheduler::Sequential` (`main.rs:1796-1803`) because the
report struct has no variant for "no candidates were scheduled". The scheduler
field there is inert.

## Streaming and SSE comparison

Stream and SSE comparison are **opt-in**, and the default policy proves it:
`ComparisonPolicy::default()` is all-false/empty (`report.rs:72-86`) and
`default_preserving()` returns exactly that (`report.rs:90-92`). The doc comment
states the contract: the default preserves "ordinary status/header/trailer/raw-body
regression with no stream/SSE-only findings" (`report.rs:66-71`).

Enabling is implicit in two places, and both are deliberate. `is_stream_enabled`
(`report.rs:96`) is true if `--compare-stream-events` was passed **or** a cadence
tolerance was given. `is_sse_enabled` (`report.rs:102`) is true if `--compare-sse`
was passed **or** any `--sse-ignore` field was named — naming a field to ignore
is itself a request to compare. `ComparisonPolicy::validate`
(`report.rs:107-117`) restricts the ignore vocabulary to
`data,event,id,retry,comments` and returns a `configuration` error otherwise, so
a typo becomes exit 2 rather than a silently ignored flag. The CLI converts
milliseconds to nanoseconds with a checked multiply and classifies an overflow as
`configuration` (`main.rs:325-333`).

`compare_stream_events` (`report.rs:335-383`) works per direction. It first asks
whether the ordered event lists are the same *shape* — same length and each
pair equal under `same_stream_event` (`report.rs:385-426`, which compares
`offset`/`length` for `Data`, field *names* only for `Trailers`, and `offset`
plus `category` plus `phase` for `Error`) — and emits one `stream.<dir>.events`
finding if not. Cadence is then compared per index, but only when a tolerance is
supplied: the gap between consecutive events is derived by subtracting the
previous `delta_ns` (`report.rs:359-369`), so it measures *inter-event* spacing
rather than absolute capture time. The doc comment is explicit: "Absolute
capture timestamps are never compared" (`report.rs:333-334`).

On the candidate side, `CandidateObservation::response_events` is the only
stream input. The baseline comes from the `stream-events` extension, loaded and
validated once per run, and the CLI refuses to continue if stream comparison was
requested but the extension is absent or invalid — `main.rs:1401-1432` returns a
`fixture` error, never a silent fallback to shape-only comparison. Per flow,
`compare_candidate_flow` narrows both sides to the response direction
(`main.rs:1677-1688`) because request events do not exist for a candidate, and
missing per-flow metadata is again an error (`main.rs:1668-1676`).

SSE comparison is derived, not authoritative. When enabled and both sides are
`text/event-stream` (`is_sse`, `report.rs:475-484`), both bodies are parsed with
comments included and compared through `crate::compare_sse`
(`crates/eggreplay-core/src/stream.rs:395`); a parse error on either side
yields a bounded `Sse` finding naming which side was malformed
(`report.rs:268-281`). The raw-body finding is *not* suppressed by an SSE
finding — they are independent, and the code says so at `report.rs:259-262`.

`push_candidate_event` (`regression.rs:728-761`) is the bound enforcer for
candidate events. Zero-length `Data` is dropped, a delay above
`MAX_STREAM_DELAY_NS` (60s, `stream.rs:16`) is a hard `Body` error, and the list
is capped at half of `MAX_STREAM_EVENTS_PER_FLOW` (2048, `stream.rs:10`) so both
directions fit the per-flow bound. At the cap, contiguous `Data` is coalesced
into the previous event (`regression.rs:747-757`), mirroring recording; a
non-contiguous event at the cap fails closed rather than truncating silently.

## WebSocket candidate regression

`compare_websocket_candidate` (`regression.rs:37`) is gated on the `websocket`
feature and takes the *same* `Client` and target `Uri` the HTTP path uses, so a
WebSocket candidate is routed through `EggressDialer` on the same terms as an
HTTP one.

It builds the handshake by hand rather than through a helper: baseline headers
are copied except a hop-by-hop/session set that must not be replayed —
`host`, `connection`, `upgrade`, `content-length`, `transfer-encoding`, and the
`sec-websocket-*` handshake headers (`regression.rs:54-67`) — then
`Connection: Upgrade`, `Upgrade: websocket`, `Sec-WebSocket-Version: 13`, and a
fresh random `Sec-WebSocket-Key` are appended (`regression.rs:72-77`). Fresh
handshake material is required: a replayed key would not be a live handshake.

Handshake findings are checked and short-circuit the conversation on any
mismatch (`regression.rs:143-145`): non-101 status (`regression.rs:94`), an
accept key that does not equal `derive_accept_key` of the key sent
(`regression.rs:106`), a missing or wrong `Upgrade`/`Connection` token
(`regression.rs:109-126`), any negotiated extension (`regression.rs:127`), and
subprotocol selection against the recorded `selected_subprotocol`
(`regression.rs:130-142`).

The conversation loop (`regression.rs:172-290`) walks
`conversation.messages` in recorded order. `ClientToServer` messages are
reconstructed from the recorded blob and sent with a 30s bound
(`regression.rs:182-238`); a `Close` that was auto-answered or a `Ping` that
was auto-ponged is handled as a flush rather than a send, because the recorded
side never wrote it (`regression.rs:195-216`, `regression.rs:273-278`).
`ServerToClient` messages are read with a 30s bound clamped to the remaining
one-hour conversation budget (`regression.rs:240-264`).

Comparison is payload-safe. `candidate_message_matches`
(`regression.rs:414`) requires the kind to match and, for `Close`, the close
*code*; then honours redactions: `WholeMessage` or `CloseReason` accepts
anything (`regression.rs:437-444`), and `JsonPointers` redacts the same
pointers on both sides before comparing (`regression.rs:455-468`). Anything else
compares raw bytes. Findings only ever carry summaries — kind, length, SHA-256
(`regression.rs:342-384`) — so a payload byte never reaches a report. Cadence
findings are emitted per message when a tolerance is configured
(`regression.rs:280-289`).

Two classification details are worth flagging. A handshake transport failure and
a handshake timeout both return the same single finding,
`handshake.transport / 101 / unavailable` (`regression.rs:83-92`). And for a
recorded *abnormal* terminal, an immediate error or EOF is accepted
(`regression.rs:306`) while a connection that stays open past 2s produces
`terminal / abnormal_end / connection_remained_open` (`regression.rs:307-311`).

`compare_candidate_flow` routes a 101 baseline here and returns immediately
(`main.rs:1599-1634`). Note what that means: for a 101 baseline the
`compare_flows` call never runs, so a WebSocket flow's report contains *only*
WebSocket findings — no status, header, or body dimension is evaluated for it.

## Output and exit codes

`Envelope<T>` (`main.rs:651-659`) is the JSON contract: `command`,
`schema_version` (always 1, `main.rs:2740`), `success`, `failure_class`,
`warnings`, `payload`. JSON and JUnit go to stdout, diagnostics to stderr as
`{failure_class}: {message}` (`main.rs:677`), and the two agree because both are
derived from the same `Err((class, message))` value.

`emit_reports` (`main.rs:2725-2771`) is the regression-path emitter:

- **JSON**: `payload` carries the redacted target, the full `reports` array,
  `finding_count`, and `outbound_timeout` (`main.rs:2744`). The target is passed
  through `redact_url` first.
- **JUnit**: `junit_for_reports` (`main.rs:2676`) emits one `<testsuite>` with
  `tests` = flow count and `failures` = non-success report count, and one
  `<testcase>` per flow id. A failing case carries a `<failure>` whose text is
  the `kind field baseline=… candidate=…` list, XML-escaped
  (`main.rs:2691-2706`). It reads `report.is_success()` and never re-evaluates.
- **Human**: two lines of terminal text (`main.rs:2755-2769`) that are
  explicitly not a parsing contract.

`emit` / `emit_with_warnings` (`main.rs:2773-2836`) are the single-assertion
emitters for other commands: JUnit projects one testcase, human prints
`{command}: ok` or `{command}: failed ({class})`. `warnings` is a field
distinct from `failure_class` by design — a warning never changes `success` or
the exit code. In the current tree, though, nothing populates it:
`emit_reports` hardcodes `warnings: Vec::new()` (`main.rs:2743`) and the only
caller of `emit_with_warnings` passes an empty vec (`main.rs:2780`).

`exit_code_for_class` (`main.rs:662-670`) is the whole mapping, and
`main` applies it to the error class (`main.rs:673-681`):

| Exit | Class | Meaning |
| --- | --- | --- |
| `0` | — | success (`replay` reports differences with `0`; `test`/`diff` assert) |
| `1` | `regression`, `diff` | regression/assertion mismatch |
| `2` | `configuration` | invalid CLI/config/policy, including a malformed `--route` |
| `3` | `fixture` | invalid or corrupt fixture |
| `4` | `runtime` | network/runtime execution failure |
| `5` | anything else | internal/unexpected failure |

This is the compatibility contract published in `docs/cli.md:11-20`. Two design
points follow from it. First, `replay` and `test` share one implementation and
differ only in the `enforce` flag: with `enforce` set, findings produce
`Err(("regression", "candidate differs from baseline"))` after the report is
already emitted (`main.rs:1552-1570`); without it, the same findings are
reported and the command returns `Ok` (`main.rs:1572-1584`). Second, a timed-out
*transaction* is not exit 4: it is recorded as `FlowOutcome::Error` and
compared like any other outcome, so the command still exits 0 or 1. M016's plan
originally required exit 4 here and was corrected before closure
(`plans/closure/m016-post-m015-corrective.md:51-57`) — a gateway that records
"the origin timed out" did its job.

## Review checklist

- **Error classification fidelity.** Does every candidate failure site consult
  a category table, or does one still hardcode a pair? `regression.rs:542-546`,
  `regression.rs:561-565`, and `regression.rs:596-600` still hardcode
  `("other", "body")`; `map_error` (`regression.rs:693`) still lacks the
  `CustomTransport` arm that M016 added to recording. Ask whether a candidate
  failure is distinguishable from every other candidate failure, not merely
  recorded.
- **Comparison-dimension completeness.** Is every semantic field a dimension?
  Status, headers, trailers, body digest+length, and outcome class are all
  covered; SSE, stream shape, cadence, and WebSocket are conditional on policy
  or feature. If a dimension is missing, is it missing deliberately and
  documented, or silently?
- **Timing assertion determinism.** Timing fires only from an explicit
  `TimingAssertion` (`report.rs:61`, `report.rs:309`) and only when the
  candidate has `completed_at_ms`. If someone adds an assertion, it must come
  from configuration, never from an implicit default; and note that no caller
  supplies one today.
- **Report schema stability.** Any change to `DiffKind`, to the header summary
  vocabulary, or to the `RegressionReport` shape is a
  `REPORT_SCHEMA_VERSION` bump (`lib.rs:69`) with a consumer-compatibility
  story. New findings are additive; changed meanings are not.
- **Exit-code mapping.** A new failure class needs an `exit_code_for_class` arm
  (`main.rs:662`) and a `docs/cli.md:11-20` row together. A new condition must
  not be routed to the catch-all 5, which is deliberately untestable in
  subprocess terms (`plans/closure/c004-cli-eggress-contracts.md:17`).
- **Can a regression silently pass?** The specific ways this happens here:
  1. A candidate body cut off by `--timeout-secs` and one killed by a reset
     record the same terminal event (`regression.rs:542`), so a baseline with the
     same hardcoded pair matches both.
  2. A candidate behind a dead Eggress route collapses to `(Other, Other)`
     (`regression.rs:713`) — if the baseline also failed, the categories match
     and the run passes even though neither failure was attributed.
  3. A 101 baseline skips `compare_flows` entirely (`main.rs:1599`), so no
     HTTP dimension is evaluated for a WebSocket flow.
  4. Header findings record `<present>` on both sides
     (`report.rs:449-458`), so a value drift is invisible in the report even
     though the run correctly fails.
  5. Stream and SSE comparison are opt-in and silent when off
     (`report.rs:66-71`), and request-direction stream comparison does not exist
     for candidates (`regression.rs:472-477`) — a chunking change on the request
     side is unobservable by construction.
  Each of these is a deliberate, documented boundary except (1) and (2), which
  are the M016/M017 defect pattern still present in the candidate path.
