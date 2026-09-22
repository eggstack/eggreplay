# M007 — Eggress Routing and Primary CLI

Status: blocked
Depends on: M003, M004, M006
Release gate: v0.1

## Objective

Expose the v0.1 product as a coherent CLI and add optional listener-free Eggress routing without changing semantic flow behavior.

## Eggress adapter

Implement an EggFetch `Dialer` backed by `eggress-outbound::OutboundConnector` for supported TCP HTTP/HTTPS routes.

Requirements:

- direct mode remains default;
- unsupported chains fail closed;
- map detailed Eggress failure facts without parsing display strings;
- redact route credentials;
- logical Host/SNI remains EggFetch-owned;
- no local Eggress listener/subprocess;
- no H3 claim through the TCP dialer.

## CLI

- `eggreplay record`: gateway capture with explicit listen/upstream/fixture and overwrite policy.
- `eggreplay serve`: offline replay/mock server.
- `eggreplay replay`: send recorded requests and emit observations/report without enforcing assertions by default.
- `eggreplay test`: regression comparison with stable CI exit classes.
- `eggreplay diff`: compare fixture sessions offline.
- `eggreplay inspect`: summaries/flows/effective policies; body dumping explicit.
- `eggreplay validate`: schema/blob integrity and compatibility.

## Output

Every result-producing command supports `--output human|json`; `test` also supports JUnit. JSON has a versioned envelope with command, schema version, success/failure class, warnings, and payload.

Operational logs go to stderr; machine results to stdout.

## Configuration

CLI > explicit config file > documented defaults. Environment variables may inject runtime secrets but must not silently alter matcher semantics.

## Acceptance

The documented sequence record -> inspect/validate -> offline serve -> replay/test -> routed replay works, and every result can be consumed from JSON without scraping text.
