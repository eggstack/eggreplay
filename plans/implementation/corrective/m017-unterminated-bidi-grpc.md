# M017 — Un-Terminated Bidirectional gRPC: Faithful Recording, Classified Body Errors, and a Pinned Replay Contract

Status: **closed** — see `plans/closure/m017-unterminated-bidi-grpc.md`

Depends on: M016 closure, M015D closure

Research: `plans/research/m017-bidirectional-grpc-deferral-research.md`

## Why this exists

M015D and the M015 umbrella deferred un-terminated bidirectional gRPC, on the
stated grounds that closing it needs "either a gateway that forwards request
DATA while response DATA is still arriving, or a canonical model that records
cross-direction ordering on one stream — both are new canonical semantics."

The research found that premise does not hold. Three things, each verified
against source:

1. **The gateway is already full-duplex.** EggFetch hands the streaming request
   body to `hyper_util::client::legacy::Client`, whose `ResponseFuture` resolves
   on response *headers* while the connection task pumps the request body
   concurrently (`hyper-util-0.1.21/src/client/legacy/client.rs:754`). The
   transport blocker does not exist. What is true but narrower: the *recorder*
   never observes the interleaving (`recording.rs:189` awaits before the frame
   loop at `:217`).

2. **The "missing terminal status" signal is already recorded.** The outbound
   deadline surfaces as a response-body *error*
   (`eggfetch-core-0.2.2/src/body.rs:539-545`), and the recorder pushes a
   `StreamEventKind::Error` terminal event and suppresses `End`
   (`recording.rs:219-236`). M015D's test only asserted on trailers.

3. **Replaying a status-less fixture is faithful, not dangerous.** Replay emits
   no trailer block when the recorded trailers are empty
   (`replay.rs:1009-1011`), and tonic maps HTTP 200 + no `grpc-status` to
   `Code::Unknown` with a diagnostic, which its own comment says is what the
   gRPC spec defines for an absent final status
   (`tonic-0.14.6/src/status.rs:790-830`).

So there is no canonical-semantics project here. There is one real defect, one
unpinned contract, and one support-matrix correction.

## 1. Body-stream errors are categorised as `Other` — the M016 defect class, one layer down

### Finding

Three sites hardcode `category: "other".into(), phase: "body".into()`, and two
of them discard the actual error:

| Site | Direction | Error type | Disposition |
|---|---|---|---|
| `recording.rs:227` | response | `eggfetch_core::Error` | **classify** |
| `recording.rs:481` | response | `eggfetch_core::Error` | **classify** |
| `recording.rs:2185` | request (tee) | generic inbound `B::Error` | keep `Other` — see below |

```rust
Err(error) => {
    push_stream_event(..., StreamEventKind::Error {
        offset: response_offset,
        category: "other".into(),   // hardcoded
        phase: "body".into(),
    })?;
    let _ = error;                  // discarded
    response_failed = true;
    break;
}
```

A deadline cut-off, a connection reset, and a protocol violation all record
identically. **This is the same defect M016 just fixed for dial errors**, where
`map_fetch_error` sent every `DialErrorKind` to `(Other, Other)` and made
`ErrorCategory::ConnectionRefused` unreachable in the product. It survived one
layer down because nobody looked there.

### Scope

One classifier in `eggreplay-http`, used by both response sites:

| `eggfetch_core::Error` | `ErrorCategory` | `ErrorPhase` |
|---|---|---|
| `is_timeout()` | `Timeout` | `Timeout` |
| `Protocol`, `Decompression` | `Protocol` | `Body` |
| `Io`, `Hyper`, `HyperClient` | `Unreachable` | `Body` |
| everything else | `Other` | `Body` |

`ErrorCategory`/`ErrorPhase` need `as_str()` to reach
`StreamEventKind::Error`'s bounded string fields — an additive core API, no
serialized change (both enums are already `#[serde(rename_all = "snake_case")]`
and the stream validator only bounds those strings to 64 chars).

**The request tee keeps `Other` deliberately.** Its error is a generic inbound
`eggserve` body error with no EggFetch category to consult. Claiming a category
there would be a guess. The honest record is `Other`/`Body`.

## 2. The replay contract for a status-less gRPC response is unpinned

No test asserts what a real gRPC client sees when a fixture with no
`grpc-status` is replayed. The M015D deferral rested on the belief that such a
replay is "worse than not replaying it" — which is a claim about client
behaviour that was never tested.

This milestone turns the code-reading argument into evidence: replay an
un-terminated bidi fixture to a real tonic client and assert it receives
`Code::Unknown` with the protocol diagnostic. That is the *faithful* outcome —
it is what the live client saw when the gateway timeout cut the call — and
pining it is what makes the row legitimately "supported" rather than "deferred".

## 3. Support matrix correction

The M015 umbrella records `Bidirectional, un-terminated` as **deferred**. On
the evidence above that was a scoped refusal made on a premise that does not
hold. The row becomes **supported (experimental)**, with the honest scope note:
the call's *ending* is not recorded because it had none; its partial progress
is recorded whole, and the reason it stopped is in `stream-events`.

## Non-goals

- **No synthesized `grpc-status`.** Writing `DEADLINE_EXCEEDED` into the
  trailers is the smallest available change and it fabricates an outcome the
  upstream never sent, hiding the very signal that makes the fixture truthful.
- **No cross-direction ordering.** The research confirmed `delta_ns` is
  per-direction with independent origins (`stream.rs:68`; origins at
  `recording.rs:430` and `:468`). Making it shared is a real canonical
  milestone — schema v2, additive field, cross-cutting consumers
  (`compare_stream_events`, HAR, store) — but an un-terminated call has no
  ending to be faithful about. It is a separate plan.
- **No change to the recorder's half-duplex observation.** Making the recorder
  observe interleaving is the same schema-v2 work.
- **No change to `grpc.rs`.** The view already degrades a missing status to
  `status: None` and is not in the record/replay path.
- No change to `Flow::validate()`, the store validator, or exit codes.

## Acceptance criteria

- A response body cut off by the outbound deadline records
  `category: "timeout"`, not `"other"`, in `stream-events`.
- A response body that ends in a transport failure is not recorded as
  `timeout`.
- The request-direction tee is unchanged and its `Other` is documented as
  honest rather than an oversight.
- Replaying an un-terminated bidirectional fixture to a real tonic client
  yields `Code::Unknown` with the missing-`grpc-status` diagnostic.
- The recorded fixture for that call contains no `grpc-status` **and** a
  terminal `Error` stream event, so both "it did not finish" and "why" are
  readable.
- A terminated bidirectional call is unchanged: still records, still replays
  with its real terminal status.
- The support matrix records un-terminated bidi as supported (experimental),
  in the M015 umbrella, the M015D closure, `docs/grpc-and-faults.md`, and M016's
  non-goal note is annotated as resolved.
- The full workspace gate is green on Linux, Linux MSRV, macOS, and Windows.

## Evidence

- `plans/research/m017-bidirectional-grpc-deferral-research.md`
- `plans/closure/m015d-grpc-over-http2-integration-qualification.md`
- `plans/closure/m016-post-m015-corrective.md`
