//! M013E operator surface: the `proxy` and `ca` namespaces.
//!
//! Ordinary gateway recording (`record`/`serve`) stays interception-free.
//! These commands exist in every build so scripts see stable command names,
//! but the implementations are capability-gated: without the `intercept`
//! Cargo feature every handler fails with a clear capability message
//! ([`NOT_COMPILED_MESSAGE`], exit class `configuration`) instead of silently
//! using another mode. The interception feature remains opt-in; whether
//! release binaries enable it is decided at M013F.
//!
//! Security notes (also see `docs/interception-ca-trust.md`):
//!
//! - Installing the interception CA in clients allows `EggReplay` to impersonate
//!   hosts within the configured interception policy.
//! - This module never installs trust anywhere and never emits CA/leaf private
//!   key contents, key paths, proxy credentials, or decrypted payloads.

use clap::{Args, Subcommand, ValueEnum};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Whether this binary was compiled with interception support.
pub const INTERCEPTION_COMPILED: bool = cfg!(feature = "intercept");
/// Capability error for interception commands in builds without the feature.
///
/// Only referenced by the capability stubs (and tests); allowed dead in
/// interception-capable builds.
#[cfg_attr(feature = "intercept", allow(dead_code))]
pub const NOT_COMPILED_MESSAGE: &str =
    "interception support not compiled; rebuild with --features intercept";

/// `eggreplay proxy ...`: explicit-proxy recording and policy validation.
///
/// The proxy binds loopback by default; non-loopback binds require the
/// explicit `--allow-non-loopback` opt-in plus an operator-managed ingress
/// allow policy (M013 provides no proxy authentication).
#[derive(Debug, Args)]
#[cfg_attr(
    feature = "intercept",
    command(about = "Explicit proxy recording (interception-capable build)")
)]
#[cfg_attr(
    not(feature = "intercept"),
    command(about = "Explicit proxy recording (not compiled; rebuild with --features intercept)")
)]
pub struct ProxyArgs {
    /// Proxy subcommand.
    #[command(subcommand)]
    pub command: ProxyCommand,
}

/// Proxy subcommands.
///
/// The `Record` variant is large because clap owns its args by value for one
/// process invocation; boxing would only add friction with the derive macro.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum ProxyCommand {
    /// Record through an explicit proxy listener into a fixture.
    Record(ProxyRecordArgs),
    /// Validate a policy file and print its normalized form (no recording).
    Validate(ProxyValidateArgs),
}

// `name`/`output` feed the capability stubs and the machine-output contract;
// allowed dead in interception-capable non-test builds.
#[cfg_attr(all(feature = "intercept", not(test)), allow(dead_code))]
impl ProxyCommand {
    /// Envelope command name for machine-readable output.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Record(_) => "proxy-record",
            Self::Validate(_) => "proxy-validate",
        }
    }

    /// Output selection for this invocation.
    #[must_use]
    pub fn output(&self) -> super::OutputChoice {
        match self {
            Self::Record(args) => args.output.output,
            Self::Validate(args) => args.output.output,
        }
    }
}

/// `--default-action` values for `proxy record`.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum DefaultAction {
    /// Refuse `CONNECT` targets that are not explicitly allowed.
    #[default]
    Deny,
    /// Relay allowed `CONNECT` targets as opaque tunnels.
    Tunnel,
    /// Terminate TLS for allowed `CONNECT` targets and record (needs `--ca-dir`).
    Intercept,
}

/// `eggreplay proxy record ...`.
#[derive(Debug, Clone, Args)]
pub struct ProxyRecordArgs {
    /// Listener address (loopback by default).
    #[arg(long, default_value = "127.0.0.1:0")]
    pub listen: SocketAddr,
    /// Destination fixture directory.
    #[arg(long)]
    pub fixture: PathBuf,
    /// Replace an existing fixture directory.
    #[arg(long, default_value_t = false)]
    pub overwrite: bool,
    /// Outbound route: `direct` or a pproxy URI. Never falls back to direct.
    #[arg(long, default_value = "direct")]
    pub route: String,
    /// Versioned JSON policy file (`eggreplay-intercept-policy/v1`).
    /// Conflicts with `--allow-host`/`--deny-host`.
    #[arg(long, conflicts_with = "allow_hosts")]
    pub policy_file: Option<PathBuf>,
    /// Default `CONNECT` action when no file is given.
    #[arg(long, value_enum, default_value_t = DefaultAction::Deny)]
    pub default_action: DefaultAction,
    /// Allowed host pattern (exact, `*.suffix`, or IP). Repeatable.
    /// Conflicts with `--policy-file`.
    #[arg(long = "allow-host", conflicts_with = "policy_file")]
    pub allow_hosts: Vec<String>,
    /// Denied host pattern (exact, `*.suffix`, or IP). Repeatable.
    /// Conflicts with `--policy-file`.
    #[arg(long = "deny-host", conflicts_with = "policy_file")]
    pub deny_hosts: Vec<String>,
    /// CA directory for `intercept` rules (explicitly selected identity).
    #[arg(long)]
    pub ca_dir: Option<PathBuf>,
    /// Explicit opt-in for non-loopback listeners (open-proxy warning applies).
    #[arg(long, default_value_t = false)]
    pub allow_non_loopback: bool,
    /// Additional sensitive header names (added to secure defaults unless replaced).
    #[arg(long = "redact-header")]
    pub redact_headers: Vec<String>,
    /// Additional sensitive query keys (also applies to form bodies).
    #[arg(long = "redact-query")]
    pub redact_queries: Vec<String>,
    /// Sensitive JSON Pointer body paths (e.g. `/secret`).
    #[arg(long = "redact-json-path")]
    pub redact_json_paths: Vec<String>,
    /// Persisted redaction policy identifier.
    #[arg(long = "redaction-profile", default_value = "explicit-proxy-v1")]
    pub redaction_profile: String,
    /// Clearly named unsafe override: replace secure defaults instead of extending them.
    #[arg(long = "unsafe-replace-default-redaction", default_value_t = false)]
    pub unsafe_replace: bool,
    /// Concurrent tunnel ceiling.
    #[arg(long, default_value_t = 16)]
    pub max_tunnels: usize,
    /// Concurrent connection ceiling (listener admission).
    #[arg(long, default_value_t = 64)]
    pub max_connections: usize,
    /// Hard request-body ceiling in bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    pub max_body_bytes: u64,
    /// Leaf certificate validity in hours (1..=720; cache holds 128 entries).
    #[arg(long, default_value_t = 168)]
    pub leaf_validity_hours: u32,
    /// Total tunnel byte ceiling (both directions).
    #[arg(long, default_value_t = 256 * 1024 * 1024)]
    pub tunnel_max_bytes: u64,
    /// Tunnel lifetime ceiling in seconds.
    #[arg(long, default_value_t = 300)]
    pub tunnel_max_duration_secs: u64,
    /// Tunnel no-progress ceiling in seconds.
    #[arg(long, default_value_t = 60)]
    pub tunnel_idle_timeout_secs: u64,
    /// Outbound route establishment ceiling in seconds.
    #[arg(long, default_value_t = 10)]
    pub tunnel_connect_timeout_secs: u64,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay proxy validate --policy-file ...`.
#[derive(Debug, Clone, Args)]
pub struct ProxyValidateArgs {
    /// Versioned JSON policy file to validate.
    #[arg(long)]
    pub policy_file: PathBuf,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay ca ...`: dedicated interception-CA lifecycle.
///
/// The CA is an operator-owned identity in a caller-selected directory (never
/// a fixture). No command installs trust anywhere; installing the public CA
/// certificate in clients is a manual operator action documented in
/// `docs/interception-ca-trust.md`.
#[derive(Debug, Args)]
#[cfg_attr(
    feature = "intercept",
    command(about = "Interception CA lifecycle (interception-capable build)")
)]
#[cfg_attr(
    not(feature = "intercept"),
    command(about = "Interception CA lifecycle (not compiled; rebuild with --features intercept)")
)]
pub struct CaArgs {
    /// CA subcommand.
    #[command(subcommand)]
    pub command: CaCommand,
}

/// CA subcommands.
#[derive(Debug, Subcommand)]
pub enum CaCommand {
    /// Create a new CA (refuses to overwrite an existing directory).
    Init(CaInitArgs),
    /// Import an existing self-signed root CA (refuses to overwrite).
    Import(CaImportArgs),
    /// Show public metadata and fingerprint (never key material).
    Inspect(CaInspectArgs),
    /// Copy the public certificate only (refuses to overwrite).
    Export(CaExportArgs),
    /// Rotate by creating a distinct identity in a new directory.
    Rotate(CaRotateArgs),
}

// `name`/`output` feed the capability stubs and the machine-output contract;
// allowed dead in interception-capable non-test builds.
#[cfg_attr(all(feature = "intercept", not(test)), allow(dead_code))]
impl CaCommand {
    /// Envelope command name for machine-readable output.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Init(_) => "ca-init",
            Self::Import(_) => "ca-import",
            Self::Inspect(_) => "ca-inspect",
            Self::Export(_) => "ca-export",
            Self::Rotate(_) => "ca-rotate",
        }
    }

    /// Output selection for this invocation.
    #[must_use]
    pub fn output(&self) -> super::OutputChoice {
        match self {
            Self::Init(args) => args.output.output,
            Self::Import(args) => args.output.output,
            Self::Inspect(args) => args.output.output,
            Self::Export(args) => args.output.output,
            Self::Rotate(args) => args.output.output,
        }
    }
}

/// `eggreplay ca init --dir ...`.
#[derive(Debug, Args)]
pub struct CaInitArgs {
    /// New CA directory (must not exist).
    #[arg(long)]
    pub dir: PathBuf,
    /// Subject/issuer common name.
    #[arg(long, default_value = "EggReplay Interception CA")]
    pub common_name: String,
    /// Validity in days (31..=1825).
    #[arg(long, default_value_t = 365)]
    pub validity_days: u32,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay ca import --dir ... --cert ... --key ...`.
#[derive(Debug, Args)]
pub struct CaImportArgs {
    /// Destination CA directory (must not exist).
    #[arg(long)]
    pub dir: PathBuf,
    /// Source self-signed root certificate (PEM, read-only).
    #[arg(long)]
    pub cert: PathBuf,
    /// Source PKCS#8 private key (PEM, read-only).
    #[arg(long)]
    pub key: PathBuf,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay ca inspect --dir ...`.
#[derive(Debug, Args)]
pub struct CaInspectArgs {
    /// CA directory to inspect.
    #[arg(long)]
    pub dir: PathBuf,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay ca export --dir ... --out ...`.
#[derive(Debug, Args)]
pub struct CaExportArgs {
    /// CA directory holding the public certificate.
    #[arg(long)]
    pub dir: PathBuf,
    /// Destination for the public certificate (must not exist).
    #[arg(long)]
    pub out: PathBuf,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// `eggreplay ca rotate --new-dir ...`.
#[derive(Debug, Args)]
pub struct CaRotateArgs {
    /// New CA directory for the rotated identity (must not exist).
    #[arg(long)]
    pub new_dir: PathBuf,
    /// Subject/issuer common name for the new identity.
    #[arg(long, default_value = "EggReplay Interception CA")]
    pub common_name: String,
    /// Validity in days (31..=1825).
    #[arg(long, default_value_t = 365)]
    pub validity_days: u32,
    /// Output rendering.
    #[command(flatten)]
    pub output: super::OutputArgs,
}

/// Validate a proxy listener bind: loopback always passes, anything else
/// requires the explicit remote opt-in.
///
/// Returns the failure-class/message pair on denial.
#[cfg(any(test, feature = "intercept"))]
pub fn check_listener_bind(
    bind: SocketAddr,
    allow_non_loopback: bool,
) -> Result<(), (String, String)> {
    if bind.ip().is_loopback() || allow_non_loopback {
        Ok(())
    } else {
        Err((
            "configuration".to_owned(),
            format!(
                "refusing non-loopback proxy bind on {bind}: remote listeners require \
                 --allow-non-loopback and an operator-managed ingress allow policy; \
                 remote exposure without proxy authentication creates an open forward proxy"
            ),
        ))
    }
}

/// Dispatch `eggreplay proxy ...`.
///
/// `async` is required by the interception-capable build (`proxy record`
/// awaits the listener); allowed unused otherwise.
#[cfg_attr(not(feature = "intercept"), allow(clippy::unused_async))]
pub async fn run_proxy(args: ProxyArgs) -> Result<(), (String, String)> {
    #[cfg(feature = "intercept")]
    {
        match &args.command {
            ProxyCommand::Record(record) => proxy_record(record.clone()).await,
            ProxyCommand::Validate(validate) => proxy_validate(validate),
        }
    }
    #[cfg(not(feature = "intercept"))]
    {
        let output = args.command.output();
        super::emit(
            args.command.name(),
            output,
            false,
            Some("configuration"),
            serde_json::json!({"interception_compiled": INTERCEPTION_COMPILED}),
        );
        Err(("configuration".into(), NOT_COMPILED_MESSAGE.into()))
    }
}

/// Dispatch `eggreplay ca ...`.
pub fn run_ca(args: &CaArgs) -> Result<(), (String, String)> {
    #[cfg(feature = "intercept")]
    {
        match &args.command {
            CaCommand::Init(init) => ca_init(init),
            CaCommand::Import(import) => ca_import(import),
            CaCommand::Inspect(inspect) => ca_inspect(inspect),
            CaCommand::Export(export) => ca_export(export),
            CaCommand::Rotate(rotate) => ca_rotate(rotate),
        }
    }
    #[cfg(not(feature = "intercept"))]
    {
        let output = args.command.output();
        super::emit(
            args.command.name(),
            output,
            false,
            Some("configuration"),
            serde_json::json!({"interception_compiled": INTERCEPTION_COMPILED}),
        );
        Err(("configuration".into(), NOT_COMPILED_MESSAGE.into()))
    }
}

#[cfg(feature = "intercept")]
fn connect_action_of(action: DefaultAction) -> eggreplay_intercept::ConnectAction {
    match action {
        DefaultAction::Deny => eggreplay_intercept::ConnectAction::Deny,
        DefaultAction::Tunnel => eggreplay_intercept::ConnectAction::Tunnel,
        DefaultAction::Intercept => eggreplay_intercept::ConnectAction::Intercept,
    }
}

#[cfg(feature = "intercept")]
fn connect_action_str(action: eggreplay_intercept::ConnectAction) -> &'static str {
    match action {
        eggreplay_intercept::ConnectAction::Deny => "deny",
        eggreplay_intercept::ConnectAction::Tunnel => "tunnel",
        eggreplay_intercept::ConnectAction::Intercept => "intercept",
    }
}

#[cfg(feature = "intercept")]
fn map_ca_error(error: &eggreplay_intercept::CaError) -> (String, String) {
    use eggreplay_intercept::CaError as E;
    let message = error.to_string();
    let class = match error {
        E::Io(_) | E::CertGeneration | E::Signing => "runtime",
        _ => "configuration",
    };
    (class.to_owned(), ca_error_remediation(error, &message))
}

#[cfg(feature = "intercept")]
fn ca_error_remediation(error: &eggreplay_intercept::CaError, message: &str) -> String {
    use eggreplay_intercept::CaError as E;
    let hint = match error {
        E::AlreadyExists => " choose a new directory (rotation creates a distinct identity)",
        E::Incomplete => " select an initialized CA directory (see `ca init`)",
        E::InsecurePermissions { .. } => {
            " restrict directory/key permissions (0700/0600 on Unix) and retry"
        }
        E::Expired(_) | E::NotYetValid(_) => {
            " rotate to a new identity (`ca rotate`) and select it explicitly"
        }
        E::NotSelfSigned | E::NotCa | E::MissingKeyCertSign | E::UnsupportedAlgorithm => {
            " import a self-signed ECDSA P-256 root CA with keyCertSign usage"
        }
        E::Mismatch | E::CertUnparseable | E::KeyUnparseable | E::UnexpectedChain => {
            " check that the certificate and PKCS#8 key correspond"
        }
        E::ExportDestExists => " choose a new export destination",
        _ => "",
    };
    format!("{message}.{hint}")
}

#[cfg(feature = "intercept")]
fn load_record_policy(
    args: &ProxyRecordArgs,
) -> Result<
    (
        eggreplay_intercept::FilePolicy,
        eggreplay_intercept::TargetPolicy,
    ),
    (String, String),
> {
    if let Some(path) = &args.policy_file {
        let file = eggreplay_intercept::FilePolicy::load_file(path).map_err(|error| {
            (
                "configuration".to_owned(),
                format!("invalid policy file: {error}"),
            )
        })?;
        let target = file.to_target_policy().map_err(|error| {
            (
                "configuration".to_owned(),
                format!("invalid policy rules: {error}"),
            )
        })?;
        Ok((file, target))
    } else {
        let default = connect_action_of(args.default_action);
        let file =
            eggreplay_intercept::policy_from_flags(default, &args.allow_hosts, &args.deny_hosts)
                .map_err(|error| {
                    (
                        "configuration".to_owned(),
                        format!("invalid policy flags: {error}"),
                    )
                })?;
        let target = file.to_target_policy().map_err(|error| {
            (
                "configuration".to_owned(),
                format!("invalid policy flags: {error}"),
            )
        })?;
        Ok((file, target))
    }
}

#[cfg(feature = "intercept")]
#[allow(clippy::too_many_lines)]
async fn proxy_record(args: ProxyRecordArgs) -> Result<(), (String, String)> {
    use eggreplay_intercept::{
        CaAuthority, ConnectAction, ExplicitProxyConfig, LeafIssuer, LeafOptions, MitmConfig,
        ProxyRoute, TunnelLimits, start_explicit_proxy,
    };

    check_listener_bind(args.listen, args.allow_non_loopback)?;
    if args.fixture.exists() && !args.overwrite {
        super::emit(
            "proxy-record",
            args.output.output,
            false,
            Some("configuration"),
            serde_json::json!({"fixture": args.fixture}),
        );
        return Err((
            "configuration".into(),
            "fixture exists; pass --overwrite to replace it".to_owned(),
        ));
    }
    if args.fixture.exists() {
        std::fs::remove_dir_all(&args.fixture)
            .map_err(|error| ("runtime".into(), error.to_string()))?;
    }
    let (file_policy, target_policy) = load_record_policy(&args)?;
    let default_action = file_policy.default_connect_action();
    let (redaction, profile_id) = super::redaction_policy(
        &args.redact_headers,
        &args.redact_queries,
        &args.redact_json_paths,
        &args.redaction_profile,
        args.unsafe_replace,
    );
    let route = if args.route == "direct" {
        ProxyRoute::direct()
    } else {
        ProxyRoute::from_pproxy_uri(&args.route).map_err(|error| {
            (
                "configuration".to_owned(),
                format!("invalid --route: {error}"),
            )
        })?
    };
    let redacted_route = eggreplay_http::redact_route_credentials(&args.route);
    let tunnel_limits = TunnelLimits::new(
        args.tunnel_max_bytes,
        std::time::Duration::from_secs(args.tunnel_max_duration_secs),
        std::time::Duration::from_secs(args.tunnel_idle_timeout_secs),
        args.max_tunnels,
        std::time::Duration::from_secs(args.tunnel_connect_timeout_secs),
    )
    .map_err(|message| ("configuration".into(), message))?;
    if !(1..=4096).contains(&args.max_connections) {
        return Err((
            "configuration".into(),
            "--max-connections must be within 1..=4096".to_owned(),
        ));
    }

    let mut warnings = Vec::new();
    if !args.listen.ip().is_loopback() {
        warnings.push(
            "non-loopback listener: remote exposure without proxy authentication \
             creates an open forward proxy; the ingress allow policy is operator-managed"
                .to_owned(),
        );
    }
    // Interception issuer: only armed for the intercept default, bound to one
    // explicitly selected CA. Client trust of this CA has no effect on
    // upstream trust (normal EggFetch verification still applies).
    let mut ca_fingerprint: Option<String> = None;
    let mitm = if default_action == ConnectAction::Intercept {
        warnings.push(
            "installing this CA in clients allows EggReplay to impersonate hosts \
             within the configured interception policy; \
             see docs/interception-ca-trust.md"
                .to_owned(),
        );
        let dir = args.ca_dir.as_ref().ok_or_else(|| {
            (
                "configuration".to_owned(),
                "intercept default requires --ca-dir selecting an initialized CA \
                 (see `eggreplay ca init`)"
                    .to_owned(),
            )
        })?;
        let authority = CaAuthority::open(dir).map_err(|error| map_ca_error(&error))?;
        ca_fingerprint = Some(authority.fingerprint().to_owned());
        if !(eggreplay_intercept::MIN_LEAF_VALIDITY_HOURS
            ..=eggreplay_intercept::MAX_LEAF_VALIDITY_HOURS)
            .contains(&args.leaf_validity_hours)
        {
            return Err((
                "configuration".into(),
                format!(
                    "leaf validity must be {}..={} hours",
                    eggreplay_intercept::MIN_LEAF_VALIDITY_HOURS,
                    eggreplay_intercept::MAX_LEAF_VALIDITY_HOURS
                ),
            ));
        }
        let issuer = LeafIssuer::new(
            authority,
            &LeafOptions::default().with_validity_hours(args.leaf_validity_hours),
        )
        .map_err(|error| ("configuration".into(), error.to_string()))?;
        let mut config = MitmConfig::new(std::sync::Arc::new(issuer));
        config.redaction = redaction.clone();
        config.profile_id.clone_from(&profile_id);
        config.max_request_body_bytes = args.max_body_bytes;
        Some(config)
    } else {
        if args.ca_dir.is_some() {
            warnings.push("--ca-dir is ignored without an intercept default action".to_owned());
        }
        None
    };

    let session = eggreplay_store::RecordingSession::create(
        &args.fixture,
        eggreplay_core::SessionMetadata {
            capture_mode: "explicit-proxy".into(),
            target: Some(redacted_route.clone()),
            redaction_profile: profile_id.clone(),
            ..eggreplay_core::SessionMetadata::default()
        },
        eggreplay_store::StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;

    let mut proxy_config = ExplicitProxyConfig::new(session.clone(), target_policy, route);
    proxy_config.redaction = redaction;
    proxy_config.profile_id = profile_id;
    proxy_config.tunnel_limits = tunnel_limits;
    proxy_config.max_request_body_bytes = args.max_body_bytes;
    proxy_config.max_connections = args.max_connections;
    proxy_config.mitm = mitm;

    let listener = if args.allow_non_loopback {
        eggreplay_intercept::ProxyListenerConfig::with_remote_opt_in(args.listen)
    } else {
        eggreplay_intercept::ProxyListenerConfig::loopback(args.listen)
            .map_err(|error| ("configuration".into(), error.to_string()))?
    };
    let handle = start_explicit_proxy(listener, proxy_config)
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    let bind = handle.local_addr();
    let stats = handle.stats();
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    eprintln!("explicit proxy recording on {bind}");
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    handle.shutdown();
    handle.wait().await;
    session.shutdown();
    for _ in 0..100 {
        if session.active_blobs() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let recorded = session
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let snapshot = stats.snapshot();
    let payload = serde_json::json!({
        "bind": bind.to_string(),
        "interception_compiled": INTERCEPTION_COMPILED,
        "ca_fingerprint": ca_fingerprint,
        "policy_version": eggreplay_intercept::INTERCEPT_POLICY_VERSION,
        "policy_rule_count": file_policy.rule_count(),
        "policy_action_counts": file_policy.action_counts(),
        "default_connect_action": connect_action_str(default_action),
        "accepted": snapshot.accepted,
        "rejected": snapshot.rejected,
        "tunneled": snapshot.tunneled,
        "intercepted": snapshot.intercepted,
        "recorded_flows": recorded.manifest().flow_count,
        "failures": snapshot.failures,
        "route": redacted_route,
        "fixture": args.fixture,
        "status": "finalized",
    });
    super::emit_with_warnings(
        "proxy-record",
        args.output.output,
        true,
        None,
        payload,
        warnings,
    );
    Ok(())
}

#[cfg(feature = "intercept")]
fn proxy_validate(args: &ProxyValidateArgs) -> Result<(), (String, String)> {
    let file = eggreplay_intercept::FilePolicy::load_file(&args.policy_file).map_err(|error| {
        (
            "configuration".to_owned(),
            format!("invalid policy file: {error}"),
        )
    })?;
    let target = file.to_target_policy().map_err(|error| {
        (
            "configuration".to_owned(),
            format!("invalid policy rules: {error}"),
        )
    })?;
    let mut summary = file.normalized_summary();
    if let serde_json::Value::Object(map) = &mut summary {
        map.insert(
            "target_policy_rules".into(),
            serde_json::json!(target.rule_count()),
        );
        map.insert(
            "interception_compiled".into(),
            serde_json::json!(INTERCEPTION_COMPILED),
        );
    }
    super::emit("proxy-validate", args.output.output, true, None, summary);
    Ok(())
}

#[cfg(feature = "intercept")]
fn ca_payload(metadata: &eggreplay_intercept::CaMetadata) -> serde_json::Value {
    // Public facts only: fingerprint, subject/issuer display, validity,
    // origin, format. Never key contents or key paths.
    serde_json::json!({
        "interception_compiled": INTERCEPTION_COMPILED,
        "fingerprint_sha256": metadata.fingerprint_sha256,
        "subject": metadata.subject_display,
        "issuer": metadata.issuer_display,
        "not_before": metadata.not_before_rfc3339,
        "not_after": metadata.not_after_rfc3339,
        "origin": metadata.origin,
        "format_version": metadata.format_version,
        "key_algorithm": metadata.key_algorithm,
    })
}

#[cfg(feature = "intercept")]
fn ca_init(args: &CaInitArgs) -> Result<(), (String, String)> {
    use eggreplay_intercept::{CaAuthority, CaOptions};
    let authority = CaAuthority::create_new(
        &args.dir,
        &CaOptions::default()
            .with_common_name(&args.common_name)
            .with_validity_days(args.validity_days),
    )
    .map_err(|error| map_ca_error(&error))?;
    let payload = ca_payload(authority.metadata());
    super::emit("ca-init", args.output.output, true, None, payload);
    Ok(())
}

#[cfg(feature = "intercept")]
fn ca_import(args: &CaImportArgs) -> Result<(), (String, String)> {
    use eggreplay_intercept::CaAuthority;
    let authority = CaAuthority::import(&args.dir, &args.cert, &args.key)
        .map_err(|error| map_ca_error(&error))?;
    let payload = ca_payload(authority.metadata());
    super::emit("ca-import", args.output.output, true, None, payload);
    Ok(())
}

#[cfg(feature = "intercept")]
fn ca_inspect(args: &CaInspectArgs) -> Result<(), (String, String)> {
    let metadata =
        eggreplay_intercept::inspect_ca(&args.dir).map_err(|error| map_ca_error(&error))?;
    super::emit(
        "ca-inspect",
        args.output.output,
        true,
        None,
        ca_payload(&metadata),
    );
    Ok(())
}

#[cfg(feature = "intercept")]
fn ca_export(args: &CaExportArgs) -> Result<(), (String, String)> {
    let metadata =
        eggreplay_intercept::inspect_ca(&args.dir).map_err(|error| map_ca_error(&error))?;
    eggreplay_intercept::export_ca_cert(&args.dir, &args.out)
        .map_err(|error| map_ca_error(&error))?;
    let mut payload = ca_payload(&metadata);
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert("exported".into(), serde_json::json!(true));
    }
    super::emit("ca-export", args.output.output, true, None, payload);
    Ok(())
}

#[cfg(feature = "intercept")]
fn ca_rotate(args: &CaRotateArgs) -> Result<(), (String, String)> {
    use eggreplay_intercept::{CaAuthority, CaOptions};
    // Rotation is creation of a distinct identity in a new directory; the old
    // directory is never mutated. Selection stays explicit via `--ca-dir`.
    let authority = CaAuthority::create_new(
        &args.new_dir,
        &CaOptions::default()
            .with_common_name(&args.common_name)
            .with_validity_days(args.validity_days),
    )
    .map_err(|error| map_ca_error(&error))?;
    let mut payload = ca_payload(authority.metadata());
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "selection".into(),
            serde_json::json!("explicit --ca-dir (previous identity untouched)"),
        );
    }
    super::emit("ca-rotate", args.output.output, true, None, payload);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_const_matches_feature_flag() {
        assert_eq!(INTERCEPTION_COMPILED, cfg!(feature = "intercept"));
    }

    #[cfg(not(feature = "intercept"))]
    #[test]
    fn feature_off_commands_fail_with_capability_message() {
        // `cfg` gates this test to feature-off builds; the assertion below
        // pins the user-facing contract, not the constant.
        assert!(NOT_COMPILED_MESSAGE.contains("--features intercept"));
        // Exit-class contract: capability failures are configuration errors.
        assert_eq!(super::super::exit_code_for_class("configuration"), 2);
    }

    #[cfg(feature = "intercept")]
    #[test]
    fn proxy_record_defaults_to_loopback() {
        use clap::Parser as _;
        let cli = super::super::Cli::try_parse_from([
            "eggreplay",
            "proxy",
            "record",
            "--fixture",
            "fixture.eggr",
        ])
        .expect("proxy record must parse");
        let super::super::Command::Proxy(proxy) = cli.command else {
            panic!("expected proxy command");
        };
        let ProxyCommand::Record(record) = proxy.command else {
            panic!("expected proxy record");
        };
        assert!(record.listen.ip().is_loopback());
        assert!(!record.allow_non_loopback);
    }

    #[cfg(feature = "intercept")]
    #[test]
    fn command_names_and_outputs_are_stable() {
        use clap::Parser as _;
        let cli = super::super::Cli::try_parse_from([
            "eggreplay",
            "proxy",
            "validate",
            "--policy-file",
            "policy.json",
        ])
        .expect("proxy validate must parse");
        let super::super::Command::Proxy(proxy) = cli.command else {
            panic!("expected proxy command");
        };
        assert_eq!(proxy.command.name(), "proxy-validate");
        assert!(matches!(
            proxy.command.output(),
            super::super::OutputChoice::Human
        ));
        let cli = super::super::Cli::try_parse_from(["eggreplay", "ca", "inspect", "--dir", "ca"])
            .expect("ca inspect must parse");
        let super::super::Command::Ca(ca) = cli.command else {
            panic!("expected ca command");
        };
        assert_eq!(ca.command.name(), "ca-inspect");
        assert!(matches!(
            ca.command.output(),
            super::super::OutputChoice::Human
        ));
    }

    #[cfg(feature = "intercept")]
    #[test]
    fn proxy_validate_accepts_a_minimal_policy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("policy.json");
        std::fs::write(
            &path,
            br#"{"version": "eggreplay-intercept-policy/v1", "rules": []}"#,
        )
        .unwrap();
        let file = eggreplay_intercept::FilePolicy::load_file(&path).unwrap();
        assert_eq!(file.rule_count(), 0);
        let summary = file.normalized_summary().to_string();
        assert!(summary.contains("eggreplay-intercept-policy/v1"));
        assert!(!summary.contains("PRIVATE KEY"));
    }

    #[cfg(feature = "intercept")]
    #[test]
    fn proxy_validate_rejects_unknown_versions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("policy.json");
        std::fs::write(
            &path,
            br#"{"version": "eggreplay-intercept-policy/v99", "rules": []}"#,
        )
        .unwrap();
        let error = eggreplay_intercept::FilePolicy::load_file(&path).unwrap_err();
        assert!(matches!(
            error,
            eggreplay_intercept::PolicyFileError::UnsupportedVersion(_)
        ));
    }

    #[test]
    fn non_loopback_bind_requires_explicit_opt_in() {
        let remote: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        assert!(check_listener_bind(remote, false).is_err());
        assert!(check_listener_bind(remote, true).is_ok());
        let loopback: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(check_listener_bind(loopback, false).is_ok());
    }

    #[test]
    fn interception_exit_codes_follow_the_contract() {
        // configuration=2, fixture=3, runtime=4, internal=5.
        assert_eq!(super::super::exit_code_for_class("configuration"), 2);
        assert_eq!(super::super::exit_code_for_class("fixture"), 3);
        assert_eq!(super::super::exit_code_for_class("runtime"), 4);
        assert_eq!(super::super::exit_code_for_class("unexpected"), 5);
    }

    fn subcommand_names(command: &clap::Command) -> Vec<String> {
        let mut names = Vec::new();
        for sub in command.get_subcommands() {
            names.push(sub.get_name().to_owned());
            names.extend(subcommand_names(sub));
        }
        names
    }

    #[test]
    fn help_distinguishes_compiled_interception_support() {
        use clap::CommandFactory as _;
        let mut command = super::super::Cli::command();
        let names = subcommand_names(&command);
        for token in [
            "proxy", "ca", "record", "validate", "init", "inspect", "export", "rotate", "import",
        ] {
            assert!(
                names.contains(&token.to_owned()),
                "help must mention {token}"
            );
        }
        let help = command.render_long_help().to_string();
        if INTERCEPTION_COMPILED {
            assert!(help.contains("interception-capable build"));
        } else {
            assert!(help.contains("--features intercept"));
        }
    }

    #[test]
    fn docs_command_names_match_clap_help() {
        use clap::CommandFactory as _;
        let command = super::super::Cli::command();
        let names = subcommand_names(&command);
        let docs = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/interception-ca-trust.md"
        ))
        .expect("trust docs must exist");
        for token in [
            "eggreplay proxy record",
            "eggreplay proxy validate",
            "eggreplay ca init",
            "eggreplay ca import",
            "eggreplay ca inspect",
            "eggreplay ca export",
            "eggreplay ca rotate",
        ] {
            assert!(docs.contains(token), "docs must document {token}");
            let short = token.split_whitespace().last().unwrap_or(token);
            assert!(names.contains(&short.to_owned()), "help must offer {short}");
        }
    }

    #[test]
    fn python_package_stays_interception_free() {
        let manifest = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../eggreplay-python/Cargo.toml"
        ))
        .expect("python manifest must exist");
        assert!(
            !manifest.contains("eggreplay-intercept"),
            "default Python wheel must not depend on interception"
        );
        assert!(
            !manifest.contains("rcgen"),
            "default Python wheel must not depend on rcgen"
        );
    }
}
