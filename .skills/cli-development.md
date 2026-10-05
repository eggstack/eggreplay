# CLI Development

Use when changing the command surface, flags, envelopes, or exit codes in
`crates/eggreplay-cli`. The CLI is a thin presentation and orchestration
layer: it owns Clap parsing, exit-code mapping, and report emission, and
nothing else. It must not contain HTTP logic, matching, or storage rules.

## Command inventory

Eleven subcommands (`crates/eggreplay-cli/src/main.rs:46-59`), of which `har`
and `ca` nest, plus the `intercept`-gated `proxy`:

| Command | Purpose |
|---|---|
| `record` | Gateway acquisition into a `.eggr` fixture |
| `serve` | Offline deterministic replay server (and `re-record` modes) |
| `replay` | Client replay + diff, reported but not asserted |
| `test` | The same report, enforced via exit code |
| `diff` | Compare two fixtures, fully offline (never opens a socket) |
| `inspect` | Bounded fixture inspection (`--bodies`, `--websockets`, `--sse`) |
| `validate` | Fixture validation only |
| `har import` / `har export` | Lossy HAR 1.2 interchange |
| `migrate` | On-disk session-schema upgrade |
| `proxy record` / `proxy validate` | Explicit proxy (needs `--features intercept`) |
| `ca init/import/inspect/export/rotate` | CA lifecycle (needs `--features intercept`) |

`validate` is easy to forget — it exists, and it is not the same as
`inspect --bodies`.

## Flag value names — get these exactly right

Clap `ValueEnum` renders kebab-case from the variant names, so the accepted
spellings are the ones below. Passing a plausible-but-wrong value is a hard
parse error (exit `2`), not a fallback.

| Flag | Accepted values | Default |
|---|---|---|
| `--output` | `human`, `json`, `junit` | `human` |
| `--route` | `direct`, or a pproxy URI (`socks5://…`, two-hop `socks5://…__http://…`) | `direct` |
| `--inbound` | `http1` (aliases `h1`), `http2` (aliases `h2`, `h2c`, `http2-cleartext`) | `http1` |
| `--outbound-version` | `auto`, `http1`, `http2` | `auto` |
| `--record-mode` | `sealed`, `once`, `append-new`, `re-record` | `sealed` |
| `--target-schema` | `1`, `2` | `2` |

Two traps that have shipped wrong in these docs before:

- **`h1`/`h2` are not `--outbound-version` values.** The values are `http1` and
  `http2`. `--outbound-version h1` fails to parse.
- **`--inbound h2-tls` is deliberately rejected** even though the name is
  *recognised*. `parse_protocol` returns a `TlsIdentity` error for
  `http2-tls`/`h2-tls` on purpose (`eggreplay-http/src/inbound.rs:448-452`):
  TLS serving requires operator certificate and key material, so you build the
  policy from `--inbound-tls-cert` + `--inbound-tls-key` rather than naming
  the protocol. Never document `h2-tls` as a usable `--inbound` choice.

`--output` is flattened onto **every** subcommand rather than declared global,
so it must follow the subcommand: `eggreplay validate --output json`, not
`eggreplay --output json validate`.

## `auto` means HTTP/1.1

Both `auto` and `http1` map to `HttpVersionPolicy::Http1Only`
(`main.rs:385`). This is deliberate: an upstream EggFetch release must not be
able to change the protocol of an existing invocation by negotiating more
eagerly. Opting into HTTP/2 is an explicit act.

Similarly `--timeout-secs` is **unset by default**. The per-phase `read` budget
is "time between response body chunks", so an origin that accepts a connection
and then says nothing is bounded only by `total` — which is why M016 had to
populate `total` explicitly. A new default deadline would change every existing
invocation, so the flag stays opt-in and the JSON result reports
`{"bounded": false, "total_secs": null}` when unset. A timed-out transaction is
recorded as a flow outcome, not a command error, so the command still exits `0`.

## Exit codes

```
0  success
1  regression/assertion mismatch  (regression | diff)
2  invalid CLI/config/policy      (configuration)
3  invalid/corrupt fixture       (fixture)
4  network/runtime execution     (runtime)
5  internal/unexpected           (catch-all)
```

`exit_code_for_class` (`main.rs:661-670`) maps the envelope's `failure_class`
string to the code. `5` is a defensive fallback — no current code path emits a
fifth class. Keep machine-readable results on **stdout** and operational
diagnostics on **stderr** as `{failure_class}: {message}`; human rendering is
terminal text and is explicitly not a parsing contract.

## JSON envelope

```rust
struct Envelope<T> { command, schema_version: u16, success, failure_class, warnings, payload }
```

`schema_version` is `1` and is a *different counter* from
`REPORT_SCHEMA_VERSION` (2) and from the session/flow schemas. Do not conflate
them. `warnings` is currently always empty — `emit` passes `Vec::new()` — so do
not build a contract on it appearing.

## Adding a flag

- Add it to the correct shared arg group so it behaves consistently
  (`ComparisonArgs` covers `replay`/`test`/`diff`; `InboundServingArgs` covers
  `record`/`serve`). Note that comparison flags such as
  `--websocket-cadence-tolerance-ms` exist on `replay`/`test`/`diff` only —
  **not** on `record`/`serve`, despite being a WebSocket-named option.
- Wire the same documentation into `docs/cli.md` and the relevant architecture
  deep dive in the same change.
- If it selects an opt-in capability, it must fail closed with exit `2` when the
  build lacks the feature, naming the feature. Never silently serve H1 under an
  H2 label.

## Architecture References

- [`architecture/09-cli-surface.md`](../architecture/09-cli-surface.md) — full
  command/flag inventory, shared arg groups, envelope and exit-code emission.
- [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md)
  — CLI feature flags and which ones are default (none are).
- [`docs/cli.md`](../docs/cli.md) — the operator-facing contract.
