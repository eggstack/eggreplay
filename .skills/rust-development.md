# Rust Development

Use when writing, modifying, or reviewing Rust in the EggReplay workspace.

## Workflow

1. Read `AGENTS.md` for the index and the boundaries summary.
2. Start at `architecture/overview.md` for the whole system, then the deep dive
   for the crate you are touching (see Architecture References).
3. Only two crates declare features — `eggreplay-http` and `eggreplay-cli`.
   `core`, `store`, `har`, `intercept`, and `python` have no `[features]` table
   at all, which is itself a boundary: they cannot grow a capability flag.
4. Run the full gate from `.skills/verification-qualification.md` before
   committing.

## The boundary that matters most

**EggReplay never owns an HTTP or TLS stack.** Every wire concern is delegated
to EggFetch (outbound), EggServe (inbound), and Eggress (optional routing). Do
not add a parallel HTTP implementation, a raw `hyper::client::conn` path, or a
hand-rolled framing layer "just for one case". If a seam is missing, that is a
research task with a plan and a closure record, not a local workaround.

- `eggreplay-core` — no transport, no Tokio, no filesystem, no CLI parsing.
  If you add a `tokio` or `hyper` edge here, CI fails in the
  `dependency-boundary` lane.
- `eggreplay-store` — same: filesystem yes, transport no.
- `eggreplay-har` — no network I/O at all; it projects fixtures to and from
  documents.
- `eggreplay-intercept` — a **leaf**. No product crate and no Python build may
  depend on it. It never adopts the multiprotocol serving layer, so
  interception can never silently gain HTTP/2.
- `eggreplay-python` — a **leaf**. Rust product crates must not depend on it,
  and it must not depend on the interception capability.

## Feature discipline

```toml
# eggreplay-http
default = ["direct"]                       # the ONLY default
direct        = []                          # marker profile, admits nothing
eggserve      = ["dep:eggserve-primitives", "dep:eggserve-server"]
websocket     = ["dep:base64", "dep:tokio-tungstenite"]
h2            = ["eggfetch-core/native-http2"]            # OUTBOUND only
h2-inbound    = ["eggserve", "dep:eggserve-core", "eggserve-core/http2"]
h2-inbound-tls = ["h2-inbound", "eggserve-core/tls", "dep:eggnet-tls"]
grpc          = ["dep:prost-reflect"]
eggress       = ["dep:eggress-outbound", "eggress-outbound/pproxy-compat"]
```

- **`h2-inbound` is the only thing that may admit `eggserve-core`**, and with it
  the `eggserve-static` closure and the Hyper/h2 protocol graph. It is never
  default. ADR 0010's alternative — making `eggserve-core` a default to get
  inbound H2 — was rejected for exactly this reason.
- **`h2` is outbound-only and independent of `h2-inbound`.** An operator may
  record over H2 while serving H1, or the reverse. Never make one imply the
  other.
- **No HTTP/2 capability is default in any profile**, including the Python
  wheel and the interception graph.
- **`eggress` enables `pproxy-compat` only.** Never enable Eggress extended,
  SSH, QUIC, listener, or server surfaces.
- **`tonic` is a dev-dependency of `eggreplay-http` only.** An independent
  gRPC implementation must participate in qualification as a *test* peer; no
  product graph may gain a gRPC stack.
- The CLI's `default = []`. Its dependency line on `eggreplay-http`
  unconditionally adds `eggserve`, `eggress`, and `websocket` — those three are
  always in the CLI graph, which is why the inbound-H2 opt-in must not
  disturb them.

When you add a profile, check it on **both** sides of the boundary. A feature
that compiles only in the all-features graph has not been tested against the
default graph, and that is exactly the M015B regression.

## Invariants a reviewer will check

These are load-bearing. Breaking one is silent — the code still compiles and
most tests still pass.

1. **Redaction precedes blob finalization.** Structured JSON/form bodies
   transform before any finalized blob exists; malformed, oversized, or
   unsupported requested transformations fail closed. Redacted request fields
   become matcher **wildcards**, not literal placeholders.
2. **Fixture immutability while open.** Concurrent blob replacement must fail
   as an integrity error, never serve stale or partial bytes.
3. **Matcher consumption.** A flow that matches but cannot be consumed is a
   **near-miss**, not a silent repeat and not a silent skip.
4. **Fail closed, never downgrade.** An unbuildable protocol policy, an
   unknown required extension, a missing operator identity, a dead route — all
   are explicit errors. There is no silent fallback to a weaker protocol, and
   no ignore-required-extension switch.
5. **No network on a replay miss.** Offline replay never falls back to direct.
6. **Bounds are enforced at both ends.** Extension size/count limits are
   checked when writing *and* when opening.
7. **A candidate records the baseline request.** Target remapping is a
   transport concern; the observed flow keeps the recorded request verbatim so
   a later report cannot compare against something silently rewritten.

## Error classification

`eggreplay-core::error` is the stable cross-layer taxonomy. Every adapter maps
its failure into an `ErrorPhase` × `ErrorCategory` pair.

- `ErrorPhase`: `request`, `connect`, `tls`, `headers`, `body`, `timeout`,
  `cancelled`, `policy`, `other` — *where* it broke.
- `ErrorCategory`: `dns`, `connection_refused`, `unreachable`, `tls`,
  `protocol`, `policy`, `timeout`, `cancelled`, `other` — *what* broke.

The axes are deliberately different; `protocol` is a category, not a phase.
Never collapse a typed failure to `Other` to make a test pass — M016 existed
because body-stream errors were hardcoded to `("other", "body")` with the
underlying error discarded, so a deadline, a reset, and a protocol violation
recorded identically. `FlowError` messages truncate at 512 bytes.

## Style

- Edition 2024, MSRV 1.89.
- Workspace lints: `unsafe_code = "forbid"`, `missing_docs = "warn"`, clippy
  `all` + `pedantic` at warn with `-D warnings` in CI. Never add `#![allow(warnings)]`
  or a blanket pedantic allow — use a specific lint name with a justifying comment.
- **Caveat worth knowing:** `[lints] workspace = true` is opt-in per crate, and
  only `eggreplay-cli`, `eggreplay-har`, `eggreplay-intercept`, and
  `eggreplay-python` declare it. `core`, `store`, and `http` — the three
  crates with the most surface — do **not** currently inherit the workspace
  lint table. `cargo clippy --workspace -- -D warnings` still denies the
  default warning set everywhere, but the pedantic/`forbid` policy does not
  reach those three. Do not assume a new crate inherits the policy; declare it.
- Public items need doc comments.
- Dependencies: every Eggstack and TLS artifact is pinned exactly (`=`) except
  `eggfetch-core`, which is a caret range held by `Cargo.lock`. A transport
  version bump is a plan with a closure record, not a `cargo update`.

## Architecture References

| Crate | Deep dive |
|---|---|
| `eggreplay-core` | [`architecture/02-core-semantic-model.md`](../architecture/02-core-semantic-model.md) |
| `eggreplay-store` | [`architecture/03-store-persistence.md`](../architecture/03-store-persistence.md) |
| `eggreplay-http` recording | [`architecture/04-http-recording.md`](../architecture/04-http-recording.md) |
| `eggreplay-http` replay/serving | [`architecture/05-http-replay-and-serving.md`](../architecture/05-http-replay-and-serving.md) |
| `eggreplay-http` regression | [`architecture/06-regression-and-reporting.md`](../architecture/06-regression-and-reporting.md) |
| Protocol + routing tiers | [`architecture/07-protocol-and-routing-tiers.md`](../architecture/07-protocol-and-routing-tiers.md) |
| Boundaries, features, CI lanes | [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md) |
