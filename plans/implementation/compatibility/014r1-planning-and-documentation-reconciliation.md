# M014-R1 — Planning and Documentation Reconciliation Gate

Status: closed (see `closure/m014r1-planning-and-documentation-reconciliation.md`).
Current-state header retained for auditability; the body below is the
historical implementation plan and must not be re-opened.
Depends on: M013 closure
Corrective gate: M014A/M014B feature implementation
Parent: M014

## Trigger

M013 is formally closed on qualifying implementation `5efc6f9`, with the
closure commit `2bcf493` re-qualified by Actions run `36456906216`.
The plan registry and top-level README record that transition correctly, but
the canonical planning corpus is not internally consistent at the M013 -> M014
boundary.

The current debt is planning/documentation debt, not a product-code defect.
Do not begin HAR, migration, HTTP/2, HTTP/3, or gRPC/fault implementation as
part of this corrective.

Observed inconsistencies include:

- `plans/002-long-term-roadmap.md` still describes M013F as ready/in progress,
  M013 as open, and M014/M014A/M014B as blocked;
- `plans/004-research-and-compatibility-baseline.md` still lists M013
  interception qualification as an unresolved post-M012 question and contains
  M013-era prospective language that is now historical;
- `plans/registry.md` marks both M014A and M014B ready while its global rule
  says only the first unblocked subplan of a decomposed milestone is
  executable;
- the roadmap, registry, README, compatibility plans, closure records, and
  research baseline therefore do not currently express one unambiguous M014
  execution policy.

This corrective is the sole M014 handoff until those sources agree.

## Objective

Reconcile the canonical planning/documentation surface to the proven M013
closure and establish one explicit dependency-driven execution policy for M014,
without changing product behavior, support claims, protocol implementation, or
historical evidence.

After this plan closes:

- M013 and M013A-M013F are uniformly documented as closed;
- M014 is uniformly documented as the active compatibility program;
- M014A and M014B may become independently ready because neither depends on the
  other;
- M014C and M014D remain blocked on M014B;
- the registry rule explicitly permits parallel execution of independent
  decomposed sibling plans whose dependencies are closed;
- no document implies H2/H3/gRPC/HAR support before its owning M014 track
  produces EggReplay-local evidence.

## 1. Reconcile the canonical long-term roadmap

Update `plans/002-long-term-roadmap.md` so Stage 9 reflects the actual M013
closure:

- M013A, M013B0, M013B, M013C, M013D, M013E, and M013F are closed;
- M013 umbrella is closed;
- cite the canonical M013 closure record rather than reconstructing evidence in
  the roadmap;
- remove language that says hosted M013F work is still pending.

Update Stage 10 so it reflects the post-M013 dependency state:

- M014 is the current decomposed compatibility stage;
- M014A is eligible after this reconciliation gate closes;
- M014B is eligible after this reconciliation gate closes;
- M014C remains blocked on M014B;
- M014D remains blocked on M014B and M010.

Do not make the roadmap an execution log. Keep exact run IDs, test counts, and
detailed qualification evidence in closure records/registry rather than
duplicating them unnecessarily.

## 2. Reconcile the research/compatibility baseline

Update `plans/004-research-and-compatibility-baseline.md` to distinguish
historical research snapshots from current unresolved questions.

Required corrections:

- remove M013 interception qualification from the current unresolved set;
- state that M013 closed and identify the current qualified dependency line
  without rewriting the historical M013A/M013B0 research narrative;
- convert prospective statements such as "M013B will use ..." into historical
  resolution or explicitly label them as pre-implementation context;
- identify M014's actual unresolved compatibility questions: HAR/migration,
  direct/routed H2 qualification, H3 architecture feasibility, gRPC-derived
  views/fault polish, and separately evidence-gated Python 3.15/free-threaded
  promotion if it remains outside M014;
- do not promote sibling EggFetch/EggServe/Eggress capability into an EggReplay
  support claim.

Historical dated sections may remain if their date and status make it clear
that they are evidence snapshots rather than current execution state.

## 3. Resolve the decomposed-milestone execution rule

The current registry rule, "only the first unblocked subplan is executable",
conflicts with M014A and M014B having independent satisfied dependencies.

Replace that rule with dependency-driven semantics:

- an umbrella milestone never authorizes skipping subplan dependencies;
- each decomposed subplan is executable when its own declared dependencies and
  decision gates are satisfied;
- independent sibling subplans may be ready/active concurrently;
- a parent milestone closes only when all required child tracks have explicit
  closure/support decisions.

Apply the rule consistently to M014. After M014-R1 closes, M014A and M014B
should both be `ready` unless this reconciliation discovers a concrete
dependency that requires serialization. Do not invent serialization merely for
bookkeeping convenience.

M014C and M014D must remain blocked until M014B produces its HTTP/2
qualification/support-tier decision.

## 4. Audit all current-status surfaces

Search the default branch for stale M013/M014 status language and reconcile
current-state documents, including at minimum:

- `README.md`;
- `plans/README.md`;
- `plans/002-long-term-roadmap.md`;
- `plans/003-qualification-and-release-strategy.md`;
- `plans/004-research-and-compatibility-baseline.md`;
- `plans/registry.md`;
- `plans/implementation/compatibility/014-compatibility-program.md`;
- M014A-M014D implementation plans.

Also inspect current architecture/support documentation for statements that
could be read as H2/H3/gRPC/HAR support claims.

Rules for this audit:

- historical closure records are immutable audit artifacts except for a clear
  factual erratum;
- old implementation plans may preserve historical status/evidence when doing
  so is necessary for auditability, but their header/status must not falsely
  advertise a current executable gate;
- current-state READMEs, registry text, and canonical roadmap/baseline must
  agree;
- links/SHAs/run IDs that are intended as current evidence must resolve to the
  final canonical evidence, especially M013 run `36456906216` on
  `2bcf493`.

## 5. Keep support claims conservative

This corrective does not change the support matrix.

The supported baseline remains:

- semantic HTTP/1.1 record/replay/regression;
- M009 stateful/dynamic replay;
- M010 streaming/SSE semantics;
- M011 cleartext RFC 6455 WebSocket semantics within the qualified scope;
- M012 Python/pytest bindings and qualified wheel matrix;
- M013 explicit HTTP/1.1 proxying, CONNECT deny/tunnel, and opt-in
  policy-gated HTTPS MITM HTTP/1.1 recording.

The following remain unsupported or evidence-gated until their owning plans
close:

- HTTP/2 general support and HTTP/2 MITM;
- HTTP/3/QUIC;
- WSS interception;
- gRPC semantic views;
- HAR interchange/migration claims beyond whatever existing internal tooling
  already proves;
- transparent/TUN interception.

Do not use wording such as "supported by EggServe" as a substitute for an
EggReplay support claim.

## 6. Preserve architecture and product code

Expected implementation is documentation/planning-only.

Do not modify Rust/Python source, dependency versions, schema versions, CLI
behavior, feature flags, fixtures, or CI behavior unless the reconciliation
uncovers a documentation build/check that cannot otherwise pass. Any product
or dependency change discovered to be necessary must be split into a new
implementation plan rather than smuggled into this corrective.

No new ADR is required merely to reconcile status. Create or amend an ADR only
if the audit discovers a genuine architectural decision not already covered by
the M014 plans.

## Verification

At minimum run:

```text
git diff --check
git grep -n -E 'M013(F)? .*ready|M013 .*open|M014(A|B)?.*blocked' -- README.md plans
```

Review every match semantically; historical text may legitimately match.

Because the repository CI runs on documentation-only commits, allow the normal
hosted workflow to complete and require it to be green before final closure.
No new protocol qualification run is required beyond the repository's normal
CI because this corrective changes no support surface or product code.

## Closure/status procedure

Implementation should use a documentation-only commit (or tightly bounded
series) with M014-R1 marked `active` or `implemented` while reconciliation is
being performed.

After the corrected documentation is committed and normal CI is green:

1. create
   `plans/closure/m014r1-planning-and-documentation-reconciliation.md`;
2. record the reconciliation commit SHA and CI run;
3. record the files audited and any historical text intentionally retained;
4. mark M014-R1 `closed`;
5. mark M014A and M014B `ready` if no new dependency was discovered;
6. keep M014C and M014D blocked on M014B;
7. update `README.md`, `plans/README.md`, and the registry current execution
   gate to name the resulting M014 handoff.

The closure record should be concise: this is a truth-source reconciliation,
not a second M013 qualification record.

## Acceptance

M014-R1 closes only when all of the following are true:

- canonical roadmap Stage 9 states M013 is closed;
- canonical roadmap Stage 10 and registry agree on M014 readiness;
- the research baseline no longer treats M013 qualification as unresolved;
- the decomposed-milestone execution rule is dependency-driven and
  unambiguous;
- README, planning README, registry, M014 umbrella, and M014A-D headers agree
  on current statuses;
- M014A/M014B parallel eligibility is either explicitly established or a real
  dependency causing serialization is documented;
- M014C/M014D remain correctly gated by M014B;
- no unsupported protocol/interchange capability is promoted by documentation;
- historical closure evidence remains intact;
- `git diff --check` passes;
- the normal hosted CI run for the reconciliation commit is green;
- a closure record exists and the registry reflects the final handoff.

Until this plan closes, do not start M014A or M014B implementation.
