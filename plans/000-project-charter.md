# 000 — Project Charter

Status: canonical
Project: EggReplay

## Mission

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic replay/mock server, client-side replay engine, and network regression tester.

The product combines the useful abstractions of mitmproxy's HTTP flow model, WireMock-style matching/stateful behavior, and VCR-style cassette reuse without making traffic interception the primary operating model.

## Primary user stories

1. Record an application's HTTP interactions through an explicit local gateway and persist them as a portable fixture.
2. Run an application offline against those recorded interactions with deterministic request matching and useful mismatch diagnostics.
3. Replay a recorded request suite against a live candidate service and compare observed behavior to a baseline.
4. Run the same replay suite through direct networking or an Eggress outbound route without changing semantic expectations.
5. Inspect, validate, redact, diff, and migrate fixture sets from the CLI with machine-readable output suitable for CI.
6. Later, author stateful scenarios/templates and record richer streaming/WebSocket traffic.
7. Later, optionally acquire traffic through an explicit interception adapter without imposing CA/MITM machinery on ordinary users.

## Product invariants

- Semantic replay, not packet capture.
- Interception optional.
- Transport authority stays external.
- Machine-readable behavior is first class.
- Large bodies are streamable; eager whole-body buffering is not acceptable architecture.
- Loss is explicit for imports, normalization, redaction, and protocol downgrades.
- Secrets are filtered before durable persistence where configured and never intentionally printed in diagnostics.
- Fixture formats are versioned and migration-aware from the first release.
- Determinism wins over cleverness in replay behavior.

## v0.1 target

- Rust workspace and CLI.
- Canonical request/response/error flow model.
- Versioned `.eggr` directory storage with content-addressed blobs.
- Gateway recording using EggServe inbound + EggFetch outbound.
- Offline replay/mock serving.
- Strict/practical matching, ordered consumption, and near-miss diagnostics.
- Client replay against a target plus semantic regression diff.
- Optional Eggress outbound routing.
- Default header/cookie credential redaction plus configurable query/body redaction.
- Human + JSON CLI output and JUnit regression output.
- HTTP/1.1 qualification as the required release protocol baseline.

HTTP/2 may be supported only if end-to-end evidence justifies the claim.

## Explicit non-goals for v0.1

Transparent interception, TUN/WireGuard capture, packet capture, TLS MITM, browser DevTools replacement, full mitmproxy/WireMock parity, arbitrary embedded scripting, malformed-packet synthesis, distributed fixture databases, automatic-secret-detection completeness claims, WebSocket deterministic replay, and a public Python API.

## Success criteria

A v0.1 user can record a local integration flow, disconnect the upstream, replay it successfully, deliberately change a request and receive a precise mismatch explanation, replay the same baseline against a changed candidate service, receive a deterministic machine-readable regression report, and repeat the test through an Eggress route without EggReplay implementing proxy protocols itself.
