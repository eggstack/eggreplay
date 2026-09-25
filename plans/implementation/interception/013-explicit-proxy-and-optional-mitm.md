# M013 — Explicit Proxy Acquisition and Optional HTTPS Interception

Status: ready (decomposed; M013A and M013B0 closed, execute M013B next)
Depends on: M012 closure
Roadmap stage: 9
Architecture: ADR 0008

## Objective

Add a separately compiled, opt-in acquisition adapter for clients that cannot
be instrumented through EggFetch directly:

- explicit HTTP/1.1 forward-proxy recording;
- policy-controlled CONNECT deny/tunnel;
- dedicated CA lifecycle;
- optional policy-controlled HTTPS MITM for HTTP/1.1 only.

Interception must not become ambient behavior or a dependency of default
EggReplay library/Python builds.

## Execution decomposition

| ID | Plan | Result |
|---|---|---|
| M013A | `013a-substrate-dependency-and-threat-preflight.md` | crate/dependency/TLS-stream substrate + threat model |
| M013B0 | `013b0-eggserve-0-3-adoption-and-absolute-form-qualification.md` | adopt/qualify EggServe 0.3 + absolute-form seam |
| M013B | `013b-explicit-http-proxy-and-connect-policy.md` | absolute-form HTTP proxy + CONNECT deny/tunnel |
| M013C | `013c-ca-lifecycle-and-leaf-issuance.md` | CA/key lifecycle + bounded exact-host leaf issuance |
| M013D | `013d-https-mitm-http1-recording.md` | policy-gated HTTPS MITM recording |
| M013E | `013e-cli-policy-and-operator-experience.md` | optional CLI, policy files, trust/operator UX |
| M013F | `013f-hardening-qualification-and-closure.md` | security/resource/interoperability qualification + closure |

M013A and M013B0 are closed. M013B is the sole ready task; M013C and later
subplans remain blocked by their declared dependencies.

## Ownership boundary

ADR 0008 is binding:

- `eggreplay-intercept` is a new leaf crate;
- EggServe owns inbound H1 parsing/lifecycle and decrypted-stream H1 execution;
- Eggress owns raw CONNECT route establishment;
- EggFetch owns semantic upstream HTTP/TLS verification;
- `eggnet-tls` may provide published neutral PEM/server-config helpers;
- maintained rcgen/rustls tooling owns X.509/TLS mechanics;
- EggReplay owns proxy target policy, CA lifecycle, leaf cache, authority
  coherence, recording integration, and CLI orchestration.

Do not add another Hyper client/server, proxy protocol stack, TLS verifier, or
hand-built X.509 implementation.

## Security posture

- loopback listener by default;
- non-loopback bind requires explicit remote-listener opt-in and ingress policy;
- every target is policy-checked before route establishment/certificate issue;
- CONNECT action is exactly deny/tunnel/intercept;
- passthrough tunnels are opaque and never represented as semantic HTTP;
- MITM is explicit and per-policy, not global merely because compiled;
- no automatic OS/browser trust-store installation;
- CA/private leaf keys never enter fixtures/logs/reports;
- upstream TLS verification remains normal EggFetch verification;
- strict CONNECT/SNI/Host authority coherence prevents cross-origin tunnel reuse.

## Initial support target

M013 targets HTTP/1.1 only.

Not claimed:

- H2 MITM / Extended CONNECT;
- H3/QUIC interception;
- WSS interception;
- arbitrary non-HTTP TLS;
- client mTLS interception;
- transparent/TUN interception;
- certificate-pinned applications;
- automatic trust installation.

## Python boundary

The default `eggreplay` abi3 Python wheel remains interception-free.
M013 does not pull CA-generation dependencies into that wheel. A future
separately qualified Python distribution/feature strategy may expose
interception without weakening this default boundary.

## Closure

M013 closes only through M013F after one qualifying hosted revision proves the
explicit-proxy/MITM support matrix, key non-leakage, upstream TLS verification,
resource bounds, and platform behavior.

Final closure:
`plans/closure/m013-explicit-proxy-and-optional-mitm.md`.

M014 and its currently M013-dependent tracks remain blocked until M013 closes.
