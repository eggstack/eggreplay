# CLI and exit contracts

Every result-producing command accepts `--output human|json|junit` (JUnit is
meaningful for `replay`/`test`/`diff`; other commands project a single
assertion). JSON is a versioned envelope (`command`, `schema_version`,
`success`, `failure_class`, `warnings`, `payload`); stdout carries JSON/JUnit,
stderr carries `{failure_class}: {message}` diagnostics that agree with the
envelope. Human rendering is terminal text (`ok`/`failed (...)`), never machine
JSON, and is not a parsing contract.

Stable exit codes (compatibility contract):

- `0` success (`replay` reports differences with `0`; `test`/`diff` assert);
- `1` regression/assertion mismatch (`replay` findings are reported, `test`
  enforces with `regression`, `diff` with `diff`);
- `2` invalid CLI/config/policy (including malformed `--route`, no
  fallback-to-direct);
- `3` invalid/corrupt fixture;
- `4` network/runtime execution failure;
- `5` internal/unexpected failure.

`record`, `replay`, and `test` accept a common `--route` (`direct` default, or
a pproxy URI such as `socks5://127.0.0.1:1080` and two-hop
`socks5://...__http://...`). Non-direct routes use `EggressDialer` via
EggFetch's custom Dialer seam; redaction-safe `physical_route` metadata is
recorded in flows. `inspect --bodies` performs an explicit bounded body read
(`--max-body-bytes`, default 64 KiB, truncates with counts): UTF-8 text when
valid, otherwise length + digest (bounded base64 only with `--bodies-base64`).
Stored bodies are already redacted, so inspection never bypasses markers.
