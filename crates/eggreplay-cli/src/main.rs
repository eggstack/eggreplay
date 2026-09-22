use clap::{Args, Parser, Subcommand, ValueEnum};
use eggreplay_core::{FlowOutcome, ReportScheduler, SessionMetadata, compare_flows};
use eggreplay_http::{ReplayFixture, execute_candidate};
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
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: std::net::SocketAddr,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct ReplayArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    target: String,
    #[command(flatten)]
    output: OutputArgs,
}
#[derive(Debug, Args)]
struct TestArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    target: String,
    #[command(flatten)]
    output: OutputArgs,
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

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err((class, message)) => {
            eprintln!("{class}: {message}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<(), (String, String)> {
    match cli.command {
        Command::Record(args) => record(args).await,
        Command::Serve(args) => serve(args).await,
        Command::Replay(args) => {
            regression(args.fixture, args.target, "replay", args.output, false).await
        }
        Command::Test(args) => {
            regression(args.fixture, args.target, "test", args.output, true).await
        }
        Command::Diff(args) => diff(args).await,
        Command::Inspect(args) => inspect(args).await,
        Command::Validate(args) => validate(args).await,
    }
}

async fn record(args: RecordArgs) -> Result<(), (String, String)> {
    if args.fixture.exists() && !args.overwrite {
        return Err((
            "policy".into(),
            "fixture exists; pass --overwrite to replace it".into(),
        ));
    }
    if args.fixture.exists() {
        std::fs::remove_dir_all(&args.fixture)
            .map_err(|error| ("filesystem".into(), error.to_string()))?;
    }
    let (redaction, profile_id) = effective_redaction_policy(&args);
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
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .build();
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
    )
    .await
    .map_err(|error| ("runtime".into(), error.to_string()))?;
    eprintln!("recording on {}", server.local_addr());
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
    // Documented shutdown policy: stop admission, drain active gateway
    // tasks via server wait, then finalize only when no transaction can
    // still append.
    server.shutdown();
    server.wait().await;
    session.shutdown();
    // Spin briefly for any just-completed tasks to release their sinks;
    // finish fails closed if actives remain rather than racing.
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
    let session = Session::open(&args.fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let fixture =
        ReplayFixture::load(&session).map_err(|error| ("fixture".into(), error.to_string()))?;
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
        json!({"fixture": args.fixture, "status": "stopped"}),
    );
    Ok(())
}

async fn regression(
    fixture: PathBuf,
    target: String,
    command: &str,
    output: OutputArgs,
    enforce: bool,
) -> Result<(), (String, String)> {
    let session = Session::open(&fixture, StoreLimits::default())
        .map_err(|error| ("fixture".into(), error.to_string()))?;
    let target: http::Uri = target
        .parse()
        .map_err(|error: http::uri::InvalidUri| ("configuration".into(), error.to_string()))?;
    let client = eggfetch_core::Client::builder()
        .retry_canceled_requests(false)
        .build();
    let mut reports = Vec::new();
    for item in session
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
    {
        let baseline = item.map_err(|error| ("fixture".into(), error.to_string()))?;
        let request_body = body(&session, &baseline.request.body)
            .map_err(|error| ("fixture".into(), error.to_string()))?;
        let candidate = execute_candidate(
            &client,
            &baseline.request,
            &request_body,
            &target,
            StoreLimits::default().max_blob_bytes,
        )
        .await
        .map_err(|error| ("runtime".into(), error.to_string()))?;
        let baseline_response = match &baseline.outcome {
            FlowOutcome::Response(response) => body(&session, &response.body)
                .map_err(|error| ("fixture".into(), error.to_string()))?,
            FlowOutcome::Error(_) => Vec::new(),
        };
        reports.push(compare_flows(
            &baseline,
            &candidate.flow,
            &baseline_response,
            &candidate.response_body,
            ReportScheduler::Sequential,
        ));
    }
    let findings = reports
        .iter()
        .flat_map(|report| report.findings.clone())
        .collect::<Vec<_>>();
    let success = findings.is_empty();
    emit(
        command,
        output.output,
        success,
        (!success).then_some("regression"),
        json!({"target": redact_url(&target.to_string()), "reports": reports, "finding_count": findings.len()}),
    );
    if enforce && !success {
        return Err((
            "regression".into(),
            "candidate differs from baseline".into(),
        ));
    }
    Ok(())
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
    emit(
        "diff",
        args.output.output,
        success,
        (!success).then_some("diff"),
        json!({"reports": reports, "baseline_flows": left.len(), "candidate_flows": right.len()}),
    );
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
        // Expose policy identifier + markers without secret values. Markers
        // contain only field paths and profile IDs, never redacted values.
        flows.push(json!({
            "id": flow.id,
            "method": flow.request.method,
            "path": flow.request.path,
            "outcome": match flow.outcome { FlowOutcome::Response(response) => json!({"status": response.status}), FlowOutcome::Error(error) => json!({"error": format!("{:?}", error.category)}) },
            "redactions": flow.redactions,
            "body_dump": if args.bodies { json!("explicit body dump is bounded by CLI policy") } else { json!(null) }
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

fn effective_redaction_policy(args: &RecordArgs) -> (eggreplay_core::RedactionConfig, String) {
    use std::collections::BTreeSet;
    if args.unsafe_replace {
        let config = eggreplay_core::RedactionConfig {
            headers: args
                .redact_headers
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            query_keys: args.redact_queries.iter().cloned().collect::<BTreeSet<_>>(),
            json_paths: args
                .redact_json_paths
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
        };
        (config, args.redaction_profile.clone())
    } else {
        let mut config = eggreplay_core::RedactionConfig::default_secure();
        config.headers.extend(
            args.redact_headers
                .iter()
                .map(|name| name.to_ascii_lowercase()),
        );
        config
            .query_keys
            .extend(args.redact_queries.iter().cloned());
        config
            .json_paths
            .extend(args.redact_json_paths.iter().cloned());
        (config, args.redaction_profile.clone())
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
fn emit(
    command: &str,
    output: OutputChoice,
    success: bool,
    failure: Option<&str>,
    payload: serde_json::Value,
) {
    let envelope = Envelope {
        command: command.into(),
        schema_version: 1,
        success,
        failure_class: failure.map(str::to_owned),
        warnings: Vec::new(),
        payload,
    };
    match output {
        OutputChoice::Json => println!(
            "{}",
            serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"success\":false}".into())
        ),
        OutputChoice::Junit => println!(
            "<testsuite name=\"{command}\" tests=\"1\" failures=\"{}\"></testsuite>",
            usize::from(!success)
        ),
        OutputChoice::Human => println!(
            "{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        ),
    }
}
