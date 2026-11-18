# CLI and exit contracts

Every result-producing command accepts `--output human|json|junit` (JUnit is
meaningful for `replay`/`test`/`diff`; other commands project a single
assertion). The flag is **per-subcommand, not global**. It must follow the
command name (`eggreplay validate --output json`). JSON is a versioned envelope
(`command`, `schema_version`, `success`, `failure_class`, `warnings`,
`payload`); stdout carries JSON/JUnit, stderr carries `{failure_class}:
{message}` diagnostics that agree with the envelope. Human rendering is
terminal text (`ok`/`failed (...)`), never machine JSON, and is not a parsing
contract. `schema_version` on the envelope is `1` and is a separate counter from
the report schema and the session/flow schemas.

Commands: `record`, `serve`, `replay`, `test`, `diff`, `inspect`, `validate`,
`har import`, `har export`, `migrate`, plus the `--features intercept`-gated
`proxy record`, `proxy validate`, and `ca init|import|inspect|export|rotate`.
`validate` checks a fixture without reading bodies; it is not the same as
`inspect --bodies`.

Stable exit codes (compatibility contract):

- `0` success (`replay` reports differences with `0`; `test`/`diff` assert);
- `1` regression/assertion mismatch (`replay` findings are reported, `test`
  enforces with `regression`, `diff` with `diff`);
- `2` invalid CLI/config/policy (including malformed `--route`, no
  fallback-to-direct);
- `3` invalid/corrupt fixture;
- `4` network/runtime execution failure;
- `5` internal/unexpected failure.

## Replacing fixtures and destinations

`--overwrite` on `record`, `har import`, `migrate --to`, and (interception
builds) `proxy record` means **replace an existing fixture or empty
directory**. It is not permission to delete an arbitrary path. A replacement
target must be one of:

- absent (it is created);
- an empty directory (replacing it discards nothing);
- a directory that opens as a valid `.eggr` fixture, including supported older
  session schemas (`eggreplay validate` succeeds).

Anything else is a `configuration` refusal (exit `2`) that names the path and
the reason. `.`, `..`, filesystem roots, paths containing a `.` or `..`
component, symlinks (never followed), regular files, non-empty directories
that are not fixtures, and fixtures that fail to open (missing manifest,
unsupported future schema, malformed flows) are all refused.

Without `--overwrite`, an existing target is refused even when it is a valid
fixture or an empty directory. `serve --record-mode re-record` has no
`--overwrite`: it implicitly replaces its own fixture under the same rules and
validates the target before binding the listener.

Every destructive command validates its configuration and inputs first,
records or migrates into a sibling staging directory, finalizes and re-opens
the staged fixture, and only then swaps it into place. A failure at any point
up to the swap preserves the old fixture **byte-for-byte**; a failed swap
restores the backup. Normal error paths remove the staging tree. Only a crash
or `SIGKILL` can leave a hidden `.incomplete-*` directory, and nothing sweeps
those automatically on startup.

`record`, `replay`, and `test` accept a common `--route` (`direct` default, or
a pproxy URI such as `socks5://127.0.0.1:1080` and two-hop
`socks5://...__http://...`). Non-direct routes use `EggressDialer` via
EggFetch's custom Dialer seam; redaction-safe `physical_route` metadata is
recorded in flows. `inspect --bodies` performs an explicit bounded body read
(`--max-body-bytes`, default 64 KiB, truncates with counts): UTF-8 text when
valid, otherwise length + digest (bounded base64 only with `--bodies-base64`).
Stored bodies are already redacted, so inspection never bypasses markers.

WebSocket recording is disabled by default. Use `record --websockets` or
`serve --record-mode once|re-record --websockets` to admit HTTP/1.1 WebSocket
upgrades. `--redact-websocket-text` and `--redact-websocket-binary` replace
whole message payloads before publication; configured JSON Pointer redaction
also applies to valid JSON text messages. JSON-pointer policy fails **safe**:
a text message that is not valid JSON is recorded as `[REDACTED]` with a
whole-message marker instead of keeping the original bytes, and the
conversation continues. Close reasons are redacted whenever any WebSocket
redaction is configured (`--redact-websocket-text`,
`--redact-websocket-binary`, or a JSON pointer); close codes stay
authoritative. `serve --record-mode append-new`
rejects `--websockets`. Offline `replay`/sealed `serve` and candidate `test`
automatically use recorded WebSocket transcripts. The optional
`--websocket-cadence-tolerance-ms` adds message cadence findings. It is a
**comparison** flag, so it exists on `replay`, `test`, and `diff`, not on
`record` or `serve`.

Acquisition without payload redaction emits one advisory warning. It does not
enable redaction; see
[`configuration.md`](configuration.md#advisory-redaction-warnings).

`record`, `har import`, and the network-capable `serve` recording modes also
warn when a credential-like request header or query key is outside the
effective redaction set. The default profile covers `authorization`,
`proxy-authorization`, `cookie`, `set-cookie`, `x-api-key`, `api-key`,
`x-auth-token`, `x-access-token`, `x-amz-security-token`, and
`x-goog-api-key`; recommend `--redact-header x-csrf-token` and explicit
`--redact-query` selectors for query credentials. The full list, compatibility
impact, and the reason `location`/`referer` are excluded are in
[`configuration.md`](configuration.md#redaction-policy-c003). Warnings are
value-free and advisory: they never change exit codes or payload schemas.

`inspect --websockets` reports conversation IDs, flow links, message kind,
direction, timing, payload digest/length, close metadata, and redaction markers;
it never prints message bytes.

## Listener exposure

`serve` defaults to `127.0.0.1:0` and `record` requires `--listen`; either way,
a **non-loopback address is refused** unless `--allow-non-loopback` is passed.
The listener is unauthenticated: a reachable replay server serves its fixture
to any client, and a reachable recording gateway drives the configured
`--upstream` on behalf of any client. TLS does not change that —
`--inbound-tls-cert`/`--inbound-tls-key` protect bytes in transit and do not
authenticate the caller, so a public bind with TLS is still an open listener.

The refusal is a `configuration` failure (exit `2`) that names the bind. It runs
immediately after the inbound protocol policy resolves, before fixture
recovery, replacement-target classification, staging, or `bind()`, so a refused
listener never touches a fixture. It applies to `record` and to every
`serve --record-mode` value (`sealed`, `once`, `append-new`, `re-record`), and
no `--inbound` value can bypass it. The explicit proxy has the same gate; see
[`interception-ca-trust.md`](interception-ca-trust.md).

## Transport protocol flags

`serve` and `record` take `--inbound`, which selects the **serving** protocol.
Accepted values are `http1` (aliases: `h1`, the default) and `http2` (aliases:
`h2`, `h2c`, `http2-cleartext`; all select cleartext HTTP/2 prior knowledge).

`--inbound h2-tls` / `http2-tls` is a **recognised name that is deliberately
rejected**: TLS serving requires operator certificate and key material, so the
policy is built from `--inbound-tls-cert` and `--inbound-tls-key` rather than by
naming the protocol. Passing it exits `2` with a diagnostic. ALPN negotiation
therefore happens when the identity is supplied, not when the name is chosen.

`http2` requires a build with the `h2-inbound` (or `h2-inbound-tls`) cargo
feature; without it the flag fails closed with exit code `2` rather than
silently serving HTTP/1.1. For TLS, pass both `--inbound-tls-cert` and
`--inbound-tls-key` to supply the operator's identity. These are not a minted CA.
`--h2-max-concurrent-streams` bounds concurrent streams per connection. The JSON
result of both commands reports the effective `serving` policy.

`serve`, `replay`, `record`, and `test` take `--outbound-version`, which selects
the **outbound** client policy. Accepted values are `auto` (the default),
`http1`, and `http2`. Use the spellings `http1` and `http2`, not `h1` or `h2`,
which are rejected at parse time. Selecting `http2` requires the CLI's `h2`
feature. **`auto` means HTTP/1.1**, not EggFetch's `Auto`: an upstream release
must not be able to change the protocol of an existing invocation.

`record`, `replay`, `test`, and `serve` take `--timeout-secs <N>`, a wall-clock
ceiling for one outbound request. It bounds every phase **including `total`**,
which is the part that matters most: the per-phase `read` budget is "time
between response body chunks", so an origin that accepts a connection and then
says nothing is bounded only by `total`. The flag is **unset by default**,
preserving existing invocation behavior, just as `--outbound-version auto`
does. A timed-out transaction is recorded in the flow as
`ErrorCategory::Timeout`; the command still exits 0. The effective timeout is
reported as `outbound_timeout` in the JSON result (`{"bounded": false,
"total_secs": null}` when unset). `diff` reports `outbound_timeout: null`
because it never opens a socket.

A fixture recorded over `https` replays only over TLS. A scheme mismatch is a
bounded refusal (not a relaxed match), and a gateway flow records the
*upstream* authority it reached rather than the inbound `:authority`.

See `docs/http2-support.md` for tiers, invariants, and known limitations.

`serve --scenario <ID>` explicitly enables one authored scenario from the
fixture's `rules` extension. Without the flag, a required scenario extension
fails closed instead of being silently ignored. Scenario state is isolated to
that server process.

`serve --record-mode sealed` is the default and never opens an upstream.
`once` records only when the destination fixture is new; if it already exists,
the policy resolves to sealed replay. `append-new` and `re-record` require an
explicit `--upstream`; append-new uses EggFetch on exact misses and transactionally
publishes the merged fixture when the server stops, while re-record stages a
replacement, refuses a target that is neither a fixture nor an empty
directory before binding, and preserves the old fixture until the new
recording validates.
`--route` is only accepted with a network-capable mode. `--matcher-profile`
selects `strict` or `practical`, and serve's JSON result reports the effective
record, matcher, upstream, and timing policies, plus the redaction profile and
outbound timeout in the network-capable modes (`once`, `append-new`,
`re-record`). The **sealed** mode reports record, matcher, upstream, and timing
but omits `redaction_profile` and `outbound_timeout` even though it accepts both
flags; a sealed server opens no socket, so nothing is timed. `serve --timing-mode`
accepts `immediate` (default), `recorded`, or `scaled:<factor>` with factors
from `0.01` to `100`. Timed replay needs a validated `stream-events`
extension; recorded data delays are capped at 60 seconds each and five minutes
per flow. Dropping a response cancels its pending sleep.

`inspect --sse` adds a derived view for `text/event-stream` response bodies.
The view is bounded by `--max-body-bytes` and the 16 MiB parser cap; raw body
bytes remain authoritative. `replay`, `test`, and fixture `diff` compare
ordered response stream events only with `--compare-stream-events`, cadence
only with `--cadence-tolerance-ms <N>` (which implies stream comparison), and
derived SSE semantics only with `--compare-sse` or repeatable
`--sse-ignore <field>` (`data,event,id,retry,comments`, which implies SSE
comparison). Raw body comparison remains authoritative; SSE findings never
suppress raw-body findings. Requested stream comparison requires valid metadata
on both sides; missing metadata is an explicit fixture/configuration error.

Stream comparison covers the **response direction only**, and that is a
structural decision rather than a gap. Recorded request events are inbound
transport-frame boundaries, and a candidate's request is synthesized by the
replay client, so a request-direction comparison would measure the original
client's framing against EggReplay's own. The volatile `Date` header is
suppressed when present on both sides and listed in the report's `suppressed`
array. If it appears on only one side, the presence mismatch remains a finding.
Other headers are compared normally.

`--max-elapsed-ms <N>` asserts that each candidate flow finishes within N
milliseconds of starting, emitting a `timing.elapsed_ms` finding when it does
not. It is a **comparison** bound on a finished run, not playback configuration:
it is independent of `--timing-mode`, which controls how a fixture is paced
during replay. `diff` compares two fixtures and applies the same bound to the
candidate side.

`replay` and `test` keep sequential candidate execution by default. Add
`--scheduler timeline` to start candidate requests at their recorded monotonic
offsets; equal offsets follow fixture order, and `--max-concurrency` (default
8, maximum 1,024) bounds active requests. Timeline mode requires one validated
start offset for every flow and reports findings in fixture order.

HAR import and export are offline and lossy. Import a HAR 1.2 file with
`eggreplay har import --har input.har --fixture out.eggr`; the selected redaction
policy is applied before publication. The HAR is read and parsed into a staged
fixture before the destination is replaced, so `--overwrite` still means
"replace a fixture or empty directory" and a malformed document leaves the old
fixture untouched. Import writes a non-required
`interop-provenance` extension and can emit a loss report with `--loss-report`.
Export with `eggreplay har export --fixture in.eggr --har out.har`; the output
includes `_eggreplay` loss/provenance metadata and can emit a separate loss
report with `--loss-report`.
On Unix both the HAR and every `--loss-report` are created with mode `0600`
from creation, because they contain recorded bodies. `--overwrite` keeps its
usual refusal/acceptance semantics. HAR comments and import provenance are
user-supplied metadata outside the header/query/body policy; see
[`har-interchange.md`](har-interchange.md).
Neither direction is lossless. Upgrade a schema-1 fixture transactionally with
`eggreplay migrate --fixture old.eggr --to new.eggr` or
`eggreplay migrate --fixture old.eggr --in-place` to replace it. A `--to`
destination is validated before the source is read and must be absent, empty,
or a valid fixture; `--overwrite` refuses any other existing path. Migration
checks registered extensions and rejects unknown required extensions and future
schemas. `validate` warns, without naming any value, when a fixture's
`stream-events` extension still contains pre-fix trailer values. Remediate
explicitly with
`eggreplay migrate --fixture old.eggr --in-place --scrub-stream-trailers` (or
`--to <destination>`). The scrub is **opt-in**: migration without the flag
preserves the extension bytes verbatim. It rewrites trailer values to
`<redacted>` idempotently and leaves names, order, delays, DATA events, and
other extensions untouched. See
[`har-interchange.md`](har-interchange.md) for the loss matrices and redaction
rules.

Interception builds add `eggreplay proxy record`,
`eggreplay proxy validate --policy-file ...`, and
`eggreplay ca init|import|inspect|export|rotate`. Proxy recording uses an
explicit-proxy listener and a CONNECT policy (`deny`, `tunnel`, or `intercept`);
intercept rules require a CA directory. Validation prints the normalized
policy. CA commands manage operator-owned certificates and keys, expose public
metadata only, and do not install trust. These commands require an explicit
`--features intercept` build; default builds do not enable interception. JSON
reports the bind address, capability, public CA fingerprint, policy counts,
traffic counters, and bounded failures. It never includes key contents, key
paths, credentials, or decrypted payloads. See
[`interception-ca-trust.md`](interception-ca-trust.md) for manual trust setup.
