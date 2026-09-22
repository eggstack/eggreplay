# C001 — Lazy Replay Bodies and Bounded Fixture Materialization

Status: closed
Depends on: M001–M008 historical baseline
Corrective gate: v0.1 requalification

## Finding

The current `ReplayFixture::load()` eagerly calls `Session::read_blob()` for every recorded request and response body and retains response payloads as `Vec<Vec<u8>>`. Selected responses are cloned again into `ResponseBody::Bytes`.

This contradicts the original M004 bounded-memory/streaming acceptance criteria and makes memory consumption scale with aggregate fixture payload size rather than active traffic.

The matcher also stores materialized request bodies in every `MatchCandidate`, which creates the same aggregate-memory failure mode for large request fixtures.

## Objective

Make replay fixture loading metadata-bounded and stream selected response bodies directly from validated fixture storage while retaining deterministic matching semantics.

## Required design

### Store read seam

Add a narrow, validated blob streaming/open seam to `eggreplay-store`. Prefer an opaque/read-only handle or opened file over exposing unchecked arbitrary filesystem paths.

The seam must:

- validate the digest form and configured byte bound;
- reject symlinked blobs exactly as ordinary validation does;
- preserve length metadata;
- avoid allocating the full blob;
- remain usable by EggServe response streaming;
- document the fixture immutability/TOCTOU contract while a session is open.

Do not add Tokio to `eggreplay-store` solely for this. `eggreplay-http` may adapt a validated std file/handle into Tokio async IO.

### Replay response model

`ReplayFixture` retains flow metadata and body references, not response byte vectors. On a successful match, open the selected response body and return an EggServe `ResponseBody::Stream` with bounded chunking and recorded trailers.

Empty/absent bodies remain zero-allocation paths. HEAD and body-forbidden statuses continue to delegate normalization to EggServe.

### Matcher candidate bodies

Refactor exact-byte matching so candidate request bodies do not need to be eagerly materialized merely to prove equality. Exact matching can use authoritative body length + SHA-256 against the bounded actual request body hash.

Keep `eggreplay-core` filesystem-free. Introduce a semantic candidate-body descriptor such as absent/empty/digest metadata rather than a store handle in core.

Exact-text/semantic-JSON matching may materialize only the narrowed candidate body when that mode is explicitly requested. If a clean lazy callback/adapter cannot be added without contaminating core with storage concerns, keep those higher-cost modes in the HTTP/store adapter and document the boundary.

### Request body handling

Incoming replay requests may remain bounded-materialized for matching in v0.1, because EggServe already enforces `max_body_bytes`. The corrective requirement is that fixture-wide stored request bodies are not all resident simultaneously.

## Tests

Add deterministic tests that prove:

1. loading a fixture with multiple multi-megabyte response blobs does not read those blob bytes during `ReplayFixture::load()`;
2. a selected large response streams in bounded chunks and preserves exact bytes;
3. response trailers survive streaming replay;
4. an unselected corrupt/replaced body fails according to the documented validation/open contract rather than being silently served;
5. exact request-body matching works from digest/length metadata;
6. semantic JSON/text matching, if exposed through replay, materializes only the candidate(s) required by the selected mode;
7. two concurrent replay responses do not clone whole payloads.

Where feasible, add an allocator/RSS-independent structural assertion (for example instrumented blob reader/open counts) rather than a flaky process-memory threshold.

## Non-goals

No recorded-timing replay, SSE semantics, WebSockets, packed fixture format, or mmap optimization.

## Acceptance

- aggregate fixture response size does not determine `ReplayFixture` resident payload memory;
- selected bodies are streamed from validated content-addressed storage;
- strict exact-body matching does not require fixture-wide request body vectors;
- all existing matching/consumption semantics remain deterministic;
- docs/architecture.md and schema/replay docs describe the lazy storage boundary;
- closure record: `plans/closure/c001-lazy-replay-streaming.md`.
