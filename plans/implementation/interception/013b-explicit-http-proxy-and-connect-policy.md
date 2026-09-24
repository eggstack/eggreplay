# M013B — Explicit HTTP Proxy and CONNECT Policy

Status: blocked
Depends on: M013A
Parent milestone: M013

## Current blocker

Published `eggserve-server 0.2.1` rejects HTTP/1 absolute-form request targets
inside its canonical request adapter before the service is invoked
(`connection/request.rs`, the `scheme_str().is_some()` check). M013B requires
the proxy service to receive and validate the absolute URI before deriving
the logical origin and removing proxy-only headers. The current public
EggServe service API cannot provide that request to the service.

M013B is blocked pending an EggServe-owned API change that safely exposes
absolute-form HTTP/1 requests to the caller-owned service path. Keep HTTP
parsing and transport ownership in EggServe; do not introduce another parser
or Hyper server in EggReplay. Reassess this plan after the upstream seam is
published and qualified.

## Objective

Implement a safe explicit HTTP/1.1 forward-proxy acquisition path with
absolute-form HTTP recording and policy-controlled CONNECT deny/tunnel behavior.
Do not implement TLS interception yet.

## A. Listener boundary

Use EggServe for the explicit proxy listener/runtime.

Defaults:

- bind loopback only;
- finite connection/tunnel limits;
- no remote management endpoint;
- no implicit system proxy configuration.

A non-loopback bind requires an explicit remote-listener opt-in and an ingress
client allow policy. Without proxy authentication in M013, documentation must
warn that remote exposure can create an open proxy.

## B. Target policy

Add transport-neutral policy types owned by `eggreplay-intercept`.

Initial match dimensions:

- exact DNS name;
- DNS suffix only when explicitly configured and label-boundary safe;
- exact IPv4/IPv6 literal;
- exact port or bounded port set/range;
- request kind: plain proxy HTTP vs CONNECT.

Rules have deterministic priority. Deny wins over allow at equal specificity.
The default policy is deny for interception and configurable deny/tunnel for
CONNECT. No “allow all Internet” default.

Normalize:

- DNS case/trailing dot;
- IPv6 bracket forms;
- default ports;
- IDNA policy explicitly (initial implementation may reject non-ASCII names);
- reject userinfo and malformed/multiple authorities.

Bound rule count/string lengths and diagnostics.

## C. Absolute-form HTTP

For non-CONNECT explicit-proxy requests:

1. require an absolute `http://` URI;
2. derive canonical logical origin from that URI;
3. reject contradictory Host authority;
4. remove proxy-only hop-by-hop fields correctly;
5. pass the semantic request to the existing EggReplay recorder;
6. execute upstream through EggFetch;
7. preserve duplicate end-to-end headers/body/trailers;
8. return the upstream semantic response through EggServe.

Do not create a second HTTP serializer/parser.

Initial explicit proxy HTTP supports `http://` only. HTTPS uses CONNECT.

## D. Hop-by-hop/proxy header policy

Implement RFC-oriented filtering in one tested helper.

At minimum handle:

- `Proxy-Connection`;
- `Proxy-Authorization` (never forward upstream);
- `Proxy-Authenticate`;
- `Connection` nominated headers;
- `Keep-Alive`;
- `TE`/trailers semantics;
- `Transfer-Encoding`;
- `Upgrade` according to supported proxy mode.

Never persist proxy credentials. M013 does not implement upstream proxy auth
through this client-facing listener.

## E. CONNECT deny/tunnel

For CONNECT:

1. validate authority form and target policy before dialing;
2. choose only `deny` or `tunnel` in M013B;
3. for `tunnel`, establish raw target IO through Eggress;
4. send 200 only after the target route is established;
5. relay EggServe `TunnelIo` and outbound stream with direct backpressure;
6. apply byte, duration, idle/no-progress, concurrent-tunnel, and shutdown
   bounds.

The tunnel remains opaque. Do not parse TLS, sniff application bytes, or append
an EggReplay HTTP flow.

Operational events may contain only bounded target/action/error metadata.

## F. Routing

Reuse EggReplay's existing route grammar and narrow Eggress
`pproxy-compat` surface. Direct and routed behavior must share the same target
policy.

No fallback from configured proxy route to direct.

## G. Recording/session behavior

Plain absolute-form HTTP uses the existing `RecordingSession`, redaction, and
flow-store authority.

CONNECT tunnels do not affect flow counts. If an operational summary is
needed, keep it outside `.eggr` semantic flow authority unless a later ADR
defines an optional provenance extension.

## Required tests

Local only:

- absolute-form GET/POST/streaming/trailers;
- duplicate headers/query keys;
- Host mismatch;
- origin-form request rejected on explicit proxy listener;
- proxy/hop-by-hop header stripping;
- Proxy-Authorization never forwarded/persisted/logged;
- exact host/port allow and deny;
- DNS suffix label-boundary cases;
- IPv4/IPv6 CONNECT authority;
- CONNECT deny;
- direct CONNECT tunnel;
- routed CONNECT tunnel;
- no-direct-fallback failure;
- target dial failure before 200;
- half-close/backpressure;
- duration/byte/concurrency limits;
- shutdown with active tunnel;
- non-loopback bind safety gate;
- tunnel traffic produces no semantic HTTP flow.

## Closure

Create `plans/closure/m013b-explicit-http-proxy-and-connect-policy.md`.
M013C becomes ready only after proxy/CONNECT policy is stable.
