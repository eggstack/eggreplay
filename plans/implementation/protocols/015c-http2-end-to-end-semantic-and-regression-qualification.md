# M015C — HTTP/2 End-to-End Semantic and Regression Qualification

Status: blocked
Depends on: M015B closure, M010 closure, M014B closure
Parent: M015

## Objective

Qualify one coherent EggReplay HTTP/2 path across acquisition, offline replay,
candidate regression, and optional Eggress TCP routing.

M014B already proves experimental outbound H2. M015C must prove that the
new inbound path composes with the same canonical semantics rather than merely
demonstrating that an H2 socket can serve requests.

## Required matrix

Exercise combinations sufficient to prove:

- H2 client -> EggReplay recording gateway -> direct H2 upstream;
- H2 client -> EggReplay recording gateway -> Eggress-routed H2 upstream;
- H2 client -> offline H2 replay;
- recorded fixture -> direct H2 regression candidate;
- recorded fixture -> Eggress-routed H2 regression candidate;
- H1 fixtures replayed through the H2 serving path where semantics are
  protocol-neutral;
- H2-recorded fixtures replayed through H1 where no H2-only semantic is
  required, with explicit loss/unsupported behavior otherwise.

Use local deterministic TLS and proxy fixtures only.

## Semantic evidence

Prove at minimum:

- repeated/query/header normalization remains deterministic;
- request and response trailers survive where the canonical model supports
  them;
- large request/response streaming remains bounded;
- multiple concurrent H2 streams do not serialize through shared fixture
  state except where ordered consumption requires it;
- cancellation/reset is stream-local;
- M010 stream events/timing remain coherent under multiplexing;
- strict/practical/semantic JSON matching behaves identically to H1;
- stateful scenarios and deterministic templates behave identically;
- target remapping works;
- regression diff/report/JUnit/JSON contracts remain stable;
- protocol annotations are observational metadata, not an accidental matching
  dimension;
- GOAWAY/shutdown cannot publish partial/corrupt sessions;
- EggFetch 0.2.2 failure classification maps only evidence-backed
  connect/TLS/protocol/cancelled failures.

## Independent interoperability

Use EggFetch plus at least two independent H2 peers where practical (for
example Hyper and raw `h2`) so a shared implementation bug does not establish
support by self-consistency.

## Support decision

At closure, write an explicit tier matrix for:

- outbound H2 record/regression;
- inbound H2 recording gateway;
- inbound H2 offline replay;
- direct H2;
- Eggress-routed H2;
- TLS ALPN H2;
- h2c, if M015B intentionally qualified it;
- H2 interception (must remain unsupported in M015).

Because the adopted EggServe H2 tier is currently experimental, M015C must not
label EggReplay H2 generally supported unless that upstream classification
changes and the changed line is requalified locally.

## Acceptance

Create
`plans/closure/m015c-http2-end-to-end-semantic-and-regression-qualification.md`
with exact dependency versions, test counts, independent-client evidence,
resource/cancellation evidence, and the support matrix. M015D becomes ready
after C closes.
