# M017 — Research: What Closing the Un-Terminated Bidirectional gRPC Deferral Actually Requires

Status: **research only** — no implementation. Prepared after M016 closed, to scope
the deferral that M015D and the M015 umbrella explicitly refused to absorb.

## Executive summary

**The deferral as written overstates the problem.** Its stated blocker was:

> Supporting the remaining case needs either a gateway that forwards request DATA
> while response DATA is still arriving, or a canonical model that records
> cross-direction ordering on one stream. Both are new canonical semantics.

Three findings, each verified against source:

1. **The gateway is already full-duplex.** EggFetch hands the streaming request
   body to `hyper_util::client::legacy::Client`, whose `ResponseFuture` resolves
   on response *headers* while the connection task pumps the request body
   concurrently. The transport blocker does not exist.

2. **A missing `grpc-status` is a spec-defined condition, not undefined
   behaviour.** tonic maps HTTP 200 + no `grpc-status` to
   `Code::Unknown` with a diagnostic message, and its source comment states the
   gRPC spec "defines [Unknown] as the appropriate code when a final status is
   absent or indeterminate."

3. **Therefore replaying a status-less fixture is faithful, not dangerous.** A
   client sees `Unknown` on replay; it saw `Unknown` live. The M015D worry — that
   serving a response with no terminal status is "worse than not replaying it" —
   does not hold up against what a real gRPC client does with it.

What genuinely remains is narrower, and one piece of it is a real defect of the
same class M016 just fixed.

## Finding 1 — the transport is already full-duplex

| Layer | Evidence |
|---|---|
| Gateway wraps the inbound body, unbuffered | `crates/eggreplay-http/src/recording.rs:1011` — `StreamBody::new(eggserve_body_stream(body))` |
| Gateway sends it upstream | `recording.rs:189` — `client.execute_http_body_default(upstream).await` |
| EggFetch delegates | `eggfetch-core-0.2.2/src/client.rs:463` → `pipeline::send_native_http_body` |
| EggFetch uses hyper legacy client | `eggfetch-core-0.2.2/src/transport/direct.rs:157` — `hyper_client.request(...)` |
| hyper resolves on headers | `hyper-util-0.1.21/src/client/legacy/client.rs:754` — `type Output = Result<Response<hyper::body::Incoming>, Error>` |

`ResponseFuture` yields the response once headers arrive; the request body keeps
being pumped by the connection's background task. The deferred test is itself
evidence: the upstream server replies per-message while the request half stays
open (`grpc_integration.rs:1874-1878`).

**What is true, and narrower:** the *recorder* never observes the interleaving.
`recording.rs:189` awaits the response before the frame loop at `:217`, and the
request tee at `:432-438` runs under hyper's task, not the recorder's. Two
`Instant::now()` origins (`:430` request, `:468` response) are never compared.

So the correct statement is: **the gateway is full-duplex; the recorder is
half-duplex in what it observes.** Those are different problems with different
costs.

## Finding 2 — the recorder already records *that* the call was cut off

This is the finding that most changes the framing. When the outbound deadline
fires, EggFetch surfaces it as a **body error**, not a clean end
(`eggfetch-core-0.2.2/src/body.rs:539-545` — `Poll::Ready(Some(Err(...)))`).

The recorder handles that explicitly and already writes a terminal event:

```rust
Err(error) => {
    push_stream_event(..., StreamEventKind::Error {
        offset: response_offset,
        category: "other".into(),
        phase: "body".into(),
    })?;
    response_failed = true;
    let _ = error;          // recording.rs:219-236
    break;
}
```

Because `response_failed` is set, the `End` event is *not* pushed
(`recording.rs:276-284`). So the fixture already distinguishes this call from a
completed one in the `stream-events` extension, in addition to the empty
`response.trailers` that the existing test asserts.

**The signal the deferral says is missing is present.** The M015D test only
checked trailers; it did not check stream events.

### The real defect hiding here

`let _ = error;` — the actual error is discarded and the category is hardcoded
to `"other"`. A deadline cut-off, a connection reset, and a decoder failure all
record identically. This is **the same defect class M016 just fixed** for dial
errors, where `map_fetch_error` sent every `DialErrorKind` to
`(Other, Other)` and made `ErrorCategory::ConnectionRefused` unreachable.

A future milestone that does not fix this will leave a gRPC fixture that says
"the body failed for unspecified reasons", which is not much better than the
status-less response it replaced.

## Finding 3 — a status-less response replays to a well-defined client outcome

Replay is status-agnostic and unguarded. Recorded trailers are cloned into a
future that returns `Ok(None)` when empty (`replay.rs:1009-1011`), so **no
trailer block is emitted**. There is no gRPC check in `replay.rs` at all, and
nothing synthesizes or refuses.

What a client does with that is well-defined. tonic
(`tonic-0.14.6/src/status.rs:790-830`):

```rust
trace!("trailers missing grpc-status");
...
// Per the gRPC-over-HTTP/2 protocol, grpc-status MUST be present in Trailers
// even when the HTTP status is 200 OK. A clean end-of-stream without a
// grpc-status trailer is therefore a protocol violation.
// ... We map this to Unknown, which the gRPC spec defines as the appropriate
// code when a final status is absent or indeterminate.
http::StatusCode::OK => {
    return Err(Some(Status::unknown(
        "protocol error: missing grpc-status trailer, stream was terminated \
         without a final status (possible truncation by a proxy or load balancer)",
    )));
}
```

So a replayed un-terminated bidi fixture yields a real, diagnostic
`Code::Unknown` — the same class of outcome the live client got when the gateway
timeout cut the call.

**This does not make the case supportable.** It makes the case *faithful*. The
honest position is that replay already reproduces the observed behaviour, and
the deferral's stated reason does not hold.

## What is genuinely still open

| # | Item | Status |
|---|---|---|
| 1 | **Body-error categorisation.** `let _ = error;` discards the cause and hardcodes `"other"`. | Real defect, same class as M016's dial-error mapping. **Recommend: own it.** |
| 2 | **Replay contract for a status-less gRPC response is unpinned.** No test asserts what a real client sees on replay. | Testable today, no semantic change needed. **Recommend: own it.** |
| 3 | **Cross-direction ordering** in `StreamEvents`. `delta_ns` is per-direction with independent origins (`stream.rs:68`; origins at `recording.rs:430` and `:468`). `validate()` iterates the two vectors independently (`stream.rs:152`). | Claim verified true. But see below — it is **not needed for this deferral**. |
| 4 | The gRPC view module has **no in-product callers** — `grpc.rs` is referenced only by its own tests. | True, and orthogonal. |

### On cross-direction ordering

The M015D reasoning conflated two things. Cross-direction ordering would be
needed to replay the *interleaving* of a completed bidirectional call. An
un-terminated call has no ending to be faithful about, and its recorded body is
already whole and in order. So item 3 is a real canonical-model milestone —
schema v2, additive field, cross-cutting consumers (`compare_stream_events`,
HAR, store) — but it is **not the missing piece for this deferral** and should
not be bundled with it.

## Recommendations

**Narrow the milestone.** Do not scope "support un-terminated bidi" as a
canonical-semantics project. Scope it as:

1. Classify body errors properly, reusing the `ErrorCategory`/`ErrorPhase`
   vocabulary M016 established, so a deadline cut-off is distinguishable from a
   reset. Fix the `let _ = error;` discard.
2. Pin the replay contract with a test that replays an un-terminated bidi
   fixture to a real tonic client and asserts `Code::Unknown` with the
   diagnostic — i.e. assert the behaviour is *faithful*, not that it is refused.
3. Reclassify the support matrix row from **deferred** to **supported
   (experimental)**, with the honest scope note: the call's *ending* is not
   recorded because it had none; its partial progress is.

**Explicitly out of scope:** cross-direction ordering (separate milestone),
and any synthesis of a `grpc-status` the upstream never sent. Synthesizing
`DEADLINE_EXCEEDED` is the smallest code change available and is the one thing
that should **not** be done — it fabricates an outcome, and it would hide the
very signal that makes the fixture truthful.

## Evidence quality

**Verified in source:** all `file:line` claims above, plus EggFetch 0.2.2,
hyper-util 0.1.21, and tonic 0.14.6 read from the local cargo registry.

**Not verified:** whether EggFetch's timeout could ever surface as a clean
`None` rather than an `Err` (the code paths found all produce `Err`, but the
transport layer was not exhaustively traced); any claim about how a *non-tonic*
gRPC client renders the status-less response.

**Untested claim worth stating plainly:** nobody has actually replayed an
un-terminated bidi fixture to a real client. Finding 3 is a code-reading
argument, not an observed result. Item 2 above is what would turn it into
evidence, and it is cheap.
