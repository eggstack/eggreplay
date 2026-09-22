# Architecture and dependency policy

`eggreplay-core` owns semantic flow types and policies and has no transport,
Tokio, filesystem, or CLI dependency. `eggreplay-store` owns `.eggr` files.
`eggreplay-http` owns adapters and orchestration while EggFetch owns outbound
HTTP/TLS/framing, EggServe owns inbound H1 runtime/framing/lifecycle, and
Eggress owns optional listener-free route establishment. `eggreplay-cli` is a
thin presentation and orchestration layer.

## Lazy replay storage boundary (C001)

`ReplayFixture::load` is metadata-bounded: it retains flow records and
`CandidateBody` descriptors (`absent`/`empty`/`digest`) without reading blob
bytes. `eggreplay-store::Session::open_blob` returns an opaque validated
`BlobHandle` (digest form, byte bound, symlink rejection, exact length) with
an already-opened std file; `eggreplay-http` adapts it to Tokio async IO and
streams selected responses as `ResponseBody::Stream` in 64 KiB chunks with
incremental SHA-256 verification and recorded trailers. Empty/absent bodies
use `ResponseBody::Empty` with no allocation. Exact-byte matching compares
length + SHA-256 without materialization; semantic JSON/text modes
materialize only narrowed candidates via an explicit loader owned by the
HTTP/store adapter, keeping core filesystem-free. Fixtures must be treated
as immutable while a session or handle is open; concurrent blob replacement
fails as integrity errors rather than silent serving.

## Concurrent recording session (C002)

`RecordingSession` is the cloneable concurrent owner: `begin_blob` never
holds the flow-log mutex while body bytes stream to independent staging
files, and `append_flow` serializes only the final bounded JSONL write plus
flow-count/total-bytes accounting. No Tokio mutex spans the awaited upstream
transaction, so gateway requests execute upstream simultaneously up to
EggServe/EggFetch limits. No unbounded channel buffers whole flows/bodies.
Flow IDs embed start milliseconds plus a random UUID, unique under
concurrency; chronology stays in start/completion timestamps with JSONL
append order as the tie-breaker (no schema change). Aborted body writers
clean staging files via Drop, so failed/cancelled transactions never leave a
manifest reference to an incomplete blob. Shutdown policy: `shutdown` stops
admission, `ServerHandle::shutdown` + `wait` drains in-flight gateway tasks,
then `finish` fails closed if any sink remains active rather than racing.

## Persistence-time redaction (C003)

The effective redaction policy (`RedactionConfig` from `RedactionProfile` plus
CLI `--redact-header/--redact-query/--redact-json-path`) is an explicit
recording input, persisted by identifier in `SessionMetadata.redaction_profile`
and inspectable via `inspect` (markers only, never secret values). Secure
defaults (Authorization, Proxy-Authorization, Cookie, Set-Cookie) are preserved
unless `--unsafe-replace-default-redaction` explicitly replaces them. URL
userinfo never reaches persisted authority or diagnostics. Header/query
redaction applies before flow append with typed markers. Structured JSON/form
bodies buffer boundedly (`DEFAULT_MAX_STRUCTURED_REDACTION_BYTES`, 1 MiB) and
transform before any finalized blob exists; oversized, malformed, or unsupported
media types with requested redaction fail closed without publishing raw bytes
or appending flows. Body transforms reconcile framing: `Content-Length`
recomputed, `Content-MD5`/`Digest`/`Signature` and strong ETags removed with
markers, weak ETags preserved with warning. Redacted request fields are
matcher wildcards (headers/query ignored, JSON paths ignored semantically),
never literal `"<redacted>"`. Opaque binary secret discovery is explicitly
out of scope. Cookie redaction remains whole-value for v0.1; selective
cookie-name parsing is not implemented.

EggFetch 0.2.0 is consumed from crates.io. EggServe's generic runtime and
Eggress's listener-free connector are not yet published as independent
crates, so M001 pins the exact revisions used by v0.1. The removal gate is
that equivalent published crates exist with compatible generic H1, streaming,
trailer, lifecycle, and typed route-failure surfaces; then the Git revisions
must be replaced and the dependency matrix requalified.

No default feature enables Eggress or TLS interception. Direct HTTP is the
default acquisition route.
