# M011D — Offline WebSocket Replay Closure

Status: closed

## Implementation

- Implementation commit: `ec5a6ff678f7278608ca8447cb6b75c2ee574a0a`
- Required WebSocket transcripts load and validate automatically. Conversation
  metadata indexes by flow ID; payload blobs open only when replay selects a
  message.
- Upgrade matching reuses the ordinary matcher, excludes volatile keys,
  validates H1 Upgrade/version/body semantics, and returns a newly derived
  accept value. A selected subprotocol is returned only when offered.
- Strict ordered scripts compare client messages and emit recorded server
  messages. Redaction markers supply wildcard behavior; abnormal terminal
  state does not become a clean close. Immediate timing is default, and
  recorded/scaled timing reuses bounded M010 policy.
- Replay uses EggServe tunnel cancellation and a bounded explicit conversation
  duration. Codec-generated control replies are accounted for to avoid
  duplicate recorded pongs or close replies.

## Verification

The full local workspace gates passed on `ec5a6ff`, including 132 tests.
Focused tests cover automatic required-transcript loading and an end-to-end
offline H1 upgrade/message/close script. Existing replay tests also cover lazy
blob loading, bounded streaming, and recorded timing. The hosted matrix passed
on `63d9e6c`; evidence is listed in the M011 umbrella closure.

M011E is closed and its implementation is recorded in the next subplan
closure.
