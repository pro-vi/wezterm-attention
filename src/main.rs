use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use serde::Serialize;

use wezterm_attention::protocol::{AttentionError, Diagnostic};
use wezterm_attention::query::read_bindings_with_ports;
use wezterm_attention::wezterm::{
    Clock, SystemClock, SystemProcessProbe, SystemTtyWriter, WeztermPaneLister, default_ports,
};

#[derive(Debug, Parser)]
#[command(
    name = "attention",
    about = "Publish and maintain mux-native WezTerm attention state.",
    after_help = "Example: attention hooks claim"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(hide = true)]
    Claim,
    /// Callback entrypoints.
    Hooks {
        #[command(subcommand)]
        command: Option<HookCommand>,
    },
    /// List validated binding facts.
    Bindings(BindingsArgs),
    /// Set current activity or a source-owned review.
    Mark(MarkArgs),
    /// Inspect CLI integration health.
    Doctor(OutputArgs),
    /// Preview or apply conservative retention work.
    Sweep(SweepArgs),
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Persist and publish the current launch claim.
    Claim(OutputArgs),
    /// Republish one pane or one mux realm.
    Publish(PublishArgs),
    /// Accept one provider callback.
    Event(EventArgs),
}

#[derive(Clone, Debug, Args)]
#[command(
    after_help = "Events: SessionStart, PreToolUse, PermissionRequest, Notification, Stop, SubagentStop, SessionEnd, session_start, agent_start, tool_execution_start, agent_settled, bus, session_shutdown"
)]
struct EventArgs {
    provider: String,
    event: String,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    debug: bool,
}

#[derive(Clone, Debug, Args)]
struct OutputArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Args)]
struct PublishArgs {
    #[arg(long)]
    realm: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    quiet: bool,
    #[arg(long)]
    all_details: bool,
}

#[derive(Clone, Debug, Args)]
struct BindingsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    realm: Option<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long)]
    all: bool,
}

#[derive(Clone, Debug, Args)]
struct MarkArgs {
    #[arg(value_parser = ["thinking", "stop", "notify", "review", "clear"])]
    state: String,
    #[arg(long, default_value = "manual")]
    source: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    frame: Option<u64>,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    ttl_ms: Option<u64>,
}

#[derive(Clone, Debug, Args)]
struct SweepArgs {
    #[arg(long)]
    realm: Option<String>,
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    operation_id: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    all_details: bool,
}

#[derive(Debug, Serialize)]
struct Response<T: Serialize> {
    schema: u8,
    command: String,
    status: String,
    complete: bool,
    result: T,
    diagnostics: Vec<Diagnostic>,
}

fn emit<T: Serialize>(response: &Response<T>, as_json: bool, quiet: bool) {
    if quiet {
        return;
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string(response).expect("response serializes")
        );
    } else {
        println!("{}", response.status);
    }
}

fn emit_error(error: &AttentionError, as_json: bool, command: &str) -> ExitCode {
    if as_json {
        let response = Response {
            schema: 1,
            command: command.to_owned(),
            status: if error.exit_code == 2 {
                "usage_error"
            } else {
                "unavailable"
            }
            .to_owned(),
            complete: true,
            result: serde_json::json!({}),
            diagnostics: vec![error.diagnostic.clone()],
        };
        println!(
            "{}",
            serde_json::to_string(&response).expect("response serializes")
        );
    } else {
        eprintln!(
            "attention: {}: {}",
            error.diagnostic.code, error.diagnostic.message
        );
        eprintln!("help: {}", error.diagnostic.help);
    }
    ExitCode::from(error.exit_code as u8)
}

fn emit_hook_error(error: &AttentionError, debug: bool, strict: bool, command: &str) -> ExitCode {
    if debug {
        let response = Response {
            schema: 1,
            command: command.to_owned(),
            status: if error.exit_code == 2 {
                "usage_error"
            } else {
                "unavailable"
            }
            .to_owned(),
            complete: true,
            result: serde_json::json!({}),
            diagnostics: vec![error.diagnostic.clone()],
        };
        eprintln!(
            "{}",
            serde_json::to_string(&response).expect("response serializes")
        );
    } else {
        eprintln!(
            "attention: {}: {}",
            error.diagnostic.code, error.diagnostic.message
        );
    }
    if strict {
        ExitCode::from(error.exit_code as u8)
    } else {
        ExitCode::SUCCESS
    }
}

fn run(cli: Cli) -> std::result::Result<ExitCode, (Box<AttentionError>, bool, String)> {
    let environment = wezterm_attention::environment();
    let clock = SystemClock;
    let tty = SystemTtyWriter;
    let panes = WeztermPaneLister;
    let processes = SystemProcessProbe;
    let ports = default_ports(&clock, &tty, &panes);
    match cli.command {
        None => {
            let mut command = Cli::command();
            println!("{}", command.render_help());
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Hooks { command: None }) => {
            let mut command = Cli::command();
            let hooks = command
                .find_subcommand_mut("hooks")
                .expect("hooks subcommand is declared");
            println!("{}", hooks.render_help());
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Claim) => Err((
            Box::new(AttentionError::usage(
                "obsolete command; use `attention hooks claim`",
            )),
            false,
            "claim".to_owned(),
        )),
        Some(Command::Hooks {
            command: Some(HookCommand::Claim(args)),
        }) => {
            let result = wezterm_attention::claim_launch(&environment, &ports)
                .map_err(|error| (Box::new(error), args.json, "hooks claim".to_owned()))?;
            let complete = result.publication_diagnostic.is_none();
            let diagnostics = result
                .publication_diagnostic
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            if args.json {
                emit(
                    &Response {
                        schema: 1,
                        command: "hooks claim".to_owned(),
                        status: "ok".to_owned(),
                        complete,
                        result,
                        diagnostics,
                    },
                    true,
                    false,
                );
            } else {
                println!("{}", result.launch_id);
                if let Some(diagnostic) = &result.publication_diagnostic {
                    eprintln!(
                        "attention: publication pending: {}: {}",
                        diagnostic.code, diagnostic.message
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Hooks {
            command: Some(HookCommand::Publish(args)),
        }) => {
            if args.json && args.quiet {
                return Err((
                    Box::new(AttentionError::usage(
                        "--json and --quiet are mutually exclusive",
                    )),
                    true,
                    "hooks publish".to_owned(),
                ));
            }
            let mut report = match args.realm.as_deref() {
                Some(socket) => wezterm_attention::publish_realm(socket, &environment, &ports),
                None => wezterm_attention::publish_current(&environment, &ports),
            }
            .map_err(|error| (Box::new(error), args.json, "hooks publish".to_owned()))?;
            if args.realm.is_none()
                && environment
                    .get("WEZTERM_ATTENTION_LAUNCH_ID")
                    .is_some_and(|value| !value.is_empty())
            {
                match clock.monotonic_ns20().and_then(|observation| {
                    wezterm_attention::lifecycle::prompt_return(&environment, &observation)
                }) {
                    Ok(result) => {
                        if let Some(diagnostic) = result.diagnostic {
                            report.diagnostics.push(diagnostic);
                        }
                    }
                    Err(error) => report.diagnostics.push(error.diagnostic),
                }
            }
            let skipped = report.skipped;
            let status = if skipped == 0 && report.diagnostics.is_empty() {
                "ok"
            } else {
                "findings"
            };
            let diagnostics = if args.all_details {
                report.diagnostics.clone()
            } else {
                report.diagnostics.iter().take(50).cloned().collect()
            };
            let complete = diagnostics.len() == report.diagnostics.len();
            let result = serde_json::json!({
                "attempted": report.attempted,
                "published": report.published,
                "v2_published": report.v2_published,
                "skipped": report.skipped,
                "detail_count": diagnostics.len(),
                "total_detail_count": report.diagnostics.len(),
            });
            emit(
                &Response {
                    schema: 1,
                    command: "hooks publish".to_owned(),
                    status: status.to_owned(),
                    complete,
                    result,
                    diagnostics,
                },
                args.json,
                args.quiet,
            );
            Ok(if skipped == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some(Command::Hooks {
            command: Some(HookCommand::Event(args)),
        }) => {
            let command = "hooks event".to_owned();
            let observation = match clock.monotonic_ns20() {
                Ok(observation) => observation,
                Err(error) => {
                    return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
                }
            };
            let mut stdin = std::io::stdin();
            if stdin.is_terminal() {
                let error = AttentionError::usage("hooks event requires JSON on stdin");
                return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
            }
            let maximum = match wezterm_attention::protocol::manifest() {
                Ok(manifest) => manifest.limits.max_json_bytes,
                Err(error) => {
                    return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
                }
            };
            let mut bytes = Vec::new();
            if stdin
                .by_ref()
                .take((maximum + 1) as u64)
                .read_to_end(&mut bytes)
                .is_err()
            {
                let error = AttentionError::usage("hooks event could not read stdin");
                return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
            }
            let payload: serde_json::Value = if bytes.is_empty() || bytes.len() > maximum {
                let error = AttentionError::usage("hooks event received empty or oversized stdin");
                return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
            } else {
                match serde_json::from_slice(&bytes) {
                    Ok(payload) => payload,
                    Err(_) => {
                        let error = AttentionError::usage("hooks event received invalid JSON");
                        return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
                    }
                }
            };
            let event = wezterm_attention::providers::parse_provider_event(
                &args.provider,
                &args.event,
                &payload,
                &environment,
            );
            let result = match wezterm_attention::lifecycle::apply_provider_event(
                &event,
                &environment,
                &observation,
                &ports,
            ) {
                Ok(result) => result,
                Err(error) => {
                    return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
                }
            };
            let failed = matches!(
                result.disposition.as_str(),
                "ignored" | "conflict" | "partial"
            );
            if args.debug {
                let response = Response {
                    schema: 1,
                    command,
                    status: if failed { "findings" } else { "ok" }.to_owned(),
                    complete: true,
                    diagnostics: result.diagnostic.iter().cloned().collect(),
                    result,
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&response).expect("response serializes")
                );
            } else if let Some(diagnostic) = &result.diagnostic {
                eprintln!("attention: {}: {}", diagnostic.code, diagnostic.message);
            }
            Ok(if args.strict && failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Some(Command::Bindings(args)) => {
            if !(1..=1000).contains(&args.limit) {
                return Err((
                    Box::new(AttentionError::usage("--limit must be between 1 and 1000")),
                    args.json,
                    "bindings".to_owned(),
                ));
            }
            if args.realm.as_ref().is_some_and(|realm| {
                realm.len() != 64
                    || !realm
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) {
                return Err((
                    Box::new(AttentionError::usage(
                        "--realm must be 64 lowercase hex characters",
                    )),
                    args.json,
                    "bindings".to_owned(),
                ));
            }
            if args.provider.as_ref().is_some_and(|provider| {
                wezterm_attention::protocol::manifest()
                    .is_ok_and(|manifest| !manifest.enums.providers.contains(provider))
            }) {
                return Err((
                    Box::new(AttentionError::usage("--provider is not supported")),
                    args.json,
                    "bindings".to_owned(),
                ));
            }
            let root = wezterm_attention::records::state_root(&environment)
                .map_err(|error| (Box::new(error), args.json, "bindings".to_owned()))?;
            let (mut rows, diagnostics) =
                read_bindings_with_ports(&root, Some(&panes), Some(&processes))
                    .map_err(|error| (Box::new(error), args.json, "bindings".to_owned()))?;
            if let Some(realm) = args.realm {
                rows.retain(|row| row.address.realm_id == realm);
            }
            if let Some(provider) = args.provider {
                rows.retain(|row| row.provider == provider);
            }
            let scanned = rows.len();
            if !args.all {
                rows.truncate(args.limit);
            }
            let returned = rows.len();
            let truncated = returned < scanned;
            let result = serde_json::json!({
                "rows": rows,
                "scanned": scanned,
                "returned": returned,
                "truncated": truncated,
            });
            emit(
                &Response {
                    schema: 1,
                    command: "bindings".to_owned(),
                    status: if diagnostics.is_empty() {
                        "ok"
                    } else {
                        "findings"
                    }
                    .to_owned(),
                    complete: !truncated && diagnostics.len() <= 50,
                    result,
                    diagnostics: diagnostics.iter().take(50).cloned().collect(),
                },
                args.json,
                false,
            );
            Ok(if diagnostics.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some(Command::Mark(args)) => {
            let result = if matches!(args.state.as_str(), "review" | "clear") {
                wezterm_attention::lifecycle::apply_mark_review(
                    &environment,
                    &args.source,
                    args.state == "clear",
                )
            } else {
                let observation = clock
                    .monotonic_ns20()
                    .map_err(|error| (Box::new(error), args.json, "mark".to_owned()))?;
                let written_at = clock
                    .unix_ns20()
                    .map_err(|error| (Box::new(error), args.json, "mark".to_owned()))?;
                wezterm_attention::lifecycle::apply_mark_activity(
                    &environment,
                    &args.state,
                    &args.source,
                    args.frame,
                    args.label.as_deref(),
                    args.ttl_ms,
                    &observation,
                    &written_at,
                )
            }
            .map_err(|error| (Box::new(error), args.json, "mark".to_owned()))?;
            emit(
                &Response {
                    schema: 1,
                    command: "mark".to_owned(),
                    status: "ok".to_owned(),
                    complete: true,
                    result,
                    diagnostics: Vec::new(),
                },
                args.json,
                false,
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Doctor(args)) => {
            let root = wezterm_attention::records::state_root(&environment)
                .map_err(|error| (Box::new(error), args.json, "doctor".to_owned()))?;
            let (result, diagnostics) =
                wezterm_attention::maintenance::doctor(&root, Some(&panes), Some(&processes))
                    .map_err(|error| (Box::new(error), args.json, "doctor".to_owned()))?;
            let unavailable = diagnostics
                .iter()
                .any(|item| item.code == "probe_unavailable");
            let status = if unavailable {
                "unavailable"
            } else if diagnostics.is_empty() {
                "ok"
            } else {
                "findings"
            };
            emit(
                &Response {
                    schema: 1,
                    command: "doctor".to_owned(),
                    status: status.to_owned(),
                    complete: diagnostics.len() <= 50,
                    result,
                    diagnostics: diagnostics.iter().take(50).cloned().collect(),
                },
                args.json,
                false,
            );
            Ok(if unavailable {
                ExitCode::from(3)
            } else if diagnostics.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some(Command::Sweep(args)) => {
            if args.operation_id.is_some() && !args.apply {
                return Err((
                    Box::new(AttentionError::usage("--operation-id requires --apply")),
                    args.json,
                    "sweep".to_owned(),
                ));
            }
            let root = wezterm_attention::records::state_root(&environment)
                .map_err(|error| (Box::new(error), args.json, "sweep".to_owned()))?;
            let (mut result, diagnostics) = wezterm_attention::maintenance::sweep(
                &root,
                args.realm.as_deref(),
                args.apply,
                args.operation_id.as_deref(),
                &clock,
                &panes,
                Some(&processes),
            )
            .map_err(|error| (Box::new(error), args.json, "sweep".to_owned()))?;
            let total_details = result.details.len();
            if !args.all_details {
                result.details.truncate(50);
                result.detail_count = result.details.len();
            }
            let unavailable = diagnostics
                .iter()
                .any(|item| item.code == "probe_unavailable");
            let status = if unavailable {
                "unavailable"
            } else if diagnostics.is_empty() {
                "ok"
            } else {
                "findings"
            };
            let shown_diagnostics: Vec<_> = if args.all_details {
                diagnostics.clone()
            } else {
                diagnostics.iter().take(50).cloned().collect()
            };
            emit(
                &Response {
                    schema: 1,
                    command: "sweep".to_owned(),
                    status: status.to_owned(),
                    complete: result.details.len() == total_details
                        && shown_diagnostics.len() == diagnostics.len(),
                    result,
                    diagnostics: shown_diagnostics,
                },
                args.json,
                false,
            );
            Ok(if unavailable {
                ExitCode::from(3)
            } else if diagnostics.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
    }
}

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if arguments
                .iter()
                .skip(1)
                .any(|argument| argument == "--json")
            {
                let command = arguments
                    .get(1)
                    .and_then(|argument| argument.to_str())
                    .unwrap_or("attention");
                return emit_error(&AttentionError::usage(error.to_string()), true, command);
            }
            let exit_code = error.exit_code();
            let _ = error.print();
            return ExitCode::from(exit_code as u8);
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err((error, as_json, command)) => emit_error(&error, as_json, &command),
    }
}
