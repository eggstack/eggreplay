# M013C closure — Interception CA Lifecycle and Leaf Issuance

M013C is closed on implementation revision `d39f4b7` (working tree; M013B
sources likewise uncommitted per its closure note) with local verification
green. Hosted qualification is deferred to M013F per the M013 closure plan;
M013C required tests are local-only by plan.

## Implementation

Leaf crate `eggreplay-intercept` (remains a leaf; no reverse dependencies):

- `src/ca.rs` — dedicated CA lifecycle in caller-selected directories
  (`metadata.json` + `ca-cert.pem` + `ca-key.pem`, never `.eggr`):
  `CaAuthority::{create_new, import, open}`, `CaOptions` (CN bounded to 128
  chars, validity 31..=1825 days, 365-day default), public `inspect_ca` /
  `export_ca_cert` paths that never touch the key, explicit
  `repair_ca_permissions`, redacted `CaError` (no key bytes, key paths, PEM
  contents, or source locations in any message) and redacted `Debug`.
  Generation is ECDSA P-256 via `ring` through `rcgen` (self-signed root,
  `BasicConstraints CA:true` with pathlen 0, KU
  `digitalSignature/keyCertSign/cRLSign`, no SAN, 63-bit positive process-
  unique serials). Import enforces the 64 KiB PEM bound, exactly one key and
  one certificate, `eggnet-tls::parse_identity_pem` pairing, self-signed
  root (subject `==` issuer plus cryptographic self-signature proof),
  `CA:true`, `keyCertSign`, ECDSA-P-256/SHA-256 on both key and certificate,
  and expiry with 5-minute clock tolerance; sources are verbatim-copied and
  never modified. Staged atomic publish (sibling temp dir, `create_dir`
  claim so existing CA dirs are never overwritten, same-filesystem renames).
  Unix modes `0700`/`0600`/`0644` enforced on open with an explicit repair
  action; Windows behavior is documented as unprovable-without-new-OS-deps
  (files created with default sharing, no enforcement, repair unsupported).
  `open` revalidates structure, fingerprint binding, pairing, and permissions
  but not expiry, so expired CAs stay inspectable/exportable during rotation.
  Rotation is a new directory/identity; handles are immutable snapshots and
  active handles are never invalidated. No trust installation, no private
  export (export refuses to overwrite its destination).
- `src/leaf.rs` — bounded in-memory issuer: `LeafIssuer::new(ca, options)`
  takes explicit ownership of the selected CA; `issue(&NormalizedHost)`
  mints exact-SAN-only leaves (`ExplicitNoCa`, `digitalSignature`,
  `serverAuth` EKU, AKI, default 7-day / max 30-day / min 1-hour validity
  always capped by CA expiry, process-unique serials, memory-only P-256
  keys). Cache key is CA fingerprint + target, max 128 entries, deterministic
  FIFO eviction, expiry-aware reuse/removal. One `tokio::sync::Mutex` guards
  the cache and is held across the fast local signing step only (no network
  or disk I/O under the lock), so concurrent same-target requests share a
  single issuance. Redacted `Debug`/errors. `INTERCEPT_ALPN_HTTP1_1`
  (`http/1.1`) is published for M013D; no `ServerConfig` is built yet.
- `src/lib.rs` — module wiring, re-exports, substrate additions
  (`X509_PARSER 0.16.0`, `TIME 0.3.55`); `forbid(unsafe_code)` retained.

Ownership respected: `rcgen` owns generation/signing, `eggnet-tls` owns
pairing, `x509-parser 0.16` (new direct leaf-only dep, read-only validation
and displays) owns certificate-property checks — no custom DER parser, no
second TLS stack. `rustls` types are used only via the existing dependency
(plus `eggnet-tls`); no `rustls-pki-types` direct dep was needed. `rcgen`
gained the `x509-parser` feature (workspace, only consumed here) to support
`CertificateParams::from_ca_cert_der` for the reopened/imported issuer
descriptor, whose self-signature is never published. New workspace deps
`time 0.3` (validity arithmetic) and direct `x509-parser 0.16`; `sha2`,
`serde`, `serde_json`, `tempfile`, `chrono` promoted into
`eggreplay-intercept` main deps (all already in the lock). Lock delta is the
`x509-parser` subtree only (`asn1-rs`, `der-parser`, `nom`, `oid-registry`,
and small transitive deps).

## Evidence

Local verification on the implementation tree:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
```

All green. Workspace suite: 223 tests passed (baseline 188 + 24 new
`eggreplay-intercept` lib unit tests + 11 new `ca_leaf` integration tests;
substrate 12 and `proxy_policy` 23 unchanged):

- initialize/reopen identity preservation; create_new/no-overwrite with
  originals untouched; invalid/mismatched/non-CA/expired/not-yet-valid
  imports rejected without publishing; Unix modes enforced, insecure key
  rejected, explicit repair reopens; operator sources never chmodded;
  Windows documented-behavior test (conditional);
- public export holds exactly the certificate (no key, refuses overwrite;
  key-independent export path proven by exporting with broken key perms);
  tampered cert breaks the fingerprint binding; rotation identities distinct
  while the old handle keeps issuing;
- DNS/IPv4/IPv6 exact SANs, no wildcard, `serverAuth` EKU +
  `digitalSignature` KU, issuer == selected CA subject, leaf validity
  bounded by policy and CA expiry, serial uniqueness (1024-sample);
- leaf verifies under its CA with a real `rustls` WebPKI verifier (wrong
  name fails), fails under an unrelated CA, imported-CA handle issues
  verifiable leaves (reconstructed issuer descriptor path);
- cache hit shares one `Arc` (single issuance), FIFO eviction at 128
  entries with re-mint on demand, CA fingerprint in the cache key, 32-task
  concurrent same-target race issues exactly once, expired-entry removal;
- sentinel scans (key payload slice) over metadata, exports, `Debug`,
  error strings, staging/parent walk, plus `.eggr` absence: no
  `PRIVATE KEY` or sentinel leakage; every `CaError`/`LeafError` Display
  asserted free of PEM markers and key paths.

## Handoff / blockers for M013D

M013C unblocks M013D with this surface: `LeafIssuer` owns its selected
`CaAuthority`; `LeafCertificate::{cert_der, cert_pem, key_pair}` plus
`CaAuthority::{cert_der, cert_pem}` supply everything M013D needs to build
per-SNI `rustls` server configs (chain `[leaf, CA]`, ALPN
`INTERCEPT_ALPN_HTTP1_1` only). Deliberately left to M013D: `ServerConfig`
construction, the `CONNECT` `intercept` policy action with SNI/authority
coherence, and any listener wiring. Known limitations carried forward:
serial uniqueness is per-process (documented; multi-process issuers sharing
one CA dir could theoretically collide — use one issuer per process); no
OCSP/CRL plumbing; trust installation stays an external operator action;
Windows CA secrecy rests on operator profile ACLs (documented, untested
here — no Windows runner in this environment).
