# M021D — Listener and Request-Boundary Hardening

Status: **implemented** (local gate green; hosted CI and closure record pending)
Depends on: none (independent of M020; no `Cargo.lock` changes planned)
Closure record: `plans/closure/m021d-listener-and-request-boundaries.md` (not
created; requires hosted qualification of the committed revision)

## Why this plan exists

Two network trust boundaries are weaker than the interception stack's:

1. `serve` and `record` default to loopback but do not stop an operator from
   binding an **unauthenticated** replay server or recording gateway to
   `0.0.0.0`/`::`. The explicit proxy has a dedicated loopback gate; these two
   commands do not. TLS does not add authentication, so `--inbound-tls` does
   not make a public listener safe.
2. `execute_candidate` copies every recorded request header onto the candidate
   request, including `Host`, `Connection`, `Transfer-Encoding`, and
   `Content-Length`. The WebSocket candidate path already strips these. A
   malicious or malformed fixture can therefore set a `Host` that differs from
   the operator's `--target`, or inject hop-by-hop framing into a request the
   tool builds.

A third, lower-severity issue is route credential redaction: the current
`__`-split-then-`@`-scan can leave a fragment of a password that contains the
hop separator.

None of these is a remote code execution issue. They are boundary hygiene:
the tool should not silently expose an unauthenticated fixture/upstream, and a
fixture should not control request framing beyond the target's authority.

## Audit evidence

- `crates/eggreplay-cli/src/main.rs:217-218` — `serve` defaults to
  `127.0.0.1:0`, but there is no `is_loopback` gate in the serve/record
  dispatch.
- `crates/eggreplay-intercept/src/proxy.rs:241-250` — the interception proxy's
  `validate_bind` is the model to mirror.
- `crates/eggreplay-http/src/regression.rs:502-515` — every recorded header is
  appended to the candidate request, including `host` and framing headers.
- `crates/eggreplay-http/src/regression.rs:54-66` — the WebSocket path strips
  `host`, `connection`, `upgrade`, `content-length`, `transfer-encoding`, and
  the WebSocket handshake headers.
- `crates/eggreplay-http/src/eggress.rs:89-110` — `redact_route_credentials`
  splits on `__` before redacting userinfo, so a password containing `__` can
  leave a fragment visible.

## Track A — Loopback gate for `serve` and `record`

### Required behavior

Add an explicit `--allow-non-loopback` flag to both `serve` and `record`,
defaulting to `false`. The existing default bind remains `127.0.0.1:0`.

Validation:

- `bind.ip().is_loopback()` is always allowed;
- any other IP requires `--allow-non-loopback`;
- refusal is a `configuration` failure (CLI exit `2`) with a message that
  names the bind, states that the listener is unauthenticated, and states that
  TLS does not add authentication;
- validation runs before fixture existence checks, staging, or listener bind,
  consistent with the CLI's "configuration before filesystem" ordering;
- the flag is accepted by both commands, including `serve --record-mode
  re-record` and `record`.

Do not rely on a warning alone. The proxy's gate is a refusal, and this plan
keeps the two boundaries consistent. An operator who really wants a public
listener can pass the flag.

### Python bindings

`open_server` and the recording lifecycle accept a `bind` string. Add an
`allow_non_loopback: bool = False` keyword at the end of each signature
(keeping existing positional compatibility):

- parse the address as today;
- refuse a non-loopback address unless the keyword is true;
- raise the existing `ConfigurationError` with a value-free message;
- document the keyword and the unauthenticated-listener warning.

Tests: loopback accepted by default, `0.0.0.0`/`::` rejected by default,
allowed with the keyword, and an invalid address still raises the existing
parse error.

### Tests

- Unit-test a shared `check_listener_bind(bind, allow_non_loopback)` helper
  with `127.0.0.1`, `::1`, `0.0.0.0`, `::`, and a private LAN address.
- CLI tests: `serve --listen 0.0.0.0:0` and `record --listen 0.0.0.0:0` exit
  `2` with `failure_class == "configuration"`; with the flag they reach the
  bind path (use port `0` and shut down promptly).
- Ensure `--inbound http2`/`--inbound-tls` cannot bypass the gate.
- Python tests in `crates/eggreplay-python/tests/` for both lifecycle
  functions.

### Documentation

- `docs/cli.md`: add the flag to the `serve`/`record` surface and state that
  the listener is unauthenticated and TLS is not authentication.
- `docs/quickstart.md`: note that binding beyond loopback requires the
  explicit flag.
- Python docstrings/typing stubs: add the keyword and the warning.

## Track B — Regression header filtering

### Required behavior

Add one shared helper that classifies request headers as
hop-by-hop/framing/driver-controlled. Use it in both `execute_candidate` and
the WebSocket candidate path so the two cannot drift.

Strip at least:

`host`, `connection`, `proxy-connection`, `keep-alive`, `te`, `trailer`,
`transfer-encoding`, `content-length`, `upgrade`, `expect`,
`proxy-authorization`, `proxy-authenticate`.

- `Host` must be derived from the target, not from the fixture. If EggFetch
  does not set it from the URI, set it explicitly to `target_base.authority()`.
- `Content-Length`/`Transfer-Encoding` are recomputed from the `Full<Bytes>`
  body by the transport; a fixture value must never override them.
- `Proxy-Authorization` must never be forwarded to the target; it is a
  proxy-hop credential.
- Do not strip end-to-end fields such as `authorization`, `cookie`,
  `accept`, or `user-agent`. Those are part of the recorded request semantics
  and are already redacted according to the fixture's policy. (If their value
  is `<redacted>`, that is the fixture's chosen behavior.)
- Do not strip `forwarded`/`x-forwarded-*` in this corrective; the target is
  operator-chosen and forwarding recorded values is expected. A future plan can
  add a policy switch if needed.

### Tests

- Build a fixture whose request contains `Host: internal.example`,
  `Connection: keep-alive`, `Transfer-Encoding: chunked`,
  `Content-Length: 999`, `Proxy-Authorization: Basic SENTINEL`, and a normal
  `X-Trace` header.
- Run `test`/`diff` against a local target; assert the target sees:
  - `Host` equal to the target authority;
  - none of the hop-by-hop/framing/proxy headers;
  - `X-Trace` preserved;
  - a body length consistent with the actual body, not `999`.
- Assert `Proxy-Authorization: Basic SENTINEL` appears nowhere in the target
  request or diagnostics.
- WebSocket path: assert the same helper is used and its existing skip list is
  not weakened.
- A fixture with only ordinary headers still matches and executes.

## Track C — Route credential redaction without separator ambiguity

### Required behavior

Replace the `split("__")`-then-`rfind('@')` implementation with a scanner that
redacts each authority span before any hop splitting:

- find each `://` in the route string;
- within the authority span that follows (before the first `/`, `?`, or `#`,
  and before the next `://`), find the last `@`;
- replace everything between `://` and that `@` with `<redacted>`, preserving
  the `@`, the host/port, and any `__` hop separators outside the span;
- if a span has `://` but no `@`, leave it unchanged;
- if the string is malformed, fall back to `"<redacted-route>"` rather than
  returning a partially redacted string that might contain a secret.

This handles:

- `socks5://user:pa__ss@host:1080` — the whole `user:pa__ss` is redacted, not
  split into a hop;
- `http://user:p@ss@host:8080` — the last `@` is the authority delimiter, so
  the full `user:p@ss` is redacted;
- `socks5://u1:p1@h1__http://u2:p2@h2` — both spans redacted; `__` preserved
  between hosts;
- percent-encoded `%40` remains inside the redacted span;
- no credentials — unchanged.

Use the same helper for error messages and `physical_route.description` so a
diagnostic and a persisted description cannot disagree.

### Tests

- Each case above, asserting that no fragment of the password remains in the
  output.
- Two-hop chains with credentials in the first, second, or both hops.
- A route with `__` in a password and no second hop.
- Malformed routes (`scheme://@`, `://`, empty string) do not panic and do not
  echo a secret.
- Ensure `ProxyRoute::from_pproxy_uri` still fails closed for invalid routes;
  redaction must not turn an invalid route into a valid one.

## Acceptance

- No `serve`/`record`/Python server can bind a non-loopback address without an
  explicit opt-in, and the refusal is a `configuration` failure.
- The regression candidate never forwards fixture-controlled `Host` or
  hop-by-hop/framing/proxy headers.
- Route diagnostics and persisted descriptions never contain userinfo,
  including passwords with `__` or `@`.
- All existing transport behavior is unchanged for loopback targets and
  ordinary headers.
- Full locked workspace gate and the required hosted CI are green.
- `docs/cli.md`, `docs/quickstart.md`, and the Python stubs/docstrings are
  updated.

## Non-goals

- Adding authentication to the replay/recording server. The gate is exposure
  control, not an auth feature.
- A general header allow-list for regression; only driver-controlled and
  proxy-hop fields are stripped.
- Changing the Eggress route grammar or adding new route schemes.
- Trusting `Forwarded`/`X-Forwarded-*`; they remain unconsulted.

## Verification

Run the required locked workspace gate from `AGENTS.md`, plus:

```sh
cargo test -p eggreplay-cli
cargo test -p eggreplay-http
```

Python bindings are a separate loop: rebuild after any Rust change with
`maturin develop`, then run `python -m pytest tests` in
`crates/eggreplay-python`. Also exercise the listener gate manually with
`--listen 0.0.0.0:0`, confirming exit `2` without the opt-in and a successful
bind with it. Record the exact commands in the closure record.

## Implementation record (2026-11-18)

Implemented locally; not closed. The full locked workspace gate passed
(591 tests, 0 failures); HTTP 231, CLI 99 (`--all-features`), and Python 49
pytest cases pass after a `maturin develop` rebuild.

- **Track A.** `--allow-non-loopback` is on both `ServeArgs` and `RecordArgs`.
  `check_listener_bind` lives in `main.rs`, outside every `cfg`, and is enforced
  in `record()` and `serve()` immediately after inbound resolution and before
  fixture recovery, target classification, staging, or bind. The proxy's
  `check_listener_bind` is now a thin wrapper preserving its message. Default
  builds (no `intercept`) compile and pass the gate tests; `serve --help` and
  `record --help` show the flag. Manual results: `serve`/`record` with
  `0.0.0.0:0` exit `2`; `[::]` plus `--inbound http2` exits `2`;
  `--inbound-tls-*` cannot bypass; with the flag both bind on port `0`.
  Python's `replay_server` and `recording_gateway` take a trailing
  `allow_non_loopback=False`; `_lifecycle.open_server`/`fixture_context`
  forward it; the stubs and tests cover both.
- **Track B.** `DRIVER_CONTROLLED_REQUEST_HEADERS` and
  `is_driver_controlled_header` are shared by `execute_candidate` and the
  WebSocket candidate path; the WS-only handshake fields are layered on top.
  EggFetch/hyper do not synthesize `Host` client-side, so both paths set it
  explicitly from `target_base.authority()` (last `@` stripped). End-to-end
  tests assert the target sees its own Host, no hop-by-hop/proxy headers, a
  body length from the actual body, `X-Trace` preserved, and no
  `Proxy-Authorization` sentinel.
- **Track C.** `redact_route_credentials` scans each `://` span, ends at the
  first delimiter or next `://`, and redacts up to the last `@`. Cases covered:
  `__` inside a password, multiple `@`, two-hop routes with credentials in
  either or both hops, percent-encoded `%40`, no credentials, `scheme://@`,
  `://`, and a `://` inside unescaped userinfo (fails closed to
  `<redacted-route>`). Parsing still fails closed; redaction never validates an
  invalid route.
- **Note.** The WebSocket handshake now sends `Host`; this is a deliberate
  addition beyond replacing the skip list, and the existing WS test was
  extended to assert it.

### Accepted residual

A route segment such as `http://user:pa//ss@host` is left unchanged because the
span ends at the first `/` and is not a userinfo-bearing authority under URL
grammar.

### Remaining for closure

Commit and push; run the hosted lanes; create
`plans/closure/m021d-listener-and-request-boundaries.md` with the manual bind
commands, exit codes, and the pytest result.
