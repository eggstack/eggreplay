# M013F — Interception Hardening, Qualification, and M013 Closure

Status: blocked
Depends on: M013E
Parent milestone: M013

## Objective

Perform the final security/resource/portability audit and close M013 only when
the explicit proxy and optional HTTPS interception support matrix is backed by
hosted evidence.

## A. Threat-model closure

Revisit `docs/interception-threat-model.md` against implementation.

Explicitly verify mitigations for:

- open-proxy exposure;
- SSRF/internal target access;
- CONNECT/SNI/Host authority confusion;
- private key leakage;
- temporary-file leakage;
- CA overwrite/rotation confusion;
- route fallback;
- upstream TLS bypass;
- certificate cache exhaustion;
- connection/tunnel exhaustion;
- slowloris/no-progress behavior;
- shutdown with active TLS/tunnel tasks.

Record residual risks, especially operator trust-store installation and
certificate-pinned applications.

## B. Resource audit

Prove hard bounds for:

- listener connections;
- passthrough tunnels;
- intercepted connections;
- concurrent TLS handshakes;
- leaf certificate cache entries;
- certificate/key/PEM input size;
- request/body/session sizes inherited from EggReplay;
- relay bytes/duration/idle/no-progress;
- policy rule count;
- diagnostics/events.

Inspect for unbounded channels/tasks/caches. Network waits must not hold global
CA/cache/session locks.

## C. Secret audit

Use unique sentinels in:

- CA key;
- leaf key;
- Proxy-Authorization;
- HTTP Authorization/Cookie;
- body redaction target.

Scan:

- fixture tree;
- command stdout/stderr;
- JSON output;
- logs/tracing capture;
- error values;
- panic/backtrace capture where practical;
- metadata/policy files;
- temporary/staging paths after failure.

Private key **paths** should also be omitted/redacted from routine diagnostics.

## D. Independent interoperability

Use at least two independent client implementations where available in hosted
images, for example:

- curl using explicit proxy + custom CA;
- Python stdlib/httpx/requests local test client;
- Rustls scripted client.

No public Internet.

Qualify both plain HTTP proxying and HTTPS CONNECT/MITM.

## E. Platform/permission qualification

Hosted:

- Ubuntu stable;
- Ubuntu Rust 1.89;
- macOS stable;
- Windows stable;
- dependency-boundary.

Unix jobs verify 0700/0600 CA permissions.

Windows closure states exactly what private-key file protection was proven; do
not claim Unix-equivalent ACL semantics without evidence.

## F. Dependency/supply-chain audit

Confirm:

- default core/store/http/Python graphs contain no interception/rcgen;
- `eggreplay-intercept` uses published sibling crates only;
- no EggServe-core dependency was added merely for H1 handoff;
- Eggress remains the explicitly qualified version and narrow route surface;
- rustls floor is >= 0.23.45 wherever M013 directly controls it;
- `cargo audit` is clean or any advisory is explicitly dispositioned.

If Eggress 1.0.10 remains blocked upstream, do not make M013 closure depend on
or adopt it.

## G. Support matrix

Expected M013 claim if evidence passes:

| Capability | M013 |
|---|---|
| Explicit HTTP/1.1 proxy absolute-form recording | supported |
| CONNECT deny | supported |
| CONNECT opaque passthrough | supported |
| HTTPS MITM HTTP/1.1 recording | supported, opt-in/policy-gated |
| Direct + narrow Eggress-routed upstream | supported |
| Manual CA initialize/import/export/rotate | supported |
| Automatic OS/browser trust installation | unsupported |
| HTTP/2 MITM | unsupported/deferred M014B |
| HTTP/3/QUIC interception | unsupported/deferred |
| WSS WebSocket interception | unsupported |
| client mTLS interception | unsupported |
| certificate-pinned clients | expected to fail unless configured passthrough |
| transparent/TUN interception | unsupported |

Do not expand the matrix during closure without corresponding tests.

## H. Full verification

Run:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
git diff --check
```

Also run minimum-feature graphs:

- default workspace without interception;
- `eggreplay-intercept` standalone;
- CLI with interception;
- Python wheel/package lane to prove it remains unchanged/interception-free.

Add a dedicated interception feature CI job if ordinary all-features CI cannot
exercise local CA/CONNECT/MITM end-to-end behavior deterministically.

## I. Documentation and closure

Update:

- README support matrix;
- architecture/dependency docs;
- CLI docs;
- interception threat model/operator docs;
- roadmap;
- registry;
- planning README.

Create `plans/closure/m013-explicit-proxy-and-optional-mitm.md` containing:

- implementation SHAs;
- dependency versions/checksums where appropriate;
- CA/cert library choices;
- test counts;
- Actions run IDs/URLs;
- platform permission results;
- interoperability clients;
- exact protocol/support matrix;
- known limitations/residual risks;
- M013A–M013E closure links.

Only after hosted green evidence:

- mark M013 and M013F closed;
- advance M014/M014A/M014B according to their declared dependencies;
- do not unblock M014C before M014B closes.

## Acceptance

M013 closes only if interception remains optional, the CA key remains outside
fixtures/diagnostics, proxy targeting is fail-closed, opaque tunnels are not
misrepresented as HTTP, upstream TLS remains verified, and the advertised
HTTP/1.1 support works cross-platform with bounded lifecycle/resource behavior.
