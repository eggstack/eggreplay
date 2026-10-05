> Deep dive for [overview](overview.md).

# 02 — Core semantic model

`eggreplay-core` is the leaf of the workspace dependency graph: it defines what a
flow *means*, what "the same request" means, what a report is allowed to say, and
what a failure *is*. Everything above it — store, HAR, HTTP adapters, CLI, Python
binding — is a policy or a transport built on these types. Nothing below it exists.

The crate is 5,285 lines across ten modules plus a one-line version binary. Its
whole reason for existing is that replay determinism is a property of a data model,
not of a server.

## Crate contract

**The dependency-free guarantee is mechanical, not aspirational.** The manifest
declares exactly seven normal dependencies: `chrono`, `serde`, `serde_json`,
`sha2`, `thiserror`, `url`, `uuid` (`crates/eggreplay-core/Cargo.toml:10-17`). There
is no `tokio`, no `hyper`, no `tungstenite`, no `base64`, and no filesystem crate.
`proptest` is a dev-dependency only (`Cargo.toml:19-20`). CI asserts this three
separate ways:

| Check | Mechanism |
|---|---|
| No transport runtimes | `cargo tree -p eggreplay-core --no-default-features` grepped for `eggfetch\|eggserve\|eggress\|tokio\|hyper\|tungstenite\|base64` (`.github/workflows/ci.yml:134`) |
| No Python | `cargo tree` grepped for `pyo3`/`pyo3-async-runtimes` (`ci.yml:66-69`) |
| No interception/CA stack | `cargo tree` grepped for `eggreplay-intercept`/`rcgen` (`ci.yml:147-150`) |

(`chrono` is declared but has no `chrono::` call site in the crate's own source —
the only lexical hits are the word "chronology" in `flow.rs` comments. It is
carried for consumers, not used here.)

The crate therefore has **no clock and no file handle**. Anything that looks like
time is an `u64` handed in by the caller; anything that looks like a body is a
digest and a length. This is why `parse_sse`, `redact_json`, and the matcher all
take `&[u8]` from a loader the *adapter* owns.

**Lint posture is deliberately different from the workspace.** `eggreplay-core`
does **not** inherit `[lints] workspace = true` — it carries inline
`#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` instead
(`crates/eggreplay-core/src/lib.rs:3-4`). Only `eggreplay-cli`,
`eggreplay-har`, `eggreplay-intercept`, and `eggreplay-python` opt into the
workspace lint table. So the crate gets the two hard guarantees and *not* the
workspace clippy pedantic set, and `#![deny(missing_docs)]` is why essentially
every public item in this crate carries a doc comment.

**`lib.rs` is a curated facade.** Nine modules are declared (`:6-14`) and eight of
them are flattened into the crate root with `pub use` (`:16-54`), so downstream
code writes `eggreplay_core::Flow` rather than `eggreplay_core::flow::Flow`. Three
things are deliberately *not* re-exported and are reached by module path:

- `STREAM_EVENTS_SCHEMA_VERSION` and every `stream.rs` bound constant
  (`MAX_STREAM_*`, `MAX_SSE_*`) — used as `eggreplay_core::stream::…`
  (e.g. `crates/eggreplay-http/src/replay.rs:2205`).
- `matching::has_redaction_marker` (`:699`) — a small predicate for redaction-aware
  callers that does not exist in the root namespace.
- `websocket::payload_ref_for_bytes` (`:384`).

**Schema constants** are all `const` at the crate root (`:57-72`):

| Constant | Value | Meaning |
|---|---|---|
| `SCHEMA_VERSION` (`:57`) | = `FLOW_SCHEMA_VERSION` | Legacy alias, kept for downstream source compatibility |
| `FLOW_SCHEMA_VERSION` (`:60`) | `1` | Flow record schema; deliberately frozen at 1 while session metadata evolves |
| `SESSION_SCHEMA_V1` (`:63`) | `1` | Original manifest schema |
| `SESSION_SCHEMA_VERSION` (`:66`) | `2` | Latest manifest schema understood here; readers accept the inclusive range |
| `REPORT_SCHEMA_VERSION` (`:69`) | `2` | JSON report schema |
| `TOOL_VERSION` (`:72`) | `env!("CARGO_PKG_VERSION")` | Stamped into fixtures and reports at compile time |
| `RULES_SCHEMA_VERSION` | `1` | `scenario.rs:17`, in the scenario re-export block |
| `STREAM_EVENTS_SCHEMA_VERSION` | `1` | `stream.rs:8` |
| `WEBSOCKET_SCHEMA_VERSION` | `1` | `websocket.rs:9`, in the websocket re-export block |

The three *extension* schemas (`rules`, stream events, WebSocket transcript) live
in their own modules because each is owned by one capability; the flow, session,
and report constants live in `lib.rs` because several modules read them.

**`src/bin/eggreplay-core-version.rs` is two lines** — it prints
`eggreplay_core::TOOL_VERSION` (`:2`). It exists so build scripts and the CLI can
read the workspace version without linking the crate's types.

## Flow model (flow.rs)

`flow.rs` is the on-disk semantic contract. The organising idea is that a flow is
*one request and exactly one terminal outcome*, and that bodies are references,
never bytes.

**Ordered, duplicate-preserving collections.** `HeaderEntry` (`:12-19`) is a
`name`/`value` pair, and `HttpRequest.headers` / `HttpResponse.headers` are
`Vec<HeaderEntry>` — not a map. Duplicate `Set-Cookie` or repeated `X-Trace`
fields survive verbatim, along with their wire order. `QueryPair` (`:22-28`) is the
same for query values, and its doc calls out the important asymmetry: **an empty
value is distinct from an absent key** (`:26`). `Trailers` is a type alias for
`Vec<HeaderEntry>` (`:31`), used for the terminal trailer block on both request and
response. Header values are `String`, with the contract that opaque invalid
header fields are rejected by the HTTP adapter *before* they reach this model
(`:16-17`) — the semantic layer never sees bytes it cannot represent.

**The body tri-state.** `BodyRef` (`:34-43`) is the load-bearing type:

| Variant | Meaning | `len()` |
|---|---|---|
| `Absent` | No body was present or permitted | `None` (`:47-53`) |
| `Empty` | A body *was* present and had zero bytes | `Some(0)` |
| `Blob(BlobRef)` | Bytes live under `blobs/`, content-addressed | `Some(blob.length)` |

The distinction is not cosmetic. `Absent` means "this request had no body", `Empty`
means "a zero-length body was actually transmitted", and collapsing them would let
a `GET` with no body match a `POST` that deliberately sent `Content-Length: 0`. The
store and the matcher both lean on the distinction: `body_matches_lazy` requires
the *actual* bytes to be empty for either `Absent` or `Empty`
(`matching.rs:450`).

`BlobRef` (`:61-68`) is a lowercase hex SHA-256 plus an exact byte length, and its
constructor validates the digest form before accepting it — 64 characters, all hex
digits, otherwise `ErrorCategory::Policy` / `ErrorPhase::Policy` (`:72-83`). It
lower-cases on the way in, so uppercase digests from third-party HAR import
normalise rather than fork the keyspace.

**The flow record.** `Flow` (`:162-187`) carries `schema_version`, `id`,
`started_at_ms`, optional `completed_at_ms`, the request, the outcome, optional
`physical_route`, `provenance`, free-form `annotations`, and `redactions`.
`FlowOutcome` (`:125-133`) is a two-variant enum — `Response(HttpResponse)` or
`Error(FlowError)` — so "response and error both present" is unrepresentable rather
than merely discouraged. `PhysicalRoute` (`:135-142`) is the seam that keeps
physical routing (`kind`, e.g. `direct` or `eggress`) out of the logical
`HttpRequest.scheme`/`authority`/`path`, which describe the *logical* origin
(`:90-95`). `Provenance` (`:144-151`) records acquisition mode and observing
component. `RedactionMarker` (`:153-160`) is `(field, profile)` — a field *path*
like `request.headers.authorization` plus the policy id that redacted it. Note
that a marker records *that* a field was redacted and by which profile, never the
value that was removed.

`Flow::new` (`:198-214`) generates `flow-{started_at_ms}-{uuid_v4_simple}` (`:201`).
The timestamp prefix encodes chronology for human inspection; the random suffix is
what makes concurrent requests in the same millisecond unique, which the unit test
at `:331-343` asserts over 200 same-millisecond flows. The doc is explicit that
chronology lives in the timestamps and that flows sharing identical timestamps tie-
break by **JSONL append order**, not by ID sort — which is why no schema change was
needed to make IDs random.

`Flow::validate` (`:216-265`) enforces only cheap, storage-relevant invariants:
exact schema version; an ID that is non-empty, ≤256 bytes, and free of `/` and `\`
(the path-traversal guard that lets an ID be used for on-disk naming); a non-empty
method and an absolute path; `completed_at_ms >= started_at_ms`; and a status in
`100..=599`. It deliberately does not re-validate blob digests or header counts —
those are the store's and the adapters' jobs.

`SessionMetadata` (`:268-302`) is the `manifest.json` shape: schema version, tool
version, session id, capture mode, already-redacted `source`/`target`, and the
matcher and redaction profile *identifiers* that pin which policy produced the
fixture. Its `Default` is schema **1** (`:292`), so a bare default writes a
schema-1 manifest rather than the latest.

## Matching (matching.rs)

This is the module that decides whether a live request is "the same as" a recorded
flow, and it carries the crate's most subtle invariant.

**Two orthogonal policies.** `MatcherProfile` (defined in `config.rs:149-158`,
re-exported) is `Strict` or `Practical`; `Matcher::practical` seeds the ignore set
with `date`, `user-agent`, and `x-request-id` (`matching.rs:221-226`), and
`ignore_header` / `ignore_json_path` let a caller extend it case-insensitively
(`:241-249`). `BodyMatchMode` (`:8-18`) is the independent body policy:
`ExactBytes`, `ExactText`, or `SemanticJson` (parse both sides, delete the
configured JSON pointers from both, compare). The profiles are not a hierarchy —
`practical` does not imply semantic bodies — so a caller can have a lenient header
policy with exact bodies.

**Normalization** (`:256-270`) lower-cases scheme and authority and maps an empty
path to `/`, but leaves path and query alone. There is no trailing-default-port
stripping in the implementation despite the field doc mentioning it
(`:37-38`) — the comparison is plain string equality on the lower-cased authority.
`NormalizedRequest.headers` is a `BTreeMap<String, Vec<String>>` (`:44`), so header
*names* canonicalize to lowercase and values stay ordered (`normalize_headers`,
`:523-538`).

**Candidates are metadata, not bytes.** `CandidateBody` (`:53-68`) is
`Absent` / `Empty` / `Digest { sha256, length }` / `Inline(Vec<u8>)`.
`from_body_ref` (`:75-84`) never opens a blob: `Blob` collapses to its digest and
length. `MatchCandidate` (`:99-111`) pairs a `Flow` with a `CandidateBody` and
retains a legacy `request_body: Vec<u8>` field for API compatibility.
`MatchCandidate::from_flow` (`:131-138`) is the lazy constructor that leaves both
byte fields empty; `MatchCandidate::new` (`:119-126`) materializes and is documented
as for tests and narrowed semantic loads only. `candidate_body()` (`:141-143`) is
the accessor callers should use.

**The exact-byte fast path.** `select` computes SHA-256 of the *actual* body once
(`:321-326`), then for a `Digest` candidate in `ExactBytes` mode compares
`actual_len == length && actual_digest == sha256` (`:452`) — two integer/string
comparisons, zero materialization, zero I/O. `ExactText` adds a UTF-8 validity
check on the already-in-memory actual bytes (`:453-457`). The test at `:826-876`
pins this by counting loader invocations and asserting `loader_calls == 0` for a
successful exact match, and asserting that a one-byte-shorter payload with an
identical prefix does *not* match. This is the reason the crate can stay
filesystem-free: fixture-wide matching never needs the blob.

**The semantic path is a narrowing load.** `select_with_loader`
(`:298-398`) takes `loader: &mut dyn FnMut(usize, &MatchCandidate) -> Option<Vec<u8>>`.
The loader is invoked **at most once per otherwise-matching candidate** that needs
bytes — semantic JSON, or any candidate carrying body redaction markers
(`:416-448`, `:458-471`). Candidates that already differ on method, authority,
path, query, or headers never trigger a load, because `narrowed` is
`dimensions.is_empty()` (`:359`) and both body paths return early when
`!narrowed` (`:419-421`, `:459-461`). The test at `:878-932` proves this with two
candidates: the one differing on path is never loaded, the narrowed one is loaded
exactly once.

**Markers become wildcards, never literals.** This is the invariant that makes
redacted fixtures replayable. Per candidate, `redacted_request_headers`,
`redacted_request_query`, `redacted_body_json_paths`, and
`redacted_body_form_keys` (`:613-659`) parse the candidate's own `flow.redactions`
markers by field-path prefix:

| Marker prefix | Effect | Line |
|---|---|---|
| `request.headers.<name>` | Name added to the per-candidate ignore set, then both sides normalized (`:330-338`) | `:613-623` |
| `request.query.<key>` | Filtered out of both sides' query vectors before comparison (`:348-352`, `filter_query` `:661-670`) | `:625-635` |
| `request.body.json:<pointer>` | Pointer added to the removed set for semantic comparison (`:422-423`) | `:637-647` |
| `request.body.form:<key>` | Form-encoded wildcard attempt before the JSON attempt (`:437-441`) | `:649-659` |

Header markers are matched case-insensitively because the suffix is lower-cased
(`:620`) and `normalize_headers` lower-cases anyway; query-key markers are compared
**case-sensitively** against `pair.key` (`:667`). A redacted header or query field
is dropped from both sides, so a different secret in that field still matches while
the remainder must match exactly.

Body redaction is the sharp edge. A redacted body **forces a semantic comparison
even in `ExactBytes` mode** (`:416-418`, and the comment at `:586-587` explicitly
suppresses the unused `semantic_requested` flag), because a byte comparison against
a stored body full of `<redacted>` placeholders could only ever fail. If either
side does not parse as JSON, `body_match_with_json` returns `false` rather than
falling back to a byte compare (`:577-581`): **redacted opaque bytes must not match
literally.** Form bodies get one heuristic first — text containing `=` and not
starting with `{` (`:600`) — and only if both sides look like forms are keys
filtered (`:604-610`). The test at `:934-…` exercises header, query, and JSON-body
redaction together against a request carrying *different* secrets and expects a
match.

**Consumption is separate from matching.** `ConsumptionMode` (`:20-30`) is
`Once`, `RepeatLast`, or `Unlimited`, and it is deliberately independent of
`RecordMode` (see the config section). `MatcherSession` (`:491-521`) is the
per-replay-server state — a `BTreeSet<usize>` of consumed indices plus an optional
`last` — and it is explicitly **never persisted into fixtures** (`:491`). A fresh
`MatcherSession` per server instance is therefore what makes a replay repeatable,
and it is why re-running the same fixture twice gives the same answers.

`is_available` (`:503-512`) is the whole policy: `Once` requires the index be
unconsumed; `RepeatLast` allows the pinned `last` index and otherwise behaves like
`Once` until a first match pins it; `Unlimited` always allows. `consume`
(`:513-520`) never records anything for `Unlimited`.

**The match/exhaust distinction, and one real discrepancy.** The selection loop
(`:329-391`) walks candidates in stable fixture order. On `narrowed && body_match`
it consults `is_available`; if available it consumes and returns `Matched`; if not
it sets `matching_consumed = true` and **keeps scanning** (`:371-377`). At the end,
`matching_consumed` decides between `MatchResult::Exhausted` and
`MatchResult::NoMatch` (`:392-397`). That distinction is the right one and the test
at `:759-785` proves it: the same request against a single `Once` candidate matches
once and then returns `Exhausted` rather than a misleading `NoMatch`.

However, the doc comment on `MatchResult::Exhausted` promises
`near_misses` "including consumed exact candidates" (`:187-191`), and the
implementation does not deliver that: the consumed-exact path sets the flag at
`:376` and skips the `else` branch that pushes a `NearMiss`. In a single-candidate
fixture, `Exhausted { near_misses }` is therefore empty. The only test that observes
`Exhausted` uses `matches!(…, MatchResult::Exhausted { .. })` and never inspects the
vector (`:775-784`), so the contract is unenforced. Treat the doc as aspirational;
the flag is the signal, not the diagnostics.

**Near misses are bounded but biased.** `NearMiss` (`:164-175`) is
`(candidate_index, cost, dimensions, summary)`; `cost` is the number of differing
dimensions clamped to `u8` (`:385`), and `summary` is a fixed, redaction-safe
string (`:387`) — never any request material. `MatchDimension` (`:146-162`) is
`Method` / `Authority` / `Path` / `Query` / `Headers` / `Body`; note that scheme
and authority collapse into one `Authority` dimension (`:340-344`) and that `Body`
is appended only when the body comparison actually failed (`:379-381`).

Two behavioural details worth knowing. First, the bound is applied **during** the
scan — `near.len() < self.max_near_misses` (`:382`) — and the sort by
`(cost, candidate_index)` happens afterwards (`:392`). So the "bounded closest
candidates" are the closest among the *first N candidates in fixture order*, not
the globally closest; with a large fixture and a small bound, a genuinely near
candidate late in the list is never reported. Second, `max_near_misses` is a
constructor argument, so a `Limits::max_candidates` of 8 (the default,
`config.rs:112`) is what a CLI user actually gets.

## Scenarios (scenario.rs)

`scenario.rs` is EggReplay's escape hatch from pure fixture replay: hand-authored,
deterministic state machines stored in the session's `rules` extension.

**The model.** `ScenarioRules` (`:20-25`) is `{ schema_version, scenarios }`.
`Scenario` (`:28-39`) is a named finite state machine: `id`, `initial_state`, an
explicit `states` list, and an **ordered** `transitions` list. Isolation is per
replay-server instance — the doc at `:24` says so, and `ScenarioRuntime`
(`:277-284`) holds a private clone of the scenario, so two runtimes from the same
rules document are independent (test `:748-759`).

`ScenarioTransition` (`:41-59`) is `from` + conjunctive `when` predicates +
`extract` list + `extraction_failure` + `response` + `next_state`. The empty
predicate list matches every request (`:46`). `next_state` is explicit even when it
equals `from` (`:57`), which makes self-loops legible in authored fixtures instead
of being inferred from absence.

`RequestPredicate` (`:72-100`) is deliberately exact and duplicate-preserving:
`Method` and `Path` are string equality, `Query` matches *at least one* pair, and
`Header` matches at least one field with a case-insensitive name and exact value
(`:472-485`). No globbing, no regex.

**The state machine is first-match-wins, in authored order.** `advance`
(`:363-468`) finds the first transition with `from == self.state` and all
predicates satisfied (`:368-382`), returning `Ok(None)` when none matches. State
advances only at the very end (`:454-456`): `previous_state` is captured, then
`self.state` and `self.variables` are committed together, and only then is the
`ScenarioStep` (`:264-275`, retaining both states for diagnostics) constructed.
Because nothing mutates before that point, both failure modes leave the runtime
untouched — which the tests at `:808-827` verify.

**Variables flow request → response in three steps.**

1. *Extract.* `extract_value` (`:607-659`) resolves a `VariableSource`:
   `PathSegment { index }` is a zero-based non-empty segment split (`:629-634`),
   `Query` takes the first exact key, `Header` the first case-insensitive name,
   `JsonPointer` resolves an RFC 6901 pointer (empty string = root) and stringifies
   non-string values (`:645-650`), and `PriorVariable` reads the working variable
   map. Extractions accumulate into a **clone** of the current variables (`:401`,
   `:419`) so a later extraction in the same transition can read an earlier one and
   a failure cannot half-apply.
2. *Render.* `render` (`:661-692`) substitutes only explicit `{{name}}` pairs.
   Values are inserted as raw UTF-8 with **no implicit escaping** — stated in the
   doc (`:661-663`) and load-bearing: a JSON body must author values compatible
   with its declared content type. An unterminated `{{`, a name failing
   `validate_name`, or a missing variable is an error, not an empty string.
   The bound `MAX_TEMPLATE_BYTES` is checked on the template, during the loop, and
   again on the output (`:665`, `:683`, `:688`).
3. *Replace.* If `json_pointer_replacements` is non-empty, the *rendered* body must
   parse as JSON (`:422-424`), each pointer must already resolve (`:426-428`), and
   the target is set to a JSON **string** (`:429-432`). So a pointer replacement
   always produces a string value — `{"id":0}` with `/id` replaced by `{{product}}`
   yields `{"id":"abc"}`, pinned by `:876-892`.

Response headers are rendered the same way (`:443-453`), so `x-product: {{product}}`
works (`:755`). The status is range-checked at serve time (`:440-442`).

**Redaction blocks extraction.** `runtime_with_redaction` (`:332-352`) threads a
`RedactionConfig` into the runtime, and `extract_value` refuses up front to read a
source the policy marks sensitive — query key, lower-cased header name, or JSON
pointer (`:614-627`) — with the message "scenario extraction source is redacted"
and no echo of the value. The test at `:910-926` asserts the error text does not
contain the secret. `ScenarioRules::runtime` (`:328-330`) defaults to
`RedactionConfig::default_secure()`, so a scenario cannot silently promote a
redacted header into a response body.

**Authored faults** (`ScenarioFault`, `:162-240`) are the deterministic-failure
seam, and the doc is careful about scope (`:164-168`): head delay, inter-chunk body
delay, close-before-response, close-after-N-bytes, and a projected
`TransportError { category, phase }` — all expressible through existing
EggServe/EggFetch lifecycle controls, with no packet corruption or kernel
emulation. `ScenarioResponse.fault` is `#[serde(default)]` (`:158-159`) so
fixtures authored before M014D keep their shape. `CloseBeforeResponse` flushes
headers with the full declared length and then aborts, so the client observes a
failure with nothing delivered rather than a synthetic status (`:186-188`).

**Every bound is a private const, checked in two places.** `scenario.rs:7-14`:
`MAX_SCENARIOS` 128, `MAX_STATES` 128, `MAX_TRANSITIONS` 1024, `MAX_VARIABLES` 64,
`MAX_NAME_BYTES` 128, `MAX_TEMPLATE_BYTES` 16 KiB, `MAX_VALUE_BYTES` 4096,
`MAX_JSON_BYTES` 1 MiB. Only the two fault bounds are public: `MAX_FAULT_DELAY_MS`
(30 s) and `MAX_FAULT_CHUNK_BYTES` (16 MiB) (`:209-212`). `validate` /
`validate_transition` (`:288-325`, `:488-593`) check them at load time;
`advance` re-checks the ones that depend on runtime state — variable count
(`:383-385`), JSON body size (`:391-393`), value size (`:416-418`), rendered size
(`:436-438`) — because a *cumulative* variable map can exceed the per-transition
limit even when each transition is individually valid.

`validate` also enforces referential integrity: unique scenario ids, unique state
names, `initial_state ∈ states`, and every `from`/`next_state` declared
(`:295-322`). `validate_name` (`:595-605`) restricts ids, states, variable names,
extraction keys, and template variables to ASCII alphanumerics plus `_`, `-`, `.`,
within `MAX_NAME_BYTES` — a charset narrow enough that a name can never be a path
segment or a template delimiter. Response header names are validated against the
RFC 7230 token set and values against CR/LF/control injection (`:512-543`).

## Redaction (security.rs)

`RedactionConfig` (`:7-16`) is three `BTreeSet<String>` selectors: `headers`
(case-insensitive), `query_keys` (which also drive form-body redaction), and
`json_paths` (RFC 6901 pointers). `default_secure()` (`:20-34`) covers
`authorization`, `proxy-authorization`, `cookie`, `set-cookie` and nothing else —
query and JSON selectors start empty and must be opted into.
`from_profile` (`:36-47`) converts a named `RedactionProfile` and lower-cases
header names. `wants_body_redaction()` (`:50-52`) is the cheap predicate an
adapter uses to decide whether it must buffer a body at all.

**Redaction is a persistence-time transform that runs before any blob is
finalised.** The function split matters: `redact_flow` (`:72-120`) handles only
in-memory header and query fields — request headers, request trailers, and, for
`FlowOutcome::Response`, response headers and trailers — replacing the value with
the literal `"<redacted>"` and pushing a `RedactionMarker` naming the field path
and the profile. It is field-based and infallible: it returns `()`.

Body bytes are a different story, because rewriting them changes the digest. The
core crate cannot touch the blob, so it exposes the *transform* and lets the
adapter own buffering and publication:

- `apply_json_redaction` (`:147-163`) returns `(Vec<u8>, bool)` — redacted bytes
  plus whether any pointer matched — and **fails closed** on malformed JSON rather
  than passing bytes through (`:152`). The bool matters: a requested path that does
  not exist is *not* a failure, but it must not produce a marker.
- `apply_form_redaction` (`:166-192`) parses `x-www-form-urlencoded`, replaces
  matching values, and re-serialises. Non-UTF-8 is an error (`:170`), and a
  non-empty body that parses to zero pairs is treated as a malformed form and
  errors (`:177-179`) — the round-trip-stability check that prevents silently
  rewriting an encoding it does not understand.
- `redact_json` (`:245-251`) is the in-memory `&mut Value` form, with no error
  path because a missing pointer is simply a no-op.
- `push_body_markers` (`:123-140`) attaches the markers *after* a successful
  transform, choosing `BODY_JSON_MARKER_PREFIX` (`"request.body.json:"`) or
  `RESPONSE_BODY_JSON_MARKER_PREFIX` (`"response.body.json:"`) (`:61-64`). Markers
  are only truthful once the transform actually ran, which is why they are a
  separate call rather than something `apply_json_redaction` pushes itself.

`DEFAULT_MAX_STRUCTURED_REDACTION_BYTES` is 1 MiB (`:55-59`) and is a *documented
constant only* — enforcement lives in the HTTP adapter, which passes it in
explicitly (e.g. `crates/eggreplay-http/src/recording.rs:2881`). Its doc states the
policy: larger bodies with requested redaction fail closed rather than buffering
unboundedly.

**Framing metadata is reconciled, not trusted.** `reconcile_headers_after_body_redaction`
(`:201-242`) runs after a body transform because every one of these headers is now
wrong. It drops all `Content-Length` and appends a recomputed one (`:208`, `:237-240`),
removes `Content-MD5`, `Digest`, `Content-Digest`, `Signature`, and
`Signature-Input` with a marker recording the removal (`:212-218`), removes strong
`ETag`s the same way, and **preserves weak `W/` ETags with a warning annotation**
(`:219-233`) — a weak validator is not a digest of the bytes, so it survives a
content change. It returns `(markers, annotations)` for the caller to attach. Note
that the recomputed `Content-Length` lands at the end of the vector, so header
order after a body redaction is not the wire order.

`redact_url` (`:254-265`) is the diagnostics helper: it parses, clears username and
password, drops query and fragment, and returns `"<invalid-url>"` when parsing
fails — never the input, which could contain userinfo.

The end-to-end story: markers written at redaction time become the matcher
wildcards described above. Without them, a redacted fixture could only ever be
matched by literally sending `"<redacted>"`, which is exactly what the matcher
refuses to do (`matching.rs:307-311`, `:577-581`).

## Stream and SSE (stream.rs)

`stream.rs` models a *timeline* without storing a *payload*. Its module doc says so
(`:1`) and the design holds throughout.

**Events are shapes, not bytes.** `StreamEventKind` (`:39-63`) is
`Data { offset, length }` / `Trailers { fields }` / `End` / `Error { offset,
category, phase }`. DATA bytes stay in the body blob; only the offset and length of
each delivered frame are recorded, so a multi-gigabyte SSE capture costs kilobytes
of metadata. `StreamEvent` (`:67-72`) pairs a kind with `delta_ns` — nanoseconds
since *that* direction's body started, never a wall clock. `StreamDirection`
(`:29-34`), `FlowStreamEvents` (`:76-87`), and `StreamEvents` (`:91-96`) complete the
extension document.

`Error { category, phase }` is `String`, not `ErrorCategory`/`ErrorPhase`
(`:57-61`) — the stream extension is a string-recording surface, which is exactly
why `error.rs` exposes `as_str()` (see the error section) and why a test there
pins every wire name to ≤64 bytes (`error.rs:170-175`), the same bound
`validate` enforces on these fields (`stream.rs:179`).

**Validation enforces a real timeline.** `StreamEvents::validate` (`:128-201`)
requires the exact schema version, ≤`MAX_STREAM_EVENTS_PER_SESSION` flows, unique
non-empty flow ids ≤256 bytes, ≤4096 events per flow across both directions, and a
saturating aggregate bound (`:135-150`). Per direction it walks the events
requiring non-decreasing `delta_ns` within `MAX_STREAM_DELAY_NS`, nothing after a
terminal, **contiguous non-empty DATA offsets** (`offset == last_offset`,
`length != 0`, `:166-173`), a terminal error whose offset equals the bytes
delivered so far (`:179`), and trailers that appear at most once, after all DATA
(`trailers_seen` blocks further DATA, `:167`, `:184-194`). `End` sets terminal
(`:195`). That is enough to reject interleaved or lying timelines without ever
looking at the body.

**Bounds** (`:7-24`): 4096 events per flow, 1,000,000 per session, 16 MiB
extension, 60 s per delay, 300 s total per flow, 64 KiB per SSE line, 100,000 SSE
events, 16 MiB SSE body.

**`timeline_order_offsets`** (`:99-115`) sorts fixture indices by
`(start_offset_ns, original index)`, so concurrent starts have a total, stable
order and equal offsets tie-break by fixture position. It validates
`max_concurrency ∈ 1..=1024` and each offset against `MAX_STREAM_TOTAL_DELAY_NS`
— but note it returns a *total order* and never uses `max_concurrency` for
anything else; the actual concurrency cap is the scheduler's responsibility. The
value is an admission check on the caller's declared bound, not enforcement here.

**`StreamTimingMode`** (`:206-252`) is `Immediate`, `Recorded`, or `Scaled(f64)`.
`parse` (`:217-235`) accepts `immediate`, `recorded`, `scaled:<factor>` and
requires the factor to be finite and within `0.01..=100.0` — a float in a
`Copy` enum, but a bounded one. `delay_ns` (`:238-251`) rejects a recorded delay
over the per-delay cap, computes `ceil(delta * factor)` for `Scaled`, and rejects
a running total over `MAX_STREAM_TOTAL_DELAY_NS`. Both the input and the
accumulated output are checked, so a scaled replay cannot escape the cap by
shrinking or by accumulation.

**SSE is a derived, opt-in view.** `parse_sse` (`:280-392`) returns
`SseParseResult { events, error }` — it never fails hard and never mutates the
input bytes. An oversized body or non-UTF-8 input returns an empty event list with
a bounded error (`:281-295`). Within the body, an over-long line, too many events,
a `NUL` in an `id`, or a non-numeric `retry` each set `error` and stop the parse
(`:309-312`, `:320-323`, `:350-366`). Comments are collected only when
`include_comments` is true (`:336-341`), and multi-line `data` is joined with LF
(`:345-348`, with the trailing LF popped at dispatch, `:324`). A trailing event
without a final blank line is still emitted (`:370-389`).

`compare_sse` (`:395-405`) is ordered and field-selective: lengths must match, and
each of `data`, `event`, `id`, `retry`, `comments` is compared unless named in
`ignored`. There is no fuzzy or set-based comparison — the same events in a
different order are a difference (pinned by `report.rs`'s test at `:810-868`), and
`report.rs:871-930` proves that ignoring one field never hides a difference in
another.

## WebSocket semantics (websocket.rs)

`websocket.rs` is a transcript schema and a validator. It is explicitly **not a
codec**: `WebSocketMessageKind` (`:78-89`) has `Text`/`Binary`/`Ping`/`Pong`/
`Close` and its doc says "Frame layout, masking, and fragmentation are omitted"
(`:75`). What it stores is meaning; what an adapter does with frames is out of
scope.

`WebSocketConversation` (`:152-167`) is anchored to **exactly one** initiating HTTP
flow (`flow_id`, `:156`) and carries the offered subprotocol list in client order
plus the selected one, the strict global transcript, and a terminal.
`WebSocketMessage` (`:127-148`) has a `sequence` that is a zero-based **contiguous**
counter shared across both directions (`:128`), a direction, `delta_ns` measured
from successful upgrade completion, a kind, an optional content-addressed
`payload: Option<BodyRef>` (so a close message can carry no bytes at all),
`close_code`/`close_reason` present only on close, and per-message redactions.
`WebSocketTerminal` (`:110-123`) is `CleanClose { code, reason }` or
`Abnormal { cause }`, where `cause` is a fixed vocabulary validated at
`:324-338` — `eof`, `reset`, `shutdown`, `duration-limit`, `protocol-error`,
`read-error`, `write-error`, `timeout`, `cancelled`, `upstream-error`,
`downstream-error`, `other`.

**`validate` (`:180-345`) is where the protocol rules live.** Session-wide:
conversation count, unique conversation ids, and — notably — **unique
initiating `flow_id`s** (`:201-203`), so one HTTP upgrade cannot own two
conversations. A selected subprotocol must have been offered (`:215-220`). Per
message: `sequence == index` exactly (`:234-235`), non-decreasing `delta_ns`
within `max_duration_ns`, nothing after termination, and no data message in a
direction that has already closed (`:247-251`). Payload lengths are checked per
message and accumulated across the session with `checked_add` overflow guards
(`:252-266`), and control messages are additionally capped at
`max_control_payload_bytes.min(125)` (`:273`) — the RFC 6455 hard limit survives a
laxer config. A close message must have **no payload** (close data lives in
`close_code`/`close_reason`, `:277-279`), at most one close per direction
(`:292-294`), and the conversation is terminated only once *both* directions have
closed (`terminated = close_directions.len() == 2`, `:295`).
`valid_close_code` (`:374-376`) accepts `1000..=1014` excluding `1004..=1006`
(the reserved/undefined block, and note `1015` is not accepted) plus the
application range `3000..=4999`.

A `CleanClose` terminal is checked against the transcript, not just the
vocabulary: both closes must exist, and the **last** message must be a close whose
code and reason equal the terminal's (`:301-321`). A fixture cannot claim a clean
close it does not contain.

`WebSocketRedaction` (`:94-105`) is payload-level authority:
`JsonPointers { pointers }` (which "are wildcards when comparing client
messages", `:95-96`), `WholeMessage`, and `CloseReason`. `validate_redactions`
(`:352-371`) allows JSON pointers only on `Text` messages and only with
`/`-prefixed paths, and allows `CloseReason` only on a close.

**`WebSocketLimits` (`:13-63`) is a 15-field bundle** with sane defaults: 1024
conversations, 100k messages per conversation, 1M per session, 16 MiB per message,
1 GiB total payload, 24 h duration, 16 MiB metadata, 128 / 64 KiB handshake
headers, 4 KiB diagnostics, and 125 / 123-byte RFC control and close-reason caps.

The two handshake functions split normalisation from policy.
`normalize_websocket_handshake_headers` (`:397-411`) removes exactly one volatile
field per direction — `Sec-WebSocket-Key` from requests, `Sec-WebSocket-Accept`
from responses — and leaves every other ordered header semantic, "without changing
generic HTTP header rules" (`:393`). `validate_websocket_handshake_headers`
(`:417-444`) bounds count and aggregate bytes (with a `checked_add` overflow
guard) and rejects a **negotiated** extension in a response
(`:435-442`); an extension *offer* in a request is fine (`:415-416`).
`payload_ref_for_bytes` (`:384-391`) is the only hashing seam here, and its doc
says store code still owns durable publication and digest verification (`:382-383`).

## Reporting (report.rs)

`report.rs` is the single authority on what a regression *is*. Four entry points
funnel into one implementation:

| Entry point | Adds | Line |
|---|---|---|
| `compare_flows` | nothing; historical contract | `:145-160` |
| `compare_flows_with_timing` | `Option<TimingAssertion>` | `:165-182` |
| `compare_flows_with_policy` | `&ComparisonPolicy` | `:185-202` |
| `compare_flows_with_timing_and_policy` | both; the real evaluator | `:207-331` |

The doc at `:204-206` is a real constraint: callers "must not fork separate
evaluators for JSON/JUnit projections". Presentation layers project findings; they
do not recompute them.

**Dimensions produce findings, and only findings.** A `RegressionReport`
(`:122-131`) is `{ schema_version, scheduler, baseline_flow_ids, findings }`, and
`is_success()` is literally `findings.is_empty()` (`:135-137`). The outcome pair
selects the branch (`:217-308`):

| Condition | `DiffKind` | `field` |
|---|---|---|
| Status differs | `Status` | `response.status` |
| A header name's value-vector differs | `Header` | `response.headers.<name>` |
| A trailer name's value-vector differs | `Trailer` | `response.trailers.<name>` |
| SHA-256 **or** length differs | `Body` | `response.body` |
| Both errors, category or phase differs | `Outcome` | `outcome.error` |
| Response vs error mismatch | `Outcome` | `outcome.kind` |
| Elapsed over the assertion | `Timing` | `timing.elapsed_ms` |
| SSE differs, opt-in | `Sse` | `response.sse` |

Header and trailer comparison is **presence-only by construction** (`:428-462`):
names are lower-cased into a map of value vectors, and a finding's `baseline` /
`candidate` are literally `"<absent>"` or `"<present>"` (`:449-458`). No header
value ever reaches a report. The `Body` finding is likewise digest-shaped
(`"sha256:… length:…"`, `:247-256`) — a report cannot leak body content even when
redaction missed something. `DiffKind` (`:25-44`) is a closed 8-variant enum,
`DiffFinding` (`:48-57`) a flat, serializable quad.

Findings are sorted by `(Debug of kind, field)` before returning (`:322-324`), so
two runs of the same comparison produce byte-identical reports regardless of the
order dimensions were evaluated in. `compare_stream_events` sorts by `field` alone
(`:381`) because all its findings share a kind.

**Timing is opt-in and one-sided.** `TimingAssertion` (`:61-64`) is only
`max_elapsed_ms`, and the doc says no implicit timing comparisons are made
(`:59`). Evaluation uses the *candidate's* own elapsed time with `saturating_sub`
and is skipped entirely when `completed_at_ms` is `None` (`:309-321`). There is no
baseline timing comparison — the baseline value in the finding is the threshold.

**`ComparisonPolicy` (`:73-118`) defaults to the historical contract.** All
comparison is off by default (`Default` derive, `:72`), and `is_sse_enabled` /
`is_stream_enabled` (`:96-104`) are the opt-in predicates. The implication rules
are the interesting part: a cadence tolerance implies stream comparison
(`:76-78`), and a non-empty `sse_ignored` implies SSE comparison (`:80-83`).
`validate` (`:107-117`) rejects any ignore field outside the exact vocabulary
`data, event, id, retry, comments`. SSE findings require **both** sides to be
`text/event-stream` (`is_sse` `:475-484`, checked at `:263`) and appear *alongside*
the raw body finding, not instead of it (`:259-290`; test `report.rs:542-635`).
Malformed SSE produces a bounded finding, and the test asserts both summaries stay
under 256 bytes (`:975-983`).

One structural point worth being explicit about: `compare_flows_with_timing_and_policy`
consults only `policy.is_sse_enabled()`. It never calls `compare_stream_events`, so
`compare_stream_events` and `cadence_tolerance_ns` and `websocket_cadence_tolerance_ns`
are consumed by the *caller*, which must invoke `compare_stream_events` itself —
the CLI does exactly that behind `policy.is_stream_enabled()`
(`crates/eggreplay-cli/src/main.rs:1403`, `:1726-1756`) and so does the Python
binding (`crates/eggreplay-python/src/lifecycle.rs:519`). The policy is a
declarative bundle, not an executor.

`compare_stream_events` (`:335-383`) compares the two directions independently.
Shape is length plus element-wise `same_stream_event` (`:385-426`), which compares
DATA offset+length, trailer field *names* case-insensitively (values ignored),
`End` vs `End`, and terminal `Error` offset+category+phase. Cadence is
`delta_ns[i] - delta_ns[i-1]` via `saturating_sub` compared with `abs_diff` against
the tolerance (`:358-379`) — **relative gaps, never absolute timestamps**, and
`start_offset_ns` is not part of any comparison. The test at `:766-808` pins
determinism by asserting two runs with the same inputs produce equal findings.

**Why the scheduler is recorded.** `ReportScheduler` (`:13-20`) is `Sequential`,
`RecordedStartOrder`, or `Timeline`, and it is stored in every report (`:327`)
rather than left to the caller. A regression report is evidence, and "these findings
were produced by running candidates concurrently in timeline order" is a materially
different claim from "these findings were produced one at a time" — concurrency
changes timing, resource pressure, and therefore whether a flaky failure is
attributable to the application. Recording it lets a reader interpret a `Timing`
finding correctly instead of guessing at the run shape.

## Configuration and errors (config.rs, error.rs)

`config.rs` (175 lines) is a small, serializable skeleton: `Config { limits,
redaction, matcher, output }` (`:161-175`), all fields `#[serde(default)]`.

`RecordMode` (`:9-18`) is the recording/replay *transport* policy:
`Sealed` (replay only — "a miss is offline and never reaches the network", `:10`),
`Once` (record only when creating a new fixture), `AppendNew` (replay first, append
misses), and `ReRecord` (always upstream, atomically replace at shutdown).
`RecordMode::resolve` (`:32-50`) is the enforcement point and it is strict in both
directions: any network-capable mode without an explicit upstream is an error
(`:40-42`), and `sealed` *with* an upstream is also an error (`:43-45`). `Once` on
an existing fixture downgrades to `Sealed` with `upstream_enabled == false`
(`:37-38`, test `:69-77`). The result is `RecordPolicy { mode, upstream_enabled }`
(`:22-27`) — capability is derived once, centrally, rather than being
re-derivable from the mode at every call site.

`Limits` (`:95-104`) defaults to 64 MiB body, 4 MiB JSONL line, 100,000 flows,
8 candidates (`:106-115`). `RedactionProfile` (`:119-147`) is the *named, persisted*
form — `id`, `sensitive_headers`, `sensitive_query_keys`, `sensitive_json_paths` —
defaulting to `default-v1` and the four auth headers. `MatcherProfile` and
`OutputFormat` (`Human` / `Json` / `Junit`) are the remaining two.

Note the vocabulary split this creates: `config::MatcherProfile` is the
serialisable *profile selection*, while `matching::Matcher` is the constructed
comparator built from it plus a `BodyMatchMode`. And `config::RedactionProfile`
is the persisted identity, while `security::RedactionConfig` is the in-memory
selector set derived from it (`security.rs:37-47`).

**The error taxonomy is a matrix, and both axes are stable wire vocabularies.**
`ErrorPhase` (`:9-28`) says *where* it broke: `Request`, `Connect`, `Tls`,
`Headers`, `Body`, `Timeout`, `Cancelled`, `Policy`, `Other`. `ErrorCategory`
(`:33-52`) says *what*: `Dns`, `ConnectionRefused`, `Unreachable`, `Tls`,
`Protocol`, `Policy`, `Timeout`, `Cancelled`, `Other`. Both are `Copy + Eq +
Serialize` with `#[serde(rename_all = "snake_case")]`, so they round-trip through
JSONL and can be pattern-matched exhaustively. `FlowError` (`:57-64`) is
`{ category, phase, message }` with the explicit rule that `message` is sanitised
and bounded — "credentials and bodies must not appear" (`:62`).

`FlowError::new` truncates the message at 512 bytes (`:108-118`) — a *byte*
truncation, so a multi-byte character can be split; the result is a diagnostic, and
the truncation is what stops an unbounded transport error from bloating a fixture.
The same value doubles as the `thiserror` `Display` (`:56`).

`as_str()` exists for both enums (`:73-85`, `:91-103`) for surfaces that record a
category as a bounded string rather than structured JSON — the doc names the
`stream-events` extension. Because a hand-maintained `as_str` can silently drift
from the serde rename, a test pins each variant by serialising it and comparing
against `as_str()` (`:129-165`). That is the right way to pin it, and a second
test asserts the three longest names fit the 64-byte stream-event bound
(`:170-175`) — so a classified terminal error is never rejected at finalisation
just because a category name was too long.

**This is the seam that lets presentation layers stay dumb.**
`ErrorCategory` is a closed enum with nine variants, which means the CLI can map
it to an exit code with an exhaustive `match` that the compiler forces to stay
complete, and the Python binding can map it to an exception type the same way —
without either layer parsing strings or re-deriving the meaning of a failure. The
phase axis gives the diagnostic a place ("during `Connect`") without leaking
transport-specific error text. `ScenarioFault::TransportError { category, phase }`
(`scenario.rs:201-206`) reuses the same vocabulary, which is why an authored fault
and a recorded transport failure are indistinguishable to a consumer.

## Review checklist

When changing anything in `eggreplay-core`, these are the properties to check.
Each is a real invariant, and each names the failure mode it prevents.

**Determinism.** No wall clock, no RNG outside `Flow::new`'s id suffix, no
iteration over unordered collections, no float in a compared value except
`StreamTimingMode::Scaled`'s `ceil`-quantised output.
- Every collection that reaches output or comparison is a `BTreeSet`, `BTreeMap`,
  or `Vec` (`normalize_headers` `:523-538`, `header_map` `report.rs:464-473`).
- `NearMiss` is sorted by `(cost, candidate_index)` (`matching.rs:392`); findings
  are sorted (`report.rs:322-324`, `:381`); SSE and predicates are order-sensitive
  by design, not by accident.
- The only `f64` is the scale factor, and `delay_ns` rounds with `ceil`
  (`stream.rs:245`) and rejects non-finite or out-of-range input (`:229-231`).
- `HashSet` appears in `websocket.rs` only for *membership* checks
  (`:187-188`, `:231`, `:292`) whose results feed `len()`/containment, never
  iteration order.

**Ordering stability.** Ask what a reader would see twice.
- `HeaderEntry` and `QueryPair` preserve wire order and duplicates; never
  "normalise" them into a map in a record type.
- `reconcile_headers_after_body_redaction` moves `Content-Length` to the end
  (`security.rs:237-240`) — expected, but know it before asserting header order
  after redaction.
- Scenario transitions are first-match-wins in authored order
  (`scenario.rs:368-382`); reordering a `transitions` array is a behaviour change.
- `timeline_order_offsets` ties equal offsets by fixture index (`stream.rs:113`).
- WebSocket `sequence` must equal its index (`websocket.rs:234`).

**Wildcard correctness.** Redaction must never degrade into a literal compare.
- A redacted header, query key, JSON pointer, or form key is *removed from both
  sides*, not substituted (`matching.rs:330-352`, `:422-423`).
- Opaque bodies with redaction markers must return `false`, not fall back to bytes
  (`:577-581`); redacted form bodies get the `=`-heuristic only (`:600`).
- Response-body JSON markers are written (`security.rs:129-131`) but
  `redacted_body_json_paths` is only ever called with `response = false`
  (`matching.rs:413`), so no request-side wildcard depends on them.
- A scenario must not extract a field its `RedactionConfig` marks sensitive
  (`scenario.rs:614-627`), and the refusal message must not echo the value
  (test `:910-926`).

**Bound enforcement.** Every unbounded operation needs an explicit constant, and
validation must run at load time *and* at use time.
- Bounds that depend on accumulated runtime state must be re-checked in `advance`,
  not only in `validate` (`scenario.rs:383-385`, `:416-418`, `:436-438`).
- Configured bounds must not be able to exceed protocol floors:
  `max_control_payload_bytes.min(125)` and `max_close_reason_bytes.min(123)`
  (`websocket.rs:273`, `:286-287`).
- Capacity bounds are distinct from content bounds; both matter
  (`stream.rs:9-24`, `websocket.rs:13-42`).
- `FlowError::new` truncates at 512 bytes (`error.rs:110-112`) — the only
  diagnostic bound, and it is a byte bound.

**Fail-closed behaviour.** When a transform cannot be trusted, refuse.
- `apply_json_redaction` errors on malformed JSON rather than passing bytes through
  (`security.rs:152`); `apply_form_redaction` errors on a body that does not
  round-trip (`:177-179`).
- `Flow::validate` rejects out-of-range status and completion-before-start
  (`flow.rs:246-263`).
- Scenario rendering errors on unterminated `{{`, invalid names, and missing
  variables rather than emitting empty strings (`scenario.rs:672-680`).
- `redact_url` returns `"<invalid-url>"` rather than the unparsable input
  (`security.rs:264`).
- `FlowError` messages are sanitised and truncated; `DiffFinding` values are
  presence-only or digest-shaped (`report.rs:449-458`).
- `BlobRef::new` validates digest form before it can name a file
  (`flow.rs:74-80`).

**Two things to re-verify whenever the matcher or report changes.**

1. `MatchResult::Exhausted`'s doc claims its `near_misses` include consumed exact
   candidates (`matching.rs:187-191`); the implementation does not emit them
   (`:376`). Either fix the doc or emit a near-miss, and add the assertion the
   current test (`:775-784`) is missing.
2. The near-miss bound truncates in candidate order before sorting
   (`matching.rs:382` then `:392`), so `near_misses` is the closest of the *first N*
   candidates, not the globally closest. If that is not intended, the bound must
   be applied after scoring.
