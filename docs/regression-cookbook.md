# Regression cookbook

Three commands produce comparison reports. They differ in what they compare and
what they do with a difference.

| Command | Compares | On a difference | Opens a socket |
|---|---|---|---|
| `replay` | A recorded baseline and a live target | Reports findings, exits `0` | Yes |
| `test` | A recorded baseline and a live target | Exits `1` | Yes |
| `diff` | Two fixtures | Exits `1` | No |

`replay` and `test` produce the same report. Use `replay` to inspect a candidate
and `test` as a CI gate. Use `diff` to compare two recordings offline.

## Compare with a live target

```sh
eggreplay replay --fixture demo.eggr --target https://api.example.com --output json
eggreplay test --fixture demo.eggr --target https://api.example.com --output junit
```

The target supplies the origin. The recorded flow supplies the path, query, and
request headers. The candidate request is kept separate from the baseline, so
target remapping does not change what the report compares.

## Compare two fixtures

```sh
eggreplay diff --baseline before.eggr --candidate after.eggr --output json
```

Use existing fixture paths. The flags are `--baseline` and `--candidate`;
`diff` never opens a socket.

## What is compared

By default, the report compares status, selected headers and trailers, body
digest and length, and outcome category and phase. Each finding has a typed
`kind` and a `field` naming its location.

The default comparison policy treats `Date` as volatile. When the header is
present on both sides, its comparison is suppressed and the report records the
suppression in `suppressed`. If it is missing on either side, the presence
difference remains a finding. This policy is separate from the `practical`
matcher profile.

Optional dimensions are off by default and require supporting fixture metadata:

| Flag | Requirement |
|---|---|
| `--compare-stream-events` | A `stream-events` extension |
| `--cadence-tolerance-ms <N>` | Implies stream comparison and requires stream metadata |
| `--compare-sse` | `text/event-stream` bodies |
| `--sse-ignore <FIELDS>` | Repeatable fields: `data,event,id,retry,comments`; implies SSE comparison |
| `--websocket-cadence-tolerance-ms <N>` | A `websockets.jsonl` extension |

A requested comparison without required metadata fails explicitly. It never
silently skips the requested dimension.

## Schedulers

`--scheduler` selects how candidate requests are issued:

- `sequential` is the default. It sends one request at a time and is easiest to
  debug.
- `timeline` starts candidates at recorded offsets. It requires valid stream
  metadata and respects fixture order for equal offsets. Missing metadata is an
  error, not a fallback to sequential scheduling.

`--max-concurrency` bounds active requests with timeline scheduling. Its default
is `8`. The report records the scheduler used.

## Time bounds

`--timeout-secs` bounds each outbound transaction, including its total duration.
It is unset by default. A timeout is recorded as a flow outcome, so the command
still produces a report.

`--max-elapsed-ms <N>` is a separate comparison assertion. It checks whether a
completed candidate flow stayed within the given duration and reports a timing
finding if it did not. It is independent of `--timing-mode`, which controls
fixture playback pacing.

## Report versions

The comparison report schema is version `3`. It includes `findings` and the
machine-readable `suppressed` list. The CLI JSON envelope has its own
`schema_version` of `1`; fixture session and flow schemas are separate again.

See [CLI and exit contracts](cli.md) for flags and output details, and
[Configuration](configuration.md) for matcher and recording settings.
