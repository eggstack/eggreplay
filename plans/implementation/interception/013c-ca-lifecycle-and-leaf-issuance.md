# M013C — Interception CA Lifecycle and Leaf Issuance

Status: blocked
Depends on: M013B
Parent milestone: M013

## Objective

Implement dedicated CA creation/import/inspection/export/rotation and bounded
exact-host leaf issuance without adding HTTPS interception yet.

## A. On-disk authority

Define a versioned CA directory, separate from `.eggr`, for example:

```text
ca-dir/
  metadata.json
  ca-cert.pem
  ca-key.pem
```

Metadata contains only public/sanitized information:

- format version;
- CA fingerprint;
- subject/issuer display fields;
- not-before/not-after;
- creation/import timestamp;
- key algorithm identifier;
- public certificate filename.

Never duplicate private key bytes into metadata.

Writes are staged and atomically published. Existing CA directories are not
silently overwritten.

## B. CA initialization

Use rcgen or another qualified maintained generator.

Generate a real CA identity with:

- `BasicConstraints: CA=true`;
- appropriate certificate-signing/key-usage extensions;
- bounded validity;
- secure key algorithm supported on every qualified platform;
- no wildcard SAN authority.

Defaults should be conservative and documented. Add explicit upper bounds to
validity and subject length.

## C. Import

Import accepts explicitly supplied certificate/key paths and validates:

- bounded PEM size;
- exactly one private key;
- cert/key pairing;
- certificate is a CA permitted to sign leaves;
- not currently expired/not-yet-valid beyond documented clock tolerance;
- supported algorithm;
- no unexpected extra private keys.

Reuse published `eggnet-tls::parse_identity_pem` where it fits, plus X.509
CA-property validation from the certificate-generation/parser library. Do not
write a custom DER parser.

Import copies into the dedicated CA directory; runtime use must not depend on
the original source path after successful import.

## D. File permissions

Unix:

- CA directory 0700;
- private key 0600;
- public certificate/metadata may be 0644.

Validate permissions after creation/import and reject insecure private-key
permissions unless the operator uses an explicit repair action.

Windows:

- do not claim Unix-mode equivalence;
- create files without broad sharing and qualify the actual filesystem/ACL
  behavior available through the chosen Rust APIs;
- if M013 cannot prove a restrictive per-user ACL without a new OS-specific
  dependency, document that limitation rather than claiming one.

Never chmod or modify the operator's original imported files.

## E. Inspect/export/rotate

Library operations must support:

- inspect public metadata/fingerprint;
- export/copy only the public CA certificate;
- create a rotated CA as a new directory/identity;
- explicitly select which CA an interception listener uses.

No operation auto-installs trust or exports private keys.

Rotation does not silently invalidate/replace an active process's CA. New
listener configuration explicitly selects the new identity.

## F. Leaf issuance

Add a bounded in-memory leaf issuer/cache.

Input is a previously policy-approved exact target:

- normalized ASCII DNS name; or
- exact IP literal.

Leaf requirements:

- exact SAN only;
- no wildcard;
- serverAuth EKU;
- bounded shorter validity than CA;
- serial uniqueness;
- signed by selected CA;
- ALPN/runtime config advertises only `http/1.1` later in M013D.

Cache key includes CA fingerprint + target identity. Enforce max entries,
maximum leaf lifetime, and deterministic eviction (e.g. LRU or FIFO).

Private leaf keys are memory-only in M013; do not persist them to disk or
fixtures.

## G. Secret handling

Implement redaction-safe `Debug`/errors for all key-bearing types.

Tests must scan:

- `.eggr`;
- JSON/JUnit/human command output;
- tracing capture;
- error strings;
- CA metadata;
- temporary/staging files after failure

for private-key sentinel material/path leakage.

Do not promise memory zeroization unless the selected key types actually
provide it; minimize copies/lifetimes instead.

## Required tests

- initialize/reopen;
- create_new/no overwrite;
- invalid/mismatched import;
- non-CA import rejected;
- expired/not-yet-valid policy;
- Unix permissions;
- Windows documented permission behavior;
- public export contains cert but no private key;
- rotation keeps identities distinct;
- DNS/IP leaf SAN correctness;
- no wildcard leaf;
- leaf cert verifies under CA;
- leaf fails under unrelated CA;
- cache hit/eviction/bounds;
- concurrent leaf requests for same target do not generate unbounded copies;
- no key leakage across output/fixture/temp artifacts.

## Closure

Create `plans/closure/m013c-ca-lifecycle-and-leaf-issuance.md`.
M013D remains blocked until CA/key handling is qualified.
