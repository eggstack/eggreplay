# M013 — Explicit Proxy Acquisition and Optional HTTPS Interception

Status: ready
Depends on: M012
Roadmap stage: 9

## Objective

Add a separate, opt-in acquisition adapter for clients that cannot be
instrumented through EggFetch directly: explicit HTTP proxying and optional
HTTPS MITM. Interception must remain outside the default EggReplay trust and
dependency boundary.

## Crate/feature boundary

Create a separate `eggreplay-intercept` crate (or equivalently strict optional
feature crate boundary approved by an ADR). Default EggReplay builds must not
pull CA generation, server TLS interception, or interception key-management
dependencies.

Core/store/ordinary HTTP recording behavior remains unchanged.

## Phase 1 — explicit HTTP proxy acquisition

Implement standard explicit HTTP proxy request handling using EggServe's
generic server/tunnel surfaces:

- absolute-form HTTP requests are converted to the canonical logical request
  and forwarded through EggFetch;
- CONNECT is policy-controlled;
- non-intercepted CONNECT can be denied or tunneled, but tunnel-only traffic is
  not falsely recorded as semantic HTTP;
- outbound routing may still use Eggress.

Do not reimplement HTTP framing.

Add allow/deny host/port policies before any CONNECT or interception action.

## Phase 2 — CA lifecycle

Provide explicit CLI/library operations for a dedicated interception CA:

- initialize/import a CA in a caller-selected directory;
- strict file permissions where supported;
- print/export the public certificate separately from the private key;
- inspect fingerprint/expiry;
- rotate by creating a new identity, never silently replacing an existing key.

Do not automatically install trust into OS/browser stores in v1 of this
feature. Document platform trust installation separately.

CA/private-key material never lives inside `.eggr` fixtures, logs, JSON
reports, or crash diagnostics.

Use a maintained certificate-generation crate and rustls-compatible key types;
do not implement X.509 generation manually.

## Phase 3 — HTTPS MITM

For an allowed CONNECT target:

1. validate/connect policy and authority;
2. accept CONNECT through EggServe's tunnel capability;
3. wrap the client-side tunnel in rustls using a leaf certificate minted from
   the dedicated CA;
4. serve the decrypted HTTP stream through EggServe's caller-owned connection
   runtime;
5. forward semantic requests through EggFetch using normal upstream TLS
   verification;
6. record through the same C002/C003 recorder.

Reuse `eggnet-tls`/EggServe neutral TLS identity primitives where they fit.
If EggServe lacks a public caller-owned TLS->HTTP handoff needed here, document
and upstream the smallest generic seam rather than embedding another Hyper
server in EggReplay.

## ALPN/protocol policy

Initial MITM support is HTTP/1.1 only. Advertise only `http/1.1` to the
intercepted client until M014 separately qualifies H2 interception.

If a client requires H2-only/QUIC/pinning, fail explicitly or use configured
passthrough; never claim semantic capture.

HTTP/3/QUIC interception is out of scope.

## Certificate policy

Leaf certificates are generated only for validated DNS/IP targets from CONNECT
authority/SNI policy. Cache leaf material with bounded count/lifetime.

Document:

- certificate pinning failures;
- mTLS/client-certificate limitations;
- browser/app trust requirements;
- security implications of installing the CA;
- no guarantee for non-HTTP TLS protocols.

## Security controls

- interception disabled by default;
- explicit allowlist recommended and supported;
- private-key paths/contents redacted from diagnostics;
- no remote control endpoint for CA export;
- restrictive permissions;
- bounded certificate generation/cache;
- no wildcard leaf generation unless explicitly justified;
- fail closed on malformed CONNECT/SNI mismatch according to documented policy.

## Tests

Use a local generated CA and local TLS origin. Cover HTTP proxy absolute-form,
CONNECT deny/tunnel, successful MITM HTTP/1.1, upstream TLS verification
failure, untrusted client behavior, SNI/CONNECT mismatch, redaction, CA
permissions, key non-persistence in fixtures, Eggress-routed upstream, shutdown
with active tunnel, and certificate-cache bounds.

No public Internet or system trust-store mutation in routine tests.

## Closure

Create `plans/closure/m013-explicit-proxy-and-optional-mitm.md`. Record the
exact supported protocol matrix and threat model. M014 compatibility expansion
remains blocked until interception's protocol claims are explicit.
