# M013A closure — Interception Substrate, Dependency, and Threat Preflight

M013A is closed on revision `147655a88e2ba68573b3b89cbbb654186e24b20a`.

## Qualified dependencies

All interception dependencies are published registry releases and are pinned
in `Cargo.toml`/`Cargo.lock`:

- `eggfetch-core 0.2.0`
- `eggserve-primitives 0.2.0`
- `eggserve-server 0.2.1`
- `eggress-outbound 1.0.8` with only `pproxy-compat`
- latest published compatible `eggnet-tls 0.2.0` (queried from crates.io on
  2026-09-24)
- `rustls 0.23.45` (direct floor, patched floor required by EggServe)
- `tokio-rustls 0.26.2`
- `rcgen 0.13.2` (exact pre-1.0 pin, Rust 1.89-compatible dependency graph)

No `eggserve-core` or native TLS/OpenSSL dependency was added. Eggress remains
on the qualified 1.0.8 release; later source versions were not adopted.
`eggreplay-intercept` is a separate workspace leaf and no ordinary Rust product
crate or Python extension depends on it.

## Substrate proofs

`crates/eggreplay-intercept/tests/substrate.rs` provides hermetic local tests:

- a generated TLS server handshake captures version/SNI/ALPN metadata, creates
  truthful HTTPS `ConnectionContext::for_tcp`, and passes the decrypted stream
  to published EggServe `serve_http1_connection`; one canonical HTTP request
  receives its response and closes with a clean `ConnectionOutcome`;
- Eggress direct TCP and pproxy-compatible local HTTP CONNECT establish raw
  streams; a configured dead proxy fails while the destination is live, with
  no direct fallback;
- cancelling a live opaque route relay releases the connection;
- EggFetch succeeds with an explicitly supplied test CA, rejects that origin
  without trust, rejects a hostname mismatch, and sends the logical `localhost`
  SNI through the Eggress route adapter while the local proxy handles CONNECT.

The threat model is documented in `docs/interception-threat-model.md` and
records assets, boundaries, required controls, unsupported traffic, and
residual risks. CI now rejects `eggreplay-intercept` or `rcgen` in core, store,
HTTP, CLI, or Python dependency graphs.

## Qualification evidence

Hosted GitHub Actions [run 36011801512](https://github.com/eggstack/eggreplay/actions/runs/36011801512)
completed successfully for revision `147655a`. It passed:

- Ubuntu stable and Ubuntu Rust 1.89 workspace check, Clippy, and tests;
- macOS stable and Windows stable workspace check, Clippy, and tests;
- interception dependency-boundary checks, including the new leaf/CA graph
  assertions;
- all existing Python binding, wheel, and ABI3 jobs.

The local full workspace command sequence also passed: formatting, all-targets
all-features check, Clippy with warnings denied, and workspace tests (136
passed). The interception crate contributes four substrate integration tests.

## Decisions and support boundary

- Published caller-owned EggServe H1 serving is sufficient for decrypted
  streams; no unpublished upstream seam or Hyper implementation is needed.
- Eggress 1.0.8 supplies direct/routed raw TCP with configured-route failures
  preserved. The selected narrow compatibility grammar is sufficient for the
  M013 route use.
- EggFetch custom-root verification, hostname verification, and logical SNI
  remain independent of client-facing interception trust.
- Interception remains optional. HTTP proxying, CONNECT policy, CA storage,
  and interception behavior are not yet product capabilities until their
  subsequent M013 plans close.

M013B is ready. M013C–M013F and M014 remain blocked by their declared plan
dependencies.
