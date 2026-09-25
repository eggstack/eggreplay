# M013B0 closure — EggServe 0.3 Adoption and Absolute-Form Qualification

M013B0 is closed on implementation revision `86cf2ff2d89f6669096fd0a34a5675f95fdb6b88`.

## Qualified dependencies

The migration uses these exact registry versions:

- `eggserve-primitives 0.2.1`
- `eggserve-server 0.3.0`
- `eggfetch-core 0.2.0`
- `eggress-outbound 1.0.8` with only `pproxy-compat`
- `eggnet-tls 0.2.0`
- `rustls 0.23.45`
- `tokio-rustls 0.26.2`
- `rcgen 0.13.2`

`Cargo.lock` changes only the two EggServe package versions and checksums. The
lock graph contains one `eggserve-primitives 0.2.1` and one
`eggserve-server 0.3.0`; no older EggServe server or primitives copy remains.
No `eggserve-core`, `eggserve-static`, PHF-family package, native TLS, or
parallel HTTP stack was added.

The default Python package remains free of `eggreplay-intercept` and `rcgen`.
The interception crate remains a leaf, and the Python extension remains a leaf
binding crate with no PyO3 dependency in Rust product crates.

## Source adaptation

- Ordinary recording and replay gateways now explicitly use
  `Http1RequestTargetMode::OriginOnly`, `H1PolicyOwnership::eggserve_owned()`,
  and `AdmissionOwnership::eggserve_owned()`.
- The recording and replay WebSocket paths retain their existing tunnel limits,
  connection-total-timeout behavior, and `ServerHandle` shutdown/drain
  behavior.
- The M013A caller-owned TLS proof now uses the 0.3 builder, one projected
  `H1ConnectionPolicy`, `RuntimeState::try_new`, and
  `serve_http1_connection_with_policy` with truthful HTTPS connection
  metadata.
- `eggreplay-intercept` exposes the bounded `InterceptionProfile` runtime
  helper. It opts into `OriginOrAbsolute` only for the future interception
  listener and does not implement proxy policy or forwarding.
- Substrate diagnostic constants now report EggServe server `0.3.0` and
  primitives `0.2.1`.

## Ownership profile

The M013B0 interception helper records the following EggServe-owned policy:

| Concern | Owner | Value or boundary |
|---|---|---|
| Handler deadline | EggServe | EggServe default |
| Request-body deadline | EggServe | EggServe default |
| Keep-alive idle deadline | EggServe | EggServe default |
| Response write-progress deadline | EggServe | EggServe default |
| Global request-body ceiling | EggServe | 8 MiB |
| Semantic request-target ceiling | EggServe | 8 KiB |
| Service-call admission | EggServe | 64 in flight |
| Tunnel admission | EggServe | 64 active |
| Concurrent connections | EggServe | 64 |
| Connection total timeout | EggServe | 60 seconds |
| Keep-alive idle timeout | EggServe | 60 seconds |
| Response write timeout | EggServe | 30 seconds |

M013B will separately own outbound relay byte, total-duration, and
idle/no-progress limits. `AdmissionOwner::External` is not introduced, and
EggServe's `max_active_tunnels` is the single tunnel-admission authority.

## Focused evidence

`crates/eggreplay-intercept/tests/substrate.rs` proves:

- caller-owned TLS-to-EggServe H1 handoff on the validated 0.3 policy path;
- absolute-form `RequestTargetForm`, scheme, URI authority, canonical
  authority, path, query, and raw target metadata;
- Host/URI authority mismatch rejection with 400 before service invocation;
- semantic target-ceiling rejection with 414 before service invocation;
- origin-form compatibility under `OriginOrAbsolute`;
- CONNECT authority-form tunnel behavior, including EggServe's 200 tunnel
  transition and preserved authority;
- duplicate end-to-end header ordering;
- streamed absolute-form chunked body bytes and terminal trailers;
- explicit `OriginOnly` rejection of absolute-form input;
- all interception ownership fields and configured bounds;
- Eggress raw CONNECT routing and no-direct-fallback behavior;
- EggFetch CA, hostname, and SNI verification.

`crates/eggreplay-http` additionally proves that real ordinary replay and
recording gateways reject absolute-form requests with 400 before their
services run. The replay control request reaches the empty service and
returns 404; the recording control state records zero flows.

## Local verification

The following commands passed on the implementation revision:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
git diff --check
```

The full all-feature workspace suite passed with 146 tests. The focused
`eggreplay-intercept` suite passed all 12 tests, and the all-feature
`eggreplay-http` suite passed all 47 unit tests plus 16 qualification tests.
The existing dependency-boundary checks passed locally. The isolated
CPython 3.11 environment passed `maturin develop`, 37 Python tests, and
`maturin build --locked`; the local build used the matching x86_64 Rust
toolchain because the available CPython interpreter was x86_64.

`cargo audit` reported no known vulnerabilities.

## Hosted verification

GitHub Actions run [36094432787](https://github.com/eggstack/eggreplay/actions/runs/36094432787)
completed successfully for revision `86cf2ff2d89f6669096fd0a34a5675f95fdb6b88`.

| Job | Result | Evidence |
|---|---|---|
| Ubuntu stable | passed | 146 workspace tests |
| Ubuntu Rust 1.89.0 | passed | 146 workspace tests |
| macOS stable | passed | 146 workspace tests |
| Windows stable | passed | 144 workspace tests; two existing Unix-only symlink tests are excluded |
| Dependency boundary | passed | Core/store/direct/WebSocket/Eggress and interception/Python graph checks |
| Ubuntu CPython 3.11 / Rust 1.89 | passed | 37 Python tests, wheel and sdist checks |
| Ubuntu CPython 3.14 | passed | 37 Python tests and wheel build |
| macOS CPython 3.11 | passed | 37 Python tests and wheel build |
| Windows CPython 3.11 | passed | 37 Python tests and wheel build |
| abi3 cross-version | passed | CPython 3.11-built wheel installed and smoke-tested under CPython 3.14; 37 tests |

The hosted dependency-boundary lane passed on the exact implementation
revision. Wheel and source-archive builds were smoke-tested only; no Python
artifacts were republished.

## Supported boundary and handoff

This milestone qualifies only the EggServe 0.3 transport/runtime substrate and
the opt-in absolute-form service seam. It does not implement target policy,
HTTP forwarding, CONNECT relay, CA lifecycle, HTTPS MITM, or a public proxy
API. Those remain M013B and later responsibilities.

The closure removes the M013B publication/dependency blocker. M013B is ready
against this qualified baseline. M013C–M013F and M014 remain blocked by their
declared plan dependencies.

Non-blocking hosted annotations were limited to the existing Node.js 20
action deprecation and the scheduled Ubuntu runner-image migration notice.
