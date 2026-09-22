use clap::{Args, Parser, Subcommand, ValueEnum};
use eggreplay_core::{
    FlowOutcome, Matcher, PhysicalRoute, ReportScheduler, SessionMetadata, compare_flows,
};
use eggreplay_http::{EggressDialer, ReplayFixture, execute_candidate};
use eggreplay_store::{RecordingSession, Session, StoreLimits};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

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
}

#[derive(Debug, Clone, Args)]
struct OutputArgs {
    #[arg(long, value_enum, default_value_t = OutputChoice::Human)]
    output: OutputChoice,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputChoice {
    Human,
    Json,
    Junit,
}
#[derive(Debug, Args)]
struct RecordArgs {
    #[arg(long)]
    listen: std::net::SocketAddr,
    #[arg(long)]
    upstream: String,
    #[arg(long)]
    fixture: PathBuf,
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
    /// Outbound route: `direct` or a pproxy URI (`socks5://...`, `http://...`,
    /// two-hop `socks5://...__http://...`). Listener-free Eggress routing only.
    #[arg(long, default_value = "direct")]
    route: String,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: std::net::SocketAddr,
    /// Record behavior; defaults to sealed offline replay.
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
struct ReplayArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    target: String,
    /// Outbound route: `direct` or a pproxy URI. See `record --route`.
    #[arg(long, default_value = "direct")]
    route: String,
    /// Candidate request scheduler; timeline requires stream event metadata.
    #[arg(long, value_enum, default_value_t = SchedulerChoice::Sequential)]
    scheduler: SchedulerChoice,
    /// Maximum concurrent candidate requests for timeline scheduling.
    #[arg(long, default_value_t = 8)]
    max_concurrency: usize,
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
    /// Candidate request scheduler; timeline requires stream event metadata.
    #[arg(long, value_enum, default_value_t = SchedulerChoice::Sequential)]
    scheduler: SchedulerChoice,
    /// Maximum concurrent candidate requests for timeline scheduling.
    #[arg(long, default_value_t = 8)]
    max_concurrency: usize,
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
    output: OutputArgs,
}
#[derive(Debug, Args)]
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
            regression(
                args.fixture,
                args.target,
                args.route,
                "replay",
                args.output,
                false,
                SchedulerOptions {
                    choice: args.scheduler,
                    max_concurrency: args.max_concurrency,
                },
            )
            .await
        }
        Command::Test(args) => {
            regression(
                args.fixture,
                args.target,
                args.route,
                "test",
                args.output,
                true,
                SchedulerOptions {
                    choice: args.scheduler,
                    max_concurrency: args.max_concurrency,
                },
            )
            .await
        }
        Command::Diff(args) => diff(args).await,
        Command::Inspect(args) => inspect(args).await,
        Command::Validate(args) => validate(args).await,
    }
}

fn build_client(route: &str) -> Result<(eggfetch_core::Client, PhysicalRoute), (String, String)> {
    match eggreplay_http::parse_route(route) {
        Ok(None) => {
            let client = eggfetch_core::Client::builder()
                .retry_canceled_requests(false)
                .build();
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
            let client = eggfetch_core::Client::builder()
                .retry_canceled_requests(false)
                .dialer(dialer)
                .build();
            Ok((client, physical))
        }
        Err(message) => Err(("configuration".into(), message)),
    }
}

async fn record(args: RecordArgs) -> Result<(), (String, String)> {
    if args.fixture.exists() && !args.overwrite {
        let message = "fixture exists; pass --overwrite to replace it".to_string();
        emit(
            "record",
            args.output.output,
            false,
            Some("configuration"),
            json!({"fixture": args.fixture}),
        );
        return Err(("configuration".into(), message));
    }
    if args.fixture.exists() {
        std::fs::remove_dir_all(&args.fixture)
            .map_err(|error| ("runtime".into(), error.to_string()))?;
    }
    let (redaction, profile_id) = effective_redaction_policy(&args);
    let (client, physical_route) = build_client(&args.route)?;
    let session = RecordingSession::create(
        &args.fixture,
        SessionMetadata {
            capture_mode: "gateway".into(),
            target: Some(redact_url(&args.upstream)),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let upstream = args
        .upstream
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
    let server = eggreplay_http::recording::start_recording_gateway(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!("recording on {}", server.local_addr());
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server.wait().await;
    session.shutdown();
    for _ in 0..100 {
        if session.active_blobs() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    session
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    emit(
        "record",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "status": "finalized"}),
    );
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), (String, String)> {
    recover_fixture_transactionally(&args.fixture).map_err(|error| ("fixture".into(), error))?;
    let policy = eggreplay_core::RecordMode::from(args.record_mode)
        .resolve(args.fixture.exists(), args.upstream.is_some())
        .map_err(|message| ("configuration".into(), message))?;
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
        .start(args.listen, StoreLimits::default().max_blob_bytes)
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!(
        "serving {} on {}",
        args.fixture.display(),
        server.local_addr()
    );
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server.wait().await;
    emit(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "scenario": args.scenario, "record_mode": "sealed", "upstream_enabled": false, "matcher_profile": args.matcher_profile.as_str(), "timing_mode": args.timing_mode, "status": "stopped"}),
    );
    Ok(())
}

async fn record_once_from_serve(args: ServeArgs) -> Result<(), (String, String)> {
    let upstream = args
        .upstream
        .as_deref()
        .ok_or_else(|| ("configuration".into(), "--upstream is required".into()))?
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
    let (client, physical_route) = build_client(&args.route)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
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
    let server = eggreplay_http::recording::start_recording_gateway(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!("recording once on {}", server.local_addr());
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server.wait().await;
    session.shutdown();
    session
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    emit(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "once", "upstream_enabled": true, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": args.redaction_profile, "status": "finalized"}),
    );
    Ok(())
}

async fn serve_append_new(args: ServeArgs) -> Result<(), (String, String)> {
    let source = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let (client, physical_route) = build_client(&args.route)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
    if profile_id != source.manifest().metadata.redaction_profile {
        return Err((
            "configuration".into(),
            "append-new redaction profile must match the source fixture; select its --redaction-profile explicitly".into(),
        ));
    }
    let upstream = args
        .upstream
        .as_deref()
        .ok_or_else(|| ("configuration".into(), "--upstream is required".into()))?
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
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
    let server = fixture
        .start_append_new(
            args.listen,
            StoreLimits::default().max_blob_bytes,
            upstream,
            client,
            recording.clone(),
            redaction,
            profile_id.clone(),
            eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            physical_route,
        )
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!("append-new replay on {}", server.local_addr());
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server.wait().await;
    recording.shutdown();
    let additional = recording
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let added_flows = additional.manifest().flow_count;
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
    emit(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "append-new", "upstream_enabled": true, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": profile_id, "new_flows": added_flows, "status": "finalized"}),
    );
    Ok(())
}

async fn serve_re_record(args: ServeArgs) -> Result<(), (String, String)> {
    if args.scenario.is_some() {
        return Err((
            "configuration".into(),
            "re-record cannot select an authored offline scenario".into(),
        ));
    }
    let upstream = args
        .upstream
        .as_deref()
        .ok_or_else(|| ("configuration".into(), "--upstream is required".into()))?
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
    let (client, physical_route) = build_client(&args.route)?;
    let (redaction, profile_id) = effective_serve_redaction_policy(&args);
    let temporary = sibling_transaction_path(&args.fixture, "rerecord");
    let session = RecordingSession::create(
        &temporary,
        SessionMetadata {
            capture_mode: "re-record".into(),
            target: Some(redact_url(&args.upstream.clone().unwrap_or_default())),
            redaction_profile: profile_id.clone(),
            ..SessionMetadata::default()
        },
        StoreLimits::default(),
    )
    .map_err(|error| ("fixture".into(), error.to_string()))?;
    let server = eggreplay_http::recording::start_recording_gateway(
        args.listen,
        upstream,
        client,
        session.clone(),
        StoreLimits::default().max_blob_bytes,
        redaction,
        profile_id,
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        physical_route,
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!("re-record gateway on {}", server.local_addr());
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    server.shutdown();
    server.wait().await;
    session.shutdown();
    let recorded = session
        .finish()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let recorded_flows = recorded.manifest().flow_count;
    drop(recorded);
    replace_fixture_transactionally(&temporary, &args.fixture)
        .map_err(|error| ("runtime".into(), error))?;
    emit(
        "serve",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "record_mode": "re-record", "upstream_enabled": true, "matcher_profile": args.matcher_profile.as_str(), "redaction_profile": args.redaction_profile, "new_flows": recorded_flows, "status": "finalized"}),
    );
    Ok(())
}

fn sibling_transaction_path(fixture: &std::path::Path, label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = fixture
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("fixture.eggr");
    fixture.with_file_name(format!(".{name}.{label}-{}-{nonce}", std::process::id()))
}

fn replace_fixture_transactionally(
    staged: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    if !target.exists() {
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

async fn regression(
    fixture: PathBuf,
    target: String,
    route: String,
    command: &str,
    output: OutputArgs,
    enforce: bool,
    scheduler: SchedulerOptions,
) -> Result<(), (String, String)> {
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
        );
        ("configuration".into(), error.to_string())
    })?;
    let (client, physical_route) = build_client(&route).map_err(|(class, message)| {
        emit_reports(
            command,
            output.output,
            false,
            Some(&class),
            &target,
            &[],
            &[],
            0,
        );
        (class, message)
    })?;
    let flows = session
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let flow_ids = flows.iter().map(|flow| flow.id.clone()).collect::<Vec<_>>();
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
        );
        Ok(())
    }
}

async fn compare_candidate_flow(
    session: &Session,
    client: &eggfetch_core::Client,
    target_uri: &http::Uri,
    physical_route: &PhysicalRoute,
    baseline: eggreplay_core::Flow,
    scheduler: ReportScheduler,
) -> Result<eggreplay_core::RegressionReport, String> {
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
    Ok(compare_flows(
        &baseline,
        &candidate.flow,
        &baseline_response,
        &candidate.response_body,
        scheduler,
    ))
}

async fn diff(args: DiffArgs) -> Result<(), (String, String)> {
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
    let reports = left
        .iter()
        .zip(right.iter())
        .map(|(left, right)| {
            let left_body = match &left.outcome {
                FlowOutcome::Response(response) => {
                    body(&baseline, &response.body).unwrap_or_default()
                }
                FlowOutcome::Error(_) => Vec::new(),
            };
            let right_body = match &right.outcome {
                FlowOutcome::Response(response) => {
                    body(&candidate, &response.body).unwrap_or_default()
                }
                FlowOutcome::Error(_) => Vec::new(),
            };
            compare_flows(
                left,
                right,
                &left_body,
                &right_body,
                ReportScheduler::Sequential,
            )
        })
        .collect::<Vec<_>>();
    let success = left.len() == right.len() && reports.iter().all(|report| report.is_success());
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
    );
    if !success {
        return Err(("diff".into(), "fixtures differ".into()));
    }
    Ok(())
}

async fn inspect(args: InspectArgs) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let mut flows = Vec::new();
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
        }),
    );
    Ok(())
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
            use std::io::Read;
            let mut buf = vec![0u8; (total.min(max_bytes)) as usize];
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
                    let combined =
                        ((triple[0] as u32) << 16) | ((triple[1] as u32) << 8) | (triple[2] as u32);
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

async fn validate(args: ValidateArgs) -> Result<(), (String, String)> {
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    emit(
        "validate",
        args.output.output,
        true,
        None,
        json!({"fixture": args.fixture, "flow_count": session.manifest().flow_count, "schema_version": session.manifest().metadata.schema_version}),
    );
    Ok(())
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
    value
        .parse::<url::Url>()
        .map(|mut url| {
            if !url.username().is_empty() {
                let _ = url.set_username("<redacted>");
            }
            let _ = url.set_password(None);
            url.set_query(None);
            url.to_string()
        })
        .unwrap_or_else(|_| "<invalid-url>".into())
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
        out.push_str(&format!(
            "<testcase name=\"{}\" classname=\"{}\">",
            escape_xml(flow_id),
            escape_xml(command)
        ));
        if failed {
            out.push_str(&format!(
                "<failure message=\"mismatch\">{}</failure>",
                details
            ));
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
) {
    match output {
        OutputChoice::Json => {
            let envelope = Envelope {
                command: command.into(),
                schema_version: 1,
                success,
                failure_class: failure.map(str::to_owned),
                warnings: Vec::new(),
                payload: json!({"target": if target.is_empty() { serde_json::Value::Null } else { json!(redact_url(target)) }, "reports": reports, "finding_count": finding_count}),
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
    match output {
        OutputChoice::Json => {
            let envelope = Envelope {
                command: command.into(),
                schema_version: 1,
                success,
                failure_class: failure.map(str::to_owned),
                warnings: Vec::new(),
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
            // Human rendering is terminal text, never machine JSON.
            if success {
                println!("{command}: ok");
            } else {
                println!("{command}: failed ({})", failure.unwrap_or("error"));
            }
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

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
    fn replacement_publishes_complete_stage_and_rolls_back_failed_publish() {
        let target = temp_path("target");
        let staged = temp_path("staged");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(target.join("value"), "old").unwrap();
        std::fs::write(staged.join("value"), "new").unwrap();
        replace_fixture_transactionally(&staged, &target).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("value")).unwrap(),
            "new"
        );
        assert!(!staged.exists());

        let missing_stage = temp_path("missing-stage");
        let result = replace_fixture_transactionally(&missing_stage, &target);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("value")).unwrap(),
            "new"
        );
        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn interrupted_replacement_restores_the_last_valid_backup() {
        let target = temp_path("recover-target");
        let staging = temp_path("recover-stage");
        let writer =
            RecordingSession::create(&staging, SessionMetadata::default(), StoreLimits::default())
                .unwrap();
        writer.shutdown();
        writer.finish().unwrap();
        let backup = sibling_transaction_path(&target, "backup");
        std::fs::rename(&staging, &backup).unwrap();
        recover_fixture_transactionally(&target).unwrap();
        Session::open(&target, StoreLimits::default()).unwrap();
        assert!(!backup.exists());
        std::fs::remove_dir_all(target).unwrap();
    }
}
