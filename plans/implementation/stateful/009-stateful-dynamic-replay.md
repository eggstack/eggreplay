# M009 — Stateful and Dynamic Replay

Status: ready
Depends on: C006
Roadmap stage: 5

## Objective

Add controlled record-on-miss/pass-through behavior plus authored deterministic
scenarios without turning EggReplay into a scripting runtime.

M009 must preserve the core boundary: EggReplay owns replay semantics and
state; EggFetch owns outbound HTTP; EggServe owns inbound HTTP; Eggress owns
optional route establishment.

## A. Session schema and extension registry

Implement ADR 0005 first.

Required changes:

- split session-schema and flow-schema version constants/types;
- read schema-1 fixtures unchanged;
- add session schema 2 with a bounded manifest extension registry;
- reject unknown required extensions;
- path-confine and symlink-check every extension file;
- enforce per-extension and aggregate metadata limits;
- keep manifest-last atomic publication;
- add schema-1 golden fixtures and schema-2 extension fixtures.

No schema-1 fixture may become unreadable.

## B. Record modes

Define one typed policy used by library and CLI:

- `sealed`: replay only; a miss never reaches the network;
- `once`: recording is allowed only while creating a new fixture; an existing
  fixture behaves sealed;
- `append-new`: match/replay first, otherwise execute upstream through
  EggFetch and append the newly observed interaction;
- `re-record`: execute upstream instead of consuming existing recorded
  responses and atomically replace the fixture/session at completion.

Do not overload current consumption modes (`Once`, `RepeatLast`,
`Unlimited`) with record-mode meaning.

Pass-through must remain explicit. A malformed fixture, matcher error, exhausted
state, or policy error must not silently become network fallback.

Append-new must use the concurrent recording/session machinery from C002 and
preserve redaction-before-persistence from C003.

## C. Replay coordinator

Introduce a coordinator above the existing matcher/replay server that decides:

```
normalize -> scenario (if selected) -> recorded candidate match ->
consumption -> optional miss policy -> upstream record -> response
```

Near misses remain diagnostics only. They never trigger an implicit weaker
match.

Concurrency semantics must be defined for two simultaneous misses of the same
request. Prevent duplicate append races with a bounded key/state lock or an
equivalent deterministic mechanism; do not serialize unrelated requests.

## D. Authored scenarios

Add `rules.json` as the `rules` session extension.

Scenario model:

- stable scenario id;
- initial state;
- bounded named states;
- ordered transitions;
- each transition has request predicates, a response source, optional variable
  extraction, and an explicit next state;
- deterministic priority/order; no hidden best-match fallback;
- isolated scenario state per replay-server instance by default.

Recorded ordered consumption and authored scenarios remain distinct. A user
must explicitly select/enable a scenario; merely having recorded flows does not
synthesize a state machine.

Response sources should initially reference an existing flow/response or a
bounded authored response descriptor. Reuse blob refs for authored bodies.

## E. Variable extraction

Support a deliberately small typed set:

- path segment by explicit index/name from a configured pattern;
- query key;
- request header;
- JSON Pointer from recognized bounded JSON bodies;
- prior scenario variable.

Extraction is explicit per transition. Missing/invalid extraction has a
configured transition failure behavior; it must not become an empty string
silently.

Never expose environment variables, filesystem data, process metadata, random
values, or wall-clock time as template variables.

Do not permit extraction from fields marked redacted unless the rule explicitly
uses the redaction-safe wildcard semantics and does not recover the secret.

## F. Deterministic templates and transforms

No arbitrary scripting.

Provide a pure bounded expression/template surface sufficient for common mocks:

- literal text/bytes;
- variable substitution;
- bounded concatenation;
- response header value substitution;
- JSON Pointer value replacement in recognized JSON;
- status selection from an explicit finite set if needed.

Define limits for template length, variable count/value bytes, JSON depth,
transform count, and rendered output bytes.

Evaluation must have no I/O, subprocess, network, clock, RNG, recursion, or
user-defined code. Cycles are rejected at load time.

If text templating is supported, escaping rules must be explicit; JSON
transforming should operate on parsed values rather than string interpolation.

## G. CLI/config surface

Extend `serve` (or introduce one clearly named replay-server command) with:

- `--record-mode sealed|once|append-new|re-record`;
- explicit `--upstream` required for any network-capable mode;
- existing `--route` support reused for upstream misses;
- scenario selection;
- matcher profile selection;
- machine-readable effective-policy output through inspect/config diagnostics.

Default remains sealed/offline.

Avoid creating overlapping command semantics between `record`, `serve`, and
`replay`; document each command's ownership.

## H. Security

- all newly captured miss traffic passes through the effective redaction policy;
- scenario rules are treated as untrusted fixture input and bounded;
- template output cannot reference host resources;
- diagnostics never print extracted secret values;
- rule/path strings are bounded and path-confined;
- no dynamically loaded code.

## I. Tests

Required deterministic coverage:

1. schema-1 read compatibility;
2. schema-2 required/optional extension behavior;
3. unknown required extension rejection;
4. extension traversal/symlink rejection;
5. sealed miss never opens upstream;
6. once creates then seals;
7. append-new records exactly one miss and reuses it subsequently;
8. concurrent identical misses do not duplicate unexpectedly;
9. unrelated misses can overlap;
10. re-record atomically replaces without partial publication;
11. routed append-new uses Eggress without direct fallback;
12. redaction sentinel absent after miss recording;
13. scenario state transitions and reset/isolation;
14. transition ordering and exhaustion diagnostics;
15. extraction from query/header/path/JSON;
16. deterministic template output;
17. missing variable/failing transform behavior;
18. template/resource bounds;
19. no clock/RNG/environment dependence;
20. JSON/human/JUnit behavior remains stable where applicable.

## Closure

Create `plans/closure/m009-stateful-dynamic-replay.md` with exact commands,
test inventory, schema compatibility evidence, and hosted CI link. M010 remains
blocked until M009 closes.
