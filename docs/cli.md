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
bytes remain authoritative. `diff` compares ordered SSE fields whenever both
responses declare `text/event-stream`, alongside the ordinary raw-body
comparison.

`replay` and `test` keep sequential candidate execution by default. Add
`--scheduler timeline` to start candidate requests at their recorded monotonic
offsets; equal offsets follow fixture order, and `--max-concurrency` (default
8, maximum 1,024) bounds active requests. Timeline mode requires one validated
start offset for every flow and reports findings in fixture order.
