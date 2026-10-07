> Deep dive for [overview](overview.md).

# Interception (`eggreplay-intercept`)

`crates/eggreplay-intercept` is an optional, feature-gated leaf crate that adds
one acquisition path: an explicit HTTP/1.1 forward proxy that records
absolute-form requests, plus a policy-gated CONNECT policy with deny, opaque
passthrough, and opt-in HTTPS MITM. It is deliberately not part of the product
baseline, not part of the default Python wheel, and not a dependency of any
other product crate.

| Concern | Owner |
|---|---|
| Inbound H1 parsing, lifecycle, tunnel handoff | EggServe (`eggserve-server`, `eggserve-primitives`) |
| Raw `CONNECT` route establishment | Eggress (`eggress-outbound`, `pproxy-compat` only) |
| Semantic upstream HTTP + TLS verification | EggFetch (`eggfetch-core`) |
| X.509 generation and signing | `rcgen` |
| Neutral PEM/identity parsing, rustls helpers | `eggnet-tls` |
| Read-only certificate property checks | `x509-parser` |
| Policy, CA lifecycle, leaf cache, coherence, recording integration, CLI | this crate |

## Crate contract and opt-in discipline

The crate doc comment at `crates/eggreplay-intercept/src/lib.rs:1` is the
contract, and it is unusually explicit about what the crate must *not* become.
`lib.rs:31-37` states that interception adds no second Hyper client/server, no
SOCKS/CONNECT stack, no TLS verifier, and no X.509 implementation: generation
and signing belong to `rcgen`, cert/key pairing belongs to `eggnet-tls`, and
read-only certificate property checks belong to `x509-parser`. The same
paragraph is duplicated verbatim at `lib.rs:39-42`, which is a harmless but real
copy-paste artifact in the crate contract.

`#![forbid(unsafe_code)]` at `lib.rs:44` is the floor under that contract.

### Why it is a leaf

ADR 0003 (`plans/adrs/0003-interception-is-optional.md`) keeps TLS interception
out of the core execution path and the v0.1 release gate: "No CA generation,
trust-store mutation, certificate issuance dependency, or transparent-routing
requirement belongs in core/default builds." ADR 0008
(`plans/adrs/0008-interception-security-and-transport-boundary.md:36-47`) turns
that into a dependency rule: `eggreplay-core`, `eggreplay-store`,
`eggreplay-http`, and the default Python wheel must not depend on
certificate-generation or interception-key material dependencies, and the CLI may
expose interception only behind an explicit Cargo feature.

### How it is gated

The gate is the CLI feature, not a Cargo feature on the intercept crate itself.
`crates/eggreplay-cli/Cargo.toml:20` declares
`eggreplay-intercept = { path = "../eggreplay-intercept", optional = true }` and
`crates/eggreplay-cli/Cargo.toml:47` binds it to `intercept = ["dep:eggreplay-intercept"]`.
The default feature set deliberately omits it
(`crates/eggreplay-cli/Cargo.toml:35-36` records that release-binary policy is
decided at M013F and that library, Python wheel, and source builds stay
interception-free).

The CLI compiles both worlds. `crates/eggreplay-cli/src/intercept.rs:23` defines
`INTERCEPTION_COMPILED: bool = cfg!(feature = "intercept")`, and
`intercept.rs:29` defines `NOT_COMPILED_MESSAGE`. The clap help text is itself
feature-conditional: `intercept.rs:38-45` and `intercept.rs:198-205` switch
`about` between "interception-capable build" and "not compiled; rebuild with
`--features intercept`". So a default build still parses `eggreplay proxy` and
`eggreplay ca` and then fails with a capability error rather than reporting an
unknown command.

### What CI asserts about absence

Three separate hosted jobs police this, and the assertions are tree queries, not
prose:

- `dependency-boundary` (`.github/workflows/ci.yml:146-153`) loops over
  `eggreplay-core`, `eggreplay-store`, `eggreplay-http`, `eggreplay-cli`, and
  `eggreplay-python` and fails if any of their normal-edge `cargo tree` output
  contains a root-level `eggreplay-intercept` or `rcgen`. It then proves the
  crate still builds standalone with `cargo check -p eggreplay-intercept
  --all-targets --locked`.
- `protocol-boundary` (`.github/workflows/ci.yml:187`) includes
  `check_no_core "eggreplay-intercept" -p eggreplay-intercept --all-features`
  among the graphs that must not contain `eggserve-core`, `eggserve-static`, or
  `eggserve-h3`. This is the machine check behind the ADR 0008 rule that
  interception never adopts the multiprotocol serving layer: the intercept crate
  depends on `eggserve-server` and `eggserve-primitives` (H1 only) and can never
  silently gain HTTP/2, h2c, or QUIC serving.
- `interception` (`.github/workflows/ci.yml:369-403`) is the dedicated
  qualification lane on Ubuntu, macOS, and Windows. It runs
  `cargo check -p egggreplay-cli --no-default-features --locked` (default build
  stays interception-free), the standalone intercept check, an assertion that the
  Python graph is interception-free
  (`ci.yml:393`), then `cargo test -p eggreplay-intercept --all-features
  --no-fail-fast` and `cargo test -p eggreplay-cli --features intercept`. Using
  `--no-fail-fast` there is deliberate: every suite reports so isolation
  reproducers always emit signal even when a sibling suite fails.

## Transport ownership inside interception

### EggServe owns inbound H1

`ExplicitProxy` is an `eggserve_server::Service` (`proxy.rs:929-951`), and the
listener is an `eggserve_server::Server` started by `start_explicit_proxy`
(`proxy.rs:1142`). The service receives `TunnelCapability` from EggServe for
CONNECT (`call_with_tunnel`, `proxy.rs:944`) and issues the `200` only by calling
`capability.accept(...)` (`proxy.rs:891-892` for tunnels, `mitm.rs:648-654` for
interception). The crate never writes an HTTP response line itself for CONNECT;
it hands EggServe a `HeaderBlock::new()` and a relay closure.

Request-body policy is also EggServe's: `proxy.rs:930-938` returns
`RequestBodyPolicy::Reject` for CONNECT and
`RequestBodyPolicy::Stream { max_bytes }` otherwise.

### Eggress owns raw route establishment

`ProxyRoute` wraps an `EggressDialer` (`proxy.rs:311-316`). `direct()` uses
`EggressDialer::direct()`; anything else is parsed with
`eggress_outbound::OutboundConnector::from_pproxy_uri` (`proxy.rs:345-357`). The
crate depends on `eggress-outbound` with only the `pproxy-compat` feature
(`crates/eggreplay-intercept/Cargo.toml:18`), so it cannot reach SSH, QUIC, or
extended-transport grammars.

Two properties matter for a security crate. First, a malformed route fails
closed: `from_pproxy_uri` returns `ProxyError::Route` and never falls back to
direct (`proxy.rs:341-344`). Second, one dialer is shared by the EggFetch client
(`proxy.rs:594-597`), the CONNECT dial (`proxy.rs:861`), and MITM upstream
(`mitm.rs:434-438`), so plain, tunnel, and intercepted traffic cannot diverge
onto different routes. `ProxyRoute` also carries a `redacted_spec` used for
diagnostics and a `physical_route()` (`proxy.rs:377`) that is safe to record in
flow metadata; its `Debug` prints only the redacted spec and the direct flag
(`proxy.rs:318-326`).

### EggFetch owns semantic upstream verification

Plain requests go through `record_request_with_session` with the route dialer
(`proxy.rs:721-729`); intercepted requests do the same with the MITM client
(`mitm.rs:604-612`). Upstream TLS comes from `MitmConfig::upstream_tls`, and
`None` selects secure defaults (`mitm.rs:182-187`). There is deliberately no
verification-bypass setting; `mitm.rs:186` says so in prose, and the
`MitmError` surface has no such variant.

### `OriginOrAbsolute` vs origin-only

The proxy listener is built with
`Http1RequestTargetMode::OriginOrAbsolute` (`lib.rs:192`), because an explicit
forward proxy must accept `GET http://host/path`. Both accept forms are still
admission-controlled: `resolve_absolute_target` rejects any non-absolute target
with 400 (`proxy.rs:487-492`), so absolute-form is the only path a plain request
can take through this proxy. Conversely, a decrypted request inside an
intercepted tunnel must be origin-form: `mitm.rs:553-560` returns 400 and
*poisons* the connection for absolute-form, because an absolute target inside
one CONNECT would smuggle a second authority. Ordinary EggReplay gateway
listeners use `OriginOnly`; that is why `tests/substrate.rs:778-796` pins the
contrast — an `OriginOnly` listener rejects absolute-form with 400 before the
service is ever called, while `OriginOrAbsolute` still serves origin-form
(`tests/substrate.rs:635-641`).

### Single tunnel-admission authority

There is exactly one admission authority, and it is `TunnelCapability`.
Admission accounting happens before action dispatch
(`proxy.rs:775-790`): a `Deny` verdict counts as a rejection, and tunnel or
intercept count as admissions with a per-action counter at acceptance time.
Concurrency is one semaphore shared by both actions
(`proxy.rs:629`, `Semaphore::new(config.tunnel_limits.max_concurrent)`), acquired
with `try_acquire_owned` before any work — before dialing for tunnels
(`proxy.rs:847-858`) and before leaf issuance and `200` for interception
(`proxy.rs:813-824`). Excess CONNECT becomes 503 with `limit_reached`.

## Policy (policy.rs, policy_file.rs)

`policy.rs` is transport-neutral on purpose: its module doc at
`policy.rs:1-6` says it inspects only normalized host/port facts, "never
sockets, TLS state, or credentials."

### Bounds

| Constant | Value | Location |
|---|---|---|
| `MAX_POLICY_RULES` | 128 | `policy.rs:27` |
| `MAX_HOST_PATTERN_LEN` | 253 | `policy.rs:29` |
| `MAX_PORT_SET_SIZE` | 64 | `policy.rs:31` |
| `MAX_POLICY_FILE_BYTES` | 64 KiB | `policy_file.rs:60` |

`TargetPolicy::new` rejects an unbounded rule list with
`PolicyError::TooManyRules` (`policy.rs:588-594`); `PortMatch::set` enforces the
port-set bound; `normalize_host` rejects overlong names.

### Normalization

`NormalizedHost` is either `Dns(String)` (lowercase, no trailing dot) or
`Ip(IpAddr)` (`policy.rs:64-71`). `normalize_host` (`policy.rs:103`) accepts DNS
names case-insensitively with one optional trailing dot, IPv4 literals, and
bracketed or bare IPv6; it rejects userinfo, empty input, overlong names,
non-ASCII (IDNA is explicitly rejected in M013B), and malformed labels
(`policy.rs:93-102`).

`normalize_authority(authority, default_port)` (`policy.rs:218`) is the single
authority-parsing entry point and is where the fail-closed rules live: it
rejects an empty authority, any `@` (userinfo), any of `/ ? #` path syntax, and
unbalanced brackets (`policy.rs:222-240`). Port zero is rejected outright
(`policy.rs:296-298`). Because both the absolute-form path
(`proxy.rs:502-511`) and the CONNECT path (`proxy.rs:547`) go through it, an
authority cannot be interpreted two different ways in two different request
kinds.

`NormalizedTarget` is the normalized host plus an *explicit* port with the
authority default already applied (`policy.rs:84-91`).

### Rules and evaluation

`Rule` is the product of `HostMatch` (`ExactDns`, `SuffixDns`, `ExactIp`,
`policy.rs:304-312`), `PortMatch` (`Exact`, `Range`, `Set`, `Any`,
`policy.rs:388-403`), `RequestKind` (`Plain`, `Connect`, `Any`,
`policy.rs:484-491`), and `RuleAction` (`Allow`/`Deny`, `policy.rs:511-516`).
`SuffixDns` matches the parent and `.parent` on a label boundary only, so
`evil-example.com` never matches suffix `example.com` (`policy.rs:11-13`).
`PortMatch::Any` is permitted only inside an explicit rule — the policy default
still denies unmatched targets (`policy.rs:400-402`).

Evaluation is deterministic and fail-closed (`policy.rs:625-647`): each rule
scores as host + port + kind specificity, the most specific match wins, deny wins
ties, and unmatched targets deny. There is no allow-all default
(`policy.rs:18-20`).

`resolve_connect` (`policy.rs:665-670`) is where the three outcomes are chosen:
`Deny` stays `Deny`, and `Allow` resolves through the configured
`default_connect_action`. So `Intercept` only fires for explicitly allowed
targets under an intercept default, and unmatched targets deny.

### Default-action semantics

The default action is not a fallback allow; it is what an *allowed* CONNECT
becomes. `TargetPolicy::deny_all(default)` (`policy.rs:603`) gives a
zero-rule policy, and unmatched CONNECT still denies because
`evaluate` returns `Deny` and only `Allow` consults the default.

### File form

`policy_file.rs` is the M013E operator surface: versioned JSON, no scripting, no
regex, no new dependency (`policy_file.rs:1-6`). `INTERCEPT_POLICY_VERSION` is
`"eggreplay-intercept-policy/v1"` (`policy_file.rs:58`). `FilePolicy::parse_json`
(`policy_file.rs:183`) and `load_file` (`policy_file.rs:198`, which stats and
also uses `take(limit + 1)` on the read) are both bounded.

`FileAction` (`policy_file.rs:101-108`) is a three-value vocabulary —
`Deny`, `Tunnel`, `Intercept` — and its two projections are the key design
move: `rule_action()` maps `Tunnel | Intercept` to `RuleAction::Allow`
(`policy_file.rs:135-140`) while `connect_action()` preserves the CONNECT
distinction. `FilePolicy::check_coherence` (`policy_file.rs:242-255`) then
rejects a policy whose non-deny CONNECT-covering rules disagree with
`default_connect_action` as `PolicyFileError::MixedActions`, because such a
policy would have no single truthful operational meaning. Operators who need both
behaviors run separate listeners. `to_target_policy` (`policy_file.rs:296`) then
produces the transport-neutral policy.

`parse_default_action` (`policy_file.rs:621`) is a thin wrapper over the file's
action parser. `policy_from_flags` (`policy_file.rs:564`) builds the same
`FilePolicy` from `--allow-host`/`--deny-host` with `PortMatch::Any` and
`RequestKind::Any`; it derives the allow-side action *from the default action*
so flag-built policies are coherent by construction, and it still runs
`check_coherence` (`policy_file.rs:612`). A `deny` or `tunnel` default yields
tunnel allows; an `intercept` default yields intercept allows.

One thing that is deliberately absent: the CA directory is never `.eggr`. The
`ca.rs` module doc states the CA directory is "deliberately separate from
`.eggr` fixture storage" and lives at a caller-selected path
(`ca.rs:5-6`); the CLI takes it as an explicit `--ca-dir`
(`crates/eggreplay-cli/src/intercept.rs:130-132`).

## Header filtering (headers.rs)

`filter_proxy_headers` (`headers.rs:46`) is the only place proxy framing is
removed, and it is deliberately one function so the plain and decrypted paths
cannot drift. The policy is documented at `headers.rs:30-44`:

| Header | Treatment |
|---|---|
| `Proxy-Connection`, `Proxy-Authorization`, `Proxy-Authenticate`, `Keep-Alive` | always stripped (`headers.rs:23-28`) |
| `Connection` | stripped, and every header it nominates is stripped |
| `Upgrade` | stripped and reported via `upgrade_requested` so the caller can reject |
| `TE` | forwarded only when its sole token is `trailers` |
| `Transfer-Encoding`, `Trailer` | preserved as end-to-end framing unless `Connection`-nominated |
| everything else | passes through untouched, duplicates and order preserved |

Two details carry security weight. `Connection`-nominated tokens are collected
*before* `Connection` itself is removed (`headers.rs:51-67`), and a nominated
`upgrade` token sets `upgrade_requested` even when there is no literal `Upgrade`
header. And `HeaderMap` iteration yields `None` for continuation values of a
duplicated key, so the code carries the key's decision forward to keep
duplicates from being filtered inconsistently (`headers.rs:88-95`).

`ProxyFilterOutcome` (`headers.rs:10-20`) reports `forwarded`, deduplicated
lowercase `removed` *names*, `upgrade_requested`, and
`had_proxy_authorization`. Values are never retained, so the outcome itself is
safe to log.

Callers must still act on the flags. The plain path returns 400 for an upgrade
(`proxy.rs:708-712`); the decrypted path returns 400 for an upgrade inside
interception (`mitm.rs:585-593`), which is what makes WSS a documented
unsupported row rather than a silent misparse.

## CONNECT tunnel (tunnel.rs)

`tunnel.rs` never parses application bytes. The module doc at `tunnel.rs:1-10`
states the relay moves bytes between the EggServe `TunnelIo` downstream and the
Eggress-established upstream with direct backpressure, and that no tunnel
content enters logs, fixtures, or errors.

### Bounds

| Constant | Value | Location |
|---|---|---|
| `DEFAULT_TUNNEL_MAX_BYTES` | 256 MiB, both directions | `tunnel.rs:21` |
| `DEFAULT_TUNNEL_MAX_DURATION` | 300 s | `tunnel.rs:23` |
| `DEFAULT_TUNNEL_IDLE_TIMEOUT` | 60 s, no progress | `tunnel.rs:25` |
| `DEFAULT_TUNNEL_MAX_CONCURRENT` | 16 | `tunnel.rs:27` |
| `DEFAULT_TUNNEL_CONNECT_TIMEOUT` | 10 s | `tunnel.rs:29` |
| `MAX_CONCURRENT_TUNNELS` (hard sanity ceiling) | 4096 | `tunnel.rs:34` |

`TunnelLimits::new` (`tunnel.rs:70`) refuses zero bounds and a concurrency
ceiling outside `1..=4096`, so an operator flag cannot disable a bound by
setting it to zero.

### Outcomes and events

`TunnelOutcome` is a six-value closed set with fixed machine-readable names
(`tunnel.rs:106-134`): `completed`, `byte-limit`, `duration-limit`,
`idle-timeout`, `relay-error`, `shutdown`.

`relay_tunnel` (`tunnel.rs:370`) wraps both streams in a `Metered` reader
(`tunnel.rs:292-329`) that adds the two directional counters and fails the read
once the shared budget is spent, then races three futures: the
`copy_bidirectional` relay, the total-duration sleep, and an idle watcher
(`tunnel.rs:423-427`). The idle watcher polls at a clamped interval and resets
its quiet clock only when the *sum* of both counters changes
(`tunnel.rs:403-421`). Classification prefers the byte ceiling over `shutdown`
when a bound fired and the relay was aborted by that bound
(`tunnel.rs:441-451`), and `debug_assert!` ties `limit_fired` to
`TunnelOutcome::ByteLimit` (`tunnel.rs:464`).

`TunnelRelaySummary` carries only counts and duration. `TunnelEvent`
(`tunnel.rs:190-207`) adds normalized host (truncated to 253 chars,
`tunnel.rs:228`), port, a fixed `action` label, the outcome name, directional
bytes, duration, and an error string truncated to 128 chars (`tunnel.rs:31`,
`tunnel.rs:237`). M013D reuses the same shape with the `"intercept"` label
(`tunnel.rs:216-219`). `TunnelEventLog::MAX_EVENTS` is 256 and `push` drops the
oldest when full (`tunnel.rs:250`, `tunnel.rs:261-268`) — a poison-lock
degrades to an empty snapshot rather than panicking
(`tunnel.rs:272-276`).

### The three CONNECT outcomes

| Outcome | Policy source | Implementation behavior |
|---|---|---|
| deny | any explicit deny, or unmatched | capability dropped, 403, `policy_denied`, never dialed (`proxy.rs:779-787`) |
| opaque passthrough | `ConnectAction::Tunnel` | permit acquired, dial under `connect_timeout`, then `200`; 504 on dial timeout, 502 on dial error (`proxy.rs:847-892`) |
| intercept | `ConnectAction::Intercept` | permit acquired, leaf minted, rustls config built, then `200`; afterwards bounded TLS events only (`proxy.rs:813-836`, `mitm.rs:633-656`) |

The passthrough path dials *before* accepting, and the comment at
`proxy.rs:859-860` says why: the `200` handshake is sent only after the target
route is established, and there is no fallback to direct. The relay closure
races `relay_tunnel` against request-lifecycle cancellation
(`proxy.rs:896-906`) so shutdown ends the tunnel with a `Shutdown` outcome
rather than relaying forever.

## The explicit proxy (proxy.rs)

`ExplicitProxyConfig` (`proxy.rs:396-434`) bundles the recording session, the
policy, the route, redaction (`RedactionConfig::default_secure()`), a profile id
of `explicit-proxy-v1`, `DEFAULT_MAX_STRUCTURED_REDACTION_BYTES`, default
tunnel limits, `DEFAULT_MAX_PROXY_BODY_BYTES` (8 MiB, `proxy.rs:60`), 64
connections, and `mitm: None`.

That last field is the M013B/M013D seam. `None` preserves M013B behavior: an
`Intercept` verdict then fails closed *before* `200` with
`FAILURE_NOT_CONFIGURED` (`proxy.rs:805-812`). `ExplicitProxy::new`
(`proxy.rs:590`) builds the MITM state from the *same validated* `RuntimeConfig`
the listener runs, and carries a `debug_assert!` that the state builds
(`proxy.rs:611-614`) — in debug builds a misconfigured `MitmConfig` is a panic
rather than a silent late failure, while release degrades to `None` and the
fails-closed branch.

### Target resolution

`resolve_absolute_target` (`proxy.rs:479`) requires absolute form, the `http`
scheme only (HTTPS goes through CONNECT), a parseable authority, a present
`Host`, and that `Host` normalizes to the *same host and port* as the absolute
target (`proxy.rs:512-517`). Default-port equivalence applies. It returns a
`CanonicalProxyTarget` whose `upstream_uri` is a canonical `http://host:port/path?query`.
`resolve_connect_target` (`proxy.rs:543`) applies the 443 default and delegates
to the same `normalize_authority`, so userinfo, port zero, and path syntax are
rejected identically in both request kinds.

### Bind safety

`validate_bind` (`proxy.rs:241`) is the gate: loopback always passes; anything
else requires an explicit opt-in. `ProxyListenerConfig::loopback` refuses a
non-loopback address outright, and `with_remote_opt_in` is the only way to get
one (`proxy.rs:264-280`). `start_explicit_proxy` re-validates before building
anything (`proxy.rs:1146`). The `BindError` message
(`proxy.rs:222-227`) states the consequence plainly: remote exposure without
proxy authentication creates an open forward proxy. The CLI mirrors this —
`ProxyRecordArgs::allow_non_loopback` is a separate flag
(`crates/eggreplay-cli/src/intercept.rs:133-135`), and a non-loopback listen
emits an explicit open-proxy warning
(`crates/eggreplay-cli/src/intercept.rs:566-573`).

### Operational-event model for rejections

Rejections are counted, never narrated. `ProxyStats` (`proxy.rs:105-112`) keeps
monotonic `AtomicU64` counters: `accepted`, `rejected`, `tunneled`,
`intercepted`, `flows`, plus a fixed-size failure array indexed by
`PROXY_FAILURE_CATEGORIES`. `ProxyStatsSnapshot` (`proxy.rs:203-216`) is the
serializable form and holds counts only — the doc comment says explicitly that
it contains no key material, paths, credentials, or payloads.

The nine categories are closed and named (`proxy.rs:65-95`):
`policy_denied`, `invalid_authority`, `upstream_failed`, `tls_failed`,
`authority_mismatch`, `limit_reached`, `not_configured`, `unsupported`,
`internal`. Bounded categories are what make this safe to serialize: a new
failure mode must be added to the array, so the output shape cannot grow
unbounded with untrusted input.

The mapping is tight. Malformed authority in either kind →
`invalid_authority` (`proxy.rs:682`, `proxy.rs:747`, `proxy.rs:767`); policy
denial → `policy_denied` (`proxy.rs:692`, `proxy.rs:782`); dial failure or
timeout → `upstream_failed` (`proxy.rs:869`, `proxy.rs:875`); admission
rejection → `limit_reached` (`proxy.rs:708` etc.); upgrade or non-origin-form
inside interception → `unsupported`/`invalid_authority`
(`mitm.rs:588`, `mitm.rs:555`); cross-origin → `authority_mismatch`
(`mitm.rs:567`); TLS failure after acceptance → `tls_failed` (`proxy.rs:833`).

## CA lifecycle (ca.rs)

The CA is a dedicated operator-owned identity, never fixture content. The
directory holds exactly three files (`ca.rs:6`): `ca-cert.pem`, `ca-key.pem`,
`metadata.json`.

| Constant | Value | Location |
|---|---|---|
| `CA_FORMAT_VERSION` | 1 | `ca.rs:81` |
| `CA_CERT_FILENAME` | `ca-cert.pem` | `ca.rs:83` |
| `CA_KEY_FILENAME` | `ca-key.pem` | `ca.rs:85` |
| `CA_METADATA_FILENAME` | `metadata.json` | `ca.rs:87` |
| `MAX_PEM_FILE_BYTES` | 64 KiB | `ca.rs:89` |
| `MAX_METADATA_BYTES` | 64 KiB | `ca.rs:91` |
| `MAX_CA_SUBJECT_CN_CHARS` | 128 | `ca.rs:93` |
| `DEFAULT_CA_VALIDITY_DAYS` | 365 | `ca.rs:97` |
| `MIN_CA_VALIDITY_DAYS` | 31 | `ca.rs:99` |
| `MAX_CA_VALIDITY_DAYS` | 1825 | `ca.rs:101` |
| `CA_CLOCK_TOLERANCE` | 5 min | `ca.rs:103` |
| `CA_DIR_MODE` | `0700` | `ca.rs:109` |
| `CA_KEY_MODE` | `0600` | `ca.rs:111` |
| `CA_PUBLIC_MODE` | `0644` | `ca.rs:113` |
| `CA_KEY_ALGORITHM_ID` | `ECDSA-P256-SHA256` | `ca.rs:107` |

### Trust installation is manual

There is no code path that installs trust. `ca.rs:42` states it in one line: "No
operation installs trust anywhere or exports private keys."
`docs/interception-ca-trust.md:7-9` says the same and adds that there is
deliberately no flag to do so. The documented lifecycle is
`ca init` / `ca inspect` / `ca export` / `ca import` / `ca rotate`, with the
operator installing the exported *public* certificate through each platform's
normal trust workflow (`docs/interception-ca-trust.md:43-65`), and removal plus
directory deletion being the complete revocation story
(`docs/interception-ca-trust.md:69-78`).

### Permission policy

Unix: directory `0700`, key `0600`, cert/metadata `0644` (`ca.rs:46`).
`check_permissions` (`ca.rs:902`) requires the directory and key modes to be
*exactly* `CA_DIR_MODE` and `CA_KEY_MODE` and returns
`CaError::InsecurePermissions` with the observed mode otherwise
(`ca.rs:914-928`). `CaAuthority::open` calls it (`ca.rs:404`), so an
insecurely-permissioned directory fails closed rather than silently signing
leaves. `repair_ca_permissions` (`ca.rs:564`) is the explicit operator action and
returns `PermissionRepairUnsupported` off Unix (`ca.rs:565-569`).

Windows: `check_permissions` compiles to a trivial `Ok(())`
(`ca.rs:903-907`), and the module doc refuses to claim Unix equivalence — CA
directory secrecy there depends on the operator's profile-directory ACLs
(`ca.rs:51-56`). The threat model repeats this limit
(`docs/interception-threat-model.md:97-101`).

### Rotation

Rotation is creation of a *distinct identity in a new directory*
(`ca.rs:38-41`), never a mutation. `create_new` and `import` both refuse an
existing target with `CaError::AlreadyExists` (`ca.rs:329-331`,
`ca.rs:376-378`), and `export` refuses an existing destination with
`ExportDestExists` (`ca.rs:480-482`). Handles are immutable snapshots, so
creating a new CA cannot disturb an active `CaAuthority`. `open` deliberately
does *not* enforce expiry so expired CAs stay inspectable and exportable during
rotation; issuance is where validity is enforced (`ca.rs:34-37`).

`inspect_ca` (`ca.rs:499`) reads only public metadata, validates the format
version, the cert filename, the key-algorithm id, and the fingerprint binding
against the stored certificate — and never reads the key. `export_ca_cert`
(`ca.rs:546`) calls `inspect_ca` first and then copies the public certificate
only.

Secret hygiene is structural: `CaError` messages carry static descriptions and
public facts only (`ca.rs:119-123`), `CaAuthority`'s `Debug` is redacted to
fingerprint and validity facts (`ca.rs:308-318`), and `CaMetadata` holds public
facts exclusively so it is safe to persist (`ca.rs:224-227`).

`CaOrigin` (`ca.rs:217-222`) is `Created` or `Imported`, recorded in metadata.
Import is restricted to self-signed *roots*: subject `==` issuer plus a
cryptographic self-signature check, because an interception trust anchor
installed in clients must be a root (`ca.rs:28-30`).

## Leaf issuance (leaf.rs)

`LeafIssuer` (`leaf.rs:216-221`) is bound to one explicitly selected `CaAuthority`
and owns it, so rotation constructs a new issuer around a new authority
(`leaf.rs:212-215`).

| Constant | Value | Location |
|---|---|---|
| `MAX_LEAF_CACHE_ENTRIES` | 128 | `leaf.rs:54` |
| `DEFAULT_LEAF_VALIDITY_HOURS` | 168 (7 days) | `leaf.rs:56` |
| `MAX_LEAF_VALIDITY_HOURS` | 720 (30 days) | `leaf.rs:58` |
| `MIN_LEAF_VALIDITY_HOURS` | 1 | `leaf.rs:60` |
| `MAX_LEAF_CN_CHARS` | 64 | `leaf.rs:66` |
| `INTERCEPT_ALPN_HTTP1_1` | `b"http/1.1"` | `leaf.rs:71` |

`LeafOptions` validates the hour range (`leaf.rs:122-124`).

### Exact SAN only, `serverAuth`, TLS-server capable

`issue` (`leaf.rs:306`) rejects a target containing `*` or empty
(`leaf.rs:308-310`). `mint` (`leaf.rs:351`) builds exactly one SAN —
`SanType::DnsName` for DNS or `SanType::IpAddress` for IP
(`leaf.rs:358-365`) — and sets `IsCa::ExplicitNoCa`, key usage
`DigitalSignature`, extended key usage `ServerAuth`, and
`use_authority_key_identifier_extension` (`leaf.rs:378-381`). No wildcard leaf
is ever producible, which is the structural reason leaf cache cardinality
cannot explode through attacker-chosen names. The subject CN is a display
convenience only, dropped above 64 characters, and the doc says so
(`leaf.rs:61-66`).

Validity is always capped by the issuing CA: `not_after` is
`min(now + requested, ca_not_after)`, and a CA that cannot offer at least
`MIN_LEAF_VALIDITY_HOURS` yields `LeafError::CaExpiring` rather than a useless
leaf (`leaf.rs:316-323`). Outside the CA window entirely is `CaNotValid`
(`leaf.rs:313-315`).

### Cache and locking

The cache key is `CA-fingerprint/target` (`leaf.rs:324`), so a rotated CA can
never reuse another CA's leaves. Eviction is deterministic FIFO against
`MAX_LEAF_CACHE_ENTRIES` (`leaf.rs:336-343`); expired entries are replaced on
access (`leaf.rs:328-333`) and can be swept by `remove_expired`
(`leaf.rs:276-294`).

The lock discipline is the security-relevant part. One `tokio::sync::Mutex`
guards the cache and is held across issuance (`leaf.rs:326`), and the module doc
justifies that: signing is local CPU work with no network or disk I/O, so no
global lock is ever held across a network operation, and concurrent requests
for the same target share one issuance (`leaf.rs:21-26`). `MitmInner`'s doc
restates the audit result (`mitm.rs:373-380`): the leaf-cache mutex guards only
fast local CPU signing, the TLS handshake holds only the per-connection tunnel
permit, and session appends serialize only the final bounded metadata write.

Leaf private keys are memory-only. `LeafCertificate` holds the `rcgen::KeyPair`
(`leaf.rs:144`) with a redacted `Debug` that omits it
(`leaf.rs:147-157`); `key_pair()` returns a reference with an explicit warning
against serializing or logging it (`leaf.rs:196-203`).

## MITM recording (mitm.rs)

`mitm.rs:22-36` documents the sequence, and the ordering is the security
property: parse and normalize the CONNECT authority, evaluate policy, acquire
the exact-host leaf **before** acknowledging CONNECT, send `200` via tunnel
acceptance, handshake with ALPN `["http/1.1"]` only, require the negotiated ALPN
to be absent or `http/1.1`, check SNI coherence, construct truthful HTTPS
`TlsInfo` + `ConnectionContext`, then run the decrypted stream through the
EggServe H1 driver. A failure before `200` is an HTTP proxy error with no flow; a
TLS failure after `200` is a tunnel/TLS failure that closes the connection with
a bounded event and *never* fabricates a flow.

| Constant | Value | Location |
|---|---|---|
| `MITM_PROFILE_ID` | `explicit-proxy-mitm-v1` | `mitm.rs:101` |
| `MITM_ROUTE_KIND` | `explicit_proxy_mitm` | `mitm.rs:99` |
| `DEFAULT_TLS_HANDSHAKE_TIMEOUT` | = `DEFAULT_TUNNEL_CONNECT_TIMEOUT` (10 s) | `mitm.rs:113` |
| `MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS` | = `DEFAULT_TUNNEL_MAX_CONCURRENT` (16) | `mitm.rs:119` |

Those last two are defined as equalities to the tunnel constants rather than
independent literals, and the docs explain why
(`mitm.rs:102-120`): handshake concurrency is bounded by the *shared* tunnel
semaphore, because `serve_intercepted_connect` acquires a permit before leaf
issuance and holds it for the whole decrypted connection. The constant documents
a consequence of one semaphore rather than being a second limiter. The
`resource_bounds` integration test pins that correspondence so drift fails
(`mitm.rs:110-112`).

### Arming

`MitmConfig` (`mitm.rs:179-196`) carries the issuer, optional upstream TLS trust
(`None` = secure defaults), profile id, redaction, and body ceiling; its `Debug`
prints only `upstream_tls_custom: bool` (`mitm.rs:198-208`).
`MitmPolicy::new` (`mitm.rs:245`) refuses to construct unless the policy default
is `ConnectAction::Intercept`, returning `PolicyNotArmed` otherwise
(`mitm.rs:246-248`). Rule evaluation is fully reused, so `Intercept` still fires
only for explicitly allowed targets.

### Authority coherence

`check_sni_coherence` (`mitm.rs:275`) allows absent SNI but requires a present
one to normalize equal to the CONNECT host — which is what makes a DNS SNI on an
IP CONNECT fail closed (`mitm.rs:268-270`). `check_http_authority_coherence`
(`mitm.rs:297`) requires a `Host` that normalizes to the CONNECT host *and* port
under default-port equivalence. Neither ever consults `Forwarded` or
`X-Forwarded-*` (`mitm.rs:291`), and neither is used to derive trust.
`check_authority_coherence` (`mitm.rs:322`) composes the two.

Cross-origin reuse is poisoned, not merely rejected:
`MitmService` holds an `AtomicBool` (`mitm.rs:483`); the first mismatch sets it
and returns 421 (`mitm.rs:566-572`), absolute-form inside the tunnel sets it and
returns 400 (`mitm.rs:553-560`), and every later request on that connection
fails closed with 400 and no flow (`mitm.rs:544-549`). The service doc states
the invariant plainly (`mitm.rs:473-478`).

### The decrypted H1 driver

`build_intercept_server_config` (`mitm.rs:340`) builds chain `[leaf, ca]` with
the leaf's memory-only PKCS#8 key, `with_no_client_auth()`, safe default
protocol versions, and `alpn_protocols = vec![INTERCEPT_ALPN_HTTP1_1]`
(`mitm.rs:358`). It is a per-target configuration built per handshake, so there
is no shared mutable rustls state between origins.

`accept_decrypted_tls` (`mitm.rs:671`) wraps the handshake in
`timeout(handshake_timeout, ...)` so a silent peer cannot park an admitted
tunnel (`mitm.rs:693`). On failure it records a bounded event and returns
`None` — the `finish` closure at `mitm.rs:683-691` builds
`TunnelEvent::new_with_action(..., "intercept", ...)` and returns, and the
callers at `mitm.rs:771-773` and the rest of `serve_decrypted` do nothing else.
**This is the no-fabrication rule in code**: there is no code path from a failed
handshake to `record_request_with_session`.

The negotiated ALPN is re-checked post-handshake and any value other than
absent-or-`http/1.1` fails the connection with `unsupported`
(`mitm.rs:705-716`). `TlsInfo` is then built with
`client_authenticated: false` and `peer_certificates_present: false`
(`mitm.rs:729-736`), so client mTLS is represented as absent rather than
fabricated.

`serve_decrypted` (`mitm.rs:741`) constructs
`ConnectionContext::for_non_socket(Scheme::Https, Some(info))` — truthful HTTPS
metadata for a non-socket inner stream — and runs
`serve_http1_connection_with_policy` in a spawned task using the runtime's
`H1ConnectionPolicy` and a dedicated `RuntimeState` (`mitm.rs:774-796`). It
races the driver against request-lifecycle cancellation
(`mitm.rs:800-809`) and then allows a 5-second graceful window before reclaiming
the task, so proxy shutdown never hangs on an idle decrypted connection
(`mitm.rs:810-817`).

Recording goes through the same `RecordingSession` authority as plain requests
(`mitm.rs:604-612`), using the shared `proxy_body_stream` bridge
(`mitm.rs:597`) so body and trailer semantics are identical on both paths
(`proxy.rs:1049-1056`). The session-agnostic property is deliberate: recording
sessions and re-record/replacement temporary sessions compose without a second
updater, and a sealed or finished session fails at the store boundary
(`mitm.rs:48-55`).

## InterceptionProfile and substrate

`InterceptionProfile` (`lib.rs:135-155`) is the single place the EggServe
runtime decisions live, and its fields are public specifically so focused tests
can assert them — "a future EggServe upgrade must not silently change default
ownership or accept-form semantics" (`lib.rs:130-133`).

| Bound | Value | Location |
|---|---|---|
| `max_request_body_bytes` | 8 MiB | `lib.rs:165` |
| `max_request_target_bytes` | 8 KiB | `lib.rs:166` |
| `max_in_flight_requests` | 64 | `lib.rs:167` |
| `max_active_tunnels` | 64 | `lib.rs:168` |
| `max_connections` | 64 | `lib.rs:169` |
| `connection_total_timeout` | 60 s | `lib.rs:170` |
| `keep_alive_idle_timeout` | 60 s | `lib.rs:171` |
| `response_write_timeout` | 30 s | `lib.rs:172` |

`build_runtime_config` (`lib.rs:189`) applies them plus
`Http1RequestTargetMode::OriginOrAbsolute`,
`H1PolicyOwnership::eggserve_owned()`, and
`AdmissionOwnership::eggserve_owned()` (`lib.rs:192-194`) — so H1 policy and
admission are EggServe's, not the proxy's, and the crate cannot acquire a second
admission authority.

### Finding: the `substrate` baseline constants are stale

`pub mod substrate` (`lib.rs:107-126`) publishes a "published
transport/TLS dependency baseline qualified by M013B0". Three of its entries do
not match the workspace pins:

| `substrate` constant | Declared | Workspace pin (`Cargo.toml`) |
|---|---|---|
| `EGGSERVE_SERVER` | `0.3.0` (`lib.rs:109`) | `eggserve-server = "=0.4.0"` (`Cargo.toml:73`) |
| `EGGSERVE_PRIMITIVES` | `0.2.1` (`lib.rs:111`) | `eggserve-primitives = "=0.2.2"` (`Cargo.toml:72`) |
| `EGGRESS_OUTBOUND` | `1.0.8` (`lib.rs:113`) | `eggress-outbound = "=1.0.11"` (`Cargo.toml:74`) |

The crate's own `Cargo.toml` uses the workspace values verbatim
(`crates/eggreplay-intercept/Cargo.toml:15-18`), so the compiled code runs
against 0.4.0 / 0.2.2 / 1.0.11 while the published baseline advertises 0.3.0 /
0.2.1 / 1.0.8. The remaining entries are consistent: `EGGNET_TLS` `0.2.0`
matches `Cargo.toml:75`, `RUSTLS` `0.23.45` matches `Cargo.toml:80`,
`TOKIO_RUSTLS` `0.26.2` matches `Cargo.toml:81`, `RCGEN` `0.13.2` matches
`Cargo.toml:82`, and `X509_PARSER` `0.16.0` / `TIME` `0.3.55` are consistent with
the semver ranges at `Cargo.toml:54-55`.

The drift is not caught. `tests/resource_bounds.rs:160-170`
(`substrate_versions_are_pinned`) asserts each constant against the *same*
literal, so it passes while the published baseline is wrong. The doc comment
`"qualified by M013B0"` is accurate as history — M013B0 pinned
`eggserve-server 0.3.0` and `eggserve-primitives 0.2.1`
(`plans/closure/m013-explicit-proxy-and-optional-mitm.md:31-34`) — but the
module reads as a current baseline. Reported here as a finding; not fixed, per
scope. The practical risk is documentation, not behavior: nothing in the
build or the test suite depends on these strings, but a reviewer or an
adopter reading `substrate` would draw a wrong conclusion about the qualified
substrate.

## Support matrix

Interception-scoped rows restating `docs/testing.md` § Supported matrix and
`docs/non-goals.md` § Out of the support claim; the README no longer carries a
per-milestone matrix. No row may expand without corresponding tests
(`plans/003:57`).

| Capability | M013 |
|---|---|
| Explicit HTTP/1.1 proxy absolute-form recording | supported |
| CONNECT deny | supported |
| CONNECT opaque passthrough | supported |
| HTTPS MITM HTTP/1.1 recording | supported, opt-in/policy-gated |
| Direct + narrow Eggress-routed upstream | supported |
| Manual CA initialize/import/export/rotate | supported |
| Automatic OS/browser trust installation | unsupported |
| HTTP/2 MITM | unsupported/not qualified |
| HTTP/3/QUIC interception | unsupported (deferred by ADR 0009) |
| WSS WebSocket interception | unsupported |
| client mTLS interception | unsupported |
| certificate-pinned clients | expected to fail unless configured passthrough |
| transparent/TUN interception | unsupported |

Rows that are *not* merely "unimplemented" but actively refused: WSS inside
interception is 400 (`mitm.rs:585-593`); non-origin-form inside a tunnel is 400
plus connection poisoning (`mitm.rs:553-560`); a non-`http` absolute target is
400 (`proxy.rs:493-498`); and HTTP/2 is refused structurally because `h2` is
never in the advertised ALPN list (`mitm.rs:358`, `leaf.rs:67-71`), backed by
`ci.yml:187` which forbids the intercept graph from acquiring `eggserve-core`.
Certificate pinning fails by construction: a pinned client rejects the minted
leaf, and the operator's recourse is an explicit `tunnel` policy
(`docs/interception-threat-model.md:93-94`).

## Review checklist

- **Bind safety.** Is every non-loopback bind still gated on an explicit
  opt-in? `validate_bind` (`proxy.rs:241`), `ProxyListenerConfig::loopback`
  (`proxy.rs:264`), `with_remote_opt_in` (`proxy.rs:275`), the re-validation in
  `start_explicit_proxy` (`proxy.rs:1146`), and the CLI warning
  (`crates/eggreplay-cli/src/intercept.rs:566-573`). A new listener
  constructor that skips `validate` is a defect.
- **Policy default-action.** Unmatched must still deny (`policy.rs:647`);
  `Intercept` must fire only for explicitly allowed targets
  (`policy.rs:656-670`); file/flag policies must stay coherent
  (`policy_file.rs:242-255`, `policy_file.rs:612`). There is no allow-all
  default anywhere.
- **Permission handling.** `CA_DIR_MODE`/`CA_KEY_MODE` enforced on open
  (`ca.rs:904`, `ca.rs:914-928`); repair is explicit and Unix-only
  (`ca.rs:564-569`); no Unix-equivalence claim on Windows (`ca.rs:51-56`).
- **Key material never reaches output streams.** `CaAuthority`/`LeafIssuer`/
  `LeafCertificate`/`MitmConfig`/`ProxyStats` `Debug` are redacted; `CaError`
  and `LeafError` carry no key bytes or paths; `export_ca_cert` copies the
  public cert only (`ca.rs:546`); leaf keys are memory-only (`leaf.rs:196-203`).
  A new public accessor that returns PEM or key bytes is a defect.
- **ALPN constraint.** Only `INTERCEPT_ALPN_HTTP1_1` is ever advertised
  (`mitm.rs:358`), and the negotiated value is re-checked
  (`mitm.rs:705-716`). Any change to `leaf.rs:71` or `mitm.rs:358` is an
  M014B-scoped decision, not a local edit.
- **Authority/SNI coherence.** `check_sni_coherence` and
  `check_http_authority_coherence` must stay fail-closed, and `Forwarded` /
  `X-Forwarded-*` must never be consulted (`mitm.rs:291`). The `421` + poison
  behavior (`mitm.rs:566-572`) must not be softened to a per-request rejection.
- **No flow fabrication on TLS failure.** Every post-`200` failure path in
  `accept_decrypted_tls` / `serve_decrypted` must end in a bounded event and
  `None` (`mitm.rs:683-737`, `mitm.rs:818-836`). A new return path that records
  a flow after a failed handshake is a critical defect.
- **Resource bounds under concurrency.** One shared tunnel semaphore
  (`proxy.rs:629`, `proxy.rs:813`, `proxy.rs:847`); tunnel byte/duration/idle
  bounds (`tunnel.rs:21-27`); handshake bounded by `connect_timeout`
  (`mitm.rs:113`); leaf cache bounded at 128 with FIFO
  (`leaf.rs:54`, `leaf.rs:336-343`); event log bounded at 256
  (`tunnel.rs:250`); policy, ports, host length, PEM/metadata, and policy-file
  sizes bounded. `tests/resource_bounds.rs` pins the correspondence, and the
  `interception` CI lane runs it on three platforms
  (`.github/workflows/ci.yml:400`).

Residual risks worth re-reading before any change: trust installation is manual
and irreversible at runtime (`docs/interception-ca-trust.md:69-78`); Unix mode
bits are the whole Windows story (`docs/interception-threat-model.md:97-101`);
leaf serials are unique per process, not across processes
(`docs/interception-threat-model.md:102-103`); and JSON-path body redaction
requires a declared JSON media type or the recording fails with 502 rather than
persisting unredacted bytes (`docs/interception-threat-model.md:80-85`).
