# M014A — HAR Interchange and Fixture Migration Tooling

Status: blocked
Depends on: M013
Parent: M014

## Objective

Provide explicit lossy HAR interchange and first-class on-disk fixture
migration without making HAR canonical.

## HAR import

Map HAR entries into EggReplay flows with a structured loss report. Preserve
method, URL/query ordering where representable, headers, status, body content,
and basic timing metadata.

Reject or annotate unsupported/ambiguous fields. HAR cookies/header maps must
not silently collapse duplicates if the source representation preserves them.

Imported secrets pass through the selected EggReplay redaction policy before
fixture publication.

## HAR export

Export semantic flows to HAR only where meaningful. Emit an EggReplay-specific
loss/provenance section or side report covering trailers, typed errors,
scenario extensions, stream events, WebSockets, redaction markers, physical
routes, and any other information HAR cannot represent.

Never imply round-trip losslessness.

## Migration CLI

Add explicit commands such as:

```
eggreplay migrate --fixture old.eggr --output new.eggr
eggreplay migrate --in-place ...
```

Migration is transactional and never mutates the only source copy before the
new fixture validates. Support schema-1 -> current session schema and future
registered extension migrations.

Unknown required extensions block migration unless a registered migrator owns
them.

## Golden corpus

Maintain checked-in fixtures for every supported historical schema/extension
version and migration result. Validate idempotence of current->current
migration.

## Closure

Record loss matrices, migration fixtures, CLI contracts, and cross-platform CI
in `plans/closure/m014a-har-and-migration.md`.
