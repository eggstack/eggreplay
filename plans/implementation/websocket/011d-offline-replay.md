# M011D — Offline WebSocket Replay

Status: implemented
Depends on: M011C
Parent milestone: M011

## Objective

Serve recorded WebSocket conversations offline using the ordinary EggReplay
HTTP matcher for the initiating Upgrade and a deterministic message script for
post-upgrade behavior.

## A. Fixture loading

A replay fixture with required `websocket-messages` must load and validate the
extension automatically.

Index conversations by initiating flow id without materializing payload blobs.
Payload bytes are opened only for the selected conversation/message.

Unknown/malformed required WebSocket metadata fails fixture loading.

## B. Upgrade matching

Use the ordinary request matcher with ADR 0006 normalization:

- ignore volatile `Sec-WebSocket-Key`;
- require valid H1 WebSocket Upgrade/version;
- apply normal header/query/cookie/redaction semantics;
- use the recorded offered/selected subprotocol contract;
- do not synthesize a WebSocket conversation from an authored M009 scenario.

Consumption state remains per replay-server instance and follows the selected
flow's existing consumption rules.

## C. Handshake response

Use EggServe tunnel handoff and a maintained handshake helper to compute the
new client's `Sec-WebSocket-Accept`.

Return the recorded selected subprotocol only if the current client offered it.
Do not negotiate extensions in M011.

A recorded WebSocket flow cannot be replayed as an ordinary static 101
response.

## D. Deterministic message script

Strict ordered semantics are the initial supported policy.

Process the single recorded global sequence:

- recorded server->client message: emit it;
- recorded client->server message: wait for one client message and compare it;
- ping/pong/close remain explicit semantic records;
- mismatch returns bounded diagnostics and terminates deterministically;
- recorded abnormal terminal state closes the transport without inventing a
  clean close.

Do not add a permissive policy in M011 unless it is separately named,
documented, bounded, and tested. Strict remains default.

## E. Redaction-aware matching

Client-message comparison honors recording markers:

- JSON-redacted paths are matcher wildcards;
- whole-text/binary redaction means payload bytes are non-authoritative while
  message kind/order remain authoritative;
- redacted close reasons are non-authoritative;
- diagnostics never expose the current or recorded secret value.

Server messages replay the persisted redacted representation because the
original secret is intentionally unavailable.

## F. Timing

Default is `Immediate`: preserve sequence/terminal behavior with zero delay.

Optional `Recorded`/`Scaled` modes apply per-message monotonic deltas using
the M010 timing policy and bounds. Cancellation interrupts pending sleeps.

Equal timing values still follow stable recorded sequence order.

## G. Control-frame behavior

Characterize the selected codec's automatic ping/pong/close behavior and make
the replay authority explicit. Avoid emitting both an automatic control reply
and a recorded control message.

The semantic transcript, not codec implementation accident, owns expected
control-message behavior.

## H. Lifecycle

Use EggServe's tunnel cancellation and explicit EggReplay conversation duration
bound. Dropping the client or shutting down the server must stop pending
payload reads/sleeps promptly.

## Required tests

- fixture lazy-load behavior;
- volatile key matching;
- regenerated accept header;
- subprotocol success/mismatch;
- text/binary strict sequence;
- simultaneous/equal-delta stable order;
- ping/pong/close;
- redaction wildcard matching;
- whole-message redaction matching;
- immediate/recorded/scaled timing;
- cancellation during timing sleep;
- abnormal termination;
- mismatch near-miss bounds;
- large payload lazy streaming;
- repeated/concurrent replay server isolation;
- ordinary HTTP flows in the same fixture remain usable.

## Closure

Create `plans/closure/m011d-websocket-offline-replay.md`.
M011E becomes ready only after offline semantics are stable.
