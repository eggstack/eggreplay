# Quickstart

This guide records an HTTP request, validates the fixture, replays it offline,
and shows how to compare against a live service. It assumes the `eggreplay` CLI
is installed, plus Python 3 and `curl` for the local example.

JSON and JUnit results go to stdout. Operational logs and diagnostics go to
stderr.

## Start an origin and record a request

In terminal 1, create a response file, start a local origin in the background,
and run the recorder. Keep using this shell so `ORIGIN_PID` is available for
cleanup.

```sh
mkdir -p eggr-quickstart
cd eggr-quickstart
printf '%s\n' '{"greeting":"hello"}' > api.json
python3 -m http.server 9100 --bind 127.0.0.1 &
ORIGIN_PID=$!
eggreplay record --listen 127.0.0.1:8080 \
  --upstream http://127.0.0.1:9100 --fixture demo.eggr
```

While the recorder runs, use terminal 2 to send a request through the gateway:

```sh
curl http://127.0.0.1:8080/api.json
```

Press `Ctrl-C` in terminal 1 to finalize the fixture. Recording publishes a
fixture only after a clean shutdown. An interrupted recording leaves an
`.incomplete-*` staging directory instead of a completed fixture. Existing
fixtures are never replaced unless you pass `--overwrite`.

A `.eggr` fixture is a directory containing a manifest, flow records,
content-addressed body blobs, and any required extensions.

## Validate and inspect

```sh
eggreplay validate --fixture demo.eggr --output json
eggreplay inspect --fixture demo.eggr --output json
```

`validate` checks fixture integrity without reading body blobs. `inspect` shows
metadata. Add `--bodies` to read a bounded amount of body data or `--websockets`
to inspect conversation metadata. Inspection never prints WebSocket message
bytes. See the [fixture schema guide](eggr-schema.md) for the on-disk format.

## Replay offline

Stop the origin before starting the replay server. In terminal 1:

```sh
kill "$ORIGIN_PID"
wait "$ORIGIN_PID" 2>/dev/null || true
eggreplay serve --fixture demo.eggr --listen 127.0.0.1:9100
```

In terminal 2:

```sh
curl http://127.0.0.1:9100/api.json
```

The response comes from the fixture. Press `Ctrl-C` in terminal 1 to stop the
server.

A gateway recording stores the upstream authority it reached, so replay must
use `127.0.0.1:9100` here, not the recorder's `8080` address. A request with a
different `Host` does not match. Flows are consumed once by default, so a
second identical request returns `409`.

## Compare against a live service

Start a candidate service at the recorded authority, then run `test` or
`replay`:

```sh
eggreplay test --fixture demo.eggr \
  --target http://127.0.0.1:9100 --output junit
```

`test` exits `1` when there is a regression. `replay` produces the same typed
report but exits `0` even when it contains findings. The default comparison
suppresses `Date` values when the header is present on both sides and records
the suppression in the report's `suppressed` list. If the header is missing on
either side, its presence difference remains a finding.

To compare two existing fixtures without a network request, use `diff`:

```sh
eggreplay diff --baseline baseline.eggr --candidate candidate.eggr --output json
```

Replace the sample fixture names with paths that exist. `diff` never opens a
socket. See the [regression cookbook](regression-cookbook.md) for schedulers,
comparison options, and report fields.

## Redaction and next steps

Recording can redact selected headers, query keys, and JSON/form fields before
publishing any body bytes. Redacted request fields become matcher wildcards.
Opaque binary data cannot be semantically redacted, so fixtures remain
sensitive. See [configuration](configuration.md) for redaction options and
limits.

| Topic | Guide |
|---|---|
| Every command and flag | [CLI and exit contracts](cli.md) |
| Matcher profiles and recording modes | [Configuration](configuration.md) and [matcher profiles](matcher-profiles.md) |
| HAR import/export | [HAR interchange](har-interchange.md) |
| HTTP/2 and gRPC support | [HTTP/2 support](http2-support.md) and [gRPC views](grpc-and-faults.md) |
| Routes through Eggress | [Eggress routing](eggress-routing.md) |
| Proxy, CONNECT, and CA | [Interception threat model](interception-threat-model.md) and [CA and trust](interception-ca-trust.md) |
| Deliberate exclusions | [Non-goals](non-goals.md) |
