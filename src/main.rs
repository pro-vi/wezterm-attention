use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use wezterm_attention::protocol::{AttentionError, Diagnostic, Disposition};
use wezterm_attention::query::read_bindings_timed;
use wezterm_attention::wezterm::{
    Clock, SystemClock, SystemProcessInspector, SystemProcessProbe, SystemTtyWriter,
    WeztermPaneLister, default_ports,
};

#[derive(Debug, Parser)]
#[command(
    name = "attention",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ATTENTION_BUILD_COMMIT"), ")"),
    about = "Publish and maintain mux-native WezTerm attention state.",
    after_help = "Example: attention bindings --socket /absolute/mux.sock\nRead commands (bindings, tabs, tab-source, inspect, hooks describe) return JSON by default.\nCheck status and complete before using query results.\nRegistration requirements: attention hooks describe --provider claude"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Callback entrypoints.
    Hooks {
        #[command(subcommand)]
        command: Option<HookCommand>,
    },
    /// List validated binding facts.
    Bindings(BindingsArgs),
    /// List the tab order each GUI window's tab bar published.
    Tabs(TabsArgs),
    /// Describe a GUI socket for the tab publisher without querying or changing it.
    #[command(hide = true)]
    TabSource(TabSourceArgs),
    /// The WezTerm plugin's own writes to one pane's records.
    #[command(hide = true)]
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Read one exact canonical scope from JSON stdin without changing state.
    Inspect(InspectArgs),
    /// Set current activity or a source-owned review.
    Mark(MarkArgs),
    /// Inspect CLI integration health.
    Doctor(OutputArgs),
    /// Preview or apply conservative retention work.
    Sweep(SweepArgs),
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Describe package-owned native callback registrations; does not configure a machine.
    Describe(DescribeArgs),
    /// Persist and publish the current launch claim.
    Claim(OutputArgs),
    /// Republish one pane or one mux realm.
    Publish(PublishArgs),
    /// Accept one provider callback.
    Event(EventArgs),
}

#[derive(Clone, Debug, Args)]
#[command(after_help = provider_event_help())]
struct EventArgs {
    provider: String,
    event: String,
    /// Return nonzero on native or consumer failure; default hooks exit zero.
    #[arg(long)]
    strict: bool,
    /// Write structured diagnostics to stderr; hook stdout stays empty.
    #[arg(long)]
    debug: bool,
    /// Absolute executable receiving one transient envelope on stdin; repeatable.
    #[arg(long)]
    consumer: Vec<String>,
    /// Positive deadline per consumer, covering stdin and exit; required with --consumer.
    #[arg(long)]
    consumer_timeout_ms: Option<u64>,
    /// Include supported native reply text only in transient consumer stdin, never records.
    #[arg(long)]
    include_reply: bool,
    /// Include supported submit text only in transient consumer stdin, never records.
    #[arg(long)]
    include_prompt: bool,
}

fn provider_event_help() -> String {
    let mut help =
        "Example: attention hooks event claude Stop --consumer /absolute/reply-sink --consumer-timeout-ms 1000 --include-reply --strict < callback.json\nConsumers run after native locks release. Retrying may repeat consumer effects.\nRegistration details: attention hooks describe --provider <provider> --json".to_owned();
    if let Ok(manifest) = wezterm_attention::protocol::manifest() {
        for (provider, hooks) in &manifest.native_hooks {
            let events = hooks
                .iter()
                .filter(|(_, declaration)| {
                    declaration.registration
                        == wezterm_attention::providers::HookRegistration::Register
                })
                .map(|(event, _)| event.clone())
                .collect::<Vec<_>>()
                .join(", ");
            help.push_str(&format!("\n{provider}: {events}"));
        }
    }
    help
}

#[derive(Clone, Debug, Args)]
struct OutputArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Args)]
#[command(
    after_help = "Example: attention hooks describe --provider claude\nReturns registration requirements, not proof that hooks are installed.\nFor supported provider names: attention hooks event --help"
)]
struct DescribeArgs {
    #[arg(long)]
    provider: String,
    /// Return the JSON envelope (also the default).
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Args)]
#[command(
    after_help = "Example: attention inspect --scope - < scope.json\nScope: {\"address\":{\"realm_id\":\"<64 lowercase hex>\",\"incarnation_id\":\"<64 lowercase hex>\",\"pane_id\":\"42\"},\"launch_id\":\"<UUID>\",\"binding_id\":\"<optional 64 lowercase hex>\"}\nObtain address, launch_id and binding_id from a current row in:\n  attention bindings --socket /absolute/mux.sock\ncomplete describes the answer as a set. Read pane_presence, binding_health, reader_confidence and current on the row itself. Omit binding_id for launch selection.\nExit 0: complete response; 1: changed/degraded evidence; 2: invalid input. No GUI cache fallback."
)]
struct InspectArgs {
    /// Read the exact expected scope from JSON stdin; '-' is the only accepted value.
    #[arg(long, value_parser=["-"])]
    scope: String,
    /// Return the JSON envelope (also the default).
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Args)]
struct PublishArgs {
    /// Existing socket path.
    #[arg(long)]
    socket: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    quiet: bool,
    #[arg(long)]
    all_details: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum BindingField {
    Address,
    LaunchId,
    BindingId,
    Provider,
    ProviderSessionId,
    BindingPhase,
    PanePresence,
    ReaderConfidence,
    BindingHealth,
    Current,
    ExpectedSessionMatch,
    ExpectedSessionId,
    TranscriptPath,
    Cwd,
    ConfigDir,
    Model,
    StartSource,
}

impl BindingField {
    fn name(self) -> String {
        self.to_possible_value()
            .expect("field has a name")
            .get_name()
            .to_owned()
    }
}

fn bindings_help() -> String {
    format!(
        "Example: attention bindings --all --fields address,provider,current\nFields: {}\ncomplete is false when --limit truncated the rows or a state directory could not be read, and with --socket on any diagnostic: a probe that did not answer, a selected record that could not be read, a binding_conflict. Exit 0 when complete, 1 when not, 2 for a usage error.\nDropped diagnostics are counted: result.diagnostic_count of result.total_diagnostic_count.\nresult.timing_ms says where the call's time went: pane_list (wezterm cli list), process_list (the process probe), records (the file walk).\nIf truncated, narrow with --provider, raise --limit (maximum 1000), or explicitly use --all.\n--socket queries prevent WezTerm auto-start; --realm selects a recorded realm ID.",
        BindingField::value_variants()
            .iter()
            .map(|field| field.name())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_binding_fields(value: &str) -> Result<Vec<String>, AttentionError> {
    let mut fields = Vec::new();
    for name in value
        .split(',')
        .map(|name| name.trim_matches(|c: char| c.is_ascii_whitespace()))
    {
        let field = BindingField::from_str(name, false)
            .map_err(|_| {
                AttentionError::usage(format!(
                    "--fields contains an invalid field: {name:?}; see bindings --help"
                ))
            })?
            .name();
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    Ok(fields)
}

#[derive(Clone, Debug, Args)]
#[command(after_help = bindings_help())]
struct BindingsArgs {
    /// Return the JSON envelope (also the default).
    #[arg(long)]
    json: bool,
    /// Filter by a 64-character lowercase hex realm ID, not a socket path.
    #[arg(long)]
    realm: Option<String>,
    /// Restrict discovery to this existing socket's exact incarnation.
    #[arg(long, conflicts_with = "realm")]
    socket: Option<String>,
    /// Filter by a supported provider; see hooks event --help.
    #[arg(long)]
    provider: Option<String>,
    /// Maximum returned rows, 1..=1000; truncation sets complete=false.
    #[arg(long, default_value_t = 100)]
    limit: usize,
    /// Return every matching row instead of limiting output.
    #[arg(long)]
    all: bool,
    /// Comma-separated top-level row fields; omitted fields retain their usual absence.
    #[arg(long)]
    fields: Option<String>,
}

#[derive(Clone, Debug, Args)]
#[command(
    after_help = "Example: attention tabs\nReturns each saved tab order and a check against its publishing GUI socket.\nwindow_check.status: present, not_listed, or unavailable (with a reason).\nnot_listed means no panes were listed for that window, not proven closure.\nA window check does not refresh or verify saved tab order: read published_at_ms.\ncomplete describes publication coverage; check window_check on the window you use.\nEvery saved window is returned, including legacy files with no known source. For bound panes only: attention bindings"
)]
struct TabsArgs {
    /// Return the JSON envelope (also the default).
    #[arg(long)]
    json: bool,
}

/// The plugin is not a process in the pane, so every write names the pane by
/// its address and the launch it published.
#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// Set the user's review flag.
    SetReview(PluginPaneArgs),
    /// Withdraw the user's review flag; other owners' reviews stay.
    ClearReview(PluginPaneArgs),
    /// Acknowledge the activity the user saw, if it is still the one shown.
    Acknowledge(AcknowledgeArgs),
}

#[derive(Clone, Debug, Args)]
struct PluginPaneArgs {
    #[arg(long)]
    realm_id: String,
    #[arg(long)]
    incarnation_id: String,
    #[arg(long)]
    pane_id: String,
    #[arg(long)]
    launch_id: String,
}

#[derive(Clone, Debug, Args)]
struct AcknowledgeArgs {
    #[command(flatten)]
    pane: PluginPaneArgs,
    #[arg(long)]
    activity_event_id: String,
}

impl PluginPaneArgs {
    fn scope(&self) -> Result<wezterm_attention::query::PaneScope, AttentionError> {
        wezterm_attention::query::PaneScope::new(
            wezterm_attention::identity::PaneAddress {
                realm_id: self.realm_id.clone(),
                incarnation_id: self.incarnation_id.clone(),
                pane_id: self.pane_id.clone(),
            },
            self.launch_id.clone(),
            None,
        )
    }
}

#[derive(Clone, Debug, Args)]
struct TabSourceArgs {
    #[arg(long)]
    socket: String,
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
#[command(
    after_help = "Preview is the default and removes nothing. Example: attention sweep --json\nTab orders to collect are always listed in full; --all-details includes the rest when complete is false.\nApply: attention sweep --apply --json\nEach apply without --operation-id gets a fresh one, reported in result.operation_id.\nPass --operation-id only to replay that operation; it must be a canonical lowercase UUID."
)]
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

impl<T: Serialize> Response<T> {
    fn new(
        command: &str,
        status: &str,
        complete: bool,
        diagnostics: Vec<Diagnostic>,
        result: T,
    ) -> Self {
        Self {
            schema: 1,
            command: command.to_owned(),
            status: status.to_owned(),
            complete,
            result,
            diagnostics,
        }
    }
}

/// The envelope of a command that failed. An error answers nothing, so it
/// is never complete, and its result is empty.
fn error_response(error: &AttentionError, command: &str) -> Response<serde_json::Value> {
    let status = if error.exit_code == 2 {
        "usage_error"
    } else {
        "unavailable"
    };
    Response::new(
        command,
        status,
        false,
        vec![error.diagnostic.clone()],
        serde_json::json!({}),
    )
}

fn query_json(command: &str) -> bool {
    matches!(
        command,
        "bindings" | "tabs" | "tab-source" | "inspect" | "hooks describe"
    ) || command == "plugin"
        || command.starts_with("plugin ")
}

/// `value` as one line of JSON that is safe to print to a terminal.
fn printable_json<T: Serialize>(value: &T) -> String {
    wezterm_attention::protocol::printable_json(value).expect("response serializes")
}

/// Print one line to stdout.
///
/// A reader that closed the pipe early, as `| head` does, already has what it
/// wanted, so a broken pipe is not an error; `println!` would panic there and
/// exit 101. Any other write failure is reported on stderr.
fn print_out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{text}").and_then(|()| stdout.flush())
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        print_err(&format!("attention: stdout: {error}"));
    }
}

/// Print one line to stderr. A closed stderr leaves nowhere to say so, and a
/// hook must still exit with its own code, so a failure here is dropped.
fn print_err(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

fn emit<T: Serialize>(response: &Response<T>, as_json: bool, quiet: bool) {
    if quiet {
        return;
    }
    if as_json || query_json(&response.command) {
        print_out(&printable_json(response));
    } else {
        // The status word alone cannot say what to fix, and every
        // diagnostic points here. Its reasons go to stderr, so a script
        // that reads stdout reads only the word.
        print_out(&response.status);
        for diagnostic in &response.diagnostics {
            let message = wezterm_attention::protocol::terminal_safe(&diagnostic.message);
            print_err(&format!("attention: {}: {message}", diagnostic.code));
        }
    }
}

/// The exit code a query answer earns: 0 when it is complete, 1 when it is
/// not. Diagnostics beside a complete answer do not change it; `status` and
/// the diagnostics say what they are.
fn query_exit(complete: bool) -> ExitCode {
    if complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn emit_error(error: &AttentionError, as_json: bool, command: &str) -> ExitCode {
    let mut error = error.clone();
    if error.exit_code == 2 {
        error.diagnostic.help = format!("attention {command} --help");
    }
    if as_json || query_json(command) {
        print_out(&printable_json(&error_response(&error, command)));
    } else {
        print_err(&format!(
            "attention: {}: {}",
            error.diagnostic.code, error.diagnostic.message
        ));
        print_err(&format!("help: {}", error.diagnostic.help));
    }
    // 2 is kept for a command line that could not be used; every other
    // failure, whatever the error's own code, is 1. A hook command never
    // exits 2, because Claude Code and Codex read 2 as "block".
    ExitCode::from(if error.exit_code == 2 && !is_hook_command(command) {
        2
    } else {
        1
    })
}

/// `attention hooks ...`, which provider hook registrations, the shell
/// integration and the plugin run.
fn is_hook_command(command: &str) -> bool {
    command == "hooks" || command.starts_with("hooks ")
}

fn emit_hook_error(error: &AttentionError, debug: bool, strict: bool, command: &str) -> ExitCode {
    if debug {
        print_err(&printable_json(&error_response(error, command)));
    } else {
        print_err(&format!(
            "attention: {}: {}",
            error.diagnostic.code, error.diagnostic.message
        ));
    }
    if strict {
        ExitCode::from(1)
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
    let inspector = SystemProcessInspector;
    let ports = default_ports(&clock, &tty, &panes, &inspector);
    match cli.command {
        None => {
            let mut command = Cli::command();
            print_out(&command.render_help().to_string());
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Hooks { command: None }) => {
            let mut command = Cli::command();
            let hooks = command
                .find_subcommand_mut("hooks")
                .expect("hooks subcommand is declared");
            print_out(&hooks.render_help().to_string());
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Hooks {
            command: Some(HookCommand::Describe(args)),
        }) => {
            let result = wezterm_attention::providers::describe_hooks(&args.provider)
                .map_err(|error| (Box::new(error), args.json, "hooks describe".into()))?;
            emit(
                &Response::new("hooks describe", "ok", true, vec![], result),
                args.json,
                false,
            );
            Ok(ExitCode::SUCCESS)
        }
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
                    &Response::new("hooks claim", "ok", complete, diagnostics, result),
                    true,
                    false,
                );
            } else {
                print_out(&result.launch_id);
                if let Some(diagnostic) = &result.publication_diagnostic {
                    print_err(&format!(
                        "attention: publication pending: {}: {}",
                        diagnostic.code, diagnostic.message
                    ));
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
            let selected_socket = args.socket.as_deref();
            let mut report = match selected_socket {
                Some(socket) => wezterm_attention::publish_realm(socket, &environment, &ports),
                None => wezterm_attention::publish_current(&environment, &ports),
            }
            .map_err(|error| (Box::new(error), args.json, "hooks publish".to_owned()))?;
            if selected_socket.is_none() {
                let inherited = environment
                    .get("WEZTERM_ATTENTION_LAUNCH_ID")
                    .is_some_and(|value| !value.is_empty());
                // Without a launch id the shell started no agent here, but an
                // agent that claimed the pane for itself may have exited.
                match clock.monotonic_ns20().and_then(|observation| {
                    if inherited {
                        wezterm_attention::lifecycle::prompt_return(&environment, &observation)
                            .map(Some)
                    } else {
                        wezterm_attention::lifecycle::prompt_return_after_agent_exit(
                            &environment,
                            &observation,
                            &ports,
                        )
                    }
                }) {
                    Ok(result) => {
                        if let Some(diagnostic) = result.and_then(|result| result.diagnostic) {
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
                &Response::new("hooks publish", status, complete, diagnostics, result),
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
            let consumer_timeout = match wezterm_attention::consumer::validate_consumers(
                &args.consumer,
                args.consumer_timeout_ms,
            ) {
                Ok(timeout) => timeout,
                Err(error) => {
                    return Ok(emit_hook_error(&error, args.debug, args.strict, &command));
                }
            };
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
            if !args.consumer.is_empty() {
                use wezterm_attention::consumer::{
                    DeliveryStage, delivery_bytes, dispatch, not_dispatched,
                };
                let outcome = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
                    &event,
                    &environment,
                    &observation,
                    &ports,
                );
                let reply = wezterm_attention::providers::reply_content(
                    &event,
                    &payload,
                    args.include_reply,
                );
                let prompt = wezterm_attention::providers::prompt_content(
                    &event,
                    &payload,
                    args.include_prompt,
                );
                let delivery = delivery_bytes(&outcome, reply, prompt);
                let consumers = args
                    .consumer
                    .iter()
                    .map(|executable| match &delivery {
                        Ok(bytes) => dispatch(
                            executable,
                            bytes,
                            consumer_timeout.expect("validated consumer timeout"),
                        ),
                        Err(reason) => not_dispatched(executable, *reason),
                    })
                    .collect::<Vec<_>>();
                let failed = outcome.result.as_ref().map_or(true, |result| {
                    matches!(
                        result.disposition,
                        Disposition::Ignored | Disposition::Conflict | Disposition::Partial
                    )
                }) || consumers
                    .iter()
                    .any(|result| result.stage != DeliveryStage::Completed);
                let diagnostics = match &outcome.result {
                    Ok(result) => result.diagnostic.iter().cloned().collect(),
                    Err(error) => vec![error.diagnostic.clone()],
                };
                let result = serde_json::json!({"native": outcome.result.as_ref().ok(), "admission": outcome.admission, "persistence": outcome.persistence, "consumers": consumers});
                // Prompt/reply bodies and child output never enter this diagnostic projection.
                print_err(&printable_json(&Response::new(
                    &command,
                    if failed { "findings" } else { "ok" },
                    true,
                    diagnostics,
                    result,
                )));
                return Ok(if args.strict && failed {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                });
            }
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
                result.disposition,
                Disposition::Ignored | Disposition::Conflict | Disposition::Partial
            );
            if args.debug {
                let response = Response::new(
                    &command,
                    if failed { "findings" } else { "ok" },
                    true,
                    result.diagnostic.iter().cloned().collect(),
                    result,
                );
                print_err(&printable_json(&response));
            } else if let Some(diagnostic) = &result.diagnostic {
                print_err(&format!(
                    "attention: {}: {}",
                    diagnostic.code, diagnostic.message
                ));
            }
            Ok(if args.strict && failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Some(Command::Bindings(args)) => {
            let fields = args
                .fields
                .as_deref()
                .map(parse_binding_fields)
                .transpose()
                .map_err(|error| (Box::new(error), args.json, "bindings".to_owned()))?;
            if let Some(socket) = &args.socket {
                wezterm_attention::query::validate_socket_selector(socket)
                    .map_err(|error| (Box::new(error), args.json, "bindings".to_owned()))?;
            }
            if !(1..=1000).contains(&args.limit) {
                return Err((
                    Box::new(AttentionError::usage("--limit must be between 1 and 1000")),
                    args.json,
                    "bindings".to_owned(),
                ));
            }
            if args
                .realm
                .as_deref()
                .is_some_and(|realm| !wezterm_attention::protocol::hex64_text(realm))
            {
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
                    Box::new(AttentionError::usage(format!(
                        "--provider is not supported; expected one of: {}",
                        wezterm_attention::protocol::manifest()
                            .expect("manifest was validated above")
                            .enums
                            .providers
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))),
                    args.json,
                    "bindings".to_owned(),
                ));
            }
            let root = match wezterm_attention::records::state_root(&environment) {
                Ok(root) => root,
                Err(error) if args.socket.is_some() => {
                    return Ok(emit_error(&error, args.json, "bindings"));
                }
                Err(error) => return Err((Box::new(error), args.json, "bindings".to_owned())),
            };
            // A socket answer reports an unreadable directory as a diagnostic,
            // and any diagnostic already makes it incomplete.
            let mut walked_every_directory = true;
            let (scope, mut rows, diagnostics, timing) = if let Some(socket) = &args.socket {
                let (scope, mut rows, diagnostics, timing) =
                    match wezterm_attention::query::read_bindings_for_socket_timed(
                        &root,
                        socket,
                        Some(&WeztermPaneLister),
                        Some(&processes),
                    ) {
                        Ok(result) => result,
                        Err(error) => {
                            return Ok(emit_error(&error, args.json, "bindings"));
                        }
                    };
                // After the answer, not in its filter: a row filtered out
                // sends its diagnostics to the ignored ones, which would
                // change what makes this answer incomplete.
                if let Some(provider) = &args.provider {
                    rows.retain(|row| &row.provider == provider);
                }
                (Some(scope), rows, diagnostics, timing)
            } else {
                // The filters apply before any socket is asked, so a realm or
                // provider left out costs no pane listing.
                let filter = wezterm_attention::query::BindingFilter {
                    realm_id: args.realm.clone(),
                    incarnation_id: None,
                    provider: args.provider.clone(),
                };
                let answer = read_bindings_timed(&root, &filter, Some(&panes), Some(&processes))
                    .map_err(|error| (Box::new(error), args.json, "bindings".to_owned()))?;
                walked_every_directory = answer.walked_every_directory;
                (None, answer.rows, answer.diagnostics, answer.timing)
            };
            let scanned = rows.len();
            if !args.all {
                rows.truncate(args.limit);
            }
            let returned = rows.len();
            let truncated = returned < scanned;
            // Help is read once and output on every run, so the run says it.
            // stdout stays JSON; the line goes where a consumer's log looks.
            if truncated {
                eprintln!("attention bindings: returned {returned} of {scanned}; use --all");
            }
            let shown_diagnostics: Vec<Diagnostic> = diagnostics.iter().take(50).cloned().collect();
            let mut result = serde_json::json!({
                "rows": rows,
                "scanned": scanned,
                "returned": returned,
                "truncated": truncated,
                "diagnostic_count": shown_diagnostics.len(),
                "total_diagnostic_count": diagnostics.len(),
                "timing_ms": timing.as_millis(),
            });
            if let Some(fields) = fields {
                for row in result["rows"]
                    .as_array_mut()
                    .expect("rows serialize as an array")
                {
                    row.as_object_mut()
                        .expect("binding serializes as an object")
                        .retain(|key, _| fields.contains(key));
                }
            }
            let socket_mode = scope.is_some();
            if let Some(scope) = scope {
                result["scope"] = serde_json::to_value(scope).expect("scope serializes");
            }
            // `complete` describes the rows. A socket-scoped answer also needs
            // every probe to have answered, since a degraded probe leaves a
            // pane's presence unknown; a realm-wide answer reports its
            // diagnostics through the two counts instead, because on a machine
            // where panes outlive mux incarnations they never run out, and a
            // flag that is always false says nothing about the rows. A
            // directory that could not be read may hold rows, so it does.
            let complete =
                !truncated && walked_every_directory && (!socket_mode || diagnostics.is_empty());
            emit(
                &Response::new(
                    "bindings",
                    if diagnostics.is_empty() {
                        "ok"
                    } else {
                        "findings"
                    },
                    complete,
                    shown_diagnostics,
                    result,
                ),
                args.json,
                false,
            );
            Ok(query_exit(complete))
        }
        Some(Command::TabSource(args)) => {
            let source = wezterm_attention::query::read_tab_source(&args.socket)
                .map_err(|error| (Box::new(error), true, "tab-source".to_owned()))?;
            emit(
                &Response::new("tab-source", "ok", true, Vec::new(), source),
                true,
                false,
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Plugin { command }) => {
            let (name, pane) = match &command {
                PluginCommand::SetReview(pane) => ("plugin set-review", pane),
                PluginCommand::ClearReview(pane) => ("plugin clear-review", pane),
                PluginCommand::Acknowledge(args) => ("plugin acknowledge", &args.pane),
            };
            let failed = |error: AttentionError| (Box::new(error), true, name.to_owned());
            let scope = pane.scope().map_err(failed)?;
            let root = wezterm_attention::records::state_root(&environment).map_err(failed)?;
            let result = match &command {
                PluginCommand::SetReview(_) | PluginCommand::ClearReview(_) => {
                    wezterm_attention::lifecycle::apply_user_review(
                        &root,
                        scope.address(),
                        scope.launch_id(),
                        matches!(command, PluginCommand::SetReview(_)),
                    )
                }
                PluginCommand::Acknowledge(args) => {
                    let activity_event_id = wezterm_attention::identity::canonical_uuid(
                        Some(&args.activity_event_id),
                        "--activity-event-id",
                    )
                    .map_err(|error| AttentionError::usage(error.diagnostic.message))
                    .map_err(failed)?;
                    wezterm_attention::lifecycle::acknowledge_activity(
                        &root,
                        scope.address(),
                        scope.launch_id(),
                        &activity_event_id,
                    )
                }
            }
            .map_err(failed)?;
            emit(
                &Response::new(name, "ok", true, Vec::new(), result),
                true,
                false,
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Tabs(args)) => {
            let root = wezterm_attention::records::state_root(&environment)
                .map_err(|error| (Box::new(error), args.json, "tabs".to_owned()))?;
            let (windows, diagnostics) = wezterm_attention::query::read_checked_tab_publications(
                &root,
                &wezterm_attention::wezterm::ExistingWeztermWindowLister,
                &clock,
            )
            .map_err(|error| (Box::new(error), args.json, "tabs".to_owned()))?;
            emit(
                &Response::new(
                    "tabs",
                    if diagnostics.is_empty() {
                        "ok"
                    } else {
                        "findings"
                    },
                    // A window that could not be read is a window missing from
                    // the answer, so the answer is not the whole tab bar.
                    diagnostics.is_empty(),
                    diagnostics.iter().take(50).cloned().collect(),
                    serde_json::json!({
                        "windows": windows,
                        "diagnostic_count": diagnostics.len().min(50),
                        "total_diagnostic_count": diagnostics.len(),
                    }),
                ),
                args.json,
                false,
            );
            Ok(query_exit(diagnostics.is_empty()))
        }
        Some(Command::Inspect(args)) => {
            let maximum = wezterm_attention::protocol::manifest()
                .map_err(|e| (Box::new(e), args.json, "inspect".into()))?
                .limits
                .max_json_bytes;
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((maximum + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| {
                    (
                        Box::new(AttentionError::usage("inspect could not read scope stdin")),
                        args.json,
                        "inspect".into(),
                    )
                })?;
            if bytes.len() > maximum {
                return Err((
                    Box::new(AttentionError::usage("scope exceeds its byte bound")),
                    args.json,
                    "inspect".into(),
                ));
            }
            let scope: wezterm_attention::query::PaneScope = serde_json::from_slice(&bytes)
                .map_err(|error| {
                    (
                        Box::new(AttentionError::usage(match error.classify() {
                            serde_json::error::Category::Eof if bytes.is_empty() =>
                                "scope stdin is empty; pipe a scope JSON object or use < scope.json".to_owned(),
                            serde_json::error::Category::Syntax | serde_json::error::Category::Eof =>
                                format!("scope JSON syntax is invalid at line {}, column {}", error.line(), error.column()),
                            _ => "scope requires address (realm_id and incarnation_id: 64 lowercase hex; pane_id: canonical decimal string), launch_id (UUID), and optional binding_id (64 lowercase hex); no extra fields".to_owned(),
                        })),
                        args.json,
                        "inspect".into(),
                    )
                })?;
            let read = wezterm_attention::records::state_root(&environment)
                .and_then(|root| wezterm_attention::query::read_pane_facts(&root, &scope));
            let facts = match read {
                Ok(facts) => facts,
                Err(mut error) => {
                    error.exit_code = 1;
                    return Ok(emit_error(&error, args.json, "inspect"));
                }
            };
            let complete = facts.complete();
            emit(
                &Response::new(
                    "inspect",
                    if complete { "ok" } else { "findings" },
                    complete,
                    facts.diagnostics.clone(),
                    facts,
                ),
                args.json,
                false,
            );
            Ok(if complete {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some(Command::Mark(args)) => {
            let observation = || {
                clock
                    .monotonic_ns20()
                    .map_err(|error| (Box::new(error), args.json, "mark".to_owned()))
            };
            let result = match args.state.as_str() {
                "review" => {
                    wezterm_attention::lifecycle::apply_mark_review(&environment, &args.source)
                }
                "clear" => wezterm_attention::lifecycle::apply_mark_clear(
                    &environment,
                    &args.source,
                    &observation()?,
                ),
                _ => {
                    let observation = observation()?;
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
            }
            .map_err(|error| (Box::new(error), args.json, "mark".to_owned()))?;
            emit(
                &Response::new("mark", "ok", true, Vec::new(), result),
                args.json,
                false,
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Doctor(args)) => {
            let root = wezterm_attention::records::state_root(&environment)
                .map_err(|error| (Box::new(error), args.json, "doctor".to_owned()))?;
            let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
                &root,
                &environment,
                Some(&panes),
                Some(&processes),
                &inspector,
            )
            .map_err(|error| (Box::new(error), args.json, "doctor".to_owned()))?;
            // A probe that did not answer, or a mux that did not list its
            // panes, which its socket probe reports.
            let unavailable = diagnostics
                .iter()
                .any(|item| item.code == "probe_unavailable")
                || result["probes"].as_array().is_some_and(|probes| {
                    probes.iter().any(|probe| probe["status"] == "unavailable")
                });
            // A report in which no probe had anything of the user's to look at
            // found nothing wrong, but it did not find anything right either.
            // The versions probe is left out because it never says
            // unobserved: besides the schemas of the state records, it
            // compares this binary's embedded manifest with the one on disk,
            // and there is always an embedded one.
            let nothing_observed = result["probes"].as_array().is_some_and(|probes| {
                probes
                    .iter()
                    .filter(|probe| probe["name"] != "versions")
                    .all(|probe| probe["status"] == "unobserved")
            });
            let status = if unavailable {
                "unavailable"
            } else if diagnostics.is_empty() && nothing_observed {
                "unobserved"
            } else if diagnostics.is_empty() {
                "ok"
            } else {
                "findings"
            };
            // A probe that did not answer leaves part of the report unknown.
            let complete = !unavailable && diagnostics.len() <= 50;
            emit(
                &Response::new(
                    "doctor",
                    status,
                    complete,
                    diagnostics.iter().take(50).cloned().collect(),
                    result,
                ),
                args.json,
                false,
            );
            Ok(query_exit(complete))
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
            let (shown, total_details) = wezterm_attention::maintenance::limit_sweep_preview(
                result.details,
                args.all_details,
            );
            result.details = shown;
            result.detail_count = result.details.len();
            result.total_detail_count = total_details;
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
            // A probe that did not answer leaves some pane's fate undecided,
            // and an apply step that failed left its work undone.
            let complete = !unavailable
                && result.failed_steps == 0
                && result.details.len() == total_details
                && shown_diagnostics.len() == diagnostics.len();
            emit(
                &Response::new("sweep", status, complete, shown_diagnostics, result),
                args.json,
                false,
            );
            Ok(query_exit(complete))
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
            let first = arguments.get(1).and_then(|arg| arg.to_str());
            let second = arguments.get(2).and_then(|arg| arg.to_str());
            let command = match (first, second) {
                (Some("hooks"), Some(sub @ ("describe" | "claim" | "publish" | "event"))) => {
                    format!("hooks {sub}")
                }
                (Some(command), _) => command.to_owned(),
                _ => "attention".to_owned(),
            };
            // A provider runs `hooks event` (and would run a misspelling of
            // it), and reads exit 2 as "block": a prompt is dropped, a tool is
            // denied, a Stop hook loops on the error text. So a command line
            // it cannot parse exits like any other hook failure: 1 under
            // --strict, 0 otherwise, with the reason on stderr.
            if first == Some("hooks") && !matches!(second, Some("describe" | "claim" | "publish")) {
                let _ = error.print();
                let strict = arguments.iter().any(|argument| argument == "--strict");
                return ExitCode::from(u8::from(strict || second != Some("event")));
            }
            if query_json(&command)
                || arguments
                    .iter()
                    .skip(1)
                    .any(|argument| argument == "--json")
            {
                return emit_error(&AttentionError::usage(error.to_string()), true, &command);
            }
            let exit_code = if is_hook_command(&command) {
                1
            } else {
                error.exit_code()
            };
            let _ = error.print();
            return ExitCode::from(exit_code as u8);
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err((error, as_json, command)) => emit_error(&error, as_json, &command),
    }
}

#[cfg(test)]
mod binding_field_tests {
    use super::*;
    use std::collections::BTreeSet;
    use wezterm_attention::identity::PaneAddress;
    use wezterm_attention::query::BindingRow;

    #[test]
    fn binding_fields_match_serialized_row_keys() {
        let row = BindingRow {
            address: PaneAddress {
                realm_id: "a".repeat(64),
                incarnation_id: "b".repeat(64),
                pane_id: "1".into(),
            },
            launch_id: "00000000-0000-4000-8000-000000000001".into(),
            binding_id: "c".repeat(64),
            provider: "claude".into(),
            provider_session_id: "session".into(),
            binding_phase: "running".into(),
            pane_presence: "present".into(),
            reader_confidence: "current".into(),
            binding_health: "healthy".into(),
            current: true,
            expected_session_match: None,
            expected_session_id: Some("session".into()),
            transcript_path: Some("/test/transcript".into()),
            cwd: Some("/test".into()),
            config_dir: Some("/test/config".into()),
            model: Some("model".into()),
            start_source: Some("startup".into()),
        };
        let value = serde_json::to_value(row).unwrap();
        let keys: BTreeSet<_> = value.as_object().unwrap().keys().cloned().collect();
        let selectable: BTreeSet<_> = BindingField::value_variants()
            .iter()
            .map(|field| field.name())
            .collect();
        assert_eq!(keys, selectable);
        assert!(value["expected_session_match"].is_null());
        let help = bindings_help();
        for name in keys {
            assert!(help.contains(&name));
        }
    }
}
