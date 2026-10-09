# CLI and exit contracts

Every result-producing command accepts `--output human|json|junit` (JUnit is
meaningful for `replay`/`test`/`diff`; other commands project a single
assertion). The flag is **per-subcommand, not global** — it must follow the
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
`validate` checks a fixture without reading bodies — it is not the same as
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
also applies to valid JSON text messages. `serve --record-mode append-new`
rejects `--websockets`. Offline `replay`/sealed `serve` and candidate `test`
automatically use recorded WebSocket transcripts. The optional
`--websocket-cadence-tolerance-ms` adds message cadence findings — note it is a
**comparison** flag, so it exists on `replay`, `test`, and `diff`, not on
`record` or `serve`.

`inspect --websockets` reports conversation IDs, flow links, message kind,
direction, timing, payload digest/length, close metadata, and redaction markers;
it never prints message bytes.

## Transport protocol flags

`serve` and `record` take `--inbound`, which selects the **serving** protocol.
Accepted values are `http1` (aliases: `h1`, the default) and `http2` (aliases:
`h2`, `h2c`, `http2-cleartext` — cleartext HTTP/2 prior knowledge).

`--inbound h2-tls` / `http2-tls` is a **recognised name that is deliberately
rejected**: TLS serving requires operator certificate and key material, so the
policy is built from `--inbound-tls-cert` and `--inbound-tls-key` rather than by
naming the protocol. Passing it exits `2` with a diagnostic. ALPN negotiation
therefore happens when the identity is supplied, not when the name is chosen.

`http2` requires a build with the `h2-inbound` (or `h2-inbound-tls`) cargo
feature; without it the flag fails closed with exit code `2` rather than
silently serving HTTP/1.1. TLS additionally requires `--inbound-tls-cert` and
`--inbound-tls-key` — an operator identity, never a minted CA.
`--h2-max-concurrent-streams` bounds concurrent streams per connection. The JSON
result of both commands reports the effective `serving` policy.

`serve`, `replay`, `record`, and `test` take `--outbound-version`, which selects
the **outbound** client policy. Accepted values are `auto` (the default),
`http1`, and `http2` — note the spellings are `http1`/`http2`, not `h1`/`h2`,
which are rejected at parse time. Selecting `http2` requires the CLI's `h2`
feature. **`auto` means HTTP/1.1**, not EggFetch's `Auto`: an upstream release
must not be able to change the protocol of an existing invocation.

`record`, `replay`, `test`, and `serve` take `--timeout-secs <N>`, a wall-clock
ceiling for one outbound request. It bounds every phase **including `total`**,
which is the part that matters most: the per-phase `read` budget is "time
between response body chunks", so an origin that accepts a connection and then
says nothing is bounded only by `total`. The flag is **unset by default** —
a new deadline would change the behaviour of every existing invocation, the
same rule `--outbound-version auto` follows. A timed-out transaction is
recorded as `ErrorCategory::Timeout` in the flow outcome; it is not a
command-level error, so the command still exits 0. The effective timeout is
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
replacement and preserves the old fixture until the new recording validates.
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
client's framing against EggReplay's own. `Date` is treated as volatile for
comparison: a differing `Date` is reported in the report's `suppressed` list —
present on both sides, deliberately not compared — rather than as a finding and
rather than as a silent absence. Other response headers, and the *presence* of a
volatile header, are still compared normally.

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

HAR interchange and migration (M014A) add three offline commands with the same
envelope/exit contract: `eggreplay har import --har input.har --fixture
out.eggr` (lossy HAR 1.2 → fixture with the selected redaction policy applied
before publication, plus an optional `interop-provenance` extension and
`--loss-report`), `eggreplay har export --fixture in.eggr --har out.har`
(lossy fixture → HAR with an embedded `_eggreplay` loss/provenance section and
optional side report, never claiming round-trip losslessness), and
`eggreplay migrate --fixture old.eggr --to new.eggr` /
`eggreplay migrate --fixture old.eggr --in-place` (transactional schema-1 →
current upgrade with registered-extension checks; `--to` names the destination
because `--output` already selects `human|json|junit`). Unknown required
extensions and future schemas block migration as `fixture` errors. See
`docs/har-interchange.md` for the loss matrices, redaction rules, and golden
corpus.

Interception builds add two namespaces kept separate from ordinary gateway
recording: `eggreplay proxy record ...` (explicit-proxy listener with target
policy file/flags, `deny`/`tunnel`/`intercept` CONNECT default, CA directory
for intercept rules, and bounded tunnel/cert-cache limits) plus
`eggreplay proxy validate --policy-file ...` (dry-run policy check printing
the normalized policy), and `eggreplay ca
init|import|inspect|export|rotate ...` (operator-owned CA lifecycle; public
metadata only, no overwrites, no trust installation). These commands exist
only in `intercept`-feature builds; other builds fail them with a capability
message. Default builds do not enable the feature. M013 closed the
qualification matrix on hosted evidence; default features are unchanged by
M013F, and any change to the release-binary default for `intercept` is a
separate post-M013 decision. Source builds always require the explicit
`--features intercept` opt-in. JSON output reports the bind address,
compiled capability, CA public fingerprint, policy counts,
accepted/rejected/tunneled/intercepted counters, recorded flow count, and
bounded categorized failures, and never key contents, key paths,
credentials, or decrypted payloads. See `docs/interception-ca-trust.md` for
manual trust setup.
