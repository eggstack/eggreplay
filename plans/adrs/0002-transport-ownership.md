# ADR 0002 — Reuse Eggstack Transport Authorities

Status: accepted

## Decision

EggFetch owns outbound HTTP/TLS/framing/pooling; EggServe owns inbound HTTP runtime/framing/lifecycle; Eggress owns optional outbound route establishment. EggReplay owns recording, persistence, matching, replay policy, scenario state, redaction, regression comparison, and presentation.

## Constraint

No milestone copies a sibling protocol implementation merely to avoid an integration dependency. If a reusable seam is missing, document the gap and either add the narrow seam upstream or defer the feature.

Direct networking remains the default. Normal record/replay modes do not require interception.
