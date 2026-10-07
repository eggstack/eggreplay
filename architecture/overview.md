# EggReplay architecture overview

A bird's-eye map of the EggReplay workspace: what each crate owns, how data
flows between them, which capabilities exist today, and where the seams are
that keep HTTP, TLS, and routing authority in the Eggstack transport crates.

This document is the **index**. Each component below links to a dedicated
deep-dive file in this directory that covers its internals, invariants, and
review surface.

> Companion documents: [`../docs/architecture.md`](../docs/architecture.md)
> (dependency policy and milestone history), [`../plans/registry.md`](../plans/registry.md)
> (execution gate), [`../plans/adrs/`](../plans/adrs) (boundary decisions).

---

## 1. The one-paragraph model

EggReplay records real HTTP traffic into a versioned on-disk fixture (`.eggr`),
serves that fixture back as a deterministic server, and replays traffic against
a live target to diff the two. It owns **semantics** — flows, matching,
scenarios, redaction, comparison, reporting — and delegates **transport** —
HTTP/TLS/framing, inbound serving, route establishment — to EggFetch, EggServe,
and Eggress. A test is a fixture plus a matcher plus a report; a protocol is a
listener property, not a second code path.

---

## 2. Workspace shape

Seven crates, one dependency direction. Nothing below the CLI knows the CLI
exists; nothing but the CLI and the Python extension touches a network socket.

```
                        eggreplay-cli            eggreplay-python
                        (thin, 3.9k)             (leaf, PyO3, 1.6k)
                              │                          │
                              │  (optional feature)      │
                              ▼                          ▼
                     eggreplay-intercept          eggreplay-http
                      (opt-in, 7.1k)             (adapters, 10.5k)
                              │                          │
                              └──────────┬───────────────┘
                                         ▼
                            eggreplay-store  ──▶  eggreplay-core
                             (.eggr, 3.4k)        (semantics, 5.3k)
                                         │
                                         ▼
                       eggreplay-har  (HAR interchange, 2.5k)
```

Sizes in the diagram are **product source** (`src/**/*.rs` only).

| Crate | src | tests | Owns | Deep dive |
|---|---|---|---|---|
| `eggreplay-core` | 5,288 | — | Semantic models, matcher, scenarios, redaction, stream/WS/report semantics, error taxonomy. No transport, no Tokio, no filesystem. | [02](02-core-semantic-model.md) |
| `eggreplay-store` | 3,428 | — | `.eggr` directory fixtures: manifest, JSONL flows, content-addressed blobs, session extensions, crash-safe publication. | [03](03-store-persistence.md) |
| `eggreplay-har` | 2,479 | — | Lossy HAR 1.2 import/export with an explicit loss report and session migration helpers. No network I/O. | [11](11-har-and-migration.md) |
| `eggreplay-http` | 10,527 | 10,707 | Adapters and orchestration: recording gateway, offline replay server, candidate regression, inbound protocol policy, H2/gRPC/WebSocket/Eggress seams. | [04](04-http-recording.md), [05](05-http-replay-and-serving.md), [06](06-regression-and-reporting.md), [07](07-protocol-and-routing-tiers.md) |
| `eggreplay-intercept` | 7,099 | 6,391 | Optional explicit HTTP/1.1 proxy, CONNECT policy, CA lifecycle, leaf issuance, HTTPS MITM recording. | [08](08-interception.md) |
| `eggreplay-cli` | 3,914 | 2,098 | Clap surface, exit codes, report emission (human/json/junit), fixture inspection. | [09](09-cli-surface.md) |
| `eggreplay-python` | 1,623 | — | PyO3 bindings, asyncio lifecycle, fixture/report views, pytest plugin packaging. | [10](10-python-bindings.md) |

Lines are `.rs` under `crates/`, excluding `target/`. 59 Rust files, 53,554
lines total — but only **34,358 are product source**; the other 19,196 are
integration tests, and they are concentrated almost entirely in the three
crates that own a wire. That ratio is itself the signal: `eggreplay-http` and
`eggreplay-intercept` are as much qualification harness as product, which is
why their test suites are the load-bearing evidence for every boundary claim
in section 3.

Edition 2024, MSRV 1.89. `unsafe_code = "forbid"`, `missing_docs = "warn"`,
Clippy pedantic at `-D warnings`.

Deep dive: [01 — workspace, boundaries, and build policy](01-workspace-and-boundaries.md).

---

## 3. Transport ownership

The single most important structural rule: **EggReplay never owns an HTTP or
TLS stack.** Every wire concern is delegated, and CI asserts the delegation
rather than trusting a review comment.

| Concern | Owner | Pin | How EggReplay consumes it |
|---|---|---|---|
| Outbound HTTP/1.1 + HTTP/2, TLS, framing, 101/CONNECT upgrade IO | `eggfetch-core 0.2.2` | caret | The only client. `Client`, `HttpVersionPolicy`, `UpgradedStream`. |
| Inbound HTTP/1.1 runtime, framing, lifecycle, tunnel handoff | `eggserve-server 0.4.0` + `eggserve-primitives 0.2.2` | `=` | The only server. `OriginOnly` for record/replay gateways. |
| Inbound HTTP/2 (multiprotocol composition) | `eggserve-core 0.4.0` | `=` | **Opt-in only.** `eggserve_core::server::Service` *is* `eggserve_server::service::Service`. |
| Listener-free outbound routing (SOCKS, chains) | `eggress-outbound 1.0.11` | `=` | `pproxy-compat` grammar only, behind the `eggress` feature. |
| TLS pairing / cert-property checks | `eggnet-tls 0.2.0`, `x509-parser 0.16` | `=` / caret | Interception leaf serving and CA inspection. |
| Certificate generation/signing | `rcgen 0.13.2` | `=` | CA and leaf material, inside `eggreplay-intercept` only. |

Every Eggstack and TLS artifact is pinned **exactly** (`=`) *except*
`eggfetch-core`, which is a caret range resolved by `Cargo.lock`. State that
honestly rather than claiming a blanket exact pin: a transport bump is a plan
with a closure record, and the lockfile is what actually holds the graph in
place.

Two consequences worth internalising:

1. **One service, two runtimes.** Because EggServe Core re-exports the same
   service and request types, selecting `--inbound http2` hands the *existing*
   service implementation to a different runtime. It cannot fork the matcher,
   store, redaction, scenario, or renderer, because there is only one service
   to fork.
2. **`eggreplay-intercept` is a leaf.** No product crate and no Python build
   depends on it. It never adopts the multiprotocol serving layer, so
   interception can never silently gain HTTP/2.

---

## 4. Capability map

### Default profile (what a plain `cargo build` gives you)

| Capability | Entry point | Semantics owned by |
|---|---|---|
| Direct HTTP/1.1 recording to `.eggr` | `record --upstream … --fixture …` | `http::recording` → `store::RecordingSession` |
| Offline deterministic replay server | `serve --fixture …` | `http::replay::ReplayFixture` + `core::Matcher` |
| Client replay + semantic diff | `replay`, `test`, `diff` | `http::regression` → `core::compare_flows*` |
| Fixture inspection and validation | `inspect`, `validate` | `store::Session`, `core::BodyRef` |
| HAR import/export and session migration | `har import\|export`, `migrate` | `eggreplay-har` |
| Python fixture + replay bindings | `import eggreplay` | `eggreplay-python` |

### Opt-in tiers (feature-gated, never default)

| Capability | Feature | Tier |
|---|---|---|
| EggServe inbound H1 serving | `eggreplay-http/eggserve` | supported |
| WebSocket semantic record/replay/regression (cleartext RFC 6455) | `eggreplay-http/websocket` | supported |
| Eggress listener-free routing | `eggreplay-http/eggress` | supported |
| Outbound HTTP/2 record + regression | `eggreplay-http/h2` | experimental |
| Inbound HTTP/2 gateway and replay (h2c) | `eggreplay-http/h2-inbound` | experimental |
| Inbound HTTP/2 over TLS (ALPN, operator identity) | `eggreplay-http/h2-inbound-tls` | experimental |
| gRPC-over-H2 derived views | `eggreplay-http/grpc` | experimental |
| Explicit HTTP/1.1 proxy, CONNECT policy, CA, HTTPS MITM | `eggreplay-cli/intercept` | supported, policy-gated |

### Explicitly out of scope

Every item is **unsupported** in the tier vocabulary
([07](07-protocol-and-routing-tiers.md) § The tier model); where a deferral is
also recorded, it is named as the reason, not as a fourth tier. HTTP/3 / QUIC
(unsupported; deferred per ADR 0009), H2 interception/MITM, WSS and
extended-CONNECT WebSockets, negotiated WebSocket extensions, wire-frame
fidelity, automatic OS/browser CA trust installation, client mTLS
interception, transparent/TUN interception, and a generic reverse proxy.

"Experimental" means qualified against independent peers on loopback behind a
feature boundary — not "unverified". No HTTP/2 capability is default in any
profile.

---

## 5. Data flow

### Recording (acquisition)

```
client ──▶ EggServe inbound listener (OriginOnly, H1 or H2)
              │  policy + admission owned by EggServe
              ▼
           http::recording::record_request
              │  redact ─▶ stream to staging blob ─▶ re-verify SHA-256
              │  negotiate version annotation (H2)
              ▼
           store::RecordingSession::append_flow   (JSONL + manifest)
              │  flow record: request + exactly one response|error outcome
              ▼
           .eggr/  (manifest.json = publication marker, flows.jsonl, blobs/<sha256>)
```

Redaction happens **before** any finalized blob exists (C003), and the effective
policy is persisted by identifier. Redacted request fields become matcher
wildcards, not literal placeholders.

### Replay (offline serving)

```
client ──▶ EggServe inbound listener ──▶ http::inbound (protocol policy)
              ▼
           replay::ReplayFixture::load      (metadata-bounded, C001)
              │  flows + CandidateBody descriptors only; no blob bytes read
              ▼
           core::MatcherSession::next_match
              │  normalize ─▶ match ─▶ consume per RecordPolicy
              ▼
           open_blob ─▶ stream 64 KiB chunks, verify SHA-256 incrementally
              ▼  ResponseBody::Stream | Empty, plus recorded trailers
```

A miss is offline and never touches the network. Consumption is independent of
recording mode (`Once` / `RepeatLast` / `Unlimited`).

### Regression (live comparison)

```
fixture flow ──▶ core::Matcher ──▶ http::regression::execute_candidate
                                        │  materialise HttpRequest
                                        ▼
                                   EggFetch client (direct | EggressDialer)
                                        ▼
                                   observed response ─▶ core::compare_flows_with_timing_and_policy
                                        ▼
                                   RegressionReport ─▶ human | json | junit
```

### Interception (opt-in, separate entry)

```
client ──▶ ExplicitProxy (EggServe OriginOrAbsolute)
              │  filter_proxy_headers → policy_from_flags → ConnectAction
              ▼
        deny ─▶ bounded rejection
        tunnel ─▶ opaque CONNECT relay (Eggress for raw route, EggFetch for semantic)
        intercept ─▶ leaf cert ─▶ TLS termination ─▶ decrypted H1 ─▶ RecordingSession
```

---

## 6. The `.eggr` fixture

An `.eggr` is a directory, not a file: `manifest.json`, `flows.jsonl`,
`blobs/<sha256>`, plus any extension payloads as **root-level single
filenames**. There is no `extensions/` subdirectory — `validate_extension_path`
rejects any path with more than one component, so a nested layout is
structurally impossible.

```
demo.eggr/
├── manifest.json            # written last, then an atomic directory rename
├── flows.jsonl              # one JSON flow record per line
├── blobs/<sha256>           # content-addressed, no file extension
├── stream-events.json       # extension
├── websockets.jsonl         # extension
├── rules.json               # extension
└── interop-provenance.json  # extension
```

- **Flow schema 1** (`FLOW_SCHEMA_VERSION`) — semantic request + exactly one
  response or error outcome. Order and duplicate headers/query
  pairs/trailers preserved.
- **Session schema 2** (`SESSION_SCHEMA_VERSION`) — adds a bounded extension
  registry. Each extension is confined to a single filename, symlink-rejected,
  and bounded to 16 MiB each / 32 MiB total / 64 extensions. Unknown *required*
  extensions and future session schemas are **rejected**; there is no ignore
  switch. Schema-1 sessions remain readable; there is no schema-2→3 migration.
- **Bodies** — `absent`, `empty`, or a blob reference with SHA-256 and length.
- **Publication** — blobs, flows, and extension payloads are written and
  fsynced before `manifest.json` is created, and `manifest.json` is itself the
  last file created inside the staging directory, immediately followed by an
  atomic directory rename. The manifest is the final publication marker.

Registered extensions — `required_for_replay` is **not** uniform:

| Extension | File | `required_for_replay` | Why |
|---|---|---|---|
| `rules` | `rules.json` | `true` | Replay must apply the authored scenario machine. |
| `stream-events` | `stream-events.json` | `true` | Body event cadence is replay semantics, not decoration. |
| `websocket-messages` | `websockets.jsonl` | `true` | A recorded 101 must never replay as a static response. Forced, and validated as such. |
| `interop-provenance` | `interop-provenance.json` | `false` | HAR-import provenance is EggReplay-internal and never affects replay. |

`required_for_replay` means *the reader must understand and apply this
extension* — not that the user opted into timing. Three separate places enforce
it: store validation rejects an unknown required extension
(`eggreplay-store/src/lib.rs:2130-2138`), replay fails closed on a required
extension the build cannot honour (`eggreplay-http/src/replay.rs:177-201`), and
migration blocks on a required extension with no registered migrator
(`eggreplay-har/src/lib.rs:415-432`).

Deep dive: [03 — store persistence](03-store-persistence.md).

---

## 7. Error and reporting model

`eggreplay-core::error` defines the stable cross-layer taxonomy: an
`ErrorPhase` paired with an `ErrorCategory`. Every adapter maps its own failure
into that pair, so the CLI can pick an exit code and the Python binding can pick
an exception type without re-deriving meaning. Both enums serialize
`snake_case`, and the wire strings are pinned by unit test.

| `ErrorPhase` (9) | `ErrorCategory` (9) |
|---|---|
| `request` | `dns` |
| `connect` | `connection_refused` |
| `tls` | `unreachable` |
| `headers` | `tls` |
| `body` | `protocol` |
| `timeout` | `policy` |
| `cancelled` | `timeout` |
| `policy` | `cancelled` |
| `other` | `other` |

Note the axes are deliberately different: `phase` is *where* it broke
(`headers` and `body` are phases; `dns` and `protocol` are not), while
`category` is *what* broke. `ErrorCategory::ConnectionRefused` is only reachable
because M016 added the `FetchError::CustomTransport` arm that classifies a
typed `DialError`; before that every Eggress route failure collapsed to
`Other`. A `FlowError` message is truncated to 512 bytes, and category strings
are held under the stream-event 64-byte bound.

Regression reporting is a separate, versioned authority
(`REPORT_SCHEMA_VERSION = 2`) producing a `RegressionReport` with
`DiffFinding`s across declared comparison dimensions plus optional timing
assertions. The scheduler used is recorded in the report so a diff is
reproducible. The CLI's JSON envelope carries its own `schema_version: 1`,
which is a different counter and must not be conflated with either.

---

## 8. Deep-dive index

Read these in order for a full tour, or jump straight to a component.

| # | Component | File |
|---|---|---|
| 01 | Workspace, dependency boundaries, feature-flag matrix, CI enforcement lanes | [01-workspace-and-boundaries.md](01-workspace-and-boundaries.md) |
| 02 | `eggreplay-core` — flow model, matcher, scenarios, redaction, stream, WebSocket, report | [02-core-semantic-model.md](02-core-semantic-model.md) |
| 03 | `eggreplay-store` — `.eggr` format, sessions, blobs, extensions, crash safety | [03-store-persistence.md](03-store-persistence.md) |
| 04 | `eggreplay-http/recording` — gateway, observation, WebSocket capture, session finalization | [04-http-recording.md](04-http-recording.md) |
| 05 | `eggreplay-http/replay` + `inbound` — lazy fixture, matcher session, server composition | [05-http-replay-and-serving.md](05-http-replay-and-serving.md) |
| 06 | `eggreplay-http/regression` + `core/report` — candidate execution, diffing, timing | [06-regression-and-reporting.md](06-regression-and-reporting.md) |
| 07 | Protocol and routing tiers — H2, gRPC, WebSocket codec, Eggress dialer | [07-protocol-and-routing-tiers.md](07-protocol-and-routing-tiers.md) |
| 08 | `eggreplay-intercept` — proxy, CONNECT policy, CA, leaf, MITM, tunnel bounds | [08-interception.md](08-interception.md) |
| 09 | `eggreplay-cli` — command surface, flags, exit codes, report emission | [09-cli-surface.md](09-cli-surface.md) |
| 10 | `eggreplay-python` — bindings, asyncio lifecycle, packaging, pytest plugin | [10-python-bindings.md](10-python-bindings.md) |
| 11 | `eggreplay-har` + `migrate` — HAR loss model, provenance, transactional migration | [11-har-and-migration.md](11-har-and-migration.md) |
| 12 | Test topology, qualification evidence, CI lanes, closure discipline | [12-testing-and-qualification.md](12-testing-and-qualification.md) |

---

## 9. Review entry points

If you are reviewing for defects rather than learning the system, these are the
places where the invariants are load-bearing and where a regression would be
silent:

1. **Redaction ordering** — redaction must precede blob finalization, and
   redacted request fields must degrade to matcher wildcards.
2. **Fixture immutability while open** — concurrent blob replacement must fail as
   an integrity error, never serve stale or partial bytes.
3. **Matcher consumption** — a match that is found but cannot be consumed must
   be a near-miss, not a silent repeat or a silent skip.
4. **Feature-flag fallbacks** — an unbuildable protocol policy must be a
   configuration error, never a silent downgrade to a weaker protocol.
5. **Network-free misses** — replay must never fall back to direct on a miss.
6. **Scope leakage in the dependency graph** — a protocol or codec entering a
   default profile is a boundary break, not a feature.
