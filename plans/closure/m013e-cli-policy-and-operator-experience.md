# M013E closure — CLI, Policy, and Operator Experience

M013E is closed with local verification green. Hosted qualification is
deferred to M013F per the M013 closure plan; M013E required tests are
local-only by plan.

## Implementation

- `crates/eggreplay-intercept/src/policy_file.rs` — versioned declarative
  policy format `eggreplay-intercept-policy/v1` (JSON, `deny_unknown_fields`):
  exact / `*.`-suffix / IP hosts, `any` / exact / `lo-hi` / set ports,
  `plain` / `connect` / `any` kinds, `deny` / `tunnel` / `intercept` actions
  mapping onto `TargetPolicy` + CONNECT default. Bounded (64 KiB, 128 rules),
  fails closed on unknown version/fields/actions; rejects mixed
  tunnel+intercept listeners (fail-closed; run separate listeners).
  Includes `policy_from_flags` for CLI flag rules.
- `crates/eggreplay-intercept` — `ProxyStats` (accepted/rejected/tunneled/
  intercepted/flows + bounded failure categories, serializable snapshot)
  wired through plain/CONNECT/MITM paths; `ExplicitProxyConfig`
  `max_connections` threaded into the EggServe profile.
- `crates/eggreplay-cli` — optional `intercept` Cargo feature (default off;
  `[lints] workspace`); distinct `proxy` / `ca` clap namespaces:
  `proxy record` (listen/fixture/route/policy-file-or-allow/deny-host/
  default-action/CA-dir/non-loopback gate/redaction/tunnel+connection/
  body/leaf limits), `proxy validate` (dry-run normalized policy, no key
  material), `ca init|import|inspect|export|rotate` (no-overwrite everywhere,
  public-metadata-only output, explicit `--ca-dir` selection). Feature-off
  stubs emit the JSON envelope + `configuration` error ("interception
  support not compiled; rebuild with --features intercept"). Release-binary
  feature enablement deferred to M013F (default builds exclude intercept).
- `docs/interception-ca-trust.md` — manual public-CA install
  (macOS/Windows/Linux/Firefox/Python env/curl), removal/revocation, and
  pinning/H2/H3/untrusted-CA/policy-denial remediation. No trust-store
  mutation commands in code (audit-tested). `docs/cli.md` interception
  section documents the feature boundary.
- Python boundary preserved: default `eggreplay` wheel untouched by
  interception/CA deps (unit-tested); Python users drive a separately built
  CLI process.

Ownership respected: `eggreplay-intercept` remains a leaf; default
core/store/http/Python graphs contain no `rcgen`/interception (`cargo tree`
verified both modes).

## Evidence

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

All green after a toolchain-drift lint pass and a flaky-test fix (both
included in this closure tree, no behavior change):

- Clippy: ~30 pedantic errors under local rustc 1.98.1 (repo MSRV 1.89),
  mostly pre-existing `main.rs` / `cli_contracts.rs`
  (`struct_excessive_bools` on clap structs, `too_many_lines` on
  orchestration functions → targeted `#[allow]`; mechanical
  `format_push_string`/`items_after_statements`/`cast_*`/`redundant_closure`/
  `unused_async`+`needless_pass_by_value` fixed properly). New M013E files
  are clippy-clean.
- Flaky `ca_leaf::tampered_cert_breaks_fingerprint_binding`: raw-byte flip
  sometimes hit PEM armor (`CertUnparseable` vs `FingerprintMismatch`).
  Fixed with a deterministic base64-body flip accepting either error (both
  prove the binding rejects tampering); 5 consecutive isolated runs green.

Workspace suite: 288 tests passed (12 CLI unit + 12 `cli_contracts` + 8
`m013e_operator` + 36 core + 47 http + 16 http-qual + 64 intercept lib +
11 `ca_leaf` + 3 `m013e_proxy_stats` + 23 `mitm` + 23 `proxy_policy` + 12
substrate + 21 store), 0 failed. CLI suites: default 27, all-features 32.
Covers every plan bullet: feature-off capability message, feature-on
availability, loopback default, non-loopback gate, policy parse/validate +
unknown-version rejection, CA no-overwrite/export safety, machine-output
sentinel scans, exit-code consistency, start/shutdown/finalization,
no-trust-mutation audit, docs-vs-help consistency.

## Handoff

M013E satisfies its plan acceptance (bounded auditable CLI + policy files +
operator UX without expanding the default wheel or automating trust).
M013F becomes ready; M014 remains blocked on M013 closure.
