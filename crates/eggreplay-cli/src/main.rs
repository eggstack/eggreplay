//! `EggReplay` command-line interface: semantic HTTP recording, replay, and
//! regression testing, plus the optional M013E interception operator surface
//! (`proxy`/`ca`, compiled only with the `intercept` Cargo feature).

use clap::{Args, Parser, Subcommand, ValueEnum};
use eggreplay_core::{
    ComparisonPolicy, FlowOutcome, Matcher, PhysicalRoute, ReportScheduler, SessionMetadata,
    TimingAssertion, compare_flows_with_timing_and_policy,
};
use eggreplay_http::{EggressDialer, ReplayFixture, execute_candidate};
use eggreplay_http::{H2Limits, InboundProtocol, InboundProtocolDescription};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use serde::Serialize;
use serde_json::json;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod intercept;

fn websocket_options(
    enabled: bool,
    redact_text: bool,
    redact_binary: bool,
) -> eggreplay_http::recording::WebSocketRecordingOptions {
    eggreplay_http::recording::WebSocketRecordingOptions {
        enabled,
        redact_text,
        redact_binary,
        ..Default::default()
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "eggreplay",
    version,
    about = "Semantic HTTP recording, replay, and regression testing"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Record(RecordArgs),
    Serve(ServeArgs),
    Replay(ReplayArgs),
    Test(TestArgs),
    Diff(DiffArgs),
    Inspect(InspectArgs),
    Validate(ValidateArgs),
    Har(HarArgs),
    Migrate(MigrateArgs),
    Proxy(intercept::ProxyArgs),
    Ca(intercept::CaArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct OutputArgs {
    #[arg(long, value_enum, default_value_t = OutputChoice::Human)]
    output: OutputChoice,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum OutputChoice {
    Human,
    Json,
    Junit,
}
/// Inbound serving protocol policy shared by `record` and `serve` (M015B).
///
/// Defaults to `http1`, which is the pre-M015B behaviour in full. Selecting
/// HTTP/2 is explicit on the command line *and* opt-in at build time: a
/// binary without the inbound-HTTP/2 features cannot express `--inbound
/// http2` and refuses it rather than serving HTTP/1.1 under an HTTP/2 label.
#[derive(Debug, Clone, Default, Args)]
pub(crate) struct InboundServingArgs {
    /// Inbound protocol for this listener: `http1` (default) or `http2`
    /// (cleartext prior knowledge, a.k.a. h2c).
    #[arg(long, default_value = "http1", value_name = "PROTOCOL")]
    inbound: String,
    /// Operator PEM certificate chain for inbound HTTP/2 over TLS (ALPN).
    /// Enables the TLS serving policy; requires `--inbound-tls-key`.
    #[arg(long, value_name = "PATH", requires = "inbound_tls_key")]
    inbound_tls_cert: Option<PathBuf>,
    /// Operator PEM private key for `--inbound-tls-cert`. Never written to
    /// any output stream.
    #[arg(long, value_name = "PATH", requires = "inbound_tls_cert")]
    inbound_tls_key: Option<PathBuf>,
    /// Maximum concurrent HTTP/2 request streams advertised per connection.
    /// Unset keeps `EggServe`'s own default.
    #[arg(long, value_name = "COUNT")]
    h2_max_concurrent_streams: Option<u32>,
}

impl InboundServingArgs {
    /// Resolve the operator flags into one serving policy.
    ///
    /// Fails closed: an unbuildable policy is a configuration error, never a
    /// silent fallback to a weaker protocol.
    fn resolve(&self) -> Result<InboundProtocol, String> {
        let base = eggreplay_http::inbound::parse_protocol(&self.inbound).map_err(|error| {
            let known = eggreplay_http::inbound::supported_protocol_names().join(", ");
            format!("{error}; this build supports: {known}")
        })?;
        if self.inbound_tls_cert.is_some() || self.inbound_tls_key.is_some() {
            // TLS serving needs `eggserve-core/tls`, which is the
            // `h2-inbound-tls` feature. Supplying identity material in a build
            // that cannot use it is a refusal, never a silent downgrade to a
            // cleartext or default-identity listener.
            #[cfg(feature = "h2-inbound-tls")]
            {
                if let (Some(certificate), Some(private_key)) =
                    (&self.inbound_tls_cert, &self.inbound_tls_key)
                {
                    return Ok(InboundProtocol::Http2Tls {
                        certificate: certificate.clone(),
                        private_key: private_key.clone(),
                    });
                }
            }
            #[cfg(not(feature = "h2-inbound-tls"))]
            {
                let _ = &base;
                return Err(
                    "TLS serving is not available in this build; rebuild with the \
                     opt-in inbound HTTP/2 TLS feature"
                        .to_string(),
                );
            }
        }
        Ok(base)
    }

    /// The operator-facing limit set.
    fn limits(&self) -> H2Limits {
        H2Limits {
            max_concurrent_streams: self.h2_max_concurrent_streams,
            ..H2Limits::default()
        }
    }

    /// The secret-free policy description for status output. Carries no
    /// certificate path, no key path, and no identity material.
    fn describe(&self) -> InboundProtocolDescription {
        // The policy is already validated by `resolve` before any listener
        // starts, so falling back to the H1 description here can only be
        // reached on a path that already reported a configuration error.
        self.resolve().map_or_else(
            |_| InboundProtocol::Http1.describe(),
            |policy| policy.describe(),
        )
    }
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
struct RecordArgs {
    #[command(flatten)]
    inbound: InboundServingArgs,
    #[command(flatten)]
    outbound_version: OutboundVersionArgs,
    #[command(flatten)]
    timeout: TimeoutArgs,
    #[arg(long)]
    listen: std::net::SocketAddr,
    /// Bind the recording gateway beyond loopback. The listener is
    /// unauthenticated: any client that can reach it can drive this upstream
    /// through the recorder, and TLS does not add authentication.
    #[arg(long, default_value_t = false)]
    allow_non_loopback: bool,
    #[arg(long)]
    upstream: String,
    /// Fixture directory; may be absent, empty, or a valid fixture.
    #[arg(long)]
    fixture: PathBuf,
    /// Replace an existing fixture or empty directory. Any other existing path
    /// is refused; a failed recording leaves the old fixture untouched.
    #[arg(long, default_value_t = false)]
    overwrite: bool,
    /// Additional sensitive header names (added to secure defaults unless replaced).
    #[arg(long = "redact-header")]
    redact_headers: Vec<String>,
    /// Additional sensitive query keys (also applies to form bodies).
    #[arg(long = "redact-query")]
    redact_queries: Vec<String>,
    /// Sensitive JSON Pointer body paths (e.g. `/secret`).
    #[arg(long = "redact-json-path")]
    redact_json_paths: Vec<String>,
    /// Persisted redaction policy identifier.
    #[arg(long = "redaction-profile", default_value = "default-v1")]
    redaction_profile: String,
    /// Clearly named unsafe override: replace secure defaults instead of extending them.
    #[arg(long = "unsafe-replace-default-redaction", default_value_t = false)]
    unsafe_replace: bool,
    /// Record HTTP/1.1 WebSocket upgrades through the opt-in tunnel gateway.
    #[arg(long, default_value_t = false)]
    websockets: bool,
    /// Replace all recorded WebSocket text messages with a safe marker.
    #[arg(long, default_value_t = false)]
    redact_websocket_text: bool,
    /// Replace all recorded WebSocket binary messages with a safe marker.
    #[arg(long, default_value_t = false)]
    redact_websocket_binary: bool,
    /// Outbound route: `direct` or a pproxy URI (`socks5://...`, `http://...`,
    /// two-hop `socks5://...__http://...`). Listener-free Eggress routing only.
    #[arg(long, default_value = "direct")]
    route: String,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
struct ServeArgs {
    #[command(flatten)]
    inbound: InboundServingArgs,
    #[command(flatten)]
    outbound_version: OutboundVersionArgs,
    #[command(flatten)]
    timeout: TimeoutArgs,
    /// Fixture directory. `re-record` replaces an existing fixture or empty
    /// directory and refuses any other existing path.
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: std::net::SocketAddr,
    /// Bind the replay server beyond loopback. The listener is
    /// unauthenticated: any client that can reach it can consume this
    /// fixture, and TLS does not add authentication.
    #[arg(long, default_value_t = false)]
    allow_non_loopback: bool,
    /// Record behavior; defaults to sealed offline replay. `re-record` stages
    /// a replacement and swaps it only after the new fixture validates.
    #[arg(long, value_enum, default_value_t = ServeRecordMode::Sealed)]
    record_mode: ServeRecordMode,
    /// Required for network-capable record modes.
    #[arg(long)]
    upstream: Option<String>,
    /// Outbound route for explicit upstream execution.
    #[arg(long, default_value = "direct")]
    route: String,
    /// Additional sensitive headers, queries, and JSON Pointer paths.
    #[arg(long = "redact-header")]
    redact_headers: Vec<String>,
    #[arg(long = "redact-query")]
    redact_queries: Vec<String>,
    #[arg(long = "redact-json-path")]
    redact_json_paths: Vec<String>,
    /// Persisted redaction policy identifier.
    #[arg(long = "redaction-profile", default_value = "default-v1")]
    redaction_profile: String,
    /// Replace secure redaction defaults; unsafe and explicit.
    #[arg(long = "unsafe-replace-default-redaction", default_value_t = false)]
    unsafe_replace: bool,
    /// Record HTTP/1.1 WebSocket upgrades through the opt-in tunnel gateway.
    #[arg(long, default_value_t = false)]
    websockets: bool,
    /// Replace all recorded WebSocket text messages with a safe marker.
    #[arg(long, default_value_t = false)]
    redact_websocket_text: bool,
    /// Replace all recorded WebSocket binary messages with a safe marker.
    #[arg(long, default_value_t = false)]
    redact_websocket_binary: bool,
    /// Request matching profile used by this replay server.
    #[arg(long, value_enum, default_value_t = ServeMatcherProfile::Strict)]
    matcher_profile: ServeMatcherProfile,
    /// Body delay mode: immediate, recorded, or scaled:<factor>.
    #[arg(long, default_value = "immediate")]
    timing_mode: String,
    /// Explicitly select a named authored scenario from the `rules` extension.
    #[arg(long)]
    scenario: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServeMatcherProfile {
    Strict,
    Practical,
}

impl ServeMatcherProfile {
    fn matcher(self) -> Matcher {
        match self {
            Self::Strict => Matcher::strict(8),
            Self::Practical => Matcher::practical(8),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Practical => "practical",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServeRecordMode {
    Sealed,
    Once,
    AppendNew,
    ReRecord,
}

impl From<ServeRecordMode> for eggreplay_core::RecordMode {
    fn from(value: ServeRecordMode) -> Self {
        match value {
            ServeRecordMode::Sealed => Self::Sealed,
            ServeRecordMode::Once => Self::Once,
            ServeRecordMode::AppendNew => Self::AppendNew,
            ServeRecordMode::ReRecord => Self::ReRecord,
        }
    }
}
#[derive(Debug, Args)]
struct ComparisonArgs {
    /// Enable ordered response event comparison.
    #[arg(long, default_value_t = false)]
    compare_stream_events: bool,
    /// Enable cadence comparison with tolerance in milliseconds; implies
    /// stream event comparison.
    #[arg(long)]
    cadence_tolerance_ms: Option<u64>,
    /// Enable derived SSE semantic comparison.
    #[arg(long, default_value_t = false)]
    compare_sse: bool,
    /// SSE field to ignore (`data,event,id,retry,comments`); repeatable and
    /// implies SSE comparison.
    #[arg(long = "sse-ignore")]
    sse_ignore: Vec<String>,
    /// WebSocket message cadence tolerance in milliseconds.
    #[arg(long)]
    websocket_cadence_tolerance_ms: Option<u64>,
    /// Assert candidate flow elapsed time stays at or below this many
    /// milliseconds.
    ///
    /// This is a *comparison* bound asserted against a finished candidate run.
    /// It is independent of `--timing-mode`, which controls playback pacing:
    /// replaying a fixture immediately can still produce a slow candidate.
    #[arg(long)]
    max_elapsed_ms: Option<u64>,
}

fn comparison_policy(args: &ComparisonArgs) -> Result<ComparisonPolicy, (String, String)> {
    let cadence_tolerance_ns = match args.cadence_tolerance_ms {
        Some(ms) => Some(ms.checked_mul(1_000_000).ok_or_else(|| {
            (
                "configuration".to_owned(),
                "cadence tolerance overflows".to_owned(),
            )
        })?),
        None => None,
    };
    let websocket_cadence_tolerance_ns = match args.websocket_cadence_tolerance_ms {
        Some(ms) => Some(ms.checked_mul(1_000_000).ok_or_else(|| {
            (
                "configuration".to_owned(),
                "WebSocket cadence tolerance overflows".to_owned(),
            )
        })?),
        None => None,
    };
    let policy = ComparisonPolicy {
        compare_stream_events: args.compare_stream_events,
        cadence_tolerance_ns,
        compare_sse: args.compare_sse,
        sse_ignored: args.sse_ignore.clone(),
        websocket_cadence_tolerance_ns,
        ..ComparisonPolicy::default()
    };
    policy
        .validate()
        .map_err(|message| ("configuration".to_owned(), message))?;
    Ok(policy)
}

/// The optional timing bound asserted against each candidate flow.
///
/// This is deliberately a comparison bound, not playback configuration: it is
/// evaluated after a candidate finishes and never influences how the fixture is
/// replayed. Keeping it out of [`comparison_policy`] stops it from being
/// confused with `--timing-mode`.
fn timing_assertion(args: &ComparisonArgs) -> Option<TimingAssertion> {
    args.max_elapsed_ms
        .map(|max_elapsed_ms| TimingAssertion { max_elapsed_ms })
}

/// Outbound protocol policy (M014B outbound, M015C end-to-end).
///
/// HTTP/1.1 is the default everywhere. `http2` is an explicit per-invocation
/// opt-in that makes `EggFetch` negotiate `h2` over ALPN and fail closed if the
/// peer does not select it — it is never negotiated opportunistically, and it
/// never changes how a fixture is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum OutboundVersionChoice {
    /// The `EggReplay` default. HTTP/1.1, with no `h2` offered at all.
    #[default]
    Auto,
    /// HTTP/1.1 only; do not advertise `h2`.
    Http1,
    /// HTTP/2 only; fail rather than fall back.
    Http2,
}

impl OutboundVersionChoice {
    /// The `EggFetch` policy this choice maps to, or a refusal.
    ///
    /// `auto` is deliberately HTTP/1.1 rather than `EggFetch`'s own `Auto`:
    /// `Http1Only` advertises no `h2`, so an existing invocation cannot have
    /// its protocol changed by an upstream release that negotiates more
    /// eagerly. Opting into HTTP/2 is an explicit act.
    // With the outbound `h2` feature every arm resolves, so the `Result` is
    // only fallible in a build that genuinely cannot speak HTTP/2.
    #[cfg_attr(feature = "h2", allow(clippy::unnecessary_wraps))]
    fn policy(self) -> Result<eggfetch_core::HttpVersionPolicy, (String, String)> {
        match self {
            Self::Auto | Self::Http1 => Ok(eggfetch_core::HttpVersionPolicy::Http1Only),
            Self::Http2 => {
                // `Http2Only` without the transport feature would fail deep
                // inside the client. Refuse at the surface instead, with the
                // feature named.
                #[cfg(feature = "h2")]
                {
                    Ok(eggfetch_core::HttpVersionPolicy::Http2Only)
                }
                #[cfg(not(feature = "h2"))]
                {
                    Err((
                        "configuration".into(),
                        "outbound HTTP/2 is not available in this build; rebuild with the \
                         opt-in outbound h2 feature"
                            .into(),
                    ))
                }
            }
        }
    }
}

impl OutboundVersionArgs {
    /// Build the outbound client for this invocation's route and policy.
    fn client(
        &self,
        route: &str,
        timeout: Option<eggfetch_core::Timeout>,
    ) -> Result<(eggfetch_core::Client, PhysicalRoute), (String, String)> {
        build_client_with_version(route, self.outbound_version, timeout)
    }
}

/// A wall-clock ceiling for one outbound request.
///
/// Deliberately unset by default. Introducing a default deadline would change
/// the behaviour of every existing invocation, which is the same rule
/// `--outbound-version auto` follows: a new flag must not silently alter what
/// an operator who never asked for it gets.
///
/// When set, every phase is bounded **and** `total` is populated. `total` is
/// the part that matters most and the easiest to forget: the per-phase `read`
/// budget is "time between response body chunks", so it only starts once the
/// response has begun. An origin that accepts a connection and then says
/// nothing is bounded only by `total`.
#[derive(Debug, Clone, Default, Args)]
struct TimeoutArgs {
    /// Wall-clock ceiling, in seconds, for one outbound request. Unset leaves
    /// requests unbounded, which is the historical behaviour.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    timeout_secs: Option<u64>,
}

impl TimeoutArgs {
    /// Resolve the operator's flag into a complete timeout policy.
    ///
    /// Infallible by construction: clap's `range(1..)` has already rejected a
    /// zero or negative budget at the parse boundary, so there is no error
    /// case left here to model.
    fn resolve(&self) -> Option<eggfetch_core::Timeout> {
        self.timeout_secs.map(|seconds| {
            let budget = std::time::Duration::from_secs(seconds);
            eggfetch_core::Timeout {
                pool: Some(budget),
                connect: Some(budget),
                write: Some(budget),
                read: Some(budget),
                total: Some(budget),
            }
        })
    }

    /// A secret-free description for status output. Carries no endpoint.
    fn describe(&self) -> serde_json::Value {
        match self.timeout_secs {
            Some(seconds) => json!({"total_secs": seconds, "bounded": true}),
            None => json!({"total_secs": null, "bounded": false}),
        }
    }
}

#[derive(Debug, Clone, Default, Args)]
struct OutboundVersionArgs {
    /// Outbound protocol for the upstream transaction: `auto` (HTTP/1.1, the
    /// default), `http1`, or `http2` (ALPN `h2`, fails closed if the peer does
    /// not select it).
    #[arg(long, value_enum, default_value_t = OutboundVersionChoice::Auto)]
    outbound_version: OutboundVersionChoice,
}

#[derive(Debug, Args)]
struct ReplayArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    target: String,
    /// Outbound route: `direct` or a pproxy URI. See `record --route`.
    #[arg(long, default_value = "direct")]
    route: String,
    #[command(flatten)]
    outbound_version: OutboundVersionArgs,
    #[command(flatten)]
    timeout: TimeoutArgs,
    /// Candidate request scheduler; timeline requires stream event metadata.
    #[arg(long, value_enum, default_value_t = SchedulerChoice::Sequential)]
    scheduler: SchedulerChoice,
    /// Maximum concurrent candidate requests for timeline scheduling.
    #[arg(long, default_value_t = 8)]
    max_concurrency: usize,
    #[command(flatten)]
    comparison: ComparisonArgs,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct TestArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    target: String,
    /// Outbound route: `direct` or a pproxy URI. See `record --route`.
    #[arg(long, default_value = "direct")]
    route: String,
    #[command(flatten)]
    outbound_version: OutboundVersionArgs,
    #[command(flatten)]
    timeout: TimeoutArgs,
    /// Candidate request scheduler; timeline requires stream event metadata.
    #[arg(long, value_enum, default_value_t = SchedulerChoice::Sequential)]
    scheduler: SchedulerChoice,
    /// Maximum concurrent candidate requests for timeline scheduling.
    #[arg(long, default_value_t = 8)]
    max_concurrency: usize,
    #[command(flatten)]
    comparison: ComparisonArgs,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum SchedulerChoice {
    #[default]
    Sequential,
    Timeline,
}
#[derive(Debug, Clone, Copy)]
struct SchedulerOptions {
    choice: SchedulerChoice,
    max_concurrency: usize,
}
#[derive(Debug, Args)]
struct DiffArgs {
    #[arg(long)]
    baseline: PathBuf,
    #[arg(long)]
    candidate: PathBuf,
    #[command(flatten)]
    comparison: ComparisonArgs,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
struct InspectArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long, default_value_t = false)]
    bodies: bool,
    /// Show bounded base64 for non-UTF8 bodies (explicit opt-in only).
    #[arg(long, default_value_t = false)]
    bodies_base64: bool,
    /// Include a bounded parsed view for `text/event-stream` response bodies.
    #[arg(long, default_value_t = false)]
    sse: bool,
    /// Include bounded WebSocket conversation metadata without payload bytes.
    #[arg(long, default_value_t = false)]
    websockets: bool,
    /// CLI inspection bound per body in bytes (truncates with explicit counts).
    #[arg(long, default_value_t = 65536)]
    max_body_bytes: u64,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct ValidateArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Args)]
struct HarArgs {
    #[command(subcommand)]
    action: HarAction,
}

#[derive(Debug, Subcommand)]
enum HarAction {
    /// Import a HAR 1.2 document into a new `.eggr` fixture (lossy, redacted).
    Import(HarImportArgs),
    /// Export a fixture into a HAR 1.2 document (lossy, with `_eggreplay` loss section).
    Export(HarExportArgs),
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
struct HarImportArgs {
    #[arg(long)]
    har: PathBuf,
    /// Fixture directory; may be absent, empty, or a valid fixture.
    #[arg(long)]
    fixture: PathBuf,
    /// Replace an existing fixture or empty directory. Any other existing path
    /// is refused; a malformed HAR leaves the old fixture untouched.
    #[arg(long, default_value_t = false)]
    overwrite: bool,
    #[arg(long = "loss-report")]
    loss_report: Option<PathBuf>,
    #[arg(long = "redact-header")]
    redact_headers: Vec<String>,
    #[arg(long = "redact-query")]
    redact_queries: Vec<String>,
    #[arg(long = "redact-json-path")]
    redact_json_paths: Vec<String>,
    #[arg(long = "redaction-profile", default_value = "default-v1")]
    redaction_profile: String,
    #[arg(long = "unsafe-replace-default-redaction", default_value_t = false)]
    unsafe_replace: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Args)]
struct HarExportArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    har: PathBuf,
    #[arg(long = "loss-report")]
    loss_report: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    overwrite: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
struct MigrateArgs {
    #[arg(long)]
    fixture: PathBuf,
    /// Destination fixture directory; may be absent, empty, or a valid
    /// fixture. Mutually exclusive with `--in-place`. Named `--to` (not
    /// `--output`) because `--output` selects the result envelope format
    /// (`human|json|junit`) on every command.
    #[arg(long)]
    to: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    in_place: bool,
    /// Replace an existing fixture or empty destination directory. Any other
    /// existing path is refused; a failed migration leaves it untouched.
    #[arg(long, default_value_t = false)]
    overwrite: bool,
    /// Target session schema (defaults to current). Schema 1 fixtures upgrade
    /// to current; current-to-current migration must be idempotent.
    #[arg(long)]
    target_schema: Option<u16>,
    /// Rewrite every `stream-events` trailer value to `<redacted>` inside the
    /// transactional migration. Opt-in remediation for fixtures recorded
    /// before stream-event values were suppressed.
    #[arg(long = "scrub-stream-trailers", default_value_t = false)]
    scrub_stream_trailers: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Serialize)]
struct Envelope<T: Serialize> {
    command: String,
    schema_version: u16,
    success: bool,
    failure_class: Option<String>,
    warnings: Vec<String>,
    payload: T,
}

/// Stable process exit categories (CLI compatibility contract).
fn exit_code_for_class(class: &str) -> u8 {
    match class {
        "regression" | "diff" => 1,
        "configuration" => 2,
        "fixture" => 3,
        "runtime" => 4,
        _ => 5,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err((class, message)) => {
            eprintln!("{class}: {message}");
            ExitCode::from(exit_code_for_class(&class))
        }
    }
}

async fn run(cli: Cli) -> Result<(), (String, String)> {
    match cli.command {
        Command::Record(args) => record(args).await,
        Command::Serve(args) => serve(args).await,
        Command::Replay(args) => {
            let policy = comparison_policy(&args.comparison)?;
            let timing = timing_assertion(&args.comparison);
            regression(
                args.fixture,
                args.target,
                args.route,
                args.outbound_version.outbound_version,
                &args.timeout,
                "replay",
                args.output,
                false,
                SchedulerOptions {
                    choice: args.scheduler,
                    max_concurrency: args.max_concurrency,
                },
                policy,
                timing,
            )
            .await
        }
        Command::Test(args) => {
            let policy = comparison_policy(&args.comparison)?;
            let timing = timing_assertion(&args.comparison);
            regression(
                args.fixture,
                args.target,
                args.route,
                args.outbound_version.outbound_version,
                &args.timeout,
                "test",
                args.output,
                true,
                SchedulerOptions {
                    choice: args.scheduler,
                    max_concurrency: args.max_concurrency,
                },
                policy,
                timing,
            )
            .await
        }
        Command::Diff(args) => diff(&args),
        Command::Inspect(args) => inspect(&args),
        Command::Validate(args) => validate(&args),
        Command::Har(args) => match args.action {
            HarAction::Import(import) => har_import(&import),
            HarAction::Export(export) => har_export(&export),
        },
        Command::Migrate(args) => migrate(&args),
        Command::Proxy(args) => intercept::run_proxy(args).await,
        Command::Ca(args) => intercept::run_ca(&args),
    }
}

/// Build the outbound client for an explicit route *and* protocol policy.
///
/// The two are separate decisions on purpose: a route says how to reach the
/// peer, the version policy says which protocol to speak to it once reached.
/// Routing H2 and speaking H2 are independent, and each is opt-in.
fn build_client_with_version(
    route: &str,
    version: OutboundVersionChoice,
    timeout: Option<eggfetch_core::Timeout>,
) -> Result<(eggfetch_core::Client, PhysicalRoute), (String, String)> {
    let policy = version.policy()?;
    match eggreplay_http::parse_route(route) {
        Ok(None) => {
            let mut builder = eggfetch_core::Client::builder()
                .retry_canceled_requests(false)
                .http_version_policy(policy);
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            let client = builder.build();
            Ok((
                client,
                PhysicalRoute {
                    kind: "direct".into(),
                    description: Some("direct".into()),
                },
            ))
        }
        Ok(Some(connector)) => {
            let physical = PhysicalRoute {
                kind: "eggress".into(),
                description: Some(eggreplay_http::redact_route_credentials(route)),
            };
            let dialer = EggressDialer::new(connector);
            let mut builder = eggfetch_core::Client::builder()
                .retry_canceled_requests(false)
                .http_version_policy(policy)
                .dialer(dialer);
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            let client = builder.build();
            Ok((client, physical))
        }
        Err(message) => Err(("configuration".into(), message)),
    }
}

/// Refuse to bind an unauthenticated listener beyond loopback.
///
/// This is the single exposure gate for every listener the CLI starts:
/// `serve`, `record`, and — through the thin proxy wrapper in `intercept` —
/// the explicit proxy. Binding a replay server or a recording gateway to a
/// routable address publishes a fixture or an upstream to every client that
/// can reach it, and neither command has any notion of a caller identity.
/// TLS is not authentication: it protects bytes in transit, it does not tell
/// the listener *who* connected, so `--inbound-tls` never makes a public bind
/// safe.
///
/// Loopback always passes. Every other address requires the explicit
/// `--allow-non-loopback` opt-in and is otherwise a `configuration` refusal
/// (exit `2`).
///
/// This lives here, in an always-compiled module, and not in the
/// `intercept`-gated operator surface: `intercept` is an opt-in Cargo feature,
/// so a gate that lived behind that `cfg` would silently disappear from every
/// default build — the build that actually ships `serve` and `record`.
fn check_listener_bind(
    bind: std::net::SocketAddr,
    allow_non_loopback: bool,
) -> Result<(), (String, String)> {
    if bind.ip().is_loopback() || allow_non_loopback {
        return Ok(());
    }
    Err((
        "configuration".to_owned(),
        format!(
            "refusing non-loopback listener bind on {bind}: the listener is \
             unauthenticated and TLS does not add authentication; pass \
             --allow-non-loopback to bind beyond loopback"
        ),
    ))
}

/// Apply [`check_listener_bind`] to one command invocation.
///
/// A refusal is reported the same way for every listener: a `configuration`
/// envelope on the machine output channel carries the class and the bind, and
/// the returned error becomes the `configuration: ...` stderr diagnostic. The
/// gate itself runs before any caller-side filesystem work, so emitting here
/// cannot report a fixture that was never touched.
fn enforce_listener_gate(
    command: &str,
    output: OutputChoice,
    bind: std::net::SocketAddr,
    allow_non_loopback: bool,
) -> Result<(), (String, String)> {
    match check_listener_bind(bind, allow_non_loopback) {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(
                command,
                output,
                false,
                Some("configuration"),
                json!({"listen": bind}),
            );
            Err(error)
        }
    }
}

// Deliberately one linear script: configuration resolution, then the exposure
// gate, then staging, then the listener, then finalization. That ordering is
// the contract (a bad config or a refused bind must never touch a fixture), so
// splitting the body for a line-count lint would obscure it.
#[allow(clippy::too_many_lines)]
async fn record(args: RecordArgs) -> Result<(), (String, String)> {
    // Same ordering rule as `serve`: every configuration input is resolved
    // before any filesystem precondition is evaluated, and nothing is deleted,
    // so a bad route or upstream can never damage an existing fixture.
    let inbound = args
        .inbound
        .resolve()
        .map_err(|error| ("configuration".into(), error))?;
    // Exposure gate: upstream parsing, target classification, staging, and bind
    // all come after it.
    enforce_listener_gate(
        "record",
        args.output.output,
        args.listen,
        args.allow_non_loopback,
    )?;
    let timeout = args.timeout.resolve();
    let upstream = args
        .upstream
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
    let (redaction, profile_id) = effective_redaction_policy(&args);
    let warning_redaction = redaction.clone();
    let (client, physical_route) = args.outbound_version.client(&args.route, timeout)?;
    if let Err(error) = check_replacement_target(&args.fixture, args.overwrite, "fixture") {
        emit(
            "record",
            args.output.output,
            false,
            Some("configuration"),
            json!({"fixture": args.fixture}),
        );
        return Err(error);
    }
    // Record into a sibling staging destination. The target is only touched
    // after the staged fixture has been finalized and re-opened, and a failed
    // recording removes the staging tree without touching the target.
    let mut staging = StagingGuard::new(sibling_transaction_path(&args.fixture, "record"));
    let session = RecordingSession::create(
        staging.path(),
        SessionMetadata {
            capture_mode: "gateway".into(),
            target: Some(redact_url(&args.upstream)),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let server = eggreplay_http::recording::start_recording_gateway_with_protocol(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
        websocket_options(
            args.websockets,
            args.redact_websocket_text,
            args.redact_websocket_binary,
        ),
        inbound,
        args.inbound.limits(),
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    let serving = args.inbound.describe();
    eprintln!(
        "recording on {} (inbound {})",
        server.local_addr(),
        serving.protocol,
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    // Already drained: EggServe shutdown+wait drains tracked tunnel tasks
    // (conversation completers signalled via complete/Drop) before session
    // finalization. Blocking isolation via spawn_blocking keeps the executor
    // free even on current-thread runtimes.
    server.shutdown();
    server
        .wait()
        .await
        .map_err(|error| ("runtime".into(), error))?;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let websocket_count = session.websocket_conversation_count().unwrap_or(0);
    let recorded = eggreplay_http::recording::finish_recording_session(session)
        .await
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let warnings = session_redaction_warnings(
        &recorded,
        &warning_redaction,
        args.websockets,
        args.redact_websocket_text,
        args.redact_websocket_binary,
    );
    drop(recorded);
    replace_fixture_transactionally(staging.path(), &args.fixture)
        .map_err(|error| ("runtime".into(), error))?;
    staging.disarm();
    emit_with_warnings(
        "record",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "status": "finalized", "serving": serving, "websocket_acquisition_enabled": args.websockets, "websocket_conversations": websocket_count, "outbound_timeout": args.timeout.describe()}),
        warnings,
    );
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), (String, String)> {
    // Resolve the serving policy first. It is a pure configuration decision,
    // so reporting it before touching the filesystem means an operator who
    // mistyped `--inbound` hears about that rather than about a fixture path.
    let inbound = args
        .inbound
        .resolve()
        .map_err(|error| ("configuration".into(), error))?;
    // Exposure gate, before fixture recovery and before the record mode is
    // resolved. It applies to every mode — sealed, `once`, `append-new`, and
    // `re-record` — because the bind is chosen by the operator, not by the
    // mode, and no inbound protocol policy can make an unauthenticated
    // listener safe.
    enforce_listener_gate(
        "serve",
        args.output.output,
        args.listen,
        args.allow_non_loopback,
    )?;
    recover_fixture_transactionally(&args.fixture).map_err(|error| ("fixture".into(), error))?;
    let policy = eggreplay_core::RecordMode::from(args.record_mode)
        .resolve(args.fixture.exists(), args.upstream.is_some())
        .map_err(|message| ("configuration".into(), message))?;
    if args.websockets && policy.mode == eggreplay_core::RecordMode::AppendNew {
        return Err((
            "configuration".into(),
            "WebSocket acquisition is not available in append-new mode".into(),
        ));
    }
    if args.websockets && policy.mode == eggreplay_core::RecordMode::Sealed {
        return Err((
            "configuration".into(),
            "--websockets requires a network-capable record mode".into(),
        ));
    }
    let timing_mode = eggreplay_core::StreamTimingMode::parse(&args.timing_mode)
        .map_err(|error| ("configuration".into(), error))?;
    if timing_mode != eggreplay_core::StreamTimingMode::Immediate
        && policy.mode != eggreplay_core::RecordMode::Sealed
    {
        return Err((
            "configuration".into(),
            "timed replay is only supported in sealed serve mode".into(),
        ));
    }
    if !policy.upstream_enabled && args.route != "direct" {
        return Err((
            "configuration".into(),
            "--route requires a network-capable record mode and explicit --upstream".into(),
        ));
    }
    if policy.mode == eggreplay_core::RecordMode::Once {
        if args.scenario.is_some() {
            return Err((
                "configuration".into(),
                "--scenario is only valid with sealed replay".into(),
            ));
        }
        return record_once_from_serve(args).await;
    }
    if policy.mode == eggreplay_core::RecordMode::AppendNew {
        if !args.fixture.exists() {
            return Err((
                "configuration".into(),
                "append-new requires an existing fixture".into(),
            ));
        }
        return serve_append_new(args).await;
    }
    if policy.mode == eggreplay_core::RecordMode::ReRecord {
        return serve_re_record(args).await;
    }
    serve_sealed(args, inbound, timing_mode).await
}

/// Sealed offline replay over an explicit serving policy.
async fn serve_sealed(
    args: ServeArgs,
    inbound: InboundProtocol,
    timing_mode: eggreplay_core::StreamTimingMode,
) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let (serve_redaction, _) = effective_serve_redaction_policy(&args);
    let fixture = match args.scenario.as_deref() {
        Some(scenario_id) => ReplayFixture::load_with_scenario_and_redaction_and_timing(
            &session,
            args.matcher_profile.matcher(),
            scenario_id,
            serve_redaction,
            timing_mode,
        ),
        None => {
            ReplayFixture::load_with_timing(&session, args.matcher_profile.matcher(), timing_mode)
        }
    }
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let server = fixture
        .start_with_protocol(
            args.listen,
            StoreLimits::default().max_blob_bytes,
            inbound,
            args.inbound.limits(),
        )
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    let serving = args.inbound.describe();
    eprintln!(
        "serving {} on {} (inbound {})",
        args.fixture.display(),
        server.local_addr(),
        serving.protocol,
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server
        .wait()
        .await
        .map_err(|error| ("runtime".into(), error))?;
    emit(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "scenario": args.scenario, "record_mode": "sealed", "upstream_enabled": false, "websocket_acquisition_enabled": false, "websocket_conversations": 0, "matcher_profile": args.matcher_profile.as_str(), "timing_mode": args.timing_mode, "serving": serving, "status": "stopped"}),
    );
    Ok(())
}

async fn record_once_from_serve(args: ServeArgs) -> Result<(), (String, String)> {
    let timeout = args.timeout.resolve();
    let upstream = required_upstream(&args)?;
    let (client, physical_route) = args.outbound_version.client(&args.route, timeout)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
    let warning_redaction = redaction.clone();
    let session = RecordingSession::create(
        &args.fixture,
        SessionMetadata {
            capture_mode: "gateway-once".into(),
            target: Some(redact_url(&args.upstream.clone().unwrap_or_default())),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let inbound = args
        .inbound
        .resolve()
        .map_err(|error| ("configuration".into(), error))?;
    let server = eggreplay_http::recording::start_recording_gateway_with_protocol(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
        websocket_options(
            args.websockets,
            args.redact_websocket_text,
            args.redact_websocket_binary,
        ),
        inbound,
        args.inbound.limits(),
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    let serving = args.inbound.describe();
    eprintln!(
        "recording once on {} (inbound {})",
        server.local_addr(),
        serving.protocol,
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    // Already drained + blocking isolation (see `record`): shutdown+wait
    // drains WebSocket tunnels before session finalization runs on the
    // blocking pool.
    server.shutdown();
    server
        .wait()
        .await
        .map_err(|error| ("runtime".into(), error))?;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let websocket_count = session.websocket_conversation_count().unwrap_or(0);
    let recorded = eggreplay_http::recording::finish_recording_session(session)
        .await
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let warnings = session_redaction_warnings(
        &recorded,
        &warning_redaction,
        args.websockets,
        args.redact_websocket_text,
        args.redact_websocket_binary,
    );
    drop(recorded);
    emit_with_warnings(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "once", "upstream_enabled": true, "serving": serving, "websocket_acquisition_enabled": args.websockets, "websocket_conversations": websocket_count, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": args.redaction_profile, "outbound_timeout": args.timeout.describe(), "status": "finalized"}),
        warnings,
    );
    Ok(())
}

/// Parse the `--upstream` argument every recording path requires.
fn required_upstream(args: &ServeArgs) -> Result<http::Uri, (String, String)> {
    args.upstream
        .as_deref()
        .ok_or_else(|| ("configuration".into(), "--upstream is required".into()))?
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))
}

async fn serve_append_new(args: ServeArgs) -> Result<(), (String, String)> {
    let timeout = args.timeout.resolve();
    let source = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let (client, physical_route) = args.outbound_version.client(&args.route, timeout)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
    let warning_redaction = redaction.clone();
    if profile_id != source.manifest().metadata.redaction_profile {
        return Err((
            "configuration".into(),
            "append-new redaction profile must match the source fixture; select its --redaction-profile explicitly".into(),
        ));
    }
    let upstream = required_upstream(&args)?;
    let temporary = sibling_transaction_path(&args.fixture, "misses");
    let combined = sibling_transaction_path(&args.fixture, "combined");
    let recording = RecordingSession::create(
        &temporary,
        SessionMetadata {
            capture_mode: "append-new".into(),
            target: Some(redact_url(&args.upstream.clone().unwrap_or_default())),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let fixture = match args.scenario.as_deref() {
        Some(id) => ReplayFixture::load_with_scenario_and_redaction(
            &source,
            args.matcher_profile.matcher(),
            id,
            redaction.clone(),
        ),
        None => ReplayFixture::load_with_matcher(&source, args.matcher_profile.matcher()),
    }
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let inbound = args
        .inbound
        .resolve()
        .map_err(|error| ("configuration".into(), error))?;
    let server = fixture
        .start_with_protocol_and_append(
            args.listen,
            StoreLimits::default().max_blob_bytes,
            upstream,
            client,
            recording.clone(),
            redaction,
            profile_id.clone(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            physical_route,
            inbound,
            args.inbound.limits(),
        )
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    let serving = args.inbound.describe();
    eprintln!(
        "append-new replay on {} (inbound {})",
        server.local_addr(),
        serving.protocol,
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    // Already drained + blocking isolation: append-new carries no WebSocket
    // finalizers, so finalization is on the HTTP fast path; still run on the
    // blocking pool for executor safety.
    server.shutdown();
    server
        .wait()
        .await
        .map_err(|error| ("runtime".into(), error))?;
    recording.shutdown();
    eggreplay_http::recording::drain_active_blobs(&recording).await;
    let additional = eggreplay_http::recording::finish_recording_session(recording)
        .await
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let added_flows = additional.manifest().flow_count;
    let warnings = session_redaction_warnings(&additional, &warning_redaction, false, false, false);
    let merged = source
        .merge_to(
            &additional,
            &combined,
            eggreplay_core::SESSION_SCHEMA_VERSION,
        )
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    drop(merged);
    replace_fixture_transactionally(&combined, &args.fixture)
        .map_err(|error| ("runtime".into(), error))?;
    let _ = std::fs::remove_dir_all(&temporary);
    emit_with_warnings(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "append-new", "upstream_enabled": true, "serving": serving, "websocket_acquisition_enabled": false, "websocket_conversations": 0, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": profile_id, "new_flows": added_flows, "status": "finalized"}),
        warnings,
    );
    Ok(())
}

async fn serve_re_record(args: ServeArgs) -> Result<(), (String, String)> {
    let timeout = args.timeout.resolve();
    if args.scenario.is_some() {
        return Err((
            "configuration".into(),
            "re-record cannot select an authored offline scenario".into(),
        ));
    }
    let upstream = required_upstream(&args)?;
    let (client, physical_route) = args.outbound_version.client(&args.route, timeout)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
    let warning_redaction = redaction.clone();
    let inbound = args
        .inbound
        .resolve()
        .map_err(|error| ("configuration".into(), error))?;
    // Re-record implicitly replaces its own fixture. Refuse a path that is not
    // a valid fixture, an empty directory, or absent *before* binding the
    // listener, so a typo never starts a recording that could delete data.
    check_replacement_target(&args.fixture, true, "fixture")?;
    let mut staging = StagingGuard::new(sibling_transaction_path(&args.fixture, "rerecord"));
    let session = RecordingSession::create(
        staging.path(),
        SessionMetadata {
            capture_mode: "re-record".into(),
            target: Some(redact_url(&args.upstream.clone().unwrap_or_default())),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let server = eggreplay_http::recording::start_recording_gateway_with_protocol(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
        websocket_options(
            args.websockets,
            args.redact_websocket_text,
            args.redact_websocket_binary,
        ),
        inbound,
        args.inbound.limits(),
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    let serving = args.inbound.describe();
    eprintln!(
        "re-record gateway on {} (inbound {})",
        server.local_addr(),
        serving.protocol,
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    // Already drained + blocking isolation (see `record`).
    server.shutdown();
    server
        .wait()
        .await
        .map_err(|error| ("runtime".into(), error))?;
    session.shutdown();
    eggreplay_http::recording::drain_active_blobs(&session).await;
    let websocket_count = session.websocket_conversation_count().unwrap_or(0);
    let recorded = eggreplay_http::recording::finish_recording_session(session)
        .await
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let recorded_flows = recorded.manifest().flow_count;
    let warnings = session_redaction_warnings(
        &recorded,
        &warning_redaction,
        args.websockets,
        args.redact_websocket_text,
        args.redact_websocket_binary,
    );
    drop(recorded);
    // A failed recording never reaches this point: `finish` fails closed and
    // the staging guard removes the partial tree.
    replace_fixture_transactionally(staging.path(), &args.fixture)
        .map_err(|error| ("runtime".into(), error))?;
    staging.disarm();
    emit_with_warnings(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "re-record", "upstream_enabled": true, "serving": serving, "websocket_acquisition_enabled": args.websockets, "websocket_conversations": websocket_count, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": args.redaction_profile, "new_flows": recorded_flows, "outbound_timeout": args.timeout.describe(), "status": "finalized"}),
        warnings,
    );
    Ok(())
}

/// What an existing (or absent) replacement target actually is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplacementTarget {
    /// Nothing exists at the path; it is safe to create.
    Absent,
    /// An empty directory; replacing it discards nothing.
    EmptyDir,
    /// A directory that opens as a valid fixture (`Session::open` succeeds).
    ValidFixture,
}

impl ReplacementTarget {
    const fn exists(self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// The single authority for "may this path be replaced?".
///
/// Every destructive command routes through this classifier, and
/// [`replace_fixture_transactionally`] re-checks it as defense in depth. It
/// never follows a symlink, never treats an arbitrary non-empty directory as
/// disposable, and rejects `.`/`..`/filesystem roots lexically before any
/// filesystem call. An existing directory is disposable only when it is empty
/// or when `Session::open` with the default limits accepts it; a future
/// schema, an incomplete manifest, or malformed flows are refusals, not
/// permission to delete.
fn classify_replacement_target(path: &Path) -> Result<ReplacementTarget, String> {
    use std::path::Component;

    if path.file_name().is_none() {
        return Err(
            "the path has no final component (it is a filesystem root, `.`, or `..`)".into(),
        );
    }
    // `./fixture.eggr` is an ordinary spelling and resolves to the same
    // directory; a `..` component can escape the operator's intended parent,
    // so it is refused lexically before any filesystem call.
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("the path contains a `..` component".into());
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplacementTarget::Absent);
        }
        Err(error) => return Err(format!("cannot inspect the path: {error}")),
    };
    if metadata.file_type().is_symlink() {
        return Err("the path is a symbolic link; symlinks are never followed".into());
    }
    if !metadata.is_dir() {
        return Err("the path exists and is not a directory".into());
    }
    let mut entries =
        std::fs::read_dir(path).map_err(|error| format!("cannot read the directory: {error}"))?;
    if entries.next().is_none() {
        return Ok(ReplacementTarget::EmptyDir);
    }
    Session::open(path, StoreLimits::default()).map_err(|error| {
        format!(
            "the directory is not empty and does not open as a valid EggReplay fixture: {error}"
        )
    })?;
    Ok(ReplacementTarget::ValidFixture)
}

/// Enforce `--overwrite` and the replacement-target rules in one place.
///
/// `noun` is the operator-facing name for the target (`fixture` or
/// `destination`) and is used only in the missing-flag diagnostic.
fn check_replacement_target(
    path: &Path,
    overwrite: bool,
    noun: &str,
) -> Result<ReplacementTarget, (String, String)> {
    let target = classify_replacement_target(path).map_err(|reason| {
        (
            "configuration".to_owned(),
            format!("cannot replace {}: {reason}", path.display()),
        )
    })?;
    if target.exists() && !overwrite {
        return Err((
            "configuration".to_owned(),
            format!(
                "{noun} exists; pass --overwrite to replace an existing fixture or empty directory"
            ),
        ));
    }
    Ok(target)
}

/// Best-effort cleanup for a sibling staging destination.
///
/// Armed while a staged operation is in flight. `Drop` removes the staging
/// destination plus any `.incomplete-*` directory the store writer created
/// under that name, so every early return cleans up without repeating the code
/// at each `?`. The success path calls [`StagingGuard::disarm`] after the
/// staged fixture is published.
pub(crate) struct StagingGuard {
    destination: PathBuf,
    armed: bool,
}

impl StagingGuard {
    pub(crate) fn new(destination: PathBuf) -> Self {
        Self {
            destination,
            armed: true,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.destination
    }

    /// Keep the staging result; used only after a successful publish.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            remove_staged_fixture(&self.destination);
        }
    }
}

/// Remove a staging destination and any hidden `.incomplete-*` directory a
/// store writer created for it. Failures are ignored: cleanup must never mask
/// the original error.
fn remove_staged_fixture(destination: &Path) {
    let _ = std::fs::remove_dir_all(destination);
    let Some(parent) = destination.parent() else {
        return;
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let Some(name) = destination.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let prefix = format!(".{name}.incomplete-");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn sibling_transaction_path(fixture: &std::path::Path, label: &str) -> PathBuf {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = fixture
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("fixture.eggr");
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = fixture.with_file_name(format!(
            ".{name}.{label}-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        // Never rename over a path that already exists, even if it is stale
        // residue from a crashed run: take another name instead.
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
}

fn replace_fixture_transactionally(
    staged: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    // Defense in depth: callers validate first, but a future caller must not
    // be able to bypass the replacement-target authority here.
    let existing = classify_replacement_target(target)
        .map_err(|reason| format!("cannot replace {}: {reason}", target.display()))?;
    if existing == ReplacementTarget::Absent {
        return std::fs::rename(staged, target).map_err(|error| error.to_string());
    }
    let backup = sibling_transaction_path(target, "backup");
    std::fs::rename(target, &backup)
        .map_err(|error| format!("cannot stage old fixture: {error}"))?;
    if let Err(error) = std::fs::rename(staged, target) {
        let restore = std::fs::rename(&backup, target);
        return Err(match restore {
            Ok(()) => format!("cannot publish new fixture; old fixture restored: {error}"),
            Err(restore_error) => format!(
                "cannot publish new fixture ({error}); restore old fixture at {} failed ({restore_error})",
                backup.display()
            ),
        });
    }
    std::fs::remove_dir_all(backup).map_err(|error| {
        format!("new fixture published but old backup could not be removed: {error}")
    })
}

fn recover_fixture_transactionally(target: &std::path::Path) -> Result<(), String> {
    if target.exists() {
        return Ok(());
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "fixture path has no filename".to_owned())?;
    let prefix = format!(".{name}.backup-");
    let mut backups = std::fs::read_dir(parent)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    backups.sort();
    for backup in backups.into_iter().rev() {
        if Session::open(&backup, StoreLimits::default()).is_ok() {
            std::fs::rename(&backup, target)
                .map_err(|error| format!("cannot restore last valid fixture backup: {error}"))?;
            return Ok(());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
async fn regression(
    fixture: PathBuf,
    target: String,
    route: String,
    outbound_version: OutboundVersionChoice,
    timeout_args: &TimeoutArgs,
    command: &str,
    output: OutputArgs,
    enforce: bool,
    scheduler: SchedulerOptions,
    policy: ComparisonPolicy,
    timing: Option<TimingAssertion>,
) -> Result<(), (String, String)> {
    let timeout = timeout_args.resolve();
    // Resolved once, reported verbatim: the operator's status output says
    // whether the run was bounded, not what the resolver happened to default
    // to.
    let outbound_timeout = timeout_args.describe();
    let session = Session::open(&fixture, StoreLimits::default()).map_err(|error| {
        emit_reports(
            command,
            output.output,
            false,
            Some("fixture"),
            &target,
            &[],
            &[],
            0,
            &outbound_timeout,
        );
        ("fixture".into(), error.to_string())
    })?;
    let target_uri: http::Uri = target.parse().map_err(|error: http::uri::InvalidUri| {
        emit_reports(
            command,
            output.output,
            false,
            Some("configuration"),
            &target,
            &[],
            &[],
            0,
            &outbound_timeout,
        );
        ("configuration".into(), error.to_string())
    })?;
    let (client, physical_route) = build_client_with_version(&route, outbound_version, timeout)
        .map_err(|(class, message)| {
            emit_reports(
                command,
                output.output,
                false,
                Some(&class),
                &target,
                &[],
                &[],
                0,
                &outbound_timeout,
            );
            (class, message)
        })?;
    let flows = session
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let flow_ids = flows.iter().map(|flow| flow.id.clone()).collect::<Vec<_>>();
    // Load baseline stream events by flow id when requested. Missing requested
    // metadata is an explicit fixture/configuration error, not fallback.
    let baseline_streams: Option<
        std::collections::HashMap<String, eggreplay_core::FlowStreamEvents>,
    > = if policy.is_stream_enabled() {
        let bytes = session
                .read_extension("stream-events")
                .map_err(|error| ("fixture".into(), error.to_string()))?
                .ok_or_else(|| {
                    (
                        "fixture".to_owned(),
                        "stream comparison requested but baseline fixture has no stream-events extension".to_owned(),
                    )
                })?;
        let decoded: eggreplay_core::StreamEvents =
            serde_json::from_slice(&bytes).map_err(|error| {
                (
                    "fixture".to_owned(),
                    format!("invalid stream-events extension: {error}"),
                )
            })?;
        decoded
            .validate()
            .map_err(|error| ("fixture".to_owned(), error))?;
        Some(
            decoded
                .flows
                .into_iter()
                .map(|flow| (flow.flow_id.clone(), flow))
                .collect(),
        )
    } else {
        None
    };
    let mut reports = vec![None; flows.len()];
    match scheduler.choice {
        SchedulerChoice::Sequential => {
            for (index, baseline) in flows.iter().enumerate() {
                reports[index] = Some(
                    compare_candidate_flow(
                        &session,
                        &client,
                        &target_uri,
                        &physical_route,
                        baseline.clone(),
                        ReportScheduler::Sequential,
                        &policy,
                        timing,
                        baseline_streams.as_ref(),
                    )
                    .await
                    .map_err(|error| ("runtime".into(), error))?,
                );
            }
        }
        SchedulerChoice::Timeline => {
            if scheduler.max_concurrency == 0 || scheduler.max_concurrency > 1024 {
                return Err((
                    "configuration".into(),
                    "--max-concurrency must be within 1..=1024".into(),
                ));
            }
            let event_bytes = session
                .read_extension("stream-events")
                .map_err(|error| ("fixture".into(), error.to_string()))?
                .ok_or_else(|| {
                    (
                        "configuration".into(),
                        "timeline scheduler requires a stream-events extension".into(),
                    )
                })?;
            let events: eggreplay_core::StreamEvents = serde_json::from_slice(&event_bytes)
                .map_err(|error| {
                    (
                        "fixture".into(),
                        format!("invalid stream-events extension: {error}"),
                    )
                })?;
            events
                .validate()
                .map_err(|error| ("fixture".into(), error))?;
            let event_map = events
                .flows
                .into_iter()
                .map(|flow| (flow.flow_id, flow.start_offset_ns))
                .collect::<std::collections::HashMap<_, _>>();
            let offsets = flows
                .iter()
                .map(|flow| {
                    event_map.get(&flow.id).copied().ok_or_else(|| {
                        (
                            "fixture".into(),
                            format!("timeline offset is missing for flow {}", flow.id),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let order = eggreplay_core::timeline_order_offsets(&offsets, scheduler.max_concurrency)
                .map_err(|error| ("configuration".into(), error))?;
            let ordered = order
                .into_iter()
                .map(|index| (offsets[index], index))
                .collect::<Vec<_>>();
            let origin = tokio::time::Instant::now();
            let mut next = 0usize;
            let mut tasks = tokio::task::JoinSet::new();
            while next < ordered.len() || !tasks.is_empty() {
                while next < ordered.len() && tasks.len() < scheduler.max_concurrency {
                    let (offset, index) = ordered[next];
                    next += 1;
                    let baseline = flows[index].clone();
                    let session = session.clone();
                    let client = client.clone();
                    let target_uri = target_uri.clone();
                    let physical_route = physical_route.clone();
                    let policy = policy.clone();
                    let baseline_streams = baseline_streams.clone();
                    tasks.spawn(async move {
                        tokio::time::sleep_until(origin + std::time::Duration::from_nanos(offset))
                            .await;
                        let report = compare_candidate_flow(
                            &session,
                            &client,
                            &target_uri,
                            &physical_route,
                            baseline,
                            ReportScheduler::Timeline,
                            &policy,
                            timing,
                            baseline_streams.as_ref(),
                        )
                        .await?;
                        Ok::<_, String>((index, report))
                    });
                }
                if let Some(result) = tasks.join_next().await {
                    let (index, report) = result
                        .map_err(|error| {
                            ("runtime".into(), format!("timeline task failed: {error}"))
                        })?
                        .map_err(|error| ("runtime".into(), error))?;
                    reports[index] = Some(report);
                }
            }
        }
    }
    let reports = reports
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| ("runtime".into(), "scheduler omitted a flow result".into()))?;
    let findings = reports
        .iter()
        .flat_map(|report| report.findings.clone())
        .collect::<Vec<_>>();
    // `replay` reports differences with exit 0; `test` enforces with exit 1.
    if enforce {
        let success = findings.is_empty();
        emit_reports(
            command,
            output.output,
            success,
            (!success).then_some("regression"),
            &target_uri.to_string(),
            &flow_ids,
            &reports,
            findings.len(),
            &outbound_timeout,
        );
        if !success {
            return Err((
                "regression".into(),
                "candidate differs from baseline".into(),
            ));
        }
        Ok(())
    } else {
        emit_reports(
            command,
            output.output,
            true,
            None,
            &target_uri.to_string(),
            &flow_ids,
            &reports,
            findings.len(),
            &outbound_timeout,
        );
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn compare_candidate_flow(
    session: &Session,
    client: &eggfetch_core::Client,
    target_uri: &http::Uri,
    physical_route: &PhysicalRoute,
    baseline: eggreplay_core::Flow,
    scheduler: ReportScheduler,
    policy: &ComparisonPolicy,
    timing: Option<TimingAssertion>,
    baseline_streams: Option<&std::collections::HashMap<String, eggreplay_core::FlowStreamEvents>>,
) -> Result<eggreplay_core::RegressionReport, String> {
    if matches!(&baseline.outcome, FlowOutcome::Response(response) if response.status == 101) {
        let bytes = session
            .read_extension("websocket-messages")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "WebSocket 101 baseline is missing websocket-messages metadata".to_owned()
            })?;
        let transcript: eggreplay_core::WebSocketTranscript = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid websocket-messages extension: {e}"))?;
        let conversation = transcript
            .conversations
            .iter()
            .find(|conversation| conversation.flow_id == baseline.id)
            .ok_or_else(|| {
                format!(
                    "WebSocket 101 baseline flow {} has no conversation",
                    baseline.id
                )
            })?;
        let findings = eggreplay_http::compare_websocket_candidate(
            client,
            session,
            &baseline.request,
            target_uri,
            conversation,
            policy.websocket_cadence_tolerance_ns,
        )
        .await
        .map_err(|e| e.to_string())?;
        return Ok(eggreplay_core::RegressionReport {
            schema_version: eggreplay_core::REPORT_SCHEMA_VERSION,
            scheduler,
            baseline_flow_ids: vec![baseline.id],
            findings,
            suppressed: Vec::new(),
        });
    }
    let request_body = body(session, &baseline.request.body).map_err(|error| error.to_string())?;
    let candidate = execute_candidate(
        client,
        &baseline.request,
        &request_body,
        target_uri,
        StoreLimits::default().max_blob_bytes,
        Some(physical_route.clone()),
    )
    .await
    .map_err(|error| error.to_string())?;
    let baseline_response = match &baseline.outcome {
        FlowOutcome::Response(response) => {
            body(session, &response.body).map_err(|error| error.to_string())?
        }
        FlowOutcome::Error(_) => Vec::new(),
    };
    let mut report = compare_flows_with_timing_and_policy(
        &baseline,
        &candidate.flow,
        &baseline_response,
        &candidate.response_body,
        scheduler,
        timing,
        policy,
    );
    // Live candidate regression, response-direction authoritative:
    // 1. load baseline stream events by flow id when requested;
    // 2. compare candidate response events from the observation;
    // 3. report event-shape/terminal differences;
    // 4. apply cadence tolerance only when configured;
    // 5. merge findings into the existing RegressionReport authority;
    // 6. keep deterministic fixture/report ordering.
    if policy.is_stream_enabled() {
        let streams = baseline_streams.ok_or_else(|| {
            "stream comparison requested but baseline stream metadata is missing".to_owned()
        })?;
        let baseline_events = streams.get(&baseline.id).ok_or_else(|| {
            format!(
                "stream comparison requested but baseline metadata is missing for flow {}",
                baseline.id
            )
        })?;
        let baseline_response_only = eggreplay_core::FlowStreamEvents {
            flow_id: baseline.id.clone(),
            start_offset_ns: 0,
            request: Vec::new(),
            response: baseline_events.response.clone(),
        };
        let candidate_response_only = eggreplay_core::FlowStreamEvents {
            flow_id: baseline.id.clone(),
            start_offset_ns: 0,
            request: Vec::new(),
            response: candidate.response_events.clone(),
        };
        let mut stream_findings = eggreplay_core::compare_stream_events(
            &baseline_response_only,
            &candidate_response_only,
            policy.cadence_tolerance_ns,
        );
        report.findings.append(&mut stream_findings);
        report.findings.sort_by(|left, right| {
            (format!("{:?}", left.kind), &left.field)
                .cmp(&(format!("{:?}", right.kind), &right.field))
        });
    }
    Ok(report)
}

#[allow(clippy::too_many_lines)]
fn diff(args: &DiffArgs) -> Result<(), (String, String)> {
    let policy = comparison_policy(&args.comparison)?;
    let timing = timing_assertion(&args.comparison);
    let baseline = Session::open(&args.baseline, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let candidate = Session::open(&args.candidate, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let left = baseline
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let right = candidate
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let baseline_websockets = fixture_websockets(&baseline)?;
    let candidate_websockets = fixture_websockets(&candidate)?;
    // Requested stream comparison requires valid metadata on both sides.
    // Missing requested metadata is an explicit fixture/configuration error,
    // not fallback. Comparison is response-direction authoritative and
    // deterministic in fixture order.
    let baseline_streams = if policy.is_stream_enabled() {
        let bytes = baseline
            .read_extension("stream-events")
            .map_err(|error| ("fixture".into(), error.to_string()))?
            .ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    "stream comparison requested but baseline fixture has no stream-events extension".to_owned(),
                )
            })?;
        let decoded: eggreplay_core::StreamEvents =
            serde_json::from_slice(&bytes).map_err(|error| {
                (
                    "fixture".to_owned(),
                    format!("invalid baseline stream-events extension: {error}"),
                )
            })?;
        decoded
            .validate()
            .map_err(|error| ("fixture".to_owned(), error))?;
        Some(
            decoded
                .flows
                .into_iter()
                .map(|flow| (flow.flow_id.clone(), flow))
                .collect::<std::collections::HashMap<_, _>>(),
        )
    } else {
        None
    };
    let candidate_streams = if policy.is_stream_enabled() {
        let bytes = candidate
            .read_extension("stream-events")
            .map_err(|error| ("fixture".into(), error.to_string()))?
            .ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    "stream comparison requested but candidate fixture has no stream-events extension".to_owned(),
                )
            })?;
        let decoded: eggreplay_core::StreamEvents =
            serde_json::from_slice(&bytes).map_err(|error| {
                (
                    "fixture".to_owned(),
                    format!("invalid candidate stream-events extension: {error}"),
                )
            })?;
        decoded
            .validate()
            .map_err(|error| ("fixture".to_owned(), error))?;
        Some(
            decoded
                .flows
                .into_iter()
                .map(|flow| (flow.flow_id.clone(), flow))
                .collect::<std::collections::HashMap<_, _>>(),
        )
    } else {
        None
    };
    let mut reports = Vec::with_capacity(left.len().min(right.len()));
    for (left_flow, right_flow) in left.iter().zip(right.iter()) {
        let left_body = match &left_flow.outcome {
            FlowOutcome::Response(response) => body(&baseline, &response.body).unwrap_or_default(),
            FlowOutcome::Error(_) => Vec::new(),
        };
        let right_body = match &right_flow.outcome {
            FlowOutcome::Response(response) => body(&candidate, &response.body).unwrap_or_default(),
            FlowOutcome::Error(_) => Vec::new(),
        };
        let mut report = compare_flows_with_timing_and_policy(
            left_flow,
            right_flow,
            &left_body,
            &right_body,
            ReportScheduler::Sequential,
            timing,
            &policy,
        );
        if policy.is_stream_enabled() {
            let baseline_map = baseline_streams.as_ref().ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    "stream comparison requested but baseline stream metadata is missing"
                        .to_owned(),
                )
            })?;
            let candidate_map = candidate_streams.as_ref().ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    "stream comparison requested but candidate stream metadata is missing"
                        .to_owned(),
                )
            })?;
            let left_events = baseline_map.get(&left_flow.id).ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    format!(
                        "stream comparison requested but baseline metadata is missing for flow {}",
                        left_flow.id
                    ),
                )
            })?;
            let right_events = candidate_map.get(&right_flow.id).ok_or_else(|| {
                (
                    "fixture".to_owned(),
                    format!(
                        "stream comparison requested but candidate metadata is missing for flow {}",
                        right_flow.id
                    ),
                )
            })?;
            let left_response_only = eggreplay_core::FlowStreamEvents {
                flow_id: left_flow.id.clone(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: left_events.response.clone(),
            };
            let right_response_only = eggreplay_core::FlowStreamEvents {
                flow_id: left_flow.id.clone(),
                start_offset_ns: 0,
                request: Vec::new(),
                response: right_events.response.clone(),
            };
            let mut stream_findings = eggreplay_core::compare_stream_events(
                &left_response_only,
                &right_response_only,
                policy.cadence_tolerance_ns,
            );
            report.findings.append(&mut stream_findings);
            report.findings.sort_by(|a, b| {
                (format!("{:?}", a.kind), &a.field).cmp(&(format!("{:?}", b.kind), &b.field))
            });
        }
        let has_ws_flow = matches!(&left_flow.outcome, FlowOutcome::Response(r) if r.status == 101)
            || matches!(&right_flow.outcome, FlowOutcome::Response(r) if r.status == 101);
        if has_ws_flow {
            let left_ws = baseline_websockets.get(&left_flow.id);
            let right_ws = candidate_websockets.get(&right_flow.id);
            match (left_ws, right_ws) {
                (Some(left_ws), Some(right_ws)) => {
                    report.findings.extend(compare_fixture_websockets(
                        &baseline,
                        &candidate,
                        left_ws,
                        right_ws,
                        policy.websocket_cadence_tolerance_ns,
                    ));
                }
                _ => {
                    return Err((
                        "fixture".into(),
                        "WebSocket 101 flow has missing conversation metadata".into(),
                    ));
                }
            }
            report.findings.sort_by(|a, b| {
                (format!("{:?}", a.kind), &a.field).cmp(&(format!("{:?}", b.kind), &b.field))
            });
        }
        reports.push(report);
    }
    let success = left.len() == right.len()
        && reports
            .iter()
            .all(eggreplay_core::RegressionReport::is_success);
    let flow_ids = left.iter().map(|flow| flow.id.clone()).collect::<Vec<_>>();
    emit_reports(
        "diff",
        args.output.output,
        success,
        (!success).then_some("diff"),
        "",
        &flow_ids,
        &reports,
        reports.iter().map(|report| report.findings.len()).sum(),
        // `diff` compares two fixtures and never opens a socket, so no
        // outbound timeout applies at all. That is a different fact from
        // "unbounded", and the two are kept distinguishable.
        &serde_json::Value::Null,
    );
    if !success {
        return Err(("diff".into(), "fixtures differ".into()));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn inspect(args: &InspectArgs) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let mut flows = Vec::new();
    let websocket_view = if args.websockets {
        let bytes = session
            .read_extension("websocket-messages")
            .map_err(|error| ("fixture".into(), error.to_string()))?;
        match bytes {
            Some(bytes) => {
                let transcript: eggreplay_core::WebSocketTranscript =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        (
                            "fixture".into(),
                            format!("invalid websocket-messages extension: {error}"),
                        )
                    })?;
                Some(transcript.conversations.into_iter().map(|conversation| json!({
                    "id": conversation.id,
                    "flow_id": conversation.flow_id,
                    "selected_subprotocol": conversation.selected_subprotocol,
                    "terminal": conversation.terminal,
                    "messages": conversation.messages.into_iter().map(|message| {
                        let payload = match message.payload {
                            Some(eggreplay_core::BodyRef::Blob(blob)) => json!({"length": blob.length, "sha256": blob.sha256}),
                            Some(_) | None => json!({"length": 0, "sha256": null}),
                        };
                        json!({"sequence": message.sequence, "direction": message.direction, "kind": message.kind, "delta_ns": message.delta_ns, "payload": payload, "close_code": message.close_code, "close_reason": message.close_reason, "redactions": message.redactions})
                    }).collect::<Vec<_>>()
                })).collect::<Vec<_>>())
            }
            None => Some(Vec::<serde_json::Value>::new()),
        }
    } else {
        None
    };
    for item in session
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
    {
        let flow = item.map_err(|error| ("fixture".into(), error.to_string()))?;
        let body_view = if args.bodies {
            Some(inspect_body(
                &session,
                &flow.request.body,
                args.max_body_bytes,
                args.bodies_base64,
            ))
        } else {
            None
        };
        let response_view = if args.bodies {
            match &flow.outcome {
                FlowOutcome::Response(response) => Some(inspect_body(
                    &session,
                    &response.body,
                    args.max_body_bytes,
                    args.bodies_base64,
                )),
                FlowOutcome::Error(_) => None,
            }
        } else {
            None
        };
        let sse_view = if args.sse {
            match &flow.outcome {
                FlowOutcome::Response(response)
                    if response.headers.iter().any(|header| {
                        header.name.eq_ignore_ascii_case("content-type")
                            && header.value.split(';').next().is_some_and(|media| {
                                media.trim().eq_ignore_ascii_case("text/event-stream")
                            })
                    }) =>
                {
                    Some(inspect_sse(&session, &response.body, args.max_body_bytes))
                }
                _ => None,
            }
        } else {
            None
        };
        flows.push(json!({
            "id": flow.id,
            "method": flow.request.method,
            "path": flow.request.path,
            "outcome": match flow.outcome { FlowOutcome::Response(response) => json!({"status": response.status}), FlowOutcome::Error(error) => json!({"error": format!("{:?}", error.category)}) },
            "redactions": flow.redactions,
            "request_body": body_view,
            "response_body": response_view,
            "sse": sse_view,
        }));
    }
    emit(
        "inspect",
        args.output.output,
        true,
        None,
        json!({
            "manifest": session.manifest(),
            "redaction_profile": session.manifest().metadata.redaction_profile,
            "flows": flows
            ,"websockets": websocket_view
        }),
    );
    Ok(())
}

fn fixture_websockets(
    session: &Session,
) -> Result<
    std::collections::HashMap<String, eggreplay_core::WebSocketConversation>,
    (String, String),
> {
    let Some(bytes) = session
        .read_extension("websocket-messages")
        .map_err(|e| ("fixture".into(), e.to_string()))?
    else {
        return Ok(std::collections::HashMap::new());
    };
    let transcript: eggreplay_core::WebSocketTranscript =
        serde_json::from_slice(&bytes).map_err(|e| {
            (
                "fixture".into(),
                format!("invalid websocket-messages extension: {e}"),
            )
        })?;
    transcript
        .validate(eggreplay_core::WebSocketLimits::default())
        .map_err(|e| ("fixture".into(), e))?;
    Ok(transcript
        .conversations
        .into_iter()
        .map(|conversation| (conversation.flow_id.clone(), conversation))
        .collect())
}

#[allow(clippy::too_many_lines)]
fn compare_fixture_websockets(
    baseline: &Session,
    candidate: &Session,
    left: &eggreplay_core::WebSocketConversation,
    right: &eggreplay_core::WebSocketConversation,
    cadence_tolerance_ns: Option<u64>,
) -> Vec<eggreplay_core::DiffFinding> {
    use eggreplay_core::{DiffFinding, DiffKind, WebSocketRedaction};
    let mut findings = Vec::new();
    let mut finding = |field: String, baseline: String, candidate: String| {
        findings.push(DiffFinding {
            kind: DiffKind::WebSocket,
            field,
            baseline,
            candidate,
        });
    };
    if left.selected_subprotocol != right.selected_subprotocol {
        finding(
            "handshake.subprotocol".into(),
            left.selected_subprotocol
                .clone()
                .unwrap_or_else(|| "none".into()),
            right
                .selected_subprotocol
                .clone()
                .unwrap_or_else(|| "none".into()),
        );
    }
    if left.terminal != right.terminal {
        finding(
            "terminal".into(),
            format!("{:?}", left.terminal),
            format!("{:?}", right.terminal),
        );
    }
    if left.messages.len() != right.messages.len() {
        finding(
            "messages.count".into(),
            left.messages.len().to_string(),
            right.messages.len().to_string(),
        );
    }
    for (index, (a, b)) in left.messages.iter().zip(&right.messages).enumerate() {
        let field = format!("message.{index}");
        if a.direction != b.direction {
            finding(
                format!("{field}.direction"),
                format!("{:?}", a.direction),
                format!("{:?}", b.direction),
            );
        }
        if a.kind != b.kind {
            finding(
                format!("{field}.kind"),
                format!("{:?}", a.kind),
                format!("{:?}", b.kind),
            );
            continue;
        }
        if a.close_code != b.close_code {
            finding(
                format!("{field}.close_code"),
                format!("{:?}", a.close_code),
                format!("{:?}", b.close_code),
            );
        }
        let close_reason_redacted = a.redactions.iter().chain(&b.redactions).any(|r| {
            matches!(
                r,
                WebSocketRedaction::CloseReason | WebSocketRedaction::WholeMessage
            )
        });
        if a.close_reason != b.close_reason && !close_reason_redacted {
            finding(
                format!("{field}.close_reason"),
                "different".into(),
                "different".into(),
            );
        }
        if let Some(tolerance) = cadence_tolerance_ns
            && a.delta_ns.abs_diff(b.delta_ns) > tolerance
        {
            finding(
                format!("{field}.cadence"),
                a.delta_ns.to_string(),
                b.delta_ns.to_string(),
            );
        }
        let whole_wildcard = a
            .redactions
            .iter()
            .chain(&b.redactions)
            .any(|r| matches!(r, WebSocketRedaction::WholeMessage));
        if whole_wildcard || a.kind == eggreplay_core::WebSocketMessageKind::Close {
            continue;
        }
        let left_bytes = a
            .payload
            .as_ref()
            .and_then(|body| fixture_payload(baseline, body));
        let right_bytes = b
            .payload
            .as_ref()
            .and_then(|body| fixture_payload(candidate, body));
        let pointer_sets = a
            .redactions
            .iter()
            .chain(&b.redactions)
            .filter_map(|r| match r {
                WebSocketRedaction::JsonPointers { pointers } => Some(pointers.clone()),
                _ => None,
            })
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();
        let equal = match (left_bytes, right_bytes) {
            (Some(mut left), Some(mut right)) if !pointer_sets.is_empty() => {
                match (
                    eggreplay_core::apply_json_redaction(&left, &pointer_sets),
                    eggreplay_core::apply_json_redaction(&right, &pointer_sets),
                ) {
                    (Ok((l, _)), Ok((r, _))) => {
                        left = l;
                        right = r;
                        left == right
                    }
                    _ => false,
                }
            }
            (Some(left), Some(right)) => left == right,
            (None, None) => true,
            _ => false,
        };
        if !equal {
            finding(
                format!("{field}.payload"),
                payload_summary(a),
                payload_summary(b),
            );
        }
    }
    findings
}

fn fixture_payload(session: &Session, body: &eggreplay_core::BodyRef) -> Option<Vec<u8>> {
    match body {
        eggreplay_core::BodyRef::Blob(blob) => session.read_blob(blob).ok(),
        eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Some(Vec::new()),
    }
}

fn payload_summary(message: &eggreplay_core::WebSocketMessage) -> String {
    match &message.payload {
        Some(eggreplay_core::BodyRef::Blob(blob)) => {
            format!("sha256:{};length:{}", blob.sha256, blob.length)
        }
        Some(body) => format!("length:{}", body.len().unwrap_or(0)),
        None => "length:0".into(),
    }
}

/// Bounded explicit body view for `inspect --bodies`.
///
/// Reads at most `max_bytes` via the validated streaming seam (never beyond
/// the CLI bound), truncates with explicit counts, shows UTF-8 text only when
/// valid, otherwise length + digest (plus bounded base64 only with explicit
/// opt-in). Stored blobs are already redacted (C003), so no secret bypass.
fn inspect_body(
    session: &Session,
    body: &eggreplay_core::BodyRef,
    max_bytes: u64,
    base64: bool,
) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    match body {
        eggreplay_core::BodyRef::Absent => json!({"present": false}),
        eggreplay_core::BodyRef::Empty => json!({"present": true, "length": 0, "text": ""}),
        eggreplay_core::BodyRef::Blob(blob) => {
            let handle = match session.open_blob(blob) {
                Ok(handle) => handle,
                Err(error) => return json!({"error": error.to_string()}),
            };
            let total = handle.len();
            let mut file = handle.into_file();
            // u64->usize: `total.min(max_bytes)` is already bounded by the CLI
            // `--max-body-bytes`; `try_from` rejects >usize::MAX on 32-bit
            // instead of silently truncating.
            let capped = usize::try_from(total.min(max_bytes)).unwrap_or(usize::MAX);
            let mut buf = vec![0u8; capped];
            let read = std::io::Read::by_ref(&mut file)
                .take(max_bytes)
                .read(&mut buf)
                .unwrap_or(0);
            buf.truncate(read);
            let truncated = total > max_bytes;
            // Hash the shown prefix for diagnostics (full digest via stored ref).
            let mut hasher = Sha256::new();
            hasher.update(&buf);
            let shown_digest = format!("{:x}", hasher.finalize());
            if let Ok(text) = std::str::from_utf8(&buf) {
                json!({
                    "present": true,
                    "length": total,
                    "shown": read,
                    "truncated": truncated,
                    "encoding": "utf8",
                    "text": text,
                    "sha256": blob.sha256,
                })
            } else if base64 {
                // Bounded base64 only with explicit opt-in.
                const B64: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut encoded = String::new();
                for chunk in buf.chunks(3) {
                    let mut triple = [0u8; 3];
                    for (index, byte) in chunk.iter().enumerate() {
                        triple[index] = *byte;
                    }
                    let combined = (u32::from(triple[0]) << 16)
                        | (u32::from(triple[1]) << 8)
                        | u32::from(triple[2]);
                    let pad = 3 - chunk.len();
                    for index in 0..4 - pad {
                        encoded.push(B64[((combined >> (18 - 6 * index)) & 63) as usize] as char);
                    }
                    for _ in 0..pad {
                        encoded.push('=');
                    }
                    if encoded.len() > 4 * 1024 {
                        break;
                    }
                }
                json!({
                    "present": true,
                    "length": total,
                    "shown": read,
                    "truncated": truncated,
                    "encoding": "base64",
                    "base64": encoded,
                    "sha256": blob.sha256,
                    "shown_sha256": shown_digest,
                })
            } else {
                json!({
                    "present": true,
                    "length": total,
                    "shown": read,
                    "truncated": truncated,
                    "encoding": "binary",
                    "sha256": blob.sha256,
                    "shown_sha256": shown_digest,
                })
            }
        }
    }
}

fn inspect_sse(
    session: &Session,
    body_ref: &eggreplay_core::BodyRef,
    max_bytes: u64,
) -> serde_json::Value {
    let max_bytes = max_bytes.min(eggreplay_core::stream::MAX_SSE_BODY_BYTES as u64);
    if body_ref.len().is_some_and(|length| length > max_bytes) {
        return json!({"error": "SSE body exceeds --max-body-bytes"});
    }
    match body(session, body_ref) {
        Ok(bytes) => serde_json::to_value(eggreplay_core::parse_sse(&bytes, true))
            .unwrap_or_else(|_| json!({"error": "SSE view serialization failed"})),
        Err(_) => json!({"error": "SSE body could not be read"}),
    }
}

fn effective_redaction_policy(args: &RecordArgs) -> (eggreplay_core::RedactionConfig, String) {
    redaction_policy(
        &args.redact_headers,
        &args.redact_queries,
        &args.redact_json_paths,
        &args.redaction_profile,
        args.unsafe_replace,
    )
}

fn effective_serve_redaction_policy(args: &ServeArgs) -> (eggreplay_core::RedactionConfig, String) {
    redaction_policy(
        &args.redact_headers,
        &args.redact_queries,
        &args.redact_json_paths,
        &args.redaction_profile,
        args.unsafe_replace,
    )
}

fn redaction_policy(
    redact_headers: &[String],
    redact_queries: &[String],
    redact_json_paths: &[String],
    profile: &str,
    unsafe_replace: bool,
) -> (eggreplay_core::RedactionConfig, String) {
    use std::collections::BTreeSet;
    if unsafe_replace {
        let config = eggreplay_core::RedactionConfig {
            headers: redact_headers
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            query_keys: redact_queries.iter().cloned().collect::<BTreeSet<_>>(),
            json_paths: redact_json_paths.iter().cloned().collect::<BTreeSet<_>>(),
        };
        (config, profile.to_owned())
    } else {
        let mut config = eggreplay_core::RedactionConfig::default_secure();
        config
            .headers
            .extend(redact_headers.iter().map(|name| name.to_ascii_lowercase()));
        config.query_keys.extend(redact_queries.iter().cloned());
        config.json_paths.extend(redact_json_paths.iter().cloned());
        (config, profile.to_owned())
    }
}

/// Query keys that commonly carry credentials but are not default-redacted.
const CREDENTIAL_QUERY_KEYS: &[&str] = &[
    "token",
    "access_token",
    "api_key",
    "apikey",
    "password",
    "secret",
    "sig",
    "signature",
];

/// Whether a request header name looks credential-bearing.
///
/// Covers the curated default set, the deliberately-not-default
/// `x-csrf-token`, and the common suffix families. Matching is on the
/// lowercased name.
fn is_credential_header_name(name: &str) -> bool {
    eggreplay_core::security::DEFAULT_SENSITIVE_HEADERS.contains(&name)
        || name == "x-csrf-token"
        || name.ends_with("-api-key")
        || name.ends_with("-auth-token")
        || name.ends_with("-access-token")
}

/// Value-free, advisory redaction-completeness warnings for one recording.
///
/// Names a header or query key only, never its value or a body. Warnings are
/// deduplicated per command and never change success, exit codes, or the
/// payload schema.
fn redaction_warnings(
    flows: &[eggreplay_core::Flow],
    redaction: &eggreplay_core::RedactionConfig,
    websockets_enabled: bool,
    redact_websocket_text: bool,
    redact_websocket_binary: bool,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut warnings = Vec::new();
    let mut seen_headers = BTreeSet::new();
    let mut seen_queries = BTreeSet::new();
    for flow in flows {
        for header in &flow.request.headers {
            let name = header.name.to_ascii_lowercase();
            if is_credential_header_name(&name)
                && !redaction.headers.contains(&name)
                && seen_headers.insert(name.clone())
            {
                warnings.push(format!(
                    "request header {name} may carry a credential and is not redacted; pass --redact-header {name} to redact it"
                ));
            }
        }
        for pair in &flow.request.query {
            let key = pair.key.to_ascii_lowercase();
            if CREDENTIAL_QUERY_KEYS.contains(&key.as_str())
                && !redaction.query_keys.contains(&pair.key)
                && seen_queries.insert(key.clone())
            {
                warnings.push(format!(
                    "request query key {key} may carry a credential and is not redacted; pass --redact-query {key} to redact it"
                ));
            }
        }
    }
    if websockets_enabled
        && !redact_websocket_text
        && !redact_websocket_binary
        && redaction.json_paths.is_empty()
    {
        warnings.push(
            "WebSocket acquisition is enabled without payload redaction; pass --redact-websocket-text, --redact-websocket-binary, or --redact-json-path"
                .into(),
        );
    }
    warnings
}

/// Run [`redaction_warnings`] over a finalized session.
///
/// Scanning is advisory: an unreadable flow iterator yields no warnings rather
/// than failing a command whose recording already succeeded.
fn session_redaction_warnings(
    session: &Session,
    redaction: &eggreplay_core::RedactionConfig,
    websockets_enabled: bool,
    redact_websocket_text: bool,
    redact_websocket_binary: bool,
) -> Vec<String> {
    let flows = session
        .iter_flows()
        .ok()
        .map(|iter| iter.collect::<Result<Vec<_>, _>>().unwrap_or_default())
        .unwrap_or_default();
    redaction_warnings(
        &flows,
        redaction,
        websockets_enabled,
        redact_websocket_text,
        redact_websocket_binary,
    )
}

/// Warn about pre-fix stream-event trailer values without naming any value.
///
/// Stream-event trailer values are non-authoritative and are always written as
/// `<redacted>` by current recorders. A value other than `<redacted>` means the
/// fixture predates that guarantee and needs the opt-in scrub migration.
fn stream_trailer_warnings(session: &Session) -> Vec<String> {
    let Some(bytes) = session.read_extension("stream-events").ok().flatten() else {
        return Vec::new();
    };
    let Ok(events) = serde_json::from_slice::<eggreplay_core::StreamEvents>(&bytes) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for flow in events.flows {
        let count = flow
            .request
            .iter()
            .chain(flow.response.iter())
            .filter_map(|event| match &event.event {
                eggreplay_core::StreamEventKind::Trailers { fields } => Some(
                    fields
                        .iter()
                        .filter(|field| field.value != "<redacted>")
                        .count(),
                ),
                _ => None,
            })
            .sum::<usize>();
        if count > 0 {
            warnings.push(format!(
                "flow {} has {count} stream-event trailer field value(s) that are not <redacted>; run `eggreplay migrate --in-place --scrub-stream-trailers` to remediate",
                flow.flow_id
            ));
        }
    }
    warnings
}

fn validate(args: &ValidateArgs) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let warnings = stream_trailer_warnings(&session);
    emit_with_warnings(
        "validate",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "flow_count": session.manifest().flow_count, "schema_version": session.manifest().metadata.schema_version}),
        warnings,
    );
    Ok(())
}

fn har_redaction_policy(args: &HarImportArgs) -> (eggreplay_core::RedactionConfig, String) {
    redaction_policy(
        &args.redact_headers,
        &args.redact_queries,
        &args.redact_json_paths,
        &args.redaction_profile,
        args.unsafe_replace,
    )
}

/// Write an output that contains recorded material.
///
/// On Unix the file is created with mode `0600` *before* any bytes are written,
/// so it is never world-readable even for an instant. Replacing an existing
/// file also tightens a stale permissive mode before the write. Non-Unix keeps
/// the platform default; secrecy there depends on the destination directory
/// ACLs, matching the documented fixture caveat.
fn write_sensitive_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// Result of a bounded read of an untrusted input document.
enum BoundedInput {
    /// The document fits the configured limit.
    Read(Vec<u8>),
    /// The document exceeds the configured limit.
    TooLarge,
}

/// Read an untrusted document with a hard byte bound.
///
/// `File::metadata` gives an allocation-free early refusal. The
/// `take(limit + 1)` read then closes the race where a file grows between
/// the metadata check and the read, so no more than `limit + 1` bytes are
/// ever retained. The caller owns the failure diagnostic, which keeps the
/// CLI's existing `configuration` class and message for HAR import.
fn read_bounded_input(path: &Path, limit: usize) -> std::io::Result<BoundedInput> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Ok(BoundedInput::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Ok(BoundedInput::TooLarge);
    }
    Ok(BoundedInput::Read(bytes))
}

fn har_import(args: &HarImportArgs) -> Result<(), (String, String)> {
    // Inputs first: read and parse the HAR into a staged writer before the
    // target is replaced, so a malformed document cannot destroy the old
    // fixture. The read is bounded before parsing: `std::fs::read` would
    // allocate the whole file before the library's 32 MiB check.
    let har_bytes = match read_bounded_input(&args.har, eggreplay_har::MAX_HAR_FILE_BYTES) {
        Ok(BoundedInput::Read(bytes)) => bytes,
        Ok(BoundedInput::TooLarge) => {
            return Err((
                "configuration".into(),
                "HAR document exceeds size limit".into(),
            ));
        }
        Err(error) => {
            return Err((
                "configuration".into(),
                format!("cannot read HAR file: {error}"),
            ));
        }
    };
    let (redaction, profile_id) = har_redaction_policy(args);
    if let Err(error) = check_replacement_target(&args.fixture, args.overwrite, "fixture") {
        emit(
            "har-import",
            args.output.output,
            false,
            Some("configuration"),
            json!({"fixture": args.fixture, "har": args.har}),
        );
        return Err(error);
    }
    let mut staging = StagingGuard::new(sibling_transaction_path(&args.fixture, "har-import"));
    let mut writer = eggreplay_store::SessionWriter::create(
        staging.path(),
        SessionMetadata {
            capture_mode: "har-import".into(),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let report = eggreplay_har::import_har_to_writer(
        &mut writer,
        &har_bytes,
        &redaction,
        &profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        StoreLimits::default().max_blob_bytes,
    )
    .map_err(|error| match error {
        eggreplay_har::HarError::Store(inner) => ("fixture".into(), inner.to_string()),
        eggreplay_har::HarError::Invalid(message) | eggreplay_har::HarError::Redaction(message) => {
            ("configuration".into(), message)
        }
        eggreplay_har::HarError::UnsupportedEntry { index, reason } => {
            ("configuration".into(), format!("entry {index}: {reason}"))
        }
    })?;
    let session = writer
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let flow_count = session.manifest().flow_count;
    let warnings = session_redaction_warnings(&session, &redaction, false, false, false);
    drop(session);
    // The loss report is an output of the completed staged fixture. Writing it
    // before the swap keeps a write failure from replacing the target.
    if let Some(loss_path) = &args.loss_report {
        let loss_document = json!({
            "command": "har-import",
            "har": args.har,
            "fixture": args.fixture,
            "entries": report.entries,
            "flows": report.flows,
            "losses": report.losses,
        });
        write_sensitive_file(
            loss_path,
            &serde_json::to_vec_pretty(&loss_document)
                .map_err(|error| ("runtime".into(), error.to_string()))?,
        )
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    }
    replace_fixture_transactionally(staging.path(), &args.fixture)
        .map_err(|error| ("runtime".into(), error))?;
    staging.disarm();
    emit_with_warnings(
        "har-import",
        args.output.output,
        true,
        None,
        json!({
            "fixture": args.fixture,
            "har": args.har,
            "flow_count": flow_count,
            "entries": report.entries,
            "loss_count": report.losses.len(),
            "loss_report": args.loss_report,
        }),
        warnings,
    );
    Ok(())
}

fn har_export(args: &HarExportArgs) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let (har, report) = eggreplay_har::export_session_to_har(&session)
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    if args.har.exists() && !args.overwrite {
        return Err((
            "configuration".into(),
            "HAR output exists; pass --overwrite to replace it".into(),
        ));
    }
    if let Some(parent) = args
        .har
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| ("runtime".into(), error.to_string()))?;
    }
    write_sensitive_file(
        &args.har,
        &serde_json::to_vec_pretty(&har).map_err(|error| ("runtime".into(), error.to_string()))?,
    )
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    if let Some(loss_path) = &args.loss_report {
        let loss_document = json!({
            "command": "har-export",
            "fixture": args.fixture,
            "har": args.har,
            "entries": report.entries,
            "losses": report.losses,
        });
        write_sensitive_file(
            loss_path,
            &serde_json::to_vec_pretty(&loss_document)
                .map_err(|error| ("runtime".into(), error.to_string()))?,
        )
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    }
    emit(
        "har-export",
        args.output.output,
        true,
        None,
        json!({
            "fixture": args.fixture,
            "har": args.har,
            "entries": report.entries,
            "loss_count": report.losses.len(),
            "loss_report": args.loss_report,
        }),
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate(args: &MigrateArgs) -> Result<(), (String, String)> {
    if args.in_place && args.to.is_some() {
        return Err((
            "configuration".into(),
            "--to and --in-place are mutually exclusive".into(),
        ));
    }
    if !args.in_place && args.to.is_none() {
        return Err((
            "configuration".into(),
            "migrate requires --to <fixture> or --in-place".into(),
        ));
    }
    let target_schema = args
        .target_schema
        .unwrap_or(eggreplay_core::SESSION_SCHEMA_VERSION);
    if !(eggreplay_core::SESSION_SCHEMA_V1..=eggreplay_core::SESSION_SCHEMA_VERSION)
        .contains(&target_schema)
    {
        return Err((
            "configuration".into(),
            format!("unsupported target session schema {target_schema}"),
        ));
    }
    // Validate the destination before the source is even read. A non-fixture
    // destination is refused here, so the transactional replace can never
    // interpret `--overwrite` as permission to delete an arbitrary path.
    let destination = match args.to.as_ref() {
        Some(destination) => {
            check_replacement_target(destination, args.overwrite, "destination")?;
            Some(destination)
        }
        None => None,
    };
    let source = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    if args.in_place {
        // Transactional in-place: stage to a sibling, validate (migration
        // opens the staged copy), then atomically replace the source. The
        // source is never mutated before the new fixture validates.
        let mut staging = StagingGuard::new(sibling_transaction_path(&args.fixture, "migrate"));
        let migrated = eggreplay_har::migrate_session(
            &source,
            staging.path(),
            target_schema,
            args.scrub_stream_trailers,
        )
        .map_err(|error| match error {
            eggreplay_har::MigrationError::Blocked(message) => ("fixture".into(), message),
            eggreplay_har::MigrationError::Store(inner) => ("fixture".into(), inner.to_string()),
            eggreplay_har::MigrationError::Runtime(message) => ("runtime".into(), message),
        })?;
        let flow_count = migrated.manifest().flow_count;
        let schema_version = migrated.manifest().metadata.schema_version;
        drop(migrated);
        drop(source);
        replace_fixture_transactionally(staging.path(), &args.fixture)
            .map_err(|error| ("runtime".into(), error))?;
        staging.disarm();
        emit(
            "migrate",
            args.output.output,
            true,
            None,
            json!({"fixture": args.fixture, "in_place": true, "target_schema": schema_version, "flow_count": flow_count}),
        );
        Ok(())
    } else {
        let destination = destination.expect("destination checked above");
        // Transactional --to: migrate into a sibling staging directory, then
        // publish to the destination (replacing transactionally when it
        // exists). The source is never mutated.
        let mut staging = StagingGuard::new(sibling_transaction_path(destination, "migrate"));
        let migrated = eggreplay_har::migrate_session(
            &source,
            staging.path(),
            target_schema,
            args.scrub_stream_trailers,
        )
        .map_err(|error| match error {
            eggreplay_har::MigrationError::Blocked(message) => ("fixture".into(), message),
            eggreplay_har::MigrationError::Store(inner) => ("fixture".into(), inner.to_string()),
            eggreplay_har::MigrationError::Runtime(message) => ("runtime".into(), message),
        })?;
        let flow_count = migrated.manifest().flow_count;
        let schema_version = migrated.manifest().metadata.schema_version;
        drop(migrated);
        if destination.exists() {
            replace_fixture_transactionally(staging.path(), destination)
                .map_err(|error| ("runtime".into(), error))?;
        } else {
            if let Some(parent) = destination
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .map_err(|error| ("runtime".into(), error.to_string()))?;
            }
            std::fs::rename(staging.path(), destination)
                .map_err(|error| ("runtime".into(), error.to_string()))?;
        }
        staging.disarm();
        emit(
            "migrate",
            args.output.output,
            true,
            None,
            json!({"fixture": args.fixture, "to": destination, "target_schema": schema_version, "flow_count": flow_count}),
        );
        Ok(())
    }
}

fn body(
    session: &Session,
    body: &eggreplay_core::BodyRef,
) -> Result<Vec<u8>, eggreplay_store::StoreError> {
    match body {
        eggreplay_core::BodyRef::Absent | eggreplay_core::BodyRef::Empty => Ok(Vec::new()),
        eggreplay_core::BodyRef::Blob(blob) => session.read_blob(blob),
    }
}
fn redact_url(value: &str) -> String {
    value.parse::<url::Url>().map_or_else(
        |_| "<invalid-url>".into(),
        |mut url| {
            if !url.username().is_empty() {
                let _ = url.set_username("<redacted>");
            }
            let _ = url.set_password(None);
            url.set_query(None);
            url.to_string()
        },
    )
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn junit_for_reports(
    command: &str,
    flow_ids: &[String],
    reports: &[eggreplay_core::RegressionReport],
) -> String {
    let mut out = format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\">",
        escape_xml(command),
        flow_ids.len(),
        reports.iter().filter(|report| !report.is_success()).count()
    );
    for (index, flow_id) in flow_ids.iter().enumerate() {
        let report = reports.get(index);
        let (failed, details) = match report {
            Some(report) if !report.is_success() => {
                let details = report
                    .findings
                    .iter()
                    .map(|finding| {
                        format!(
                            "{} {} baseline={} candidate={}",
                            escape_xml(&format!("{:?}", finding.kind)),
                            escape_xml(&finding.field),
                            escape_xml(&finding.baseline),
                            escape_xml(&finding.candidate)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                (true, details)
            }
            _ => (false, String::new()),
        };
        let _ = write!(
            out,
            "<testcase name=\"{}\" classname=\"{}\">",
            escape_xml(flow_id),
            escape_xml(command)
        );
        if failed {
            let _ = write!(out, "<failure message=\"mismatch\">{details}</failure>");
        }
        out.push_str("</testcase>");
    }
    out.push_str("</testsuite>");
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_reports(
    command: &str,
    output: OutputChoice,
    success: bool,
    failure: Option<&str>,
    target: &str,
    flow_ids: &[String],
    reports: &[eggreplay_core::RegressionReport],
    finding_count: usize,
    outbound_timeout: &serde_json::Value,
) {
    match output {
        OutputChoice::Json => {
            let envelope = Envelope {
                command: command.into(),
                schema_version: 1,
                success,
                failure_class: failure.map(str::to_owned),
                warnings: Vec::new(),
                payload: json!({"target": if target.is_empty() { serde_json::Value::Null } else { json!(redact_url(target)) }, "reports": reports, "finding_count": finding_count, "outbound_timeout": outbound_timeout}),
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"success\":false}".into())
            );
        }
        OutputChoice::Junit => {
            // JUnit is a projection of the report authority, not a re-evaluation.
            println!("{}", junit_for_reports(command, flow_ids, reports));
        }
        OutputChoice::Human => {
            if success {
                println!(
                    "{command}: {finding_count} findings across {} flows",
                    flow_ids.len()
                );
            } else {
                println!(
                    "{command}: {} findings across {} flows (failure_class={})",
                    finding_count,
                    flow_ids.len(),
                    failure.unwrap_or("regression")
                );
            }
        }
    }
}

fn emit(
    command: &str,
    output: OutputChoice,
    success: bool,
    failure: Option<&str>,
    payload: serde_json::Value,
) {
    emit_with_warnings(command, output, success, failure, payload, Vec::new());
}

fn emit_with_warnings(
    command: &str,
    output: OutputChoice,
    success: bool,
    failure: Option<&str>,
    payload: serde_json::Value,
    warnings: Vec<String>,
) {
    match output {
        OutputChoice::Json => {
            let envelope = Envelope {
                command: command.into(),
                schema_version: 1,
                success,
                failure_class: failure.map(str::to_owned),
                warnings,
                payload,
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"success\":false}".into())
            );
        }
        OutputChoice::Junit => {
            // Single-assertion commands project as one testcase.
            let failures = usize::from(!success);
            let detail = failure.unwrap_or("");
            if failures == 0 {
                println!(
                    "<testsuite name=\"{}\" tests=\"1\" failures=\"0\" errors=\"0\"><testcase name=\"{}\" classname=\"{}\"/></testsuite>",
                    escape_xml(command),
                    escape_xml(command),
                    escape_xml(command)
                );
            } else {
                println!(
                    "<testsuite name=\"{}\" tests=\"1\" failures=\"1\" errors=\"0\"><testcase name=\"{}\" classname=\"{}\"><failure message=\"{}\"/></testcase></testsuite>",
                    escape_xml(command),
                    escape_xml(command),
                    escape_xml(command),
                    escape_xml(detail)
                );
            }
        }
        OutputChoice::Human => {
            // Warnings are operational diagnostics: stderr in human mode, the
            // envelope's `warnings` array in JSON mode. They never affect
            // success or the exit code.
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            // Human rendering is terminal text, never machine JSON.
            if success {
                println!("{command}: ok");
            } else {
                println!("{command}: failed ({})", failure.unwrap_or("error"));
            }
        }
    }
}

/// Listener exposure gate (M021D).
///
/// These are unit tests for [`check_listener_bind`], which is the *only*
/// place the loopback policy is decided. They are compiled in every build —
/// including one without the `intercept` feature — because the gate itself
/// must be present in every build.
#[cfg(test)]
mod listener_gate_tests {
    use super::check_listener_bind;

    fn bind(address: &str) -> std::net::SocketAddr {
        address.parse().expect("socket address literal")
    }

    #[test]
    fn loopback_binds_always_pass_without_the_opt_in() {
        for address in ["127.0.0.1:0", "127.0.0.1:8080", "[::1]:0", "[::1]:8080"] {
            assert!(
                check_listener_bind(bind(address), false).is_ok(),
                "loopback {address} must never need the opt-in"
            );
        }
    }

    #[test]
    fn wildcard_and_lan_binds_require_the_opt_in() {
        // `0.0.0.0` and `::` are the two "every interface" addresses; the LAN
        // address is the ordinary case of a routable bind. All three are
        // unauthenticated listeners once reachable.
        for address in ["0.0.0.0:0", "[::]:0", "192.168.1.20:8080"] {
            let (class, message) =
                check_listener_bind(bind(address), false).expect_err("must refuse");
            assert_eq!(
                class, "configuration",
                "the refusal class is the exit-code key"
            );
            assert!(
                message.contains(&bind(address).to_string()),
                "the refusal must name the bind: {message}"
            );
            assert!(
                message.contains("unauthenticated"),
                "the refusal must state the listener is unauthenticated: {message}"
            );
            assert!(
                message.contains("TLS does not add authentication"),
                "the refusal must state that TLS is not authentication: {message}"
            );
            assert!(
                message.contains("--allow-non-loopback"),
                "the refusal must name the opt-in flag: {message}"
            );
            assert!(
                check_listener_bind(bind(address), true).is_ok(),
                "the explicit opt-in must permit {address}"
            );
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use eggreplay_core::{BodyRef, Flow, FlowOutcome, HttpRequest, HttpResponse, Provenance};

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "eggreplay-cli-transaction-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn bounded_input_reads_exactly_the_limit_and_rejects_one_byte_over() {
        let file = temp_path("bounded-input");
        std::fs::write(&file, b"12345678").unwrap();
        match read_bounded_input(&file, 8).unwrap() {
            BoundedInput::Read(bytes) => assert_eq!(bytes, b"12345678"),
            BoundedInput::TooLarge => panic!("exactly the limit must be accepted"),
        }
        std::fs::write(&file, b"123456789").unwrap();
        assert!(
            matches!(
                read_bounded_input(&file, 8).unwrap(),
                BoundedInput::TooLarge
            ),
            "one byte over the limit must be refused"
        );
        std::fs::remove_file(&file).unwrap();
    }

    fn minimal_flow(index: usize) -> Flow {
        Flow {
            schema_version: eggreplay_core::SCHEMA_VERSION,
            id: format!("flow-{index}"),
            started_at_ms: 1,
            completed_at_ms: Some(2),
            request: HttpRequest {
                method: "GET".into(),
                scheme: "http".into(),
                authority: "example.test".into(),
                path: format!("/item-{index}"),
                query: vec![],
                headers: vec![],
                body: BodyRef::Empty,
                trailers: vec![],
            },
            outcome: FlowOutcome::Response(HttpResponse {
                status: 200,
                headers: vec![],
                body: BodyRef::Empty,
                trailers: vec![],
            }),
            physical_route: None,
            provenance: Provenance {
                mode: "test".into(),
                observer: "test".into(),
            },
            annotations: vec![],
            redactions: vec![],
        }
    }

    /// Publish a valid fixture at `path`, which must not exist yet.
    fn publish_fixture(path: &Path, schema_version: u16, flows: usize) {
        let metadata = SessionMetadata {
            schema_version,
            ..SessionMetadata::default()
        };
        let mut writer =
            eggreplay_store::SessionWriter::create(path, metadata, StoreLimits::default()).unwrap();
        for index in 0..flows {
            writer.append_flow(&minimal_flow(index)).unwrap();
        }
        drop(writer.finish().unwrap());
    }

    fn open_flow_count(path: &Path) -> usize {
        Session::open(path, StoreLimits::default())
            .unwrap()
            .manifest()
            .flow_count
    }

    #[test]
    fn replacement_publishes_complete_stage_and_rolls_back_failed_publish() {
        let target = temp_path("target");
        let staged = temp_path("staged");
        publish_fixture(&target, 2, 0);
        publish_fixture(&staged, 2, 2);
        replace_fixture_transactionally(&staged, &target).unwrap();
        assert_eq!(open_flow_count(&target), 2);
        assert!(!staged.exists());

        let missing_stage = temp_path("missing-stage");
        let result = replace_fixture_transactionally(&missing_stage, &target);
        assert!(result.is_err());
        assert_eq!(
            open_flow_count(&target),
            2,
            "failed publish must preserve the published fixture"
        );
        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn replacement_refuses_a_non_fixture_target() {
        let target = temp_path("refuse-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "keep").unwrap();
        let staged = temp_path("refuse-staged");
        publish_fixture(&staged, 2, 1);
        let error = replace_fixture_transactionally(&staged, &target).unwrap_err();
        assert!(
            error.contains("cannot replace"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep"
        );
        assert!(staged.exists(), "refusal must not consume the staged tree");
        std::fs::remove_dir_all(target).unwrap();
        std::fs::remove_dir_all(staged).unwrap();
    }

    #[test]
    fn interrupted_replacement_restores_the_last_valid_backup() {
        let target = temp_path("recover-target");
        let staging = temp_path("recover-stage");
        publish_fixture(&staging, 2, 0);
        let backup = sibling_transaction_path(&target, "backup");
        std::fs::rename(&staging, &backup).unwrap();
        recover_fixture_transactionally(&target).unwrap();
        Session::open(&target, StoreLimits::default()).unwrap();
        assert!(!backup.exists());
        std::fs::remove_dir_all(target).unwrap();
    }

    // -- replacement-target authority -------------------------------------

    fn classify(path: &Path) -> Result<ReplacementTarget, String> {
        classify_replacement_target(path)
    }

    #[test]
    fn absent_path_is_safe_to_create() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("new.eggr");
        assert_eq!(classify(&absent).unwrap(), ReplacementTarget::Absent);
        assert!(!absent.exists(), "classification must not create anything");
    }

    #[test]
    fn empty_directory_is_replaceable() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.eggr");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(classify(&empty).unwrap(), ReplacementTarget::EmptyDir);
    }

    #[test]
    fn valid_schema_one_and_two_fixtures_are_replaceable() {
        let dir = tempfile::tempdir().unwrap();
        for schema in [1, 2] {
            let fixture = dir.path().join(format!("schema-{schema}.eggr"));
            publish_fixture(&fixture, schema, 1);
            assert_eq!(
                classify(&fixture).unwrap(),
                ReplacementTarget::ValidFixture,
                "schema {schema} fixture must be replaceable"
            );
        }
    }

    #[test]
    fn fixture_with_an_extra_unrelated_file_is_still_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("extended.eggr");
        publish_fixture(&fixture, 2, 1);
        std::fs::write(fixture.join("notes.txt"), "extension file").unwrap();
        assert_eq!(classify(&fixture).unwrap(), ReplacementTarget::ValidFixture);
    }

    #[test]
    fn non_empty_unrelated_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let unrelated = dir.path().join("unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("keep.txt"), "keep").unwrap();
        let error = classify(&unrelated).unwrap_err();
        assert!(
            error.contains("valid EggReplay fixture"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(unrelated.join("keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn regular_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.eggr");
        std::fs::write(&file, "not a directory").unwrap();
        let error = classify(&file).unwrap_err();
        assert!(error.contains("not a directory"), "unexpected: {error}");
    }

    #[test]
    fn incomplete_fixture_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let incomplete = dir.path().join("incomplete.eggr");
        std::fs::create_dir(&incomplete).unwrap();
        std::fs::write(incomplete.join("flows.jsonl"), b"").unwrap();
        let error = classify(&incomplete).unwrap_err();
        assert!(
            error.contains("valid EggReplay fixture"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn future_schema_fixture_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("future.eggr");
        publish_fixture(&fixture, 2, 0);
        let manifest_path = fixture.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["schema_version"] = serde_json::json!(99);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let error = classify(&fixture).unwrap_err();
        assert!(
            error.contains("unsupported fixture schema 99"),
            "unexpected error: {error}"
        );
        assert!(fixture.exists(), "refusal must not delete the fixture");
    }

    #[test]
    fn lexical_dot_and_root_paths_are_refused() {
        for path in [".", "..", "fixture.eggr/..", "parent/../fixture.eggr"] {
            let error = classify(Path::new(path)).unwrap_err();
            assert!(
                error.contains("final component") || error.contains("contains a"),
                "path {path:?} must be refused lexically, got {error}"
            );
        }
        #[cfg(unix)]
        assert!(classify(Path::new("/")).is_err());
        #[cfg(windows)]
        assert!(classify(Path::new(r"C:\")).is_err());
    }

    #[test]
    fn current_dir_prefix_is_an_ordinary_spelling() {
        // `./name` resolves to the same directory; only `..` can escape the
        // operator's intended parent.
        assert_eq!(
            classify(Path::new("./eggreplay-m021b-absent.eggr")).unwrap(),
            ReplacementTarget::Absent
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_refused_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.eggr");
        publish_fixture(&real, 2, 0);
        let link = dir.path().join("link.eggr");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = classify(&link).unwrap_err();
        assert!(error.contains("symbolic link"), "unexpected error: {error}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must not be replaced or removed"
        );
        Session::open(&real, StoreLimits::default()).unwrap();
    }

    #[test]
    fn missing_overwrite_flag_is_a_configuration_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("existing.eggr");
        publish_fixture(&fixture, 2, 0);
        let (class, message) = check_replacement_target(&fixture, false, "fixture").unwrap_err();
        assert_eq!(class, "configuration");
        assert!(message.contains("--overwrite"), "unexpected: {message}");
        assert_eq!(
            check_replacement_target(&fixture, true, "fixture").unwrap(),
            ReplacementTarget::ValidFixture
        );
    }

    #[test]
    fn warning_scanner_is_value_free_and_deduplicates() {
        let mut first = minimal_flow(0);
        first.request.headers.push(eggreplay_core::HeaderEntry {
            name: "X-Api-Key".into(),
            value: "WARNING-SENTINEL-1".into(),
        });
        first.request.headers.push(eggreplay_core::HeaderEntry {
            name: "X-Csrf-Token".into(),
            value: "WARNING-SENTINEL-2".into(),
        });
        first.request.query.push(eggreplay_core::QueryPair {
            key: "access_token".into(),
            value: "WARNING-SENTINEL-3".into(),
        });
        // A second flow repeats the same header; warnings deduplicate.
        let mut second = minimal_flow(1);
        second.request.headers.push(eggreplay_core::HeaderEntry {
            name: "x-api-key".into(),
            value: "WARNING-SENTINEL-4".into(),
        });
        let warnings = redaction_warnings(
            &[first, second],
            &eggreplay_core::RedactionConfig::default(),
            false,
            false,
            false,
        );
        assert_eq!(
            warnings.len(),
            3,
            "one warning per distinct field: {warnings:?}"
        );
        let joined = warnings.join("\n");
        for name in ["x-api-key", "x-csrf-token", "access_token"] {
            assert!(joined.contains(name), "{joined}");
        }
        for sentinel in [
            "WARNING-SENTINEL-1",
            "WARNING-SENTINEL-2",
            "WARNING-SENTINEL-3",
        ] {
            assert!(
                !joined.contains(sentinel),
                "warnings must be value-free: {joined}"
            );
        }
    }

    #[test]
    fn websocket_acquisition_without_payload_redaction_warns_once() {
        let config = eggreplay_core::RedactionConfig::default_secure();
        let warnings = redaction_warnings(&[], &config, true, false, false);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("WebSocket"), "{warnings:?}");
        assert!(
            redaction_warnings(&[], &config, true, true, false).is_empty(),
            "text redaction is sufficient"
        );
        assert!(
            redaction_warnings(&[], &config, true, false, true).is_empty(),
            "binary redaction is sufficient"
        );
        let mut with_json = config.clone();
        with_json.json_paths.insert("/secret".into());
        assert!(
            redaction_warnings(&[], &with_json, true, false, false).is_empty(),
            "a configured JSON pointer is sufficient"
        );
        assert!(
            redaction_warnings(&[], &config, false, false, false).is_empty(),
            "no warning when acquisition is disabled"
        );
    }
}
