# M013A — Interception Substrate, Dependency, and Threat Preflight

Status: ready
Depends on: M012 closure
Parent milestone: M013
ADR: 0008

## Objective

Prove every generic transport/TLS seam needed by interception before building a
proxy or CA implementation. Freeze the dependency graph and threat assumptions
so later plans do not work around missing upstream APIs.

M013B must not begin until this closes.

## A. Add the crate boundary

Add `crates/eggreplay-intercept` to the workspace with no default feature
activation from other EggReplay crates.

Allowed dependency direction:

```text
eggreplay-intercept
  -> eggreplay-core
  -> eggreplay-store
  -> eggreplay-http
  -> EggServe / EggFetch / Eggress / eggnet-tls / rustls helpers
```

Existing Rust product crates and `eggreplay-python` must not depend on
`eggreplay-intercept`.

The CLI may add an optional `intercept` feature later in M013E.

## B. Reconfirm published sibling baselines

At implementation time query crates.io/package evidence rather than assuming
workspace versions imply publication.

Known current facts:

- EggReplay uses `eggserve-server 0.2.1` and `eggserve-primitives 0.2.0`;
- caller-owned `serve_http1_connection` is published in
  `eggserve-server 0.2.1`;
- `eggserve-core 0.2.2` is published for the Tower adapter but is not needed
  for M013's direct H1 handoff;
- EggServe main is source-versioned 0.2.2, while unchanged leaf crates were
  intentionally not republished just for version symmetry;
- EggReplay currently uses `eggress-outbound 1.0.8`;
- Eggress main is preparing 1.0.10 but that release is currently blocked on an
  H2 TLS-override correctness corrective.

Do not upgrade Eggress merely because 1.0.9/1.0.10 exists in source. Keep
1.0.8 unless M013 needs a specific newer public capability and that exact
release is fully published/qualified.

Determine the latest published compatible `eggnet-tls` version. Use a
registry release, not a Git dependency. If its public PEM/identity helpers are
not sufficient, use rustls/rcgen directly in the interception crate rather
than importing `eggserve-core`.

## C. TLS dependency floor

If M013 directly depends on rustls, enforce at least the patched
`rustls 0.23.45` floor already documented by EggServe for
RUSTSEC-2026-0285. Keep `tokio-rustls` on a compatible 0.26 line.

Evaluate a maintained `rcgen` version compatible with Rust 1.89 and the
selected rustls/pki-types line. Pin the qualified pre-1.0 version exactly for
M013.

No OpenSSL/native-tls dependency is needed for the initial implementation.

## D. Prove decrypted-stream -> EggServe H1 handoff

Using only local generated test identities:

1. accept a TCP connection;
2. complete a rustls server handshake;
3. obtain negotiated TLS version/SNI/ALPN metadata;
4. construct `ConnectionContext::for_tcp(..., Some(TlsInfo))`;
5. assert semantic scheme is HTTPS;
6. call `eggserve_server::serve_http1_connection` over the
   `tokio_rustls::server::TlsStream`;
7. serve one canonical HTTP request/response;
8. shut down and receive a classified `ConnectionOutcome`.

Advertise only `http/1.1`.

This proof must compile from published crates used by EggReplay. If it requires
an unpublished EggServe seam, stop and write the smallest upstream EggServe
plan instead of embedding Hyper in EggReplay.

## E. Prove CONNECT passthrough route establishment

Using `eggress-outbound`:

- direct CONNECT target stream;
- one local pproxy-compatible routed target;
- configured route failure with no direct fallback;
- cancellation/shutdown of a live opaque relay.

This is a raw stream proof only; do not add proxy request parsing yet.

## F. Prove EggFetch HTTPS upstream authority

Use a local TLS origin and caller-owned test CA.

Prove that an EggFetch client:

- verifies the local CA when explicitly configured;
- rejects the same origin without that trust;
- verifies hostname/SNI;
- executes a semantic HTTP/1.1 request through the current EggReplay route
  adapter;
- does not require any interception-side insecure verifier.

This is the upstream security authority M013D will reuse.

## G. Threat model

Add `docs/interception-threat-model.md` covering:

- local CA compromise;
- open-proxy exposure;
- SSRF/internal-network reachability;
- CONNECT/SNI/Host confused-deputy risks;
- key/fixture/log separation;
- certificate pinning;
- unsupported mTLS;
- upstream TLS verification;
- denial-of-service through tunnels/leaf generation;
- shutdown/temporary-file behavior.

## Required evidence

- Rust 1.89 builds;
- Linux/macOS/Windows;
- published-dependency tree;
- decrypted TLS stream -> EggServe H1 smoke;
- Eggress direct/routed/no-fallback stream smoke;
- EggFetch secure local TLS origin smoke;
- dependency-boundary check proving default core/store/http/Python do not gain
  interception/rcgen dependencies.

## Closure

Create `plans/closure/m013a-interception-substrate-and-threat-preflight.md`
with exact crate versions and support decisions. Only then move M013B to ready.
