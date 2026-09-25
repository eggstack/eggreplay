# CLI and exit contracts

Every result-producing command accepts `--output human|json|junit` (JUnit is
meaningful for `replay`/`test`/`diff`; other commands project a single
assertion). JSON is a versioned envelope (`command`, `schema_version`,
`success`, `failure_class`, `warnings`, `payload`); stdout carries JSON/JUnit,
stderr carries `{failure_class}: {message}` diagnostics that agree with the
envelope. Human rendering is terminal text (`ok`/`failed (...)`), never machine
JSON, and is not a parsing contract.

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
`--websocket-cadence-tolerance-ms` adds message cadence findings.

`inspect --websockets` reports conversation IDs, flow links, message kind,
direction, timing, payload digest/length, close metadata, and redaction markers;
it never prints message bytes.

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
record, matcher, upstream, timing, and redaction policies. `serve --timing-mode`
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
Candidate response stream observation uses monotonic deltas; request cadence is
not recorded.

`replay` and `test` keep sequential candidate execution by default. Add
`--scheduler timeline` to start candidate requests at their recorded monotonic
offsets; equal offsets follow fixture order, and `--max-concurrency` (default
8, maximum 1,024) bounds active requests. Timeline mode requires one validated
start offset for every flow and reports findings in fixture order.

Interception builds add two namespaces kept separate from ordinary gateway
recording: `eggreplay proxy record ...` (explicit-proxy listener with target
policy file/flags, `deny`/`tunnel`/`intercept` CONNECT default, CA directory
for intercept rules, and bounded tunnel/cert-cache limits) plus
`eggreplay proxy validate --policy-file ...` (dry-run policy check printing
the normalized policy), and `eggreplay ca
init|import|inspect|export|rotate ...` (operator-owned CA lifecycle; public
metadata only, no overwrites, no trust installation). These commands exist
only in `intercept`-feature builds; other builds fail them with a capability
message. Default builds do not enable the feature. Release-binary
recommendation (M013F): keep `intercept` default-off in release binaries
until the hosted qualification matrix is green, then revisit post-M013 in a
separate decision; source builds always require the explicit
`--features intercept` opt-in. Default features are unchanged by M013F. JSON output reports the bind address, compiled
capability, CA public fingerprint, policy counts, accepted/rejected/
tunneled/intercepted counters, recorded flow count, and bounded categorized
failures, and never key contents, key paths, credentials, or decrypted
payloads. See `docs/interception-ca-trust.md` for manual trust setup.
