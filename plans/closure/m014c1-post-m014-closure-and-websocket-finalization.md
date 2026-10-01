# M014-C1 — Post-M014 Closure and WebSocket Finalization Closure

Status: closed (qualified via M014-C2)

> Erratum (2026-10-01): this record was closed prematurely. The previously
> cited M014 umbrella revision `c71ffd7` / run `36778923619` predates the
> M014-C1 implementation and cannot qualify it. C1 implementation landed on
> `b188c552`; hosted run `36881596131` failed stable Clippy in an existing HAR
> test before the full verify matrix completed. M014-C2 owns requalification
> and final closure reconciliation. Historical detail below is retained for
> auditability.
>
> Reconciliation (M014-C2, 2026-10-01): C1 is qualified by the C2
> implementation lineage `cbc9257`, which contains the complete C1 repair
> plus the C2 Clippy/async-boundary/support-text corrections. Hosted
> qualification is Actions run
> [36891564494](https://github.com/eggstack/eggreplay/actions/runs/36891564494)
> (CI: verify Ubuntu stable/1.89/macOS/Windows, interception x3,
> dependency-boundary, Python bindings Ubuntu 3.14 stable, Ubuntu 3.11 on Rust
> 1.89.0, macOS 3.11 stable, Windows 3.11 stable, plus separate Python abi3
> cross-version lane) plus wheel run
> [36891564581](https://github.com/eggstack/eggreplay/actions/runs/36891564581),
> both green on `cbc9257`. The old `c71ffd7` / `36778923619` claim remains
> invalid for C1. Failed run `36881596131` is retained as audit history.

## Implementation revision and hosted evidence

Implementation is `b188c552`, qualified as amended by M014-C2 on `cbc9257`.
Actions run
[36881596131](https://github.com/eggstack/eggreplay/actions/runs/36881596131)
is a failed qualification attempt, not closure evidence. It passed several
interception/Python/dependency lanes but failed the stable verify path on
`clippy::assert_is_empty` in the pre-existing M014A HAR test, cancelling other
verify lanes. Qualification was completed by M014-C2 (see
`closure/m014c2-websocket-finalization-qualification-and-closure-reconciliation.md`).

Local verification on the qualifying workspace is green with the
repository-standard command:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Targeted WebSocket finalization tests were rerun ten consecutive times in
both single-thread and multi-thread runtime flavors on macOS to verify the
deterministic-repair claim; all runs passed (`recording_gateway_captures_upgrade_and_leading_post_101_messages`
plus the six new M014-C1 race regression tests in
`crates/eggreplay-http/src/recording.rs::tests`).

## Root cause and ownership rule

The pre-existing M011 WebSocket conversation/session-finalization race
was diagnosed against `crates/eggreplay-http/src/recording.rs`
(`gateway_websocket_request`) and
`crates/eggreplay-store/src/lib.rs` (`RecordingSession::finish`):

1. The recording gateway accepted the H1 101 upgrade via `tunnel.accept`,
   which staged the conversation handler future in the eggserve runtime
   and returned the handshake response immediately. The conversation
   handler future ran concurrently with the gateway's `session.append_flow`
   of the 101 flow and the request path returning.
2. The conversation handler future was the only place that called
   `session.append_websocket_conversation`. If the gateway caller's
   `session.finish()` ran before that future appended the transcript
   metadata, the manifest was written without the `websocket-messages`
   extension and `validate_websocket_fixture` failed closed at the end of
   `Session::open`. Under full-suite hosted load on macOS this could
   surface intermittently because the runtime-internal tunnel drain has
   a bounded budget (`graceful_shutdown_timeout` capped to 5 seconds),
   and the conversation task's lifecycle cancellation could race the
   natural completion path.

The repair establishes an explicit session-owned completion barrier:
**every successful 101 upgrade accepted for recording registers one
`WebSocketConversationFinalizer` with `RecordingSession` before the
request path is allowed to detach, and `finish()` drives each registered
finalizer to completion before publishing the manifest.**

The implementation:

- `crates/eggreplay-store/src/lib.rs`: new `WebSocketConversationFinalizer`
  trait (`drive` + `drive_with_deadline`), session-owned
  `websocket_finalizers: Mutex<Vec<Box<dyn WebSocketConversationFinalizer>>>`
  in `RecordingInner`, `RecordingSession::register_websocket_conversation_finalizer`
  / `websocket_conversation_finalizer_count`, and `RecordingSession::finish`
  drains every registered finalizer via `std::mem::take` before the
  `validate_websocket_fixture` step. The store crate remains tokio-free;
  the trait is runtime-agnostic so the eggreplay-http gateway supplies
  the runtime-aware completion primitive.
- `crates/eggreplay-http/src/recording.rs`: new `ConversationCompletion`
  (`Mutex<bool>` + `Condvar`) implementing
  `eggreplay_store::WebSocketConversationFinalizer`, plus
  `ConversationCompleter` that signals via `complete()` or, panic/cancel
  safely, via `Drop`. `gateway_websocket_request` registers one
  finalizer before `tunnel.accept` and the conversation handler calls
  `completer.complete()` after `append_websocket_conversation` returns.

The lifecycle invariant becomes self-evident in the code: the
`register_websocket_conversation_finalizer` call sits next to the
`tunnel.accept` call, the `completer.complete()` call sits next to the
`append_websocket_conversation` call, and `finish()` drains the registry
in the only path that publishes the manifest. No speculative architecture
prose is added.

## Semantics enforced

- The 101 flow is appended synchronously by the gateway while the
  conversation handler future is staged. The finalizer is registered
  before the request path returns.
- `RecordingSession::finish` cannot publish the manifest until every
  registered finalizer observes the conversation task reaching a
  terminal state (success, error, or cancellation). In the gateway's
  normal path the completer is signalled by `complete()` after the
  transcript is appended; in the abnormal path the completer's
  panic/cancel-safe `Drop` signals without requiring explicit code.
- The completer's `Drop` makes cancellation safe: even if the
  conversation task is aborted (e.g., the eggserve drain budget
  expires), the waiter in `finish()` unblocks and the resulting
  fixture correctly fails closed because the transcript was not
  appended (no manifest can claim a 101 flow without a registered
  conversation).
- `drive_with_deadline` provides a bounded-wait override for callers
  that need a precise deadline observation without busy-polling.
- Limits and shutdown/cancellation bounds remain explicit; ordinary
  HTTP sessions with no registered finalizers observe no behavior
  change.
- No task holds a store lock across an unbounded wait. `finish()`
  takes the registry under the short `Mutex` lock, then `drive()`s each
  finalizer outside the lock.

## Deterministic race regression tests

Added in `crates/eggreplay-http/src/recording.rs::tests` (under
`#[cfg(all(feature = "eggserve", feature = "websocket"))]`):

- `session_finish_waits_for_pending_websocket_conversation_finalizer`:
  registers a finalizer, spawns the conversation task on a multi-thread
  runtime, blocks the conversation at the completer, then verifies
  `finish()` (run on a `std::thread::spawn`) blocks until the completer
  is signalled, after which the fixture contains the appended
  conversation.
- `session_finish_unblocks_when_conversation_finalizer_is_dropped`:
  cancellation path — the completer is dropped without explicit
  completion; `finish()` must unblock via the completer's `Drop`
  safety net, and the resulting fixture must fail closed (101 flow
  without conversation).
- `session_finish_is_unaffected_when_no_websocket_finalizers_are_registered`:
  regression guard for ordinary HTTP-only sessions; `finish()` must
  publish without waiting.
- `conversation_completer_drop_is_safe_under_drop_without_complete`:
  completer dropped without explicit completion in a session that has
  no 101 flows; `finish()` must succeed and produce an empty fixture.
- `conversation_completer_drive_with_deadline_observations`:
  bounded-wait implementation returns `Err(elapsed)` once the deadline
  has elapsed.
- `concurrent_conversations_do_not_serialize_each_others_finalizers`:
  three registered finalizers signalled in interleaved order — `finish()`
  must remain blocked until the last one signals, and the resulting
  fixture must contain every conversation.

The original
`recording_gateway_captures_upgrade_and_leading_post_101_messages`
test remains enabled on its supported platforms and now passes
deterministically (verified across ten consecutive local runs on
macOS).

## Support / protocol matrix update

No change to the M014 support matrix. The experimental outbound H2 tier,
the H3 deferral, the gRPC helpers, the HAR tooling, the H1 baseline, the
HTTPS MITM tier, and the WebSocket cleartext baseline are exactly as
recorded in `closure/m014-compatibility-program.md`. The README and
plans/README are reconciled:

- H2 record/regression remains an explicit experimental opt-in tier;
  H2 inbound serving, H2 MITM, and `h2c` remain unsupported;
- H3 remains deferred per ADR 0009;
- WSS interception remains unsupported;
- The README `M013` matrix row for `HTTP/2 MITM` is updated from
  `unsupported/deferred M014B` to the final post-M014 wording
  `unsupported/not qualified`;
- A new README `HTTP/2 / HTTP/3 support matrix (M014B / M014C)`
  section records the experimental tier and the unsupported surfaces
  in one place.

## Dependency line and architecture boundaries

No dependency version moves, no schema bump, no new ADR. The change is
internal to the store/http boundary:

- `eggreplay-store` keeps no tokio dependency; the new trait is
  std-only.
- `eggreplay-http` already used tokio; `ConversationCompletion` wraps
  `std::sync::Mutex<bool>` + `std::sync::Condvar` and is supplied by
  the existing crate-level deps.
- EggServe / EggFetch / Eggress capabilities are not touched; the
  plan-level rule "do not replace EggServe/EggFetch/WebSocket protocol
  authorities or broaden supported protocol scope" is preserved.

## Handoff

M014-C1 is closed, qualified via M014-C2 on `cbc9257` (runs `36891564494` /
`36891564581`). M014-C2 is closed; see its closure record. Stage 11 remains
undefined pending separate research/planning.