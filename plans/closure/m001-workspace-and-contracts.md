# M001 closure — Workspace and Contract Skeleton

Status: closed

## Evidence

- Implementation commit: `3a292ef1b2a69e767c7a118ca451bb5b2a9c7588`.
- `eggfetch-core` 0.2.0 is consumed from crates.io with the native HTTP/1,
  Rustls, and native-root feature slice.
- `eggserve-primitives` and `eggserve-server` are pinned to
  `859a2bc2fe1d9e6215c122d19cd50f2f070d2da6`; `eggress-outbound` is pinned to
  `c493eab803654d1316150a9837fd01ee46b9b87d`. Neither required sibling
  surface was published as an independent crates.io package during preflight.
  The removal gate is documented in `docs/architecture.md`.
- The four-crate workspace, shared lints, MSRV metadata, release profile, MIT
  license, CI skeleton, configuration skeleton, and feature policy are present.
- `eggreplay-core` has no Tokio, filesystem, Hyper, EggFetch, EggServe, or
  Eggress dependency; `eggreplay-store` has no network dependency.

## Verification

```text
cargo fmt --all
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo run -p eggreplay-cli -- --help
cargo run -p eggreplay-cli -- --version
cargo check -p eggreplay-core --no-default-features
cargo check -p eggreplay-store --no-default-features
```

All commands passed on the implementation commit with Rust 1.98.1; the
workspace declares and CI exercises Rust 1.89.0.

## Unblocked next plan

M002 is ready. No current dependency decision blocks its transport-free flow
model and store work.
