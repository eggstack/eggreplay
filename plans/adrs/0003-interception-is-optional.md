# ADR 0003 — Interception Is an Optional Acquisition Adapter

Status: accepted

## Decision

TLS interception is not part of EggReplay's core execution path or v0.1 release gate.

Default acquisition modes are application/library-driven outbound recording and explicit reverse/gateway recording where the application points at EggReplay. A future interception component may feed the same canonical flow writer.

## Consequences

No CA generation, trust-store mutation, certificate issuance dependency, or transparent-routing requirement belongs in core/default builds. Certificate-pinned clients are not a blocker for the main product.

Any later interception milestone requires a separate threat model, certificate lifecycle design, and isolated feature/crate boundary.
