> Deep dive for [overview](overview.md).

# 09 — CLI Surface

`crates/eggreplay-cli` is the presentation layer. It owns the Clap grammar, the
exit-code contract, envelope emission, and fixture inspection; it owns almost no
HTTP. Both source files carry the same discipline in their headers: `main.rs`
is a dispatcher, `intercept.rs` is a capability-gated operator namespace.

## Crate contract

`eggreplay-cli` is the only crate in the workspace that depends on everything:
`eggfetch-core`, `eggreplay-core`, `eggreplay-har`, `eggreplay-http`,
`eggreplay-store`, and optionally `eggreplay-intercept`
(`crates/eggreplay-cli/Cargo.toml:14-29`). Nothing below it knows the CLI
exists, which is what makes the presentation layer replaceable.

| Concern | Owner |
|---|---|
| Clap grammar, flag defaults, `requires`/`conflicts_with` | `eggreplay-cli` |
| Exit classes and `exit_code_for_class` | `eggreplay-cli` |
| JSON envelope, JUnit projection, human rendering | `eggreplay-cli` |
| Bounded body reads, SSE view, WebSocket transcript view | `eggreplay-cli` |
| Transactional fixture publication | `eggreplay-cli` (filesystem only) |
| HTTP framing, TLS, inbound listeners | `eggreplay-http` / EggServe |
| Outbound client and custom dialer seam | `eggfetch-core` + `EggressDialer` |
| Matcher, comparison, findings, redaction | `eggreplay-core` |
| Fixture read/write, blobs, extensions | `eggreplay-store` |
| HAR projection, schema migration | `eggreplay-har` |
| Proxying, CA lifecycle, interception policy | `eggreplay-intercept` |

`eggreplay-http` is pulled in with `features = ["eggserve", "eggress",
"websocket"]` unconditionally (`Cargo.toml:19`): the `serve` modes, the
Eggress dialer, and offline WebSocket transcripts are always present. Only
`eggreplay-intercept` is optional (`Cargo.toml:20`, `Cargo.toml:47`). The two
HTTP/2 features are separately opt-in (`h2`, `h2-inbound`, `h2-inbound-tls`,
`Cargo.toml:48-53`) so that a default binary stays free of the multiprotocol
closure; the `protocol-boundary` CI lane asserts this.

The one place the CLI *does* hold logic rather than delegating is filesystem
transactionality: it stages a sibling directory, then renames. Nothing in
`eggreplay-store` knows about staged replacements.

## Command surface

Eleven commands, all with the same result contract. The dispatch is a single
match in `run` (`main.rs:683-736`).

| Command | Purpose | Required flags | Real work in |
|---|---|---|---|
| `record` | Proxy live traffic into a new fixture | `--listen`, `--upstream`, `--fixture` | `eggreplay-http` recording gateway |
| `serve` | Serve a fixture; may record on miss | `--fixture` | `eggreplay-http` replay / recording |
| `replay` | Run baseline flows at a target, report differences, exit 0 | `--fixture`, `--target` | `eggreplay-core` comparison |
| `test` | Same run, enforced: findings become exit 1 | `--fixture`, `--target` | `eggreplay-core` comparison |
| `diff` | Compare two fixtures offline, exit 1 on mismatch | `--baseline`, `--candidate` | `eggreplay-core` comparison |
| `inspect` | Read a fixture and report flows/bodies/extensions | `--fixture` | CLI (bounded reads) + store |
| `validate` | Open a fixture and report counts | `--fixture` | store |
| `har import` | HAR 1.2 → `.eggr`, lossy and redacted | `--har`, `--fixture` | `eggreplay-har` |
| `har export` | `.eggr` → HAR 1.2 with `_eggreplay` loss section | `--fixture`, `--har` | `eggreplay-har` |
| `migrate` | Schema-1 → current, transactional | `--fixture` + (`--to` \| `--in-place`) | `eggreplay-har` |
| `proxy` | Explicit-proxy recording and policy validation | see below | `eggreplay-intercept` |
| `ca` | Operator CA lifecycle | see below | `eggreplay-intercept` |

`HarAction` is a two-variant subcommand enum (`main.rs:582-588`), so
`har import` and `har export` are the only sub-subcommands; `Migrate` is a
top-level sibling because it operates on fixtures, not on HAR documents.

`ProxyCommand` and `CaCommand` add seven more sub-subcommands, all in
`intercept.rs` (`intercept.rs:57-63`, `intercept.rs:213-225`).

`migrate` names its destination `--to`, not `--output`, because `--output`
already selects `human|json|junit` on every command (`main.rs:634-637`).

## Shared argument groups

Six `Args` structs are flattened into the per-command structs rather than
duplicated. Flattening means a flag has one definition and therefore one name,
one default, and one validation site across every command that accepts it —
a divergent copy is the usual way CLI surfaces drift.

| Group | Contributes | Used by |
|---|---|---|
| `OutputArgs` | `--output human\|json\|junit` | all commands (`main.rs:61-71`) |
| `InboundServingArgs` | `--inbound`, `--inbound-tls-cert/-key`, `--h2-max-concurrent-streams` | `record`, `serve` |
| `OutboundVersionArgs` | `--outbound-version auto\|http1\|http2` | `record`, `serve`, `replay`, `test` |
| `TimeoutArgs` | `--timeout-secs` | `record`, `serve`, `replay`, `test` |
| `ComparisonArgs` | stream/cadence/SSE/WebSocket comparison switches | `replay`, `test`, `diff` |
| `SchedulerOptions` | `--scheduler`, `--max-concurrency` (not a clap group) | `replay`, `test` |

`OutputArgs` is the widest: every command ends in the same envelope, so
`--output json` is a universal machine contract.

`InboundServingArgs` resolves to a serving policy *before* any listener starts
and fails closed: an unbuildable policy is a configuration error, never a
silent fallback to HTTP/1.1 (`main.rs:103-135`). Supplying TLS identity
material in a build without `h2-inbound-tls` is a refusal
(`main.rs:124-132`). `describe()` returns only a secret-free
`InboundProtocolDescription` — no certificate path, no key path
(`main.rs:147-155`), and it is what lands in every status payload.

`OutboundVersionArgs` exists because route and protocol are independent
decisions: a route says how to reach the peer, the version policy says what to
speak once there (`main.rs:738-742`). `auto` deliberately maps to
`Http1Only`, not EggFetch's `Auto`, so an upstream release cannot change the
protocol of an existing invocation (`main.rs:373-406`).

`TimeoutArgs` is unset by default for the same stability reason. When set it
populates `pool`, `connect`, `write`, `read`, *and* `total` — `total` is the
one that matters, because the per-phase `read` budget only starts once the
response has begun (`main.rs:419-456`).

`ComparisonArgs` is resolved once through `comparison_policy`
(`main.rs:324-354`), which converts milliseconds to nanoseconds with checked
multiplication, builds a `ComparisonPolicy`, and calls its `validate()`; every
result is a `configuration` class.

## Record and Serve

`record` resolves the inbound policy, then the timeout, then checks the
filesystem precondition — configuration mistakes are reported before
filesystem ones (`main.rs:787-792`). If the fixture exists and `--overwrite`
was not passed, it emits a failure envelope and returns
`configuration` (`main.rs:794-804`).

`serve` resolves policy first, then **recovers an interrupted transaction**
before touching the fixture (`main.rs:888`). The order is deliberate: a
mistyped `--inbound` should be reported before a fixture path is examined.
It then resolves the record mode through core's `RecordMode::resolve`, which
is where "no fallback-to-direct" is decided (`main.rs:889-891`), and refuses
combinations that cannot work:

- `--websockets` with `append-new` (`main.rs:892-897`)
- `--websockets` with sealed, which has no upstream to acquire from
  (`main.rs:898-903`)
- timed replay outside sealed mode (`main.rs:906-913`)
- `--route` on a mode with no network (`main.rs:914-919`)
- `--scenario` with `once` (`main.rs:920-926`)
- `append-new` against a missing fixture (`main.rs:929-935`)

`serve_sealed` never opens an upstream. It loads either
`load_with_scenario_and_redaction_and_timing` or `load_with_timing`, starts
with `start_with_protocol`, prints one startup line to stderr, waits for
`ctrl_c`, then emits (`main.rs:945-998`).

`record_once_from_serve` requires `--upstream` through `required_upstream`
(`main.rs:1074-1080`) and creates a fresh session with capture mode
`gateway-once` (`main.rs:1005-1015`).

`serve_append_new` opens the source fixture, records new flows into a
sibling staging session, and on shutdown finalizes the staging session,
merges source+additional into a *second* sibling, and only then publishes
(`main.rs:1082-1180`). It additionally requires the redaction profile id to
equal the source fixture's, because appending under a different profile would
silently mix two redaction regimes in one fixture
(`main.rs:1088-1093`).

### Transactional fixture replacement

Three helpers, all in `main.rs:1264-1329`, and they are the reason an
in-place re-record cannot leave a torn fixture.

`sibling_transaction_path(fixture, label)` builds a hidden sibling name of the
form `.{name}.{label}-{pid}-{nanos}` (`main.rs:1264-1274`). Same directory, so
the later `rename` stays within one filesystem; the pid+nonce avoids collision
between concurrent invocations; the leading dot keeps the stage out of casual
`ls`.

`replace_fixture_transactionally(staged, target)` is the publish step
(`main.rs:1276-1299`):

1. If the target does not exist, a single `rename` publishes it. Atomic, no
   backup needed.
2. Otherwise rename the *old* target aside to a `.backup-` sibling. The target
   path is now briefly absent.
3. Rename the staged directory into place.
4. If step 3 fails, immediately rename the backup back. The error distinguishes
   "restored" from "restore failed", and the second case names the backup path
   an operator can use by hand.
5. Remove the backup. A failure here is reported but does not fail the
   command: the new fixture is already published, and the message says so.

`recover_fixture_transactionally(target)` closes the gap left by a crash
between steps 2 and 3 (`main.rs:1301-1329`). If the target is missing it
scans the parent directory for `.backup-` siblings, sorts them, tries the
newest first, and restores the first one that `Session::open` accepts. A
corrupt backup is skipped rather than restored. `serve` calls this before
resolving its record mode, so an interrupted re-record heals on the next
invocation.

`serve_re_record` uses the whole sequence: record into a `rerecord` sibling
(`main.rs:1193-1204`), and only after `finish_recording_session` succeeds does
it `drop` the session and publish (`main.rs:1247-1253`). The old fixture is
therefore intact for the entire recording window and is only removed once the
new one is complete. `migrate --in-place` and `migrate --to` reuse the same
staging and publish helpers (`main.rs:2565-2593`, `main.rs:2602-2632`).

Two in-tree unit tests pin this: `replacement_publishes_complete_stage_and_rolls_back_failed_publish`
and `interrupted_replacement_restores_the_last_valid_backup`
(`main.rs:2853-2893`).

## Replay, Test, and Diff

`replay` and `test` share one function, `regression`, differing only in the
`enforce` flag and the command name (`main.rs:687-724`). `regression` opens the
session, parses the target, builds the client, and loads baseline stream
events when requested — missing metadata is an explicit `fixture` or
`configuration` error, never a fallback (`main.rs:1401-1432`).

`SchedulerChoice` selects the concurrency model (`main.rs:524-534`):

- `Sequential` (default) runs flows in fixture order, one at a time.
- `Timeline` requires the `stream-events` extension, validates it, requires one
  `start_offset_ns` for *every* flow, rejects `--max-concurrency` outside
  `1..=1024`, computes an order with core's `timeline_order_offsets`, and
  spawns a bounded `JoinSet` where each task sleeps until
  `origin + offset` (`main.rs:1453-1541`). Results are written back by
  original index, so findings are reported in fixture order regardless of
  completion order.

`compare_candidate_flow` (`main.rs:1589-1701`) has two branches. A baseline
with a `101` outcome is a WebSocket flow: it loads `websocket-messages`,
finds the conversation, and hands off to
`eggreplay_http::compare_websocket_candidate` with the cadence tolerance. Any
other flow is read from blobs, executed with `execute_candidate`, and compared
with `compare_flows_with_policy`; stream findings are then appended and the
finding list re-sorted by `(kind, field)` so output is deterministic.

The exit contract is the last three lines of `regression`
(`main.rs:1551-1585`): with `enforce` false, `replay` always emits success and
returns `Ok`, reporting `finding_count`; with `enforce` true, `test` emits
success only when `findings.is_empty()`, otherwise emits with class
`regression` and returns `Err(("regression", ...))`, which `exit_code_for_class`
maps to 1.

`diff` is fully offline. It opens both fixtures, zips flows positionally, and
compares each pair; success requires equal flow counts *and* all-clean reports
(`main.rs:1887-1890`). A pair containing a `101` on either side requires
conversation metadata on both sides, otherwise it is a `fixture` error
(`main.rs:1859-1880`). `diff` reports `outbound_timeout: null` because it never
opens a socket — the comment at `main.rs:1901-1904` explicitly distinguishes
"not applicable" from "unbounded".

## Inspect and Validate

`inspect` is the only command that reads payload bytes. Bodies are read only
under `--bodies`, and only through `inspect_body`
(`main.rs:2211-2306`), which:

- opens the blob via the validated streaming seam and reads at most
  `max_bytes`, capping the buffer with `usize::try_from` so a 32-bit build
  cannot silently truncate (`main.rs:2233-2243`);
- reports `length` (true size), `shown` (bytes read), and `truncated` — a
  truncation is always an explicit, counted fact;
- emits UTF-8 `text` only when the shown prefix is valid UTF-8;
- otherwise emits `encoding: "binary"` with the stored digest plus a
  `shown_sha256` of the visible prefix;
- emits `encoding: "base64"` only with `--bodies-base64`, and stops the
  encoder at 4 KiB regardless of `--max-body-bytes` (`main.rs:2259-2282`).

`--max-body-bytes` defaults to 65536 (`main.rs:563`).

Redaction cannot be bypassed here: stored blobs are already redacted at
record time, so `inspect` reads markers, not secrets. Flow-level
`redactions` are surfaced as typed markers per flow (`main.rs:1999`), and
`--bodies` never re-derives a secret that the recorder already replaced.

`--sse` adds a derived view only for `text/event-stream` responses
(`main.rs:1977-1993`). `inspect_sse` clamps to both `--max-body-bytes` and
core's 16 MiB `MAX_SSE_BODY_BYTES` and returns an explicit error object when
the body exceeds that bound (`main.rs:2308-2322`); raw bytes remain
authoritative.

`--websockets` builds a metadata-only view: conversation id, flow link, selected
subprotocol, terminal reason, and per message sequence, direction, kind,
`delta_ns`, payload length and digest, close code/reason, and redaction markers
(`main.rs:1917-1948`). Message bytes are never printed; `payload_summary` and
`fixture_payload` are digests and lengths only (`main.rs:2194-2209`).

`validate` is deliberately thin: open the session, report `flow_count` and
`schema_version` (`main.rs:2373-2384`).

## HAR and Migrate

`har import` checks the overwrite precondition, reads the HAR bytes, builds
the redaction policy, and only then creates the writer
(`main.rs:2396-2430`). The policy is applied *before* publication, inside
`eggreplay_har::import_har_to_writer`. Error variants map to classes
explicitly: `Store` → `fixture`, everything else → `configuration`
(`main.rs:2439-2447`). `--loss-report` writes a side document
(`main.rs:2451-2466`).

`har export` opens the fixture, calls `eggreplay_har::export_session_to_har`,
and refuses to clobber an existing HAR without `--overwrite`
(`main.rs:2484-2500`). Typed flow errors are projected as ordinary recorded
statuses, so the export itself exits 0 — loss is data, not failure.

`migrate` validates flag shape first (`--to` and `--in-place` are mutually
exclusive and one is required, `main.rs:2540-2551`), then range-checks
`--target_schema` against `SESSION_SCHEMA_V1..=SESSION_SCHEMA_VERSION`
(`main.rs:2552-2562`). `MigrationError::Blocked` becomes a `fixture` error, so
unknown required extensions and future schemas fail closed. Both variants stage
to a sibling before publishing (`main.rs:2565-2593`, `main.rs:2602-2632`).

All three build redaction through the same funnel, `redaction_policy`
(`main.rs:2344-2371`): `effective_redaction_policy` for `record`,
`effective_serve_redaction_policy` for `serve`, `har_redaction_policy` for
`har import`. Without `--unsafe-replace-default-redaction` the secure defaults
are extended (header names lowercased, query keys and JSON pointers unioned);
with it, only the operator's selections apply. This is the CLI half of the
precedence documented in `docs/configuration.md`.

## Output, envelopes, and exit codes

`Envelope<T>` is the single JSON shape (`main.rs:651-659`): `command`,
`schema_version` (currently 1), `success`, `failure_class`, `warnings`,
`payload`. `emit_with_warnings` is the JSON writer
(`main.rs:2783-2805`); `emit` is the no-warnings wrapper
(`main.rs:2773-2781`).

`emit_reports` is the report-shaped variant (`main.rs:2725-2771`) used by
`replay`, `test`, and `diff`. Its payload is `target` (run through
`redact_url`, or `null` when empty), `reports`, `finding_count`, and
`outbound_timeout`.

`junit_for_reports` projects the report authority into one `<testcase>` per
flow id, with `failures` counted from `!report.is_success()`
(`main.rs:2676-2722`). Single-assertion commands project as one testcase
through `emit_with_warnings` (`main.rs:2806-2825`). `escape_xml` handles all
five entities (`main.rs:2667-2674`) and is applied to command names, flow ids,
finding kinds, fields, and baseline/candidate values, so a hostile response
header cannot break the document. JUnit is a projection, never a
re-evaluation.

Human output is `ok` / `failed (...)` or a findings count — never JSON
(`main.rs:2827-2834`, `main.rs:2755-2769`).

`exit_code_for_class` is the compatibility surface (`main.rs:661-670`):

| Class | Exit | Meaning |
|---|---|---|
| — | 0 | success (`replay` reports differences with 0) |
| `regression`, `diff` | 1 | assertion / fixture mismatch |
| `configuration` | 2 | invalid CLI, config, or policy |
| `fixture` | 3 | invalid or corrupt fixture |
| `runtime` | 4 | network/runtime execution failure |
| anything else | 5 | internal |

`main` prints `{class}: {message}` to stderr and derives the code from the same
class string (`main.rs:672-681`). Stdout and stderr therefore agree by
construction: the class that produces the exit code is the class in the
envelope's `failure_class` and the stderr prefix.

The integration suite asserts this agreement rather than the shape:
`malformed_route_fails_configuration_without_credentials` pins exit 2, a
`configuration` stderr prefix, and the absence of a credential sentinel
(`tests/cli_contracts.rs:106-142`); `missing_fixture_is_exit_three_with_fixture_class`
and `invalid_target_is_exit_two` pin the neighbouring classes.

## Interception subcommands

`intercept.rs` declares `proxy` and `ca` unconditionally so scripts see stable
command names, but every handler is behind `cfg(feature = "intercept")`. In a
feature-off build `run_proxy` and `run_ca` emit a `configuration` envelope
carrying `interception_compiled: false` and return
`NOT_COMPILED_MESSAGE` — exit 2, never a silent substitution
(`intercept.rs:355-406`, `intercept.rs:23-30`). The `about` strings also differ
by build, and a test asserts the long help advertises the right one
(`intercept.rs:958-977`).

`proxy record` (`intercept.rs:100-178`) is a separate namespace from gateway
recording: `--listen` defaults to `127.0.0.1:0`, `--default-action` is
`deny|tunnel|intercept` with `deny` as the default
(`intercept.rs:89-98`), and policy comes from either a versioned
`--policy-file` (`eggreplay-intercept-policy/v1`) or the repeatable
`--allow-host`/`--deny-host` flags, which `conflicts_with` the file
(`intercept.rs:115-129`). `--ca-dir` is required for the `intercept` action.
Bounded limits (`--max-tunnels`, cert cache, max body, connections) are all
explicit with conservative defaults.

`check_listener_bind` is the loopback-first gate
(`intercept.rs:336-353`): a non-loopback bind is a `configuration` refusal that
names the open-forward-proxy risk and requires `--allow-non-loopback`. There
is no proxy authentication in M013, so the opt-in is a deliberate, named
acknowledgement rather than a mitigation.

`proxy validate --policy-file` is a dry run that prints the normalized policy
and never records; an unknown policy version is a `configuration` error.

`ca init|import|inspect|export|rotate` (`intercept.rs:197-330`) are operator
CA lifecycle commands. Every one refuses to overwrite an existing directory,
`export` copies the public certificate only, and none installs trust anywhere.
`ca_payload` reports public metadata and fingerprint only
(`intercept.rs:732`), and `map_ca_error` appends a remediation hint per
variant (`intercept.rs:427-459`) without leaking key material.

`proxy record`'s success payload reports bind address, compiled capability, CA
fingerprint, policy version and rule/action counts, the four
accepted/rejected/tunneled/intercepted counters, recorded flow count, bounded
categorized failures, and a redacted route (`intercept.rs:673-690`).

## Review checklist

Exit-code stability. `exit_code_for_class` is a match on five strings with a
catch-all of 5. Renaming a class, or letting a new failure path borrow an
existing class, silently moves a user's CI. The three interception tests
(`intercept.rs:833-841`, `intercept.rs:940-947`) and
`cli_contracts.rs` pin the mapping rather than the prose.

Human output is not a parsing contract. `docs/cli.md:8-9` says so, and the
code honours it: human rendering never contains machine JSON. Anyone adding a
`--output human` field that a script might start matching is introducing a new
unversioned contract.

Redaction precedence. Exactly one function decides it (`redaction_policy`), and
three thin wrappers route into it. A second construction site would be a
precedence bug. `har import` must apply the policy before publication, and
`append-new` must refuse a profile change.

Transactional replacement. `re-record`, `append-new`, and `migrate` stage and
publish; `recover_fixture_transactionally` heals a crash. But `record --overwrite`
and `har import --overwrite` still call `remove_dir_all` on the target and then
create the session in place (`main.rs:805-808`, `main.rs:2417-2420`) — a crash
mid-write leaves a missing or partial fixture. That path is not a torn *merge*,
but it is not transactional either, and the asymmetry is worth a review
decision.

Can any command fail open? Checked, and no:

- unbuildable inbound/outbound policy → `configuration` refusal, not a
  weaker protocol;
- sealed `serve` never opens an upstream, and `--route` is refused when the
  resolved mode has no network;
- `timed replay` outside sealed, `--scenario` with `once`, `--websockets` with
  `append-new` or sealed → refusal;
- requested stream/SSE/WebSocket comparison with missing metadata → explicit
  `fixture` error, never silent skip;
- interception without the feature → capability refusal, not another mode;
  non-loopback proxy bind → refusal without `--allow-non-loopback`;
- `migrate` with a future schema or unknown required extension → `fixture`
  refusal, source untouched.

One documented-but-not-quite-enforced item to keep in mind: `docs/cli.md:93`
says serve's JSON reports the effective record, matcher, upstream, timing,
*and* redaction policies, but the sealed payload (`main.rs:995`) omits
`redaction_profile` and `outbound_timeout` even though `serve` accepts both
flags; the other three modes report them (`main.rs:1068`, `main.rs:1177`,
`main.rs:1259`). A sealed server opens no socket, so the omission is defensible
as behaviour but the flags are still accepted. `docs/cli.md` now states the
sealed/network-capable split explicitly; aligning the payload is a product
change, not a doc change.

The `--outbound-version` drift that used to sit here is resolved:
`docs/cli.md` and `docs/http2-support.md` now document the clap value names
(`auto|http1|http2`) and the rejected `--inbound h2-tls` spelling.
