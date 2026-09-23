# M011A — Transport Dependency and Upgrade Preflight

Status: ready
Depends on: M010 + M010-C1 closure
Parent milestone: M011

## Objective

Freeze the transport substrate before adding WebSocket semantics. Remove stale
EggServe dependency assumptions, prove EggFetch 0.2.0 can hand off HTTP/1.1
upgrades through both direct and EggReplay's narrow Eggress route path, and
avoid importing an upstream Eggress release that is currently blocked on
correctness work.

M011B must not begin until this preflight closes.

## A. EggServe registry migration

EggReplay still pins old EggServe Git revisions even though the direct server
surface is now registry-qualified.

Replace the Git dependencies with the published compatible set proven by
EggServe's downstream closure:

- `eggserve-server = 0.2.1`;
- `eggserve-primitives = 0.2.0`.

Use exact or otherwise deliberately bounded 0.2 requirements so Cargo cannot
silently advance to a later incompatible pre-1.0 minor.

Update `Cargo.lock`, `docs/architecture.md`, comments, and dependency
boundary checks. Remove the claim that these EggServe crates are unpublished.

Add a clean registry-only EggReplay smoke that uses:

- `service_fn_with_tunnel`;
- H1 Upgrade capability;
- `TunnelIo`;
- read-ahead preservation;
- shutdown/cancellation;
- the 0.2.1 total-connection-lifetime disable/control surface needed for
  long-lived upgraded sessions.

Do not import `eggserve-core` solely for WebSocket support.

## B. EggFetch 0.2.0 upgrade contract

Keep the published `eggfetch-core 0.2.0` baseline unless qualification proves
a missing required seam.

Prove from EggReplay code, not only upstream tests:

1. a direct H1 request can receive 101;
2. the high-level response exposes an owned `NetworkStream::Upgraded`;
3. leading post-101 bytes are preserved;
4. the upgraded object is `AsyncRead + AsyncWrite`;
5. closing the upgraded stream removes it from HTTP reuse;
6. caller-owned `TlsConfig` works with a local CA for a local `wss://`
   handshake if M011 will claim outbound WSS;
7. the same client-scoped custom Dialer used by EggReplay routes can reach 101
   and return an upgraded stream without direct fallback.

The existing low-level `execute_http_body_default` API returns an
`http::Response<NativeResponseBody>` and is not assumed to expose upgrade IO.
M011 may use EggFetch's high-level request path for the handshake while keeping
ordinary HTTP recording/regression on the native body path.

If EggFetch 0.2.0 cannot expose the required upgraded stream through the custom
Dialer path, stop M011 and write the smallest upstream EggFetch implementation
plan. Do not bypass EggFetch with a private Hyper client.

## C. Eggress baseline

EggReplay currently enables only
`eggress-outbound/pproxy-compat`; preserve that narrow feature boundary.

As of this preflight, Eggress 1.0.9 is prepared but upstream-blocked on pooled
route-isolation/metadata correctives. Do **not** adopt 1.0.9 merely because it
is newer.

Evaluate the current EggReplay pin against the latest safely published
`eggress-outbound` line (currently 1.0.8 unless registry state changed at
implementation time):

- if the published crate exposes the same listener-free
  `OutboundConnector::from_pproxy_uri` and typed failure contract and passes
  existing route tests plus the new 101 upgrade smoke, migrate off the Git pin;
- otherwise retain an exact known-good pin and record why.

Do not enable Eggress `extended`, `ssh`, `quic`, listener, or server
features for M011.

## D. HTTP/1.1 policy

WebSocket handshakes must force/qualify HTTP/1.1. H2 Extended CONNECT belongs to
M014B.

Prove the EggFetch WebSocket client path cannot silently negotiate H2 and then
pretend it has RFC 6455 H1 upgrade semantics.

## E. Dependency feature boundary

M011's codec dependency will be optional in `eggreplay-http`; this preflight
must preserve:

- `eggreplay-core`: no Tokio/Hyper/EggFetch/EggServe/Eggress/WebSocket codec;
- `eggreplay-store`: no transport or WebSocket codec;
- direct non-WebSocket library builds remain available;
- CLI may later enable the WebSocket feature explicitly.

## Required tests/evidence

Use local deterministic fixtures only.

- registry-only EggServe tunnel/read-ahead smoke;
- EggFetch direct 101 upgraded roundtrip;
- leading-data roundtrip;
- custom Eggress-dialed 101 roundtrip;
- no-direct-fallback route failure;
- local WSS smoke with a test CA if outbound WSS is claimed;
- dependency tree/boundary assertions;
- Linux stable + Rust 1.89 build and the existing hosted matrix.

## Closure

Create `plans/closure/m011a-transport-dependency-and-upgrade-preflight.md`.
The closure records exact dependency versions/revisions and the supported
ws/wss substrate. Only then move M011B to ready.
