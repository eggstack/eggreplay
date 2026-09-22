# C004 — CLI, Eggress Routing, Exit Codes, and Output Contracts

Status: ready
Depends on: M001–M008 historical baseline
Corrective gate: v0.1 requalification

## Finding

The `EggressDialer` adapter exists, but the CLI never constructs it or installs it on EggFetch clients. Current workspace dependency features permit `OutboundConnector::direct()` but not the CLI-friendly `from_pproxy_uri()` constructor.

The CLI also collapses all failures to process exit 1, emits only a minimal aggregate JUnit testsuite, and `inspect --bodies` currently returns placeholder text rather than an actual bounded explicit body view.

## Objective

Make the documented routing and machine-consumption contract real from the executable.

## Eggress route surface

### Dependency feature

Enable only the narrow Eggress `pproxy-compat` feature required by `OutboundConnector::from_pproxy_uri()`. Do not enable Eggress extended, SSH, QUIC, listener, or server surfaces as part of this corrective.

### CLI argument

Add a common outbound option to `record`, `replay`, and `test`:

```
--route direct
--route socks5://127.0.0.1:1080
--route socks5://127.0.0.1:1080__http://127.0.0.1:8080
```

Exact naming may differ only if documentation/tests are updated consistently.

`direct` uses the ordinary EggFetch client path. A non-direct value is parsed by `OutboundConnector::from_pproxy_uri()`, wrapped by `EggressDialer`, and supplied through EggFetch's custom Dialer seam.

Malformed/unsupported chains fail before network execution with credential-safe diagnostics. No fallback-to-direct is permitted.

Record redaction-safe physical route metadata in candidate/recorded flows where the current schema permits it.

## Exit-code contract

Define stable process categories instead of returning 1 for everything. Use a compact documented set, for example:

- 0 success;
- 1 regression/assertion mismatch;
- 2 invalid CLI/config/policy;
- 3 invalid/corrupt fixture;
- 4 network/runtime execution failure;
- 5 internal/unexpected failure.

The exact numeric mapping is part of the CLI compatibility contract once this plan closes. Errors printed to stderr must agree with JSON `failure_class`.

`replay` may return 0 when differences are merely reported; `test` returns the regression code when assertions fail.

## JSON contract

Keep the versioned envelope, but add tests for every failure class and ensure human rendering is not described as machine JSON.

Do not emit a success JSON object and then separately fail with an unrelated stderr classification.

## JUnit

Render one testcase per replayed/baseline flow (or named assertion unit), including stable testcase name/id and escaped failure details. Test counts, failure counts, and error counts must be correct.

JUnit is a projection of the report authority; it must not re-evaluate behavior independently.

## Inspect bodies

`inspect --bodies` must perform an explicit bounded body read and present a safe representation:

- UTF-8 text only when valid/appropriate;
- otherwise length + digest and optionally bounded base64/hex only if a separately explicit option is chosen.

Never bypass redaction markers. Large bodies must truncate with an explicit byte count, not allocate beyond configured CLI inspection bounds.

If actual body dumping is intentionally not supported in v0.1, remove `--bodies` instead of retaining placeholder behavior.

## Tests

- direct record/replay/test;
- SOCKS5 or HTTP proxy route using a local deterministic Eggress-compatible fixture;
- at least one two-hop pproxy expression construction/validation test;
- malformed route and credential-redacted failure;
- no silent direct fallback;
- every exit-code category through subprocess-level CLI tests;
- JSON stdout vs stderr separation;
- per-flow valid JUnit XML;
- `inspect --bodies` bounded output/redaction.

## Documentation

Update `docs/cli.md`, `docs/eggress-routing.md`, README quickstart, and release docs. Document that pproxy compatibility is used only as a construction grammar for listener-free Eggress routing.

## Acceptance

A user can run the same fixture directly and through an Eggress route from the installed CLI; failures have stable process codes; JSON/JUnit are parseable contracts; inspect no longer advertises unimplemented body behavior.

Closure record: `plans/closure/c004-cli-eggress-contracts.md`.
