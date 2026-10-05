# Interception

Use when working in `crates/eggreplay-intercept`, or on the explicit-proxy /
CA / HTTPS-MITM surface. All of it is behind the CLI's `intercept` feature.

## Opt-in discipline

`eggreplay-intercept` is a **leaf capability**. No product crate and no Python
build may depend on it, and CI proves it in the `dependency-boundary`,
`protocol-boundary`, and `interception` lanes by asserting that
`eggreplay-core`, `-store`, `-http`, `-cli`, and `-python` never pull
`eggreplay-intercept` or `rcgen`.

It is also **H1-only forever**. Interception never adopts the multiprotocol
serving layer: it depends on `eggserve-server` alone, never `eggserve-core`, and
its manifest must not mention `h2-inbound`. This is why HTTP/2 interception
cannot silently appear.

Note that `eggreplay-intercept` declares its transport dependencies
**directly and non-optionally** (`eggserve-primitives`, `eggserve-server`,
`eggnet-tls`, `eggress-outbound` with `pproxy-compat`, `rustls`, `tokio-rustls`,
`rcgen`) rather than inheriting `eggreplay-http`'s optionality gate. That is
intentional — it is a separate acquisition entry point — but it means the
dependency-boundary assertions matter more here, not less.

## What it owns

- **Explicit HTTP/1.1 proxy** in absolute-form, plus CONNECT policy with three
  outcomes: deny, opaque passthrough tunnel, and policy-gated MITM.
- **CA lifecycle**: manual initialize, import, inspect, export, rotate.
- **Leaf issuance** for intercepted hosts, signed by that CA.
- **MITM recording** into the same `RecordingSession` as ordinary recording.

## What it does not own

Transport stays delegated, exactly as elsewhere: EggServe owns the inbound H1
runtime and the tunnel handoff, EggFetch owns the upstream client and TLS
verification, Eggress owns raw route establishment for CONNECT. The interception
crate composes them; it does not reimplement a proxy, a TLS stack, or an HTTP
parser.

`OriginOrAbsolute` is opted into only for the interception helper, and even then
EggServe keeps its own bounds and there is exactly one tunnel-admission
authority. Ordinary record/replay gateways stay `OriginOnly`.

TLS termination is **caller-owned**: EggReplay does not own the certificate the
gateway presents for a given host. A CA peer that claims `:scheme: http` is
refused with 400 before the matcher runs.

## Security posture

- **Loopback-first.** Binding a non-loopback listener requires an explicit
  opt-in flag. The proxy is a privileged acquisition surface; do not make it
  convenient to expose.
- **Never mint trust automatically.** There is no OS/browser trust
  installation. CA material is an explicit operator action, and key material
  must not appear in diagnostics, reports, or `Debug` output.
- **Policy is data.** `--policy-file` is parsed and validated
  (`policy_file.rs`); host allow/deny decisions come from typed rules, not from
  parsing display strings.
- **Bounded everything**: tunnel count, connection count, body bytes, leaf
  validity, and per-tunnel byte/duration/idle/connect timeouts. A tunnel that
  exceeds its bound is closed, not left to run.
- **Secrets never reach the fixture unredacted**, and the recorded physical
  route is redacted-safe by construction.
- Hardening suites that matter when touching this crate: `hardening.rs`
  (secret audit), `resource_bounds.rs` (limits), `curl_interop.rs` (external
  peer interop; skips where `curl` is absent, never fails), and
  `tls_shutdown_isolation.rs` (one connection's TLS shutdown must not poison
  its siblings).

## Support matrix

| Capability | Status |
|---|---|
| Explicit HTTP/1.1 proxy absolute-form recording | supported |
| CONNECT deny / opaque passthrough | supported |
| HTTPS MITM HTTP/1.1 recording | supported, opt-in, policy-gated |
| Direct + narrow Eggress-routed upstream | supported |
| Manual CA init/import/inspect/export/rotate | supported |
| HTTP/2 MITM | unsupported |
| WSS interception | unsupported |
| Client mTLS interception | unsupported |
| HTTP/3 / QUIC interception | deferred (ADR 0009) |
| Automatic OS/browser trust install | unsupported |
| Transparent / TUN interception | unsupported |
| Certificate-pinned clients | expected to fail unless configured for passthrough |

No matrix row may expand without corresponding tests. Changing a row means
updating `README.md`, `docs/interception-threat-model.md`, and the support
table in `architecture/08-interception.md` together.

## Architecture References

- [`architecture/08-interception.md`](../architecture/08-interception.md) —
  proxy, CONNECT policy, header filtering, tunnel, CA, leaf, MITM, and the
  support matrix.
- [`docs/interception-threat-model.md`](../docs/interception-threat-model.md) —
  the threat model the substrate was qualified against.
- [`docs/interception-ca-trust.md`](../docs/interception-ca-trust.md) — CA
  lifecycle and trust operations.
- [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md)
  — where the opt-in boundary is enforced in CI.
