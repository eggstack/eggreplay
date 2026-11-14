# Planning Archive

Retain superseded plans here when they are no longer active but remain useful audit history. Do not delete historical plans merely because implementation changed direction.

## Archived subsystem roadmaps

These roadmaps predate the milestone execution plans and closure records. They remain useful as design history, but their former `active roadmap` status is no longer current. The implementation plans and closure records below are the qualified authorities that supersede them.

| Archived roadmap | Superseding authority | Why archived |
|---|---|---|
| [Flow and storage](subsystems/flow-storage-roadmap.md) | [M002](../closure/m002-flow-model-and-store.md), [M010](../closure/m010-streaming-timing-and-sse.md), [M011](../closure/m011-websocket-semantic-record-replay.md), [M014A](../closure/m014a-har-and-migration.md) | The schema and storage roadmap was executed and later extended by the recorded milestones. |
| [Matching and replay](subsystems/matching-replay-roadmap.md) | [M004](../closure/m004-eggserve-replay-server.md), [M005](../closure/m005-matching-consumption-near-miss.md), [M009](../closure/m009-stateful-dynamic-replay.md) | Replay and matching semantics now have qualified implementation and closure records. |
| [Regression](subsystems/regression-roadmap.md) | [M006](../closure/m006-client-replay-and-diff.md), [M007](../closure/m007-eggress-routing-and-cli.md), [M010](../closure/m010-streaming-timing-and-sse.md), [M019](../closure/m019-comparison-authority.md) | The initial comparison model was delivered and subsequently corrected/extended by later milestones. |
| [Protocol and interop](subsystems/protocol-interop-roadmap.md) | [M011](../closure/m011-websocket-semantic-record-replay.md), [M014](../closure/m014-compatibility-program.md), [M015](../closure/m015-bidirectional-http2-and-transport-baseline.md), [ADR 0009](../adrs/0009-http3-integration-boundary.md) | WebSocket/H2 support and the H3 deferral are now defined by qualified decisions, not the older roadmap. |
| [Security and privacy](subsystems/security-roadmap.md) | [C003](../closure/c003-persistence-redaction-policy.md), [M013](../closure/m013-explicit-proxy-and-optional-mitm.md) | The redaction and interception controls have qualified plans, closure evidence, and current operator documentation. |

Do not retro-edit closed plans or closure records to match later reality. A superseding plan or corrective closure must identify what it replaces and why.
