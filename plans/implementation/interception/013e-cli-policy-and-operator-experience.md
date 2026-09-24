# M013E — CLI, Policy, and Operator Experience

Status: blocked
Depends on: M013D
Parent milestone: M013

## Objective

Expose the explicit proxy/interception system through a bounded, auditable CLI
and library surface without expanding the default Python wheel or automating
trust-store mutation.

## A. Cargo/CLI feature

Add an optional CLI feature, e.g. `intercept`, that enables
`eggreplay-intercept`.

Requirements:

- ordinary/default library/Python builds remain CA-dependency-free;
- help output clearly distinguishes when interception support is compiled in;
- invoking an interception command in a build without the feature fails with a
  clear capability message rather than silently using another mode.

Decide whether release binaries enable this feature only at M013F after the
security matrix is green. Source builds must remain able to exclude it.

## B. Command model

Prefer a distinct namespace rather than overloading ordinary record flags.

Recommended shape:

```text
eggreplay proxy record ...
eggreplay ca init ...
eggreplay ca import ...
eggreplay ca inspect ...
eggreplay ca export ...
eggreplay ca rotate ...
```

Equivalent naming is acceptable if it follows current clap conventions, but
the separation between ordinary gateway recording and interception must remain
obvious.

`proxy record` exposes:

- listen address;
- fixture path;
- route;
- target policy file/flags;
- CONNECT default action;
- CA directory for intercept rules;
- recording/redaction configuration;
- bounded connection/tunnel/certificate-cache limits;
- shutdown/finalization behavior.

## C. Policy file

Add a versioned declarative policy format.

No embedded scripting or regex is required initially. Prefer simple exact/safe
suffix match rules with explicit action:

`deny | tunnel | intercept`.

Parsing is bounded and fails closed on unknown fields/versions/actions.

Support a dry-run/validate operation that prints normalized policy without CA
key material.

## D. Machine-readable output

JSON output includes:

- bind address;
- compiled interception capability;
- CA public fingerprint when interception is enabled;
- policy version/rule counts;
- accepted/rejected/tunneled/intercepted connection counters;
- recorded flow count;
- bounded categorized failures.

Never emit:

- CA/leaf private key contents;
- private key filesystem path;
- Proxy-Authorization/cookies/auth values;
- decrypted request/response payloads unless an existing explicit safe inspect
  command would already allow them.

JUnit remains for regression/test results, not proxy operational telemetry.

## E. Trust installation documentation

Add operator documentation for installing the **public CA certificate** in
common client trust stores, clearly marked as manual and external to
EggReplay.

Document at least conceptual steps/links for:

- macOS;
- Windows;
- Linux/system CA stores;
- Firefox/browser-specific stores where applicable;
- Python/requests/httpx custom CA bundle environment/config;
- curl.

Do not run `security add-trusted-cert`, `certutil`, update-ca-certificates,
registry changes, browser profile edits, or equivalent commands on behalf of
the user.

Include removal/revocation guidance.

## F. Python policy

M013 does not modify the default `eggreplay` abi3 wheel to include
interception/CA dependencies.

Python users may:

- control a separately built interception-capable CLI process; or
- use a future separately qualified optional distribution/feature.

Do not expose a Python flag that claims interception exists when the wheel
cannot provide it.

## G. Diagnostics/security UX

Warnings should state that installing the CA allows EggReplay to impersonate
hosts within configured interception policy.

Errors identify target/action/category but remain credential/key-safe.

Do not sensationalize; provide concrete remediation for pinning, unsupported
H2/H3, untrusted CA, and policy denial.

## Required tests

- feature-off build contains no rcgen/interception crate;
- feature-on CLI command availability;
- loopback default;
- non-loopback explicit gate;
- policy parse/validate and unknown-version rejection;
- CA command no-overwrite/export safety;
- machine output sentinel scans;
- exit-code consistency;
- Ctrl-C shutdown/finalization;
- no automatic trust-store mutation (test by dependency/command audit);
- docs examples validated against actual help.

## Closure

Create `plans/closure/m013e-cli-policy-and-operator-experience.md`.
M013F becomes ready only after the operational surface is stable.
