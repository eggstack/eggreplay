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

EggFetch 0.2.0 is consumed from crates.io. EggServe's generic runtime and
Eggress's listener-free connector are not yet published as independent
crates, so M001 pins the exact revisions used by v0.1. The removal gate is
that equivalent published crates exist with compatible generic H1, streaming,
trailer, lifecycle, and typed route-failure surfaces; then the Git revisions
must be replaced and the dependency matrix requalified.

No default feature enables Eggress or TLS interception. Direct HTTP is the
default acquisition route.
