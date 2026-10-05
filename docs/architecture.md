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

M011A closed after qualifying registry EggServe 0.2.1, EggFetch 0.2.0 direct
and Eggress-routed HTTP/1.1 101 upgrades, and the no-direct-fallback path.
M011B owns transport-neutral conversation semantics and an optional
`eggreplay-http/websocket` codec adapter over already-owned streams.

The recording gateway is opt-in with `--websockets` in `record` and
network-capable `serve --record-mode once|re-record`. It validates H1 Upgrade,
uses EggFetch for the upstream 101, and tunnels through EggServe. Conversation
messages are bounded and stored in the required `websocket-messages`
extension; text/binary whole-message redaction is explicit, and configured
JSON Pointer redaction runs before payload publication. Offline replay matches
the initiating HTTP flow and scripts the ordered transcript through the same
EggServe tunnel. Candidate regression executes that transcript through the
same EggFetch client and optional Eggress route.

M011 does not claim WSS, inbound TLS interception, H2 Extended CONNECT, H3,
negotiated WebSocket extensions, or frame-layout fidelity. `append-new` does
not acquire new WebSocket conversations.

## Dependency state

Current declared state (root `Cargo.toml`). Every Eggstack and TLS artifact is
pinned **exactly** except `eggfetch-core`, which is a caret range held by
`Cargo.lock`:

| Dependency | Pin | Role |
|---|---|---|
| `eggfetch-core` | `0.2.2` (caret) | The only outbound client; exposes the owned `UpgradedStream` API needed for 101/CONNECT handoff. |
| `eggserve-primitives` | `=0.2.2` | Inbound request/response primitives. |
| `eggserve-server` | `=0.4.0` | The only server runtime; the default H1 path. |
| `eggserve-core` | `=0.4.0` | **Optional.** The multiprotocol composition layer, admitted only by `h2-inbound`/`h2-inbound-tls` (ADR 0010). |
| `eggress-outbound` | `=1.0.11` | `pproxy-compat` only; listener-free outbound routing. |
| `eggnet-tls` | `=0.2.0` | TLS pairing / cert-property checks. |
| `rcgen` | `=0.13.2` | CA and leaf generation, inside `eggreplay-intercept` only. |

The exact pins hold while those APIs remain pre-1.0. A transport version bump
is a plan with a closure record, not a `cargo update`.

Ordinary record/replay gateways remain `OriginOnly` with EggServe-owned policy
and admission; the M013B interception helper opts into `OriginOrAbsolute` while
retaining EggServe-owned bounds and one tunnel-admission authority. The
`protocol-boundary` CI lane asserts that no ordinary profile — default, direct,
H1, `h2` outbound, interception, or Python — acquires `eggserve-core`,
`eggserve-static`, or the H2 protocol graph, and that interception never adopts
it at all.

Eggress remains intentionally narrow: only `eggress-outbound/pproxy-compat` is
enabled. No default library feature enables Eggress, the WebSocket codec, or TLS
interception. Direct HTTP remains the default acquisition route.

Tonic is a **dev-dependency** of `eggreplay-http` only. It qualifies gRPC
against an independent implementation and must never enter a product graph; the
`protocol-boundary` lane asserts this across every supported profile.

For the per-crate feature matrix, the boundary rules, and the CI lanes that
enforce all of the above, see
[`../architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md).
