# M014B — HTTP/2 End-to-End Qualification Closure

Status: closed

## Qualifying revision and hosted evidence

Implementation: `eggreplay-http/h2` cargo feature, `src/h2.rs` boundary
module (`check_h2_headers`, `HttpVersionPolicy` re-export), negotiated
`http-version:h2` flow annotation in both `record_request` paths,
`tests/h2_qualification.rs` (15 integration tests), and
`docs/http2-support.md`. No dependency version moves (see below).

Local verification on the qualifying revision is green with the
repository-standard command:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Workspace suite: 355 tests passed, 0 failed across 26 suites (334
pre-M014B + 6 new `h2` unit tests + 15 new `h2_qualification` integration
tests). Hosted CI run:
[36771106385](https://github.com/eggstack/eggreplay/actions/runs/36771106385)
on the implementation commit, which passed all thirteen jobs in the
standard matrix.

## Support-tier decision

- **Experimental**: H2 record and regression-candidate execution through
  EggFetch (`native-http2`, ALPN `h2` over local TLS), including routed
  H2-over-TLS through an Eggress TCP route. Opt-in only: the `h2` cargo
  feature plus an explicit `HttpVersionPolicy` per client. H1 remains the
  default policy in every EggReplay client constructor (CLI, Python, and
  all in-tree tests except the H2 suite).
- **Unsupported**: H2 inbound serving (EggServe replay/gateway), H2
  interception (MITM), and cleartext prior-knowledge (`h2c`).

Inbound H2 is unsupported because the qualified EggServe direct-runtime
line (`eggserve-server 0.3.0`) serves H1 only; even at 0.4.0 EggServe
states "H1 is the supported transport; H2/H3 are opt-in and experimental",
and H2 serving composition lives in `eggserve-core` multiprotocol
composition outside the adopted narrow closure. Adopting that closure
would drag static-serving dependencies against the transport-ownership
boundary, so M014B does not do it. H2 MITM needs its own
ALPN/caller-owned-connection evidence per the plan and stays out of
scope. `h2c` is not exposed: `Http2Only` against a cleartext endpoint
fails closed (pinned by test).

## Dependency verification (no version moves)

Exact qualified revisions, unchanged from the M013 line:

- `eggfetch-core 0.2.0` + newly enabled `native-http2` feature
  (`transport-http2`: `h2 0.4.19`, `hyper/http2`, `hyper-util/http2`,
  `hyper-rustls/http2`). Seams verified: `ClientBuilder::http_version_policy`,
  `Response::version`, `TlsConfig::builder().ca_certificate_pem`,
  internal `strip_h2_forbidden_headers`, ALPN configuration honoring the
  version policy.
- `eggserve-primitives 0.2.1`, `eggserve-server 0.3.0`: H1-only direct
  serving confirmed (H2 composition out of closure; 0.4.0 still classifies
  H2 experimental).
- `eggress-outbound 1.0.8` (`pproxy-compat` only): `EggressDialer` returns
  raw TCP from `connect_tcp_detailed`; SNI/ALPN ownership stays in
  EggFetch by construction, pinned by the routed-H2 test through a
  single-hop SOCKS5 TCP route. 1.0.10 not adopted (M013-era H2
  TLS-override concern; our TCP-only seam is unaffected).
- `eggnet-tls 0.2.0`: unchanged.

## Evidence matrix (all local, deterministic, loopback-only)

Three client families plus EggFetch product path against a hyper H2 TLS
harness (test transport only, rcgen CA trusted explicitly, ALPN `h2`):

| Requirement | Evidence |
|---|---|
| ALPN h2 over local TLS | EggFetch `Http2Only` records with `http-version:h2` annotation |
| Independent families | hyper manual-TLS H2 client; raw `h2`-crate client; both 200/`h2-ok` |
| Concurrent streams | 8 parallel `record_request_with_session`, 8 intact flows |
| Trailers | flow trailers + `Trailers`/`End` candidate events |
| Large streaming + backpressure | 8 MiB exact bytes |
| Cancellation/reset | dropped stream leaves sibling + third stream intact |
| Target remapping | `/echo/remapped` path preserved over H2 |
| Strict/practical matching | strict matches identical, rejects volatile header; practical accepts |
| Scenario behavior | `advance` on H2-recorded request renders + transitions state |
| M010 events under multiplexing | `Data`/`Trailers`/`End` observations over H2 |
| Regression findings | `compare_flows` clean over H2 re-execution |
| GOAWAY/shutdown | in-flight record completes across server graceful shutdown |
| Routed H2 via Eggress TCP | SOCKS5-routed H2 record, `physical_route.kind == "eggress"` |
| No H1 header leaks | server never observes `connection`/`transfer-encoding`; boundary helper rejects |
| No silent downgrade | `Http2Only` vs cleartext H1 fails closed |

H1 behavior is unchanged: default builds exclude `native-http2`
(`Auto` downgrades to H1 exactly as before), and the version annotation
is `None` for non-H2 responses so existing fixtures keep their shape.

## Handoff

M014B is closed (experimental outbound H2). M014C (H3 feasibility) and
M014D (gRPC + faults) are unblocked: M014B and M010 are both closed.
The M014 umbrella remains open until C and D close.
