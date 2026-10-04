# M016 — Post-M015 Corrective: Bounded Outbound Time, Route Error Attribution, and the WebSocket Shutdown Race

Status: **closed** — see `plans/closure/m016-post-m015-corrective.md`

Depends on: M015E closure, M015D closure

## Why this exists

M015E recorded three items as "candidate fixes for a future milestone: each
needs its own evidence". That milestone is this one. Two of the three turned
out, on investigation, to be more than a follow-up:

- The outbound timeout is not a mis-shaped flag. **The CLI sets no timeout at
  all**, so the hardening finding understated it.
- The WebSocket flake is not a flaky test. It is a test racing a documented
  runtime behaviour, and the product's response to that race is correct.

Nothing here changes a support claim, a canonical schema, or a matcher
dimension. This is a corrective milestone for three specific defects and one
deliberate contract test.

## 1. The CLI has no request timeout

### Finding

`eggfetch_core::Timeout::default()` has every field `None`, and the pipeline
resolves an absent timeout with `request_timeout.unwrap_or_default()`. No
deadline is therefore set at any phase.

`crates/eggreplay-cli/src/main.rs` never calls `ClientBuilder::timeout`. Every
network-capable CLI command — `record`, `replay`, `test`, and `serve` in its
recording modes — therefore has **no upper bound** on a request. An origin
that accepts a connection and never responds hangs the command indefinitely.

M015E's hardening suite found the narrower form of this: `Timeout::from_secs`
sets `pool`/`connect`/`write`/`read` but **not** `total`, and the `read` budget
is "time between response body chunks", so a stall *before* the response starts
is unbounded. Both are the same defect seen from two directions: there is no
wall-clock ceiling on a request.

### Scope

Add `--timeout-secs <N>` to `record`, `replay`, `test`, and `serve`. It sets a
`Timeout` with every phase **and** `total` populated, so the command is bounded
whatever the origin does. Unset keeps today's behaviour, because silently
introducing a default deadline would change the behaviour of every existing
invocation — the same rule M015C applied to `--outbound-version auto`.

The machine-readable status payloads report the effective timeout, so a
recorded failure is attributable to the deadline that produced it.

This is a bound, not a retry. A timed-out transaction is recorded as
`ErrorCategory::Timeout` in the flow outcome, which is what a caller already
reads back through `eggreplay inspect`. It is deliberately *not* promoted to a
command-level failure: a gateway that records "the origin timed out" did its
job, and the recorded outcome is more useful than an exit code.

## 2. Route failures are all categorised `Other`

### Finding

`map_fetch_error` in `crates/eggreplay-http/src/recording.rs` matches specific
`FetchError` variants and sends everything else to
`(ErrorCategory::Other, ErrorPhase::Other)`. `FetchError::CustomTransport` —
which is exactly what the Eggress dialer returns (`eggress.rs` builds
`DialError` and hands it to EggFetch) — has no arm.

Consequences:

- A dead Eggress route records `Other`, not a connection category.
- A route that times out records `Other`, not `Timeout`.
- A route that rejects (policy) records `Other`, not `Policy`.
- **`ErrorCategory::ConnectionRefused` is unreachable anywhere in the product.**
  It is declared in `eggreplay-core` and produced by no code path.

EggFetch 0.2.2 already exposes `FetchError::custom_transport_error() ->
Option<&DialError>` and `DialErrorKind { Connection, Timeout, Authentication,
Rejected, Other }`. The information is available and simply unused. The narrower
`transport_failure_kind()` accessor deliberately collapses route failures to
`Connect`; the product should use the richer dialer kind instead.

The M015E hardening suite pinned the current behaviour with an assertion that a
dead route reports `Other`. That assertion is correct about today and must be
updated to the fixed behaviour, with the change recorded.

### Scope

Map `DialErrorKind` to `ErrorCategory`/`ErrorPhase` in `map_fetch_error`:

| `DialErrorKind` | `ErrorCategory` | `ErrorPhase` |
|---|---|---|
| `Connection` | `Unreachable` | `Connect` |
| `Timeout` | `Timeout` | `Timeout` |
| `Authentication` | `Policy` | `Connect` |
| `Rejected` | `Policy` | `Policy` |
| `Other` | `Other` | `Other` |

`Unreachable` rather than `ConnectionRefused`: EggFetch's typed evidence does
not distinguish refused from unreachable, and an inferred distinction would be
worse than an honest general category. Making `ConnectionRefused` reachable
would require inventing evidence the transport does not provide.

## 3. The WebSocket shutdown race, and the contract it was hiding

### Finding

`recording::tests::recording_gateway_captures_upgrade_and_leading_post_101_messages`
fails roughly one run in six under load with
`Invalid("WebSocket 101 flow is missing required conversation metadata")`. It
passes 8/8 in isolation. M015E recorded it as pre-existing and out of scope.

It is not a flaky test. The sequence is:

1. The test performs a clean Close handshake and awaits the upstream task.
2. It immediately calls `server.shutdown()`.
3. EggServe's tunnel implementation handles shutdown by **aborting** the
   handler task — `eggserve-server 0.4.0` `tunnel.rs`:
   `tokio::select! { _ = &mut handler_join => {}, _ = cancel => { handler_join.abort(); ... } }`,
   with the comment "the handler remains in this tracked task, so shutdown can
   abort it".
4. If the abort lands before the relay has observed the close and appended its
   transcript, the `ConversationCompleter` is dropped. Its `Drop` impl signals
   completion **without** a recorded transcript.
5. `RecordingSession::finish` then publishes a fixture containing a 101 flow
   and no `websocket-messages` extension, and the store's validator rejects it.

**The product is right.** The relay code says so explicitly: "cancellation or
panic before this point signals via Drop with no recorded transcript, which
then fails closed in `RecordingSession::finish`." Publishing a 101 flow whose
transcript is missing would break replay, so failing closed is correct.

The defect is that the test assumes shutdown lets an in-flight conversation
finish. It does not, and under load the abort wins the race more often.

### Scope

Two changes, both in tests, plus one documentation line:

1. The 101 test waits for the conversation to be durably recorded —
   `RecordingSession::websocket_conversation_count()` is the existing public
   accessor — before shutting the server down. The wait is bounded and fails
   with a clear message rather than spinning forever.
2. **Add** a test that pins the fail-closed contract deliberately: shut the
   server down while a conversation is still in flight, and assert that
   `finish_recording_session` refuses to publish a fixture whose 101 flow has no
   transcript. The behaviour that produced a flake becomes a pinned guarantee.

This is the right shape for the fix. Deleting the flake would have hidden a
real, correct, undocumented contract; bounding the test and pinning the
contract keeps both.

### Evidence note on the flake

The pre-fix race was **not** reproduced under synthetic load during this
milestone. Reverting only the new wait and running the test 30 times with 8
`yes` processes saturating the CPU passed 30/30 both before and after the
change; the real trigger is evidently subtler than CPU contention (more likely
the full parallel workspace suite, where the 30s `mitm`/`h2_qualification`
suites run concurrently).

So the causal claim rests on the code path, not on a reproduction:
`eggserve-server`'s `tunnel.rs` aborts the handler task on shutdown, and
`ConversationCompleter::drop` signals without a transcript. That ordering is
readable in the source and is the same one the original failure message
(`missing required conversation metadata`) points at. The fix removes the
dependency on who wins the race rather than trying to lose it more often.

## Non-goals

- No default timeout. Unset must preserve today's behaviour.
- No retry or backoff changes.
- No change to `ErrorCategory` variants or their serialized values.
- No change to the WebSocket relay, the completion barrier, or the store
  validator. The product's behaviour is correct as written.
- No change to the un-terminated bidirectional gRPC deferral. That needs a
  terminal-status story and its own milestone.

## Acceptance criteria

- `--timeout-secs` bounds every phase including `total`; unset preserves
  current behaviour; the effective timeout appears in the status payloads.
- A command against an origin that never responds terminates in bounded time and
  records the flow as `ErrorCategory::Timeout`, rather than hanging. The
  command itself still succeeds: the timeout is a recorded outcome, not a
  command-level error.
- A dead Eggress route records `Unreachable`/`Connect`, not `Other`.
- A route timeout records `Timeout`; a rejected route records `Policy`.
- `ErrorCategory::Other` is no longer the outcome for any route failure the
  transport can describe.
- The 101 test passes deterministically under load, and its wait is bounded.
- A new test proves the session refuses to publish a fixture whose 101 flow
  has no transcript.
- The M015E hardening assertion for a dead route is updated to the fixed
  behaviour, and the M015E closure is annotated with the change.
- The full workspace gate is green on Linux, Linux MSRV, macOS, and Windows.

## Evidence

- `plans/closure/m015e-h2-hardening-hosted-qualification-and-closure.md`
- `plans/closure/m015d-grpc-over-http2-integration-qualification.md`
