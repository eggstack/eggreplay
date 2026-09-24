# ADR 0008 — Interception Security and Transport Boundary

Status: accepted

## Context

M013 adds an acquisition path for clients that cannot use EggFetch directly:
an explicit HTTP proxy and optional HTTPS interception. This introduces a local
forward-proxy boundary, a certificate-authority private key, dynamically minted
leaf certificates, and decrypted client traffic.

Those responsibilities are materially more privileged than EggReplay's
ordinary fixture/replay path. They must not become implicit dependencies or
ambient behavior of the default library/Python package.

## Decision

Create a separate `eggreplay-intercept` crate.

The crate is a leaf/orchestration adapter over existing Eggstack authorities:

- EggServe owns inbound HTTP/1 parsing, lifecycle, tunnel handoff, and the
  caller-owned `serve_http1_connection` driver used after TLS termination;
- Eggress owns raw outbound route establishment for CONNECT passthrough;
- EggFetch owns semantic upstream HTTP/TLS for intercepted requests;
- `eggnet-tls` is preferred for neutral bounded PEM/identity parsing and
  rustls server-configuration helpers where its published surface fits;
- a maintained certificate-generation crate (expected `rcgen`) owns X.509
  generation/signing;
- EggReplay owns proxy/interception policy, CA lifecycle, leaf-cache policy,
  target coherence, recording integration, CLI orchestration, and evidence.

No second Hyper client/server, SOCKS/CONNECT stack, TLS verifier, or hand-built
X.509 encoder is permitted.

## Optional dependency boundary

`eggreplay-core`, `eggreplay-store`, `eggreplay-http`, and the default
Python wheel must not depend on certificate-generation or interception-key
material dependencies.

The CLI may expose interception behind an explicit Cargo feature. A normal
workspace/library build without that feature remains interception-free.

M013 does not add interception to the default `eggreplay` Python wheel. A
future separately qualified distribution/feature strategy may do so without
forcing CA dependencies into every Python installation.

## Listener and open-proxy policy

The interception proxy binds loopback by default.

Binding a non-loopback address requires an explicit unsafe/remote-listener
opt-in plus an ingress allow policy. M013 does not provide a remote management
API or proxy-authentication service, so it must not silently become an open
forward proxy.

Every request/CONNECT target is checked by a typed target policy before route
establishment or certificate issuance.

Initial policy supports explicit host/IP and port allow/deny rules. Deny wins.
Malformed, ambiguous, userinfo-bearing, or unbounded authorities fail closed.

## CONNECT actions

For an allowed CONNECT target, policy selects one of exactly:

- `deny`;
- `tunnel`;
- `intercept`.

A passthrough tunnel is opaque bytes. It may emit bounded operational metadata
but is never persisted as a semantic HTTP flow.

Interception is opt-in per policy; enabling the interception feature globally
does not mean every CONNECT is intercepted.

## HTTPS interception contract

Initial MITM is RFC HTTP/1.1 only.

For `intercept`:

1. validate CONNECT authority and policy;
2. send successful CONNECT response only when interception can proceed;
3. terminate client TLS using a leaf minted by the dedicated CA;
4. advertise only `http/1.1` ALPN;
5. construct truthful EggServe `ConnectionContext` with HTTPS/TLS metadata;
6. feed the decrypted stream into EggServe's caller-owned H1 driver;
7. reconstruct the logical HTTPS origin under strict authority coherence;
8. send upstream semantic HTTP through EggFetch with normal certificate and
   hostname verification;
9. record through existing EggReplay recording/redaction authorities.

The CONNECT authority is the maximum authority of the intercepted connection.
For DNS targets, SNI and HTTP Host/authority must match that target under
documented normalization. Cross-origin requests inside one CONNECT fail closed
in M013. IP CONNECT may omit SNI but HTTP authority must remain coherent.

Do not derive trust from untrusted `Forwarded` headers.

## Certificate authority lifecycle

The interception CA is a dedicated operator-owned identity, never fixture
content.

Supported lifecycle:

- initialize new CA in a caller-selected directory;
- import an existing CA certificate/private key;
- inspect public metadata/fingerprint/expiry;
- export/copy the public certificate only;
- rotate by creating a distinct identity and explicitly selecting it.

No command automatically installs the CA into OS, browser, Java, Python, or
application trust stores. Trust installation is documented as an external
operator action.

Private key contents and paths are secret diagnostic fields. They never appear
in `.eggr`, logs, JSON/JUnit reports, Python repr/str, panic text, or crash
artifacts.

Initial leaf generation uses exact DNS/IP SANs only. No wildcard leaves.
Generated leaf validity and cache size/lifetime are explicitly bounded.

## Protocol exclusions

M013 does not claim:

- HTTP/2 MITM or Extended CONNECT;
- HTTP/3/QUIC interception;
- transparent/TUN interception;
- WebSocket-over-WSS interception;
- TLS protocols other than HTTP;
- certificate-pinned applications;
- intercepted client-certificate/mTLS authentication;
- upstream certificate-verification bypass;
- automatic client trust installation.

H2/H3 compatibility belongs to M014. Unsupported traffic fails explicitly or
uses an explicitly configured passthrough action without semantic capture.

## Security consequence

Interception deliberately creates a local trust anchor capable of decrypting
traffic trusted by clients that install it. The feature remains separately
compiled, separately enabled, policy-gated, and loopback-first. M013 closure
requires a threat model and non-leak evidence in addition to functional tests.
