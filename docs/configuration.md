# Configuration

Configuration is explicit and ordered: **CLI flag > explicit file > documented
default**. Nothing is read implicitly, and there is no ambient environment
profile. A flag you did not pass means the documented default.
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

The secure default header set is exactly:

```
authorization, proxy-authorization, cookie, set-cookie,
x-api-key, api-key, x-auth-token, x-access-token,
x-amz-security-token, x-goog-api-key
```

The default query-key and JSON-pointer sets are **empty**. `--redact-header`
and `--redact-query` extend these sets; `--unsafe-replace-default-redaction`
replaces the header set with exactly the operator's selectors, so the expanded
defaults no longer apply.

The default expansion is a deliberate security-over-compatibility change.
A request header redacted by the default set becomes a matcher **wildcard**,
and replay sends the literal `<redacted>` for a recorded request that depended
on the live value. A semantic request that cannot proceed without a real
`x-api-key` therefore needs its recorded value restored deliberately (for
example by re-recording with `--unsafe-replace-default-redaction`), not by an
implicit fallback.

`location` and `referer` are deliberately **not** default-redacted: a response
`Location` drives redirects and is functional replay data, and `referer` is
routing/context metadata rather than a credential. `x-csrf-token` is
deliberately **not** default-redacted because replaying a CSRF-protected
request may require the recorded token; select it explicitly with
`--redact-header x-csrf-token`.

Redaction runs **before** any body is finalized, so a secret is never written
and then rewritten, and a redacted request field becomes a matcher wildcard
rather than a literal placeholder.

### Advisory redaction warnings

`record`, `har import`, and the network-capable `serve` recording modes scan
the finalized recording and emit value-free, advisory warnings through the
result envelope (`warnings` in JSON; stderr in human mode):

- a request header name in the curated set above, `x-csrf-token`, or ending in
  `-api-key`, `-auth-token`, or `-access-token` that is **not** in the
  effective redaction set;
- a request query key in `token`, `access_token`, `api_key`, `apikey`,
  `password`, `secret`, `sig`, or `signature` that is not in the effective
  query set;
- WebSocket acquisition enabled without `--redact-websocket-text`,
  `--redact-websocket-binary`, or a configured JSON pointer.

Warnings name the field only — never its value or a body — deduplicate per
command, and never change exit codes or payload schemas. They are a prompt,
not a guarantee: an unlisted credential name is still recorded unless the
operator selects it. WebSocket payload redaction remains **opt-in**; the
warning does not silently enable it. Stream-event trailer values are not
governed by this policy because they are never persisted (see
[`eggr-schema.md`](eggr-schema.md)).

## Protocol and capability flags

`--inbound` (`http1`/`http2`) and `--outbound-version` (`auto`/`http1`/`http2`)
select protocols, and both fail closed with exit `2` when the build lacks the
required cargo feature. There is no silent fallback to HTTP/1.1. Optional capabilities such as interception, H2, and gRPC are cargo features,
not configuration. See [`./http2-support.md`](./http2-support.md).

