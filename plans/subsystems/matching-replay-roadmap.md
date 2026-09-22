# Matching and Replay Roadmap

Status: active roadmap
Owners: M004, M005, M009

## Pipeline

```
request -> normalization -> candidate index -> predicates -> state/priority -> consumption -> response
```

Normalization and predicate evaluation remain separate so diagnostics can explain what changed.

## v0.1 profiles

**strict** compares method, normalized scheme/authority/path/query, configured headers, and body.

**practical** compares method + normalized route + meaningful body while ignoring a documented volatile-header set. Effective defaults must be inspectable.

## Body matchers

Initial support: no-body/presence, exact bytes/hash, exact UTF-8 text when requested, and semantic JSON equality with explicit ignored paths.

## Repeated interactions

A fixture is not a key/value map. Support deterministic ordered consumption plus explicit once, repeat-last, and unlimited behavior.

## Near miss

On failure, emit a bounded candidate set with explicit mismatch dimensions and redaction-safe field differences. Ranking is diagnostic only, not a hidden fallback match.

## M009 boundary

Authored scenario state machines and variable/template evaluation remain separate from recorded ordered consumption.
