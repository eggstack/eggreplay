# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

v0.1 is fully qualified through C001–C006. M009 stateful/dynamic replay, M010
streaming timing/SSE, and M011 semantic WebSocket record/replay/regression are
closed with hosted cross-platform evidence.

M012 Python/pytest integration is closed under ADR 0007 with hosted
Rust/Python and wheel qualification. M013 explicit-proxy/optional HTTPS
validation is closed under ADR 0008 on qualifying revision `5efc6f9`
(Actions runs [36211265347](https://github.com/eggstack/eggreplay/actions/runs/36211265347)
on the implementation SHA and
[36456906216](https://github.com/eggstack/eggreplay/actions/runs/36456906216)
on the closure commit); M013A substrate/dependency/threat preflight,
M013B0 EggServe 0.3 adoption, M013B proxy/CONNECT policy, M013C CA/leaf
lifecycle, M013D HTTPS MITM recording, M013E CLI/policy/operator UX, and
M013F hardening/qualification are all closed. Explicit HTTP/1.1 proxying
and policy-gated CONNECT deny / tunnel / MITM recording are part of the
supported product baseline when the CLI is compiled with
`--features intercept`; default library and Python builds remain
interception-free. M014 is now the compatibility-stage umbrella. M014-R1 planning/documentation
reconciliation, M014A (HAR interchange/migration), M014B (experimental
outbound HTTP/2), M014C (HTTP/3 deferred), and M014D (gRPC views + bounded
faults) are closed; the M014 compatibility program is closed (see
`plans/closure/m014-compatibility-program.md` for the support matrix).
M014-C1 post-M014 WebSocket finalization repair and M014-C2 qualification/
closure reconciliation are closed on `cbc9257` (hosted CI run `36891564494`
plus wheel run `36891564581`). M014-R2 documentation reconciliation is also
closed. Stage 11 executed as M015 (bidirectional HTTP/2 and transport baseline):
M015A (published dependency set and the inbound-H2 boundary), M015B (inbound
HTTP/2 gateway and offline replay), M015C (end-to-end H2 semantic and
regression qualification), and M015D (gRPC over HTTP/2) are closed, and M015E
closes the stage with hardening and hosted qualification.

The support baseline includes direct HTTP/1.1 acquisition and EggServe inbound
HTTP/1.1 replay with optional listener-free Eggress routing. WebSocket support
covers cleartext RFC 6455 over HTTP/1.1 Upgrade (`ws://`) with bounded semantic
text, binary, ping, pong, and close messages (the qualified M011 baseline).
WSS interception remains unsupported. Outbound HTTP/2 record/regression is an
experimental opt-in tier under M014B. Inbound HTTP/2 serving and `h2c` are
qualified experimental opt-in tiers under M015B, reached only through the
`h2-inbound`/`h2-inbound-tls` features; H2 MITM remains unsupported. H3
remains unsupported/deferred per ADR 0009. Negotiated WebSocket extensions and
wire-frame fidelity remain outside the claim. See the HTTP/2 / HTTP/3 matrix
below.

## Interception support matrix (M013)

Explicit-proxy acquisition and optional HTTPS interception are separately
compiled (`--features intercept`), loopback-first, policy-gated, and outside
default library/Python builds. No matrix row may expand without
corresponding tests.

| Capability | M013 |
|---|---|
| Explicit HTTP/1.1 proxy absolute-form recording | supported |
| CONNECT deny | supported |
| CONNECT opaque passthrough | supported |
| HTTPS MITM HTTP/1.1 recording | supported, opt-in/policy-gated |
| Direct + narrow Eggress-routed upstream | supported |
| Manual CA initialize/import/export/rotate | supported |
| Automatic OS/browser trust installation | unsupported |
| HTTP/2 MITM | unsupported/not qualified |
| HTTP/3/QUIC interception | unsupported/deferred (ADR 0009) |
| WSS WebSocket interception | unsupported |
| client mTLS interception | unsupported |
| certificate-pinned clients | expected to fail unless configured passthrough |
| transparent/TUN interception | unsupported |

## HTTP/2 / HTTP/3 support matrix (M014B / M014C / M015)

| Capability | Tier | Opt-in feature |
|---|---|---|
| Outbound H2 record / regression | experimental | `eggreplay-http/h2` |
| Inbound H2 recording gateway (cleartext h2c) | experimental | `h2-inbound` |
| Inbound H2 recording gateway (ALPN TLS) | experimental | `h2-inbound-tls` |
| Inbound H2 offline replay | experimental | `h2-inbound`, `h2-inbound-tls` |
| H2 over an Eggress TCP route | experimental | `eggress` |
| gRPC over H2 (unary, server/client streaming, terminated bidi) | experimental | `grpc` |
| H1 direct / inbound replay | **default** | — |
| H2 interception (MITM) | unsupported | — |
| WSS, extended-CONNECT WebSockets | unsupported | — |
| HTTP/3 / QUIC | deferred (ADR 0009) | — |

"Experimental" means qualified against independent peers on local loopback and
opt-in behind a feature boundary — not "unverified". No HTTP/2 capability is a
default in any profile.

Outbound HTTP/2 record and regression-candidate execution uses EggFetch ALPN
`h2` over local TLS (and routed through an Eggress TCP path), under the
`eggreplay-http/h2` cargo feature with an explicit `HttpVersionPolicy`.

Stage 11 (M015) qualifies **bidirectional** HTTP/2. Inbound HTTP/2 serving —
the recording gateway and the offline replay server — is a qualified,
**experimental**, opt-in tier on an HTTP/2 seam EggServe adopted. There is one
service and two runtimes: the same matcher, store, redaction, scenarios, and
renderer serve both protocols, so the protocol is a listener property rather
than a second code path. H1 remains the default and the only
multiprotocol-free profile.

`serve` and `record` take an explicit `--inbound` policy. `http1` (alias `h1`) is
the default; `http2` (aliases `h2`, `h2c`) is an explicit, supported,
opt-in prior-knowledge policy — a client selects HTTP/2 by speaking the 24-byte
preface, so it is never an accidental downgrade. TLS is enabled by supplying
`--inbound-tls-cert` and `--inbound-tls-key`, not by naming a protocol:
`--inbound h2-tls` is a recognised name that is deliberately rejected. See
`plans/adrs/0010-inbound-http2-serving-boundary.md` and
[`docs/cli.md`](docs/cli.md) for the exact accepted values.

The following remain outside the support claim:

- HTTP/2 interception (MITM) — `eggreplay-intercept` never adopts the
  multiprotocol serving layer;
- HTTP/3 / QUIC (direct, routed, replay, and intercept) — deferred per
  ADR 0009 with documented missing seams;
- WSS and extended-CONNECT WebSockets;
- a generic reverse proxy;
- inbound HTTP/2 in any default, direct, H1, interception, or Python profile —
  it is opt-in only.

### gRPC over HTTP/2

gRPC-over-HTTP/2 is a qualified, **experimental** tier: unary, server
streaming, client streaming, and bidirectional calls are recorded, replayed,
and regressed as ordinary HTTP/2 traffic. There is no gRPC branch in
the matcher, the store, or the renderer — a gRPC call is an HTTP/2 request with
a `content-type` and a body. The gRPC view is a caller-side **derived
projection** over the recorded raw body and trailers, which stay authoritative.

Descriptor sets are **caller supplied**. Nothing in the product fetches or
resolves one, and there is no reflection or network descriptor lookup. A
supplied descriptor is bounded, and a malformed one fails as a derived-view
error without touching the fixture.

A bidirectional call whose client half never closes is also qualified (M017).
The gateway is full-duplex, so replies do reach the client, but such a call
never produces a terminal `grpc-status` — and that *missing* status is the
signal, recorded alongside the classified reason the stream stopped. Replay
never presents such a call as successful; a client observes a failure, and the
specific code it sees differs between live and replayed paths, which is
documented rather than smoothed over.

See `plans/closure/m015b-inbound-http2-gateway-and-replay.md` for the inbound
seam, `plans/closure/m015c-http2-end-to-end-semantic-and-regression-qualification.md`
for the end-to-end matrix and support tiers, and
`plans/closure/m015d-grpc-over-http2-integration-qualification.md` for the
gRPC qualification. The outbound experimental-tier evidence is
`plans/closure/m014b-http2-qualification.md`; the H3 deferral is
`plans/closure/m014c-http3-feasibility-and-qualification.md` plus
`plans/adrs/0009-http3-integration-boundary.md`.

## Quickstart routes

```sh
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9000 --fixture demo.eggr --route direct
eggreplay replay --fixture demo.eggr --target http://127.0.0.1:9000 --route direct --output json
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9000 --route socks5://127.0.0.1:1080 --output junit
```

`direct` is the default; non-direct values use listener-free Eggress routing
via the `pproxy-compat` grammar only.

## Python quickstart

The Python adapter builds from this repository and uses the same Rust fixture,
transport, matching, and regression authorities as the CLI:

```sh
cd crates/eggreplay-python
uv sync --extra dev
maturin develop
python -c 'import eggreplay; print(eggreplay.__version__)'
```

Python exposes `.eggr` directory fixtures and managed replay/recording
lifecycles. The qualified default wheel intentionally does not include the
M013 interception/CA feature (which lives in the `--features intercept`
CLI build only). See
[`crates/eggreplay-python/README.md`](crates/eggreplay-python/README.md) for
pytest fixtures, explicit record modes, and migration notes for VCR.py users.
It is not a drop-in VCR.py replacement.

## Development

Rust 1.89 is the minimum supported version. Run the verification command from
[`AGENTS.md`](AGENTS.md) before submitting changes. See
[`plans/registry.md`](plans/registry.md) for the current execution gate.

Start with [`architecture/overview.md`](architecture/overview.md) for a
bird's-eye map of the crates, transport ownership, and capability tiers; it
indexes a per-component deep dive for each one.
