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
M014-C1 post-M014 WebSocket finalization repair is implemented on `b188c552`
but remains qualification-pending after Actions run `36881596131` failed an
existing HAR stable-Clippy lint before the full verify matrix completed.
M014-C2 is the sole ready corrective handoff for requalification and closure
reconciliation. No new feature stage is currently authorized.

The support baseline includes direct HTTP/1.1 acquisition and EggServe inbound
HTTP/1.1 replay with optional listener-free Eggress routing. WebSocket support
covers RFC 6455 over cleartext HTTP/1.1 Upgrade (`ws://`) with bounded semantic
text, binary, ping, pong, and close messages. WSS interception, H2/H3,
negotiated extensions, and wire-frame fidelity remain outside the claim.

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

## HTTP/2 / HTTP/3 support matrix (M014B / M014C)

HTTP/2 record and regression-candidate execution against EggFetch ALPN `h2`
over local TLS (and routed through an Eggress TCP path) is a qualified,
**experimental** opt-in tier under the `eggreplay-http/h2` cargo feature with
an explicit `HttpVersionPolicy`. H1 remains the default policy everywhere.
The following are not qualified and remain outside the support claim:

- HTTP/2 inbound serving (EggServe replay/gateway) — no adopted seam on
  `eggserve-server 0.3.0`;
- HTTP/2 interception (MITM);
- cleartext prior-knowledge (`h2c`);
- HTTP/3 / QUIC (direct, routed, replay, and intercept) — deferred per
  ADR 0009 with documented missing seams.

See `plans/closure/m014b-http2-qualification.md` for the experimental-tier
evidence and `plans/closure/m014c-http3-feasibility-and-qualification.md`
plus `plans/adrs/0009-http3-integration-boundary.md` for the H3 deferral.

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
