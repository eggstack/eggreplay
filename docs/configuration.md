# Configuration

Configuration is explicit and ordered: **CLI flag > explicit file > documented
default**. Nothing is read implicitly, and there is no ambient environment
profile — a flag you did not pass means the documented default, every time.
Introduce a new default only alongside a change to the meaning of an existing
one, and never in a minor release.

| Area | Where it lives |
|---|---|
| Output format | `--output human\|json\|junit`, on every command |
| Matcher profile | `--matcher-profile strict\|practical` (`serve` only) |
| Timing | `--timing-mode immediate\|recorded\|scaled:<factor>` (`serve` only) |
| Recording mode | `--record-mode sealed\|once\|append-new\|re-record` (`serve` only) |
| Redaction | `--redact-header`, `--redact-query`, `--redact-json-path`, `--redaction-profile`, `--unsafe-replace-default-redaction` (`record` only) |
| Comparison | `--scheduler`, `--max-concurrency`, `--compare-*` (`replay`, `test`, `diff` only) |
| Outbound route | `--route direct\|<pproxy URI>` |
| Timeouts | `--timeout-secs` (unset by default) |

Defaults worth knowing: `--matcher-profile` is `strict`, `--output` is
`human`, `--record-mode` is `sealed` (offline replay), `--route` is `direct`,
`--inbound` is `http1`, `--outbound-version` is `auto` (which means HTTP/1.1),
and `--timeout-secs` is **unset** so requests are unbounded until you ask
otherwise.

## Redaction policy (C003)

`record` accepts repeatable `--redact-header <NAME>`, `--redact-query <KEY>`,
and `--redact-json-path <POINTER>` selectors plus `--redaction-profile <ID>`
(persisted in session metadata) and `--unsafe-replace-default-redaction`
(replace secure defaults instead of extending them). `RedactionProfile`
(`sensitive_headers`, `sensitive_query_keys`, `sensitive_json_paths`) converts
to `RedactionConfig` for recording; `inspect` exposes the profile ID and typed
`RedactionMarker`s without secret values. Structured body redaction buffers
boundedly (1 MiB default) and fails closed on overflow, malformed JSON/form,
or unsupported media types. Opaque binary rewriting, entropy scanners, DLP,
encryption-at-rest, and arbitrary JSONPath are non-goals.

Redaction runs **before** any body is finalized, so a secret is never written
and then rewritten, and a redacted request field becomes a matcher wildcard
rather than a literal placeholder.

## Protocol and capability flags

`--inbound` (`http1`/`http2`) and `--outbound-version` (`auto`/`http1`/`http2`)
select protocols, and both fail closed with exit `2` when the build lacks the
required cargo feature. There is no silent fallback to HTTP/1.1. Optional
capabilities — interception, H2, gRPC — are cargo features, not config:
see [`./http2-support.md`](./http2-support.md).

