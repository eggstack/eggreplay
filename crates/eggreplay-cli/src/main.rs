use clap::{Args, Parser, Subcommand, ValueEnum};
use eggreplay_core::{FlowOutcome, PhysicalRoute, ReportScheduler, SessionMetadata, compare_flows};
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
    #[command(flatten)]
    output: OutputArgs,
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
    /// Show bounded base64 for non-UTF8 bodies (explicit opt-in only).
    #[arg(long, default_value_t = false)]
    bodies_base64: bool,
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
    route: String,
    command: &str,
    output: OutputArgs,
    enforce: bool,
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
    let mut reports = Vec::new();
    let mut flow_ids = Vec::new();
    for item in session
        .iter_flows()
        .map_err(|error| ("fixture".into(), error.to_string()))?
    {
        let baseline = item.map_err(|error| ("fixture".into(), error.to_string()))?;
        flow_ids.push(baseline.id.clone());
        let request_body = body(&session, &baseline.request.body)
            .map_err(|error| ("fixture".into(), error.to_string()))?;
        let candidate = execute_candidate(
            &client,
            &baseline.request,
            &request_body,
            &target_uri,
            StoreLimits::default().max_blob_bytes,
            Some(physical_route.clone()),
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
        flows.push(json!({
            "id": flow.id,
            "method": flow.request.method,
            "path": flow.request.path,
            "outcome": match flow.outcome { FlowOutcome::Response(response) => json!({"status": response.status}), FlowOutcome::Error(error) => json!({"error": format!("{:?}", error.category)}) },
            "redactions": flow.redactions,
            "request_body": body_view,
            "response_body": response_view,
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
