# Interception threat model

M013 interception is an explicit, opt-in local proxy. The operator controls
the listener, target policy, CA identity, fixture location, and outbound route.
The proxy must fail closed when any of those authorities are missing or
invalid. EggServe owns inbound HTTP framing/lifecycle, EggFetch owns upstream
HTTP and TLS verification, and Eggress owns optional outbound route
establishment.

## Assets and trust boundaries

- The CA private key is a local signing authority and stays outside fixture
  data, logs, operational output, and temporary artifacts.
- Issued leaf private keys and TLS session secrets are memory-only.
- Proxy credentials, HTTP authorization/cookie values, and request/response
  bodies are sensitive. Existing redaction and persistence rules apply.
- The client-facing listener, policy file, CA directory, fixture store, and
  upstream route are separate trust boundaries. Client-supplied Host, SNI,
  Forwarded headers, and proxy headers are untrusted.

## Threats and required controls

| Threat | Control |
|---|---|
| Open-proxy exposure | Loopback bind by default; non-loopback requires explicit opt-in and ingress allow policy. No default allow-all target policy. |
| SSRF/internal-network access | Apply the same exact target policy before direct or routed dials; deny by default; constrain ports and bound names. |
| CONNECT/SNI/Host confused deputy | Normalize and compare CONNECT authority, SNI, and decrypted HTTP authority; one intercepted connection cannot change origin. |
| CA compromise or accidental overwrite | Dedicated versioned CA directory, restrictive permissions where supported, create-new publication, explicit import/rotation, public-only export. |
| Key/fixture/log leakage | No private key in metadata or fixtures; redacted diagnostics; scan outputs and staging paths with unique sentinels. |
| Certificate pinning | Pinned clients are expected to reject the generated leaf; operator may explicitly choose opaque tunnel where policy permits. |
| Unsupported mTLS | Client-certificate interception is unsupported and must fail explicitly. |
| Upstream TLS downgrade/bypass | EggFetch verifies upstream certificates and names; client trust in the local CA does not alter upstream roots. No insecure verifier is enabled. |
| Tunnel or leaf-generation denial of service | Bound connections, handshakes, concurrent tunnels, byte/time/idle budgets, input sizes, leaf cache, and policy size. |
| Shutdown/temp-file residue | Stop admission, cancel/drain active tasks, atomically publish CA state, and remove failed staging artifacts. |

## Residual risks and unsupported traffic

Installing the public CA grants the proxy the ability to impersonate names
allowed by interception policy. Trust installation is manual and external to
EggReplay. HTTP/2, HTTP/3/QUIC, WebSocket-over-WSS, client mTLS, transparent
interception, and successful interception by certificate-pinned clients are
outside the initial support claim. Opaque CONNECT tunneling does not create a
semantic HTTP flow.

Windows filesystem ACL guarantees must be stated only to the level proven by
the qualified APIs and hosted tests; Unix mode bits do not imply equivalent
Windows protection. Runtime shutdown cannot revoke certificates already
trusted by a client, so CA removal/revocation guidance is an operator task.
