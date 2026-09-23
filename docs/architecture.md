# Architecture and dependency policy

`eggreplay-core` owns semantic flow/conversation types and policies and has no
transport, Tokio, filesystem, CLI, or WebSocket codec dependency.
`eggreplay-store` owns `.eggr` files. `eggreplay-http` owns adapters and
orchestration while EggFetch owns outbound HTTP/TLS/framing/upgraded IO,
EggServe owns inbound H1 runtime/framing/lifecycle/tunnel handoff, and Eggress
owns optional listener-free route establishment. `eggreplay-cli` is a thin
presentation and orchestration layer.

## Lazy replay storage boundary (C001)

`ReplayFixture::load` is metadata-bounded: it retains flow records and
`CandidateBody` descriptors (`absent`/`empty`/`digest`) without reading blob
bytes. `eggreplay-store::Session::open_blob` returns an opaque validated
`BlobHandle` (digest form, byte bound, symlink rejection, exact length) with
an already-opened std file; `eggreplay-http` adapts it to Tokio async IO and
streams selected responses as `ResponseBody::Stream` in 64 KiB chunks with
incremental SHA-256 verification and recorded trailers. Empty/absent bodies
use `ResponseBody::Empty` with no allocation. Exact-byte matching compares
length + SHA-256 without materialization; semantic JSON/text modes materialize
only narrowed candidates via an explicit loader owned by the HTTP/store
adapter, keeping core filesystem-free. Fixtures must be treated as immutable
while a session or handle is open; concurrent blob replacement fails as
integrity errors rather than silent serving.

## Concurrent recording session (C002)

`RecordingSession` is the cloneable concurrent owner: `begin_blob` never
holds the flow-log mutex while body bytes stream to independent staging files,
and `append_flow` serializes only the final bounded JSONL write plus
flow-count/total-bytes accounting. No Tokio mutex spans the awaited upstream
transaction. No unbounded channel buffers whole flows/bodies. Aborted body
writers clean staging files via Drop. Shutdown stops admission, drains
in-flight tasks, then finalization fails closed if a sink remains active.

## Persistence-time redaction (C003)

The effective redaction policy is an explicit recording input and is persisted
by identifier. Header/query redaction applies before flow append. Structured
JSON/form bodies transform before any finalized blob exists; malformed,
oversized, or unsupported requested transformations fail closed. Redacted
request fields are matcher wildcards rather than literal placeholder
authority.

## Streaming semantics (M010)

The required-when-used `stream-events` extension carries bounded HTTP body
event/terminal semantics. Immediate replay applies terminal semantics with zero
delay; recorded/scaled timing is explicit. Candidate response stream
observation and stream/SSE semantic comparison are opt-in regression policies.

## WebSocket boundary (M011)

ADR 0006 defines WebSocket conversations as a separate required extension keyed
to the initiating HTTP Upgrade flow. EggReplay must use EggFetch's owned
post-101 stream and EggServe's generic tunnel IO; a maintained WebSocket codec
may parse messages over those already-owned streams but must not create its own
HTTP/TCP/TLS connection stack.

M011A is the dependency/upgrade gate. It must prove the substrate before M011B
adds semantic types.

## Dependency state

EggFetch 0.2.0 is consumed from crates.io and exposes the owned
`UpgradedStream` API needed for 101/CONNECT handoff.

EggServe's previous Git-pin rationale is stale: `eggserve-server 0.2.1` is now
published and registry-qualified for direct downstream embedding, resolving
with `eggserve-primitives 0.2.0`. M011A owns migration from the historical
EggReplay Git revision and must requalify tunnel/read-ahead/lifecycle behavior
from registry artifacts.

Eggress remains intentionally narrow. EggReplay enables only
`eggress-outbound/pproxy-compat`. Eggress 1.0.9 is currently upstream-blocked
on pooled route-isolation/metadata correctness work, so M011 must not adopt it
until that release is requalified. M011A may migrate the historical pin to a
safe published line (currently expected 1.0.8) only after existing route tests
and an upgraded-101 smoke pass with no direct fallback.

No default library feature enables Eggress, WebSocket codec, or TLS
interception. Direct HTTP remains the default acquisition route.
