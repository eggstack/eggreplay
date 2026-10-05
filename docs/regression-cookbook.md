# Regression cookbook

Three commands produce comparison reports. They differ in **what they compare**
and **what they do with a difference**.

| Command | Compares | On a difference | Opens a socket |
|---|---|---|---|
| `replay` | recorded baseline vs a **live target** | reports, exits `0` | yes |
| `test` | recorded baseline vs a **live target** | exits `1` | yes |
| `diff` | **two fixtures** | exits `1` | **never** |

`replay` and `test` produce the same typed report; only `test` enforces it, so
`replay` is the debugging tool and `test` is the CI gate. `diff` is for offline
"did this change?" questions — two recordings, no network.

## Live target

```sh
eggreplay replay --fixture demo.eggr --target https://api.example.com --output json
eggreplay test   --fixture demo.eggr --target https://api.example.com --output junit
```

The target's **path, query, and headers come from the recorded flow** — the
target argument supplies the origin to send them to. The candidate is recorded
with the baseline request verbatim, so nothing can silently rewrite what is
being compared.

## Two fixtures, offline

```sh
eggreplay diff --baseline before.eggr --candidate after.eggr --output json
```

The flags are `--baseline` and `--candidate`; there is no `--fixture`/`--other`
form. `diff` never opens a socket, so it is safe to run anywhere.

## What is compared

By default, ordinary status, selected headers and trailers, body digest and
length, and outcome category/phase. Report findings are typed with a `kind`
(`status`, `header`, `body`, `outcome`, `stream`, `sse`, `websocket`, …) and a
`field` naming the exact location, so a finding is actionable rather than just
"differs".

Optional dimensions are **off by default** and must be requested, because they
need fixture support that many recordings lack:

| Flag | Requires |
|---|---|
| `--compare-stream-events` | a `stream-events` extension |
| `--cadence-tolerance-ms <N>` | implies stream comparison; a request for it without support is an explicit `fixture` error, never a silent skip |
| `--compare-sse` | `text/event-stream` bodies |
| `--sse-ignore <FIELDS>` | repeatable; `data,event,id,retry,comments`; implies SSE comparison |
| `--websocket-cadence-tolerance-ms <N>` | a `websockets.jsonl` extension |

## The `date` finding

A regression against a live origin will normally report one finding:

```
Header response.headers.date baseline=<present> candidate=<present>
```

`Date` is origin-generated and second-granular, so it differs on every run.
This is expected, not a misconfiguration. The `practical` matcher profile
ignores `date` when **matching** a flow, but the regression **comparison**
authority has no header-ignore option, so it surfaces here. Comparison-level
normalization is not implemented; until it is, treat `date` as noise and read
the other findings.

## Schedulers

`--scheduler` chooses how candidate requests are issued:

- `sequential` (default) — one at a time, deterministic, simplest to debug
- `timeline` — issues candidates in recorded order using the fixture's stream
  event metadata, so a request is only sent once its recorded predecessor has
  finished. Requires that metadata; without it the command is an explicit
  error, never a silent downgrade to sequential.

`--max-concurrency` bounds concurrent requests under `timeline` scheduling
(default `8`). The scheduler actually used is recorded in the report, so a diff
is reproducible.

## Timeouts

`--timeout-secs` is **unset by default**, so no existing invocation's behaviour
changed when it was introduced. When set, every phase is bounded *and* `total`
is populated — `total` matters most, because the per-phase `read` budget means
"time since the last body chunk" and an origin that accepts a request and then
says nothing is bounded only by `total`.

A timed-out transaction is recorded as a flow **outcome**, not a command error,
so the command still exits `0` and the timeout appears as a finding.

## Routing

`--route` is accepted on the network-capable commands and takes `direct`
(default) or an Eggress pproxy URI. A configured route never falls back to
direct: an unreachable route fails the request and the failure is classified
(`connection_refused`, `unreachable`, …) rather than collapsing to `other`.

## Reading a report

`schema_version` on the report is `2` and is separate from the CLI envelope's
`schema_version` (1) and from the session/flow schemas. Each report carries its
`schema_version`, the `scheduler` used, and per-flow `findings` with typed
`kind` and `field`, plus the `baseline_flow_ids` each finding came from.
