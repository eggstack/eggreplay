# Quickstart

Every command in this guide is verified against a real build. `eggreplay`
refuses rather than guesses: an invalid flag value exits `2` with a diagnostic,
so a command that runs at all is a command that meant what you typed.

Machine-readable results go to **stdout**; operational logs and
`{failure_class}: {message}` diagnostics go to **stderr**. JSON and JUnit are
result contracts; human rendering is terminal text and is not a parsing
contract.

## Set up a throwaway origin

The guide uses a static file server so it runs anywhere with no setup. Any real
service works identically.

```sh
mkdir qs && cd qs && echo '{"greeting":"hello"}' > api.json
python3 -m http.server 9100 --bind 127.0.0.1 &
```

## Record

Run the gateway, then point your client at the gateway rather than the origin.
Traffic passes through untouched, so you can record in development, in CI, or
against a shared environment.

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9100 --fixture demo.eggr
```

```sh
curl http://127.0.0.1:8080/api.json      # {"greeting":"hello"}
```

The recorder logs `recording on 127.0.0.1:8080 (inbound http1)` and then
`record: ok`. Press `Ctrl-C` to finalize. **A fixture is only published on a
clean shutdown** — an interrupted recording leaves a `.incomplete-*` staging
directory, never a fixture that looks complete.

Recording an existing fixture requires explicit `--overwrite`; there is no
silent replacement.

### What you get

A `.eggr` is a **directory**:

```
demo.eggr/
├── manifest.json          # written last; the publication marker
├── flows.jsonl            # one JSON flow per line
├── blobs/<sha256>         # content-addressed bodies, no extension
└── stream-events.json     # required extension (not an extensions/ dir)
```

## Inspect and validate

```sh
eggreplay validate --fixture demo.eggr --output json
```

```json
{"command":"validate","schema_version":1,"success":true,"failure_class":null,
 "warnings":[],"payload":{"fixture":"demo.eggr","flow_count":1,"schema_version":2}}
```

```sh
eggreplay inspect --fixture demo.eggr --output json              # metadata only
eggreplay inspect --fixture demo.eggr --bodies --output json     # reads blobs
eggreplay inspect --fixture demo.eggr --websockets --output json # conversations
```

`validate` checks fixture integrity without reading bodies; `inspect --bodies`
reads blobs. `inspect` never prints WebSocket message bytes — it reports kind,
direction, timing, and a digest instead.

## Replay offline

Stop the origin first, so there is no doubt about where the response came from:

```sh
# the http.server from the setup step is no longer running
eggreplay serve --fixture demo.eggr --listen 127.0.0.1:9100
```

```sh
curl http://127.0.0.1:9100/api.json      # {"greeting":"hello"} — offline
```

### Two things that will surprise you

**Serve on the recorded authority.** A gateway recording stores the *upstream*
authority it reached — `127.0.0.1:9100` above, not the gateway's `8080`. Replay
is origin-only, so a request whose `Host` does not match gets a clean `404`
`eggreplay replay no match` rather than a relaxed match. Either serve on the
recorded authority (what this guide does), or send the right `Host`:

```sh
curl -H 'Host: 127.0.0.1:9100' http://127.0.0.1:8080/api.json
```

**Flows are consumed once.** Replay is a match-and-consume model, not a static
site mirror. A second identical request gets `409`. A flow that *matches* but
cannot be consumed is reported as a **near-miss** — never a silent repeat and
never a silent skip.

`serve --record-mode` also supports `once`, `append-new`, and `re-record`, which
forward to an upstream and fold the new traffic into the fixture. Those are
network-capable, and `--route` is only accepted with them.

## Regress a live service

`replay` and `test` produce the same typed report; only `test` enforces it.

```sh
eggreplay replay --fixture demo.eggr --target http://127.0.0.1:9100 --output json
eggreplay test   --fixture demo.eggr --target http://127.0.0.1:9100 --output junit
```

`replay` reports and always exits `0`. `test` exits `1` on a difference, so it
drops straight into CI:

```xml
<testsuite name="test" tests="1" failures="1" errors="0">
  <testcase name="flow-…" classname="test">
    <failure message="mismatch">Header response.headers.date …</failure>
  </testcase>
</testsuite>
```

**Expect the `date` finding.** `Date` is origin-generated and second-granular,
so it differs on every run. The `practical` matcher profile ignores `date` when
*matching*, but regression *comparison* has no header-ignore option, so it
surfaces here. This is a known limitation, not a misconfiguration.

Opt into finer comparison only when the fixture supports it:

```sh
--compare-stream-events --cadence-tolerance-ms 250
--compare-sse --sse-ignore id,retry
```

### Compare two fixtures, no network

```sh
eggreplay diff --baseline before.eggr --candidate after.eggr --output json
```

`diff` never opens a socket. Note the flags are `--baseline` and
`--candidate`, not `--fixture`/`--other`.

## Redact secrets at record time

Redaction runs **before** any body is finalized, so a secret is never written
and then rewritten.

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream https://api.example.com \
  --fixture clean.eggr \
  --redact-header authorization \
  --redact-query token \
  --redact-json-path /user/email
```

A redacted request field becomes a **matcher wildcard**, not a literal
`<redacted>` placeholder — so the recording still matches whatever the live
request sends in that position. Opaque binary payloads cannot be semantically
redacted; fixtures remain sensitive data, so restrict access to fixture
directories. See [`./non-goals.md`](./non-goals.md) for the full limit list.

## Import and export HAR

```sh
eggreplay har export --fixture demo.eggr --har demo.har --overwrite --loss-report loss.json
eggreplay har import --har demo.har --fixture imported.eggr --overwrite
```

HAR is a **lossy interchange in both directions** and never implies
round-trip losslessness. `--loss-report` is how you find out what did not
survive. See [`./har-interchange.md`](./har-interchange.md) for the matrices.

## Upgrade a fixture's schema

```sh
eggreplay migrate --fixture old.eggr --to new.eggr --target-schema 2
eggreplay migrate --fixture old.eggr --in-place            # atomic replace
```

`--to <DIR>` and `--in-place` are mutually exclusive. Migration stages a
sibling, validates it, then publishes — the source is never mutated before the
replacement validates.

## Route outbound through Eggress

Direct is always the default. An explicitly configured route never silently
falls back to direct:

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream https://api.example.com \
  --fixture demo.eggr --route socks5://127.0.0.1:1080
```

If the route is unreachable, the request fails (`502`) and the failure is
classified — it does **not** quietly bypass the proxy. EggFetch keeps ownership
of logical Host/SNI, TLS, framing, and pooling; the dialer only moves raw TCP
bytes.

## Exit codes

```
0  success
1  regression or diff mismatch
2  invalid CLI input, configuration, or policy
3  invalid or corrupt fixture
4  network or runtime execution failure
5  internal error
```

## Where to go next

| Topic | Guide |
|---|---|
| Every flag and envelope field | [`./cli.md`](./cli.md) |
| Matcher profiles, timing, record modes | [`./configuration.md`](./configuration.md) |
| `replay` vs `test` vs `diff`, schedulers | [`./regression-cookbook.md`](./regression-cookbook.md) |
| On-disk format and extension registry | [`./eggr-schema.md`](./eggr-schema.md) |
| HTTP/2, gRPC, WebSocket, Eggress tiers | [`./http2-support.md`](./http2-support.md) |
| Proxy, CONNECT policy, CA, MITM | [`./interception-threat-model.md`](./interception-threat-model.md) |
| What is deliberately unsupported | [`./non-goals.md`](./non-goals.md) |
