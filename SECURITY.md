# Security policy

EggReplay records, stores, and replays HTTP interactions. A fixture is a
transcript of real traffic, so it must be treated as sensitive data; a listener
that serves one is an unauthenticated surface unless the operator puts a real
proxy or gateway in front of it.

## Reporting a vulnerability

Use GitHub private vulnerability reporting:

<https://github.com/eggstack/eggreplay/security/advisories/new>

Do not open a public issue, discussion, or pull request for an unfixed
vulnerability. If the private form is unavailable (the repository setting has
not been enabled), open a minimal public issue that asks for a private channel
and contains no reproduction, affected version, or exploit detail.

A useful report includes the affected revision or release, the command or API
surface, a minimal reproduction, and the impact you believe it has. We aim to:

- acknowledge a report within five business days;
- provide an initial assessment and severity within ten business days;
- coordinate disclosure within ninety days, or sooner once a fix ships.
  A reporter who needs a different timeline should say so in the report.

## Supported versions

| Version | Supported |
|---|---|
| `main` (unreleased) | yes, for security fixes |
| latest `0.1.x` release | yes, for security fixes |
| older releases | no; upgrade to the latest release |

Security fixes are developed on `main`, backported to the latest release line
where the fix is compatible, and announced with the advisory.

## Fixture and HAR confidentiality

Redaction is policy-based, not semantic understanding:

- The secure default profile redacts `authorization`, `proxy-authorization`,
  `cookie`, `set-cookie`, `x-api-key`, `api-key`, `x-auth-token`,
  `x-access-token`, `x-amz-security-token`, and `x-goog-api-key` by default.
  Query keys and JSON Pointer body paths are opt-in. `x-csrf-token`, `location`,
  and `referer` are deliberately not default-redacted; see
  [`docs/configuration.md`](docs/configuration.md).
- Stream-event trailer values are always persisted as `<redacted>`; the values
  are non-authoritative. A pre-fix fixture can be remediated with
  `eggreplay migrate --in-place --scrub-stream-trailers`.
- Opaque binary payloads are not semantically redacted. See
  [`docs/non-goals.md`](docs/non-goals.md).
- HAR comments and `interop-provenance.json` metadata are user-supplied and are
  not covered by header, query, or body redaction.

Fixtures are published with `0700` directories and `0600` files on Unix. On
Windows, `set_private_permissions` is a deliberate no-op: mode bits are a Unix
contract, and secrecy depends on the containing directory's ACLs. CA files
share this caveat. Exported HAR files and `--loss-report` outputs contain
recorded bodies; on Unix they are created `0600`, and elsewhere they depend on
the destination ACLs. Never commit a fixture or an exported HAR.

## Untrusted input

Fixture readers reject future schemas, incomplete manifests, malformed or
oversized JSONL, invalid blob names, hash/length mismatches, symlinked blobs,
and oversized manifests or HAR documents. Reads are bounded before allocation,
and display truncation is UTF-8-safe.

`Session::open_blob` checks `symlink_metadata` before opening a blob, then
verifies the opened file's length and streams the SHA-256 incrementally. The
check and the open are separate syscalls, so a local attacker **with write
access to an open fixture directory** could replace a blob with a symlink in
between (a TOCTOU race). A portable no-follow open requires a platform
dependency this corrective does not add, so the residual is accepted, not
hidden: such an attacker already controls fixture contents, the length/hash
check fails closed with an integrity error, and replaced bytes are never
returned as valid. Fixtures must not be mutated while a session or handle is
open; this is an immutability contract, not a sandbox for a compromised fixture
directory.

## Destructive operations

`--overwrite` replaces an existing **valid fixture or empty directory**, never
an arbitrary path. A symlink, a non-directory, a non-empty directory that does
not open as a fixture, and a future or incomplete fixture are refused before
anything is deleted. The new fixture is staged and validated first, so a failed
recording, import, or migration leaves the old fixture byte-identical. See
[`docs/cli.md`](docs/cli.md).

## Listener exposure

`serve` and `record` bind loopback by default. Binding any other address
requires `--allow-non-loopback` (or `allow_non_loopback=True` in the Python
lifecycle functions); the same rule applies to the explicit proxy. A replay or
recording listener has no authentication of its own, and TLS does not add
authentication. Put an authenticating proxy in front of any non-loopback
listener.

Repository settings that back this policy — private vulnerability reporting,
secret scanning, Dependabot alerts, branch protection, and Actions access — are
operator actions recorded in
[`docs/release-process.md`](docs/release-process.md).
