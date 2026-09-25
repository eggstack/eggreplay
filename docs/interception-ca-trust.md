# Interception CA trust setup (manual operator action)

EggReplay's optional HTTPS interception (`eggreplay proxy record` with an
`intercept` policy) presents locally minted leaf certificates signed by a
dedicated operator-owned interception CA. Clients only trust those leaves if
the operator installs the CA's **public certificate** in the client's trust
store. Trust installation is always manual and external to EggReplay:
**no EggReplay command installs trust anywhere**, and there is deliberately no
flag to do so.

> Warning: installing this CA in a client allows EggReplay to impersonate any
> host within the configured interception policy for that client. Install it
> only in test clients you control, keep the CA directory (which holds the
> private key) restricted, and remove the trust anchor when you are done.

Typical lifecycle with the EggReplay CLI (interception-capable builds only;
see `eggreplay --help` reporting an `interception-capable build`):

```text
eggreplay ca init --dir ./intercept-ca
eggreplay ca inspect --dir ./intercept-ca
eggreplay ca export --dir ./intercept-ca --out ./intercept-ca-cert.pem
eggreplay ca import --dir ./intercept-ca-copy --cert ./other-cert.pem --key ./other-key.pem
eggreplay proxy validate --policy-file ./intercept-policy.json
eggreplay proxy record --listen 127.0.0.1:8080 --fixture ./proxy.eggr \
  --policy-file ./intercept-policy.json --ca-dir ./intercept-ca
eggreplay ca rotate --new-dir ./intercept-ca-next
```

`ca init` refuses to overwrite an existing directory, `ca export` copies the
**public certificate only** and refuses to overwrite its destination, and
`ca rotate` creates a distinct identity in a new directory (selecting it for a
listener is always an explicit `--ca-dir` choice). `proxy validate` prints the
normalized policy without CA key material.

## Installing the public certificate

Export it first (`eggreplay ca export --dir ./intercept-ca --out
./intercept-ca-cert.pem`), then install that file using your platform's normal
trust workflow. The exact UI moves over time, so prefer the vendor
documentation linked below over any remembered click path.

- macOS: add the certificate to a keychain with Keychain Access and set its
  trust to "Always Trust" for SSL, or use the Profiles/Settings trust flow.
  Start from Apple Support's documentation for managing certificates.
- Windows: install the certificate into the Current User "Trusted Root
  Certification Authorities" store via the Certificate Manager UI. Start from
  Microsoft Learn's documentation for managing trusted root certificates.
- Linux: each distribution owns its system bundle (for example a
  distribution-provided certificates directory plus a bundle refresh step, or
  the centralized trust-anchor tool on some distributions). Follow your
  distribution's documented procedure for adding a local CA, then verify the
  bundle your client actually reads.
- Firefox: Firefox can use its own certificate store instead of the OS store.
  Use Settings > Privacy & Security > Certificates > View Certificates >
  Authorities > Import, and limit trust to websites as appropriate. See
  Mozilla's documentation for managing certificates in Firefox.
- Python (`requests`/`httpx` and friends): point the client at a custom bundle
  instead of mutating the system store, for example with the conventional
  `REQUESTS_CA_BUNDLE` / `SSL_CERT_FILE` environment variables or the
  library's `verify=` parameter set to your bundle path. See the `requests`
  and `httpx` documentation for TLS/SSL configuration.
- curl: use `curl --cacert ./intercept-ca-cert.pem https://...` for a single
  invocation, or append the CA to a private bundle your invocations reference.
  See the curl documentation for `--cacert`.

## Removal and revocation

- Remove the CA from every store you added it to (reverse of the install
  steps above), or delete the custom bundle file if you used the
  environment/bundle approach.
- Rotating (`eggreplay ca rotate --new-dir ...`) mints a fresh identity; it
  does not revoke or delete the old one. Delete the old CA directory only
  after every client has stopped trusting it.
- There is no online revocation channel for a local test CA: removal from the
  client store plus deletion of the CA directory is the complete revocation
  story. Treat any CA directory you can no longer account for as compromised
  and remove its certificate from clients.

## When interception fails

- Certificate-pinned applications fail by design; record them through the
  ordinary gateway or replay path instead.
- Only HTTP/1.1 is intercepted. HTTP/2-only clients, QUIC/HTTP/3, non-HTTP
  TLS, WebSocket-over-TLS upgrades, and client-certificate (mTLS) flows fail
  explicitly or belong on an explicit `tunnel` policy without semantic capture.
- "Untrusted CA" client errors mean the public certificate is not installed in
  that client's store (each store above is independent).
- "Target denied by proxy policy" means the policy file or `--allow-host`
  flags do not cover the target; `eggreplay proxy validate` shows the
  normalized policy that was actually evaluated.
