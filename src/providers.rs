use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::observations::{
    Actor, AttemptOutcome, ElicitationMode, LifecycleObservation, NativeCorrelation, NoticeSubtype,
    ObservationBody, QuestionMode, ResultSurface, SelectionAction, classify_tool,
};
use crate::protocol::{AttentionError, Diagnostic, DiagnosticCode, free_of_control, manifest};

// Which agents exist is a contract fact — the manifest declares the same set
// as `enums.providers`, and `parse_manifest` checks the two agree.
pub use crate::protocol::Provider;

// Both are manifest vocabulary; `protocol` owns them so `parse_manifest` need
// not reach into the adapter layer that interprets them. Re-exported here
// because a provider's own code is where a reader looks for them.
pub use crate::protocol::{HookRegistration, NativeHookDeclaration};

#[derive(Debug, Serialize)]
pub struct NativeHookSpec {
    pub native_event: String,
    pub arguments: Vec<String>,
    pub registration: HookRegistration,
}

#[derive(Debug, Serialize)]
pub struct HookDescription {
    pub manifest_schema: u64,
    pub wire_version: u64,
    pub record_schema: u64,
    pub writer_version: String,
    pub provider: Provider,
    pub native_hooks: Vec<NativeHookSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_entrypoint: Option<String>,
}

pub fn describe_hooks(provider: &str) -> crate::protocol::Result<HookDescription> {
    let protocol = manifest()?;
    let unsupported = || {
        AttentionError::usage(format!(
            "--provider is not supported; expected one of: {}",
            protocol
                .enums
                .providers
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    let hooks = protocol
        .native_hooks
        .get(provider)
        .ok_or_else(unsupported)?;
    let provider = Provider::parse(provider).ok_or_else(unsupported)?;
    let name = provider.as_str();
    Ok(HookDescription {
        manifest_schema: protocol.manifest_schema,
        wire_version: protocol.wire_version,
        record_schema: protocol.record_schema,
        writer_version: protocol.writer_version.clone(),
        provider,
        extension_entrypoint: (provider == Provider::Pi).then(|| "pi/index.ts".into()),
        native_hooks: hooks
            .iter()
            .map(|(event, declaration)| NativeHookSpec {
                native_event: declaration.native_event.clone(),
                arguments: vec!["hooks".into(), "event".into(), name.into(), event.clone()],
                registration: declaration.registration,
            })
            .collect(),
    })
}

fn declared_hook(provider: Provider, event: &str) -> bool {
    manifest().is_ok_and(|protocol| {
        protocol
            .native_hooks
            .get(provider.as_str())
            .is_some_and(|hooks| hooks.contains_key(event))
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAction {
    Binding,
    Activity,
    ParentStop,
    ChildActive,
    ChildStopped,
    End,
    Review,
    Clear,
    #[default]
    Ignored,
    Observation,
}

impl ProviderAction {
    pub const ALL: [Self; 10] = [
        Self::Binding,
        Self::Activity,
        Self::ParentStop,
        Self::ChildActive,
        Self::ChildStopped,
        Self::End,
        Self::Review,
        Self::Clear,
        Self::Ignored,
        Self::Observation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binding => "binding",
            Self::Activity => "activity",
            Self::ParentStop => "parent_stop",
            Self::ChildActive => "child_active",
            Self::ChildStopped => "child_stopped",
            Self::End => "end",
            Self::Review => "review",
            Self::Clear => "clear",
            Self::Ignored => "ignored",
            Self::Observation => "observation",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderEvent {
    pub source_event: String,
    pub observation: Option<LifecycleObservation>,
    pub observation_diagnostic: Option<Diagnostic>,
    pub action: ProviderAction,
    pub provider: Option<Provider>,
    pub provider_session_id: Option<String>,
    pub start_source: Option<String>,
    pub activity_type: Option<String>,
    pub label: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// The ids of the tasks the provider lists as in flight when the lead
    /// stops. `None` when the event carries no list, or one that cannot be
    /// read in full.
    pub in_flight_task_ids: Option<Vec<String>>,
    /// Child ids explicitly stopped by a tool call (such as an `agy`
    /// `manage_subagents` `kill`).
    pub stopped_child_ids: Vec<String>,
    /// Starting step count reported on an `agy` lead `PreInvocation`, used by
    /// the turn follower to distinguish cancellations in the current turn from
    /// earlier turns.
    pub initial_step_count: Option<i64>,
    /// Whether this event is a lead user prompt (`UserPromptSubmit` for
    /// Claude/Codex, or a confirmed lead `PreInvocation(invocationNum: 0)` for
    /// Agy), independent of whether optional observation correlation fields
    /// parsed cleanly.
    pub is_lead_prompt: bool,
    /// Whether this event is a confirmed intra-turn thinking continuation or
    /// non-prompt wakeup (Agy `PreInvocation(invocationNum > 0)`, Agy
    /// `PreInvocation(invocationNum: 0)` whose transcript confirms a non-prompt
    /// wakeup, Agy lead `PostToolUse` for `manage_subagents` `kill`/`kill_all`,
    /// or Agy `Stop(fullyIdle: false)`).
    pub continues_turn: bool,
    /// Whether this event may reuse-or-claim a self-owned pane claim and create
    /// or replace the binding lazily without a preceding `SessionStart` (Agy
    /// lead `PreInvocation` carrying a valid `transcriptPath`, since as of
    /// `agy` 1.5.16 `agy --resume` skips `SessionStart`).
    pub lazy_binding: bool,
    /// The questions a Codex lead prompt answers. Only their ids are taken
    /// from the prompt; its text is never kept.
    pub answered_questions: Vec<crate::observations::AnsweredQuestion>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub config_dir: Option<String>,
    pub model: Option<String>,
    pub diagnostic: Option<Diagnostic>,
    /// Why the event was skipped on purpose; never set together with
    /// `diagnostic`.
    pub skip_reason: Option<String>,
}

impl ProviderEvent {
    fn ignored(provider: Option<Provider>, code: DiagnosticCode, message: &str) -> Self {
        Self {
            diagnostic: Some(Diagnostic::new(code, message)),
            ..Self::inert(provider)
        }
    }

    /// An event the integration knows and deliberately does nothing with:
    /// nothing went wrong, so it says why without a diagnostic.
    fn skipped(provider: Provider, reason: &str) -> Self {
        Self {
            skip_reason: Some(reason.to_owned()),
            ..Self::inert(Some(provider))
        }
    }

    fn inert(provider: Option<Provider>) -> Self {
        Self {
            provider,
            ..Self::default()
        }
    }

    /// The end of a Codex lead turn whose rollout says it ended on a model
    /// error, which as of Codex 0.162.0 runs no hook: the turn needs the
    /// user, and its attempt failed. `error_code` is Codex's name for the
    /// error; its message is never kept.
    pub fn codex_turn_failure(session_id: &str, turn_id: &str, error_code: Option<String>) -> Self {
        Self {
            source_event: crate::codex_rollout::TASK_COMPLETE.to_owned(),
            action: ProviderAction::Activity,
            provider: Some(Provider::Codex),
            provider_session_id: Some(session_id.to_owned()),
            activity_type: Some("notify".to_owned()),
            label: error_code.clone(),
            observation: Some(LifecycleObservation {
                turn_end: None,
                observation_id: uuid::Uuid::new_v4().to_string(),
                source_event: crate::codex_rollout::TASK_COMPLETE.to_owned(),
                // As of Codex 0.162.0 a resumed rollout's header names the
                // Codex that created the session, not the one that wrote
                // this end.
                source_version: None,
                observed_mono_ns: "00000000000000000000".to_owned(),
                written_at_unix_ns: "00000000000000000000".to_owned(),
                actor: Actor::Lead,
                correlation: Some(NativeCorrelation {
                    turn_id: Some(turn_id.to_owned()),
                    ..NativeCorrelation::default()
                }),
                body: ObservationBody::AttemptOutcome {
                    outcome: AttemptOutcome::Failed,
                    error_category: None,
                    error_code,
                },
            }),
            ..Self::default()
        }
    }

    fn turn_interrupt(
        provider: Provider,
        source_event: &str,
        session_id: &str,
        version: Option<String>,
    ) -> Self {
        Self {
            source_event: source_event.to_owned(),
            action: ProviderAction::Clear,
            provider: Some(provider),
            provider_session_id: Some(session_id.to_owned()),
            observation: Some(LifecycleObservation {
                turn_end: None,
                observation_id: uuid::Uuid::new_v4().to_string(),
                source_event: source_event.to_owned(),
                source_version: version,
                observed_mono_ns: "00000000000000000000".to_owned(),
                written_at_unix_ns: "00000000000000000000".to_owned(),
                actor: Actor::Lead,
                correlation: None,
                body: ObservationBody::UserInterrupt,
            }),
            ..Self::default()
        }
    }

    /// The end of a Claude Code lead turn the user stopped with Esc, which
    /// as of Claude Code 2.1.300 runs no hook: the session's status file went
    /// from a working turn to none. Nothing is left to report, so the
    /// activity is cleared, as a Codex `Interrupt` clears it. `version` is
    /// the Claude Code version the file names.
    pub fn claude_turn_interrupt(session_id: &str, version: Option<String>) -> Self {
        Self::turn_interrupt(
            Provider::Claude,
            crate::claude_status::SESSION_STATUS,
            session_id,
            version,
        )
    }

    /// The end of an `agy` lead turn the user stopped with Esc, which as of
    /// `agy` 1.5.16 runs no `Stop` hook: the conversation's local status
    /// databases went from working to idle or recorded a canceled step for the
    /// current turn. Clears the activity, as a Codex `Interrupt` or Claude
    /// status interrupt clears it.
    pub fn agy_turn_interrupt(session_id: &str) -> Self {
        Self::turn_interrupt(
            Provider::Agy,
            crate::agy_status::AGY_STATUS,
            session_id,
            None,
        )
    }
}

fn safe_label(value: &Value, field: &str) -> std::result::Result<String, Diagnostic> {
    let Some(text) = value.as_str() else {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{field} is missing or too long"),
        ));
    };
    safe_text(text, field)
}

/// `text` when it is a safe label: non-empty, within the manifest's bound,
/// and free of control characters.
pub(crate) fn safe_text(text: &str, field: &str) -> std::result::Result<String, Diagnostic> {
    let maximum = manifest()
        .map_err(|error| error.diagnostic)?
        .limits
        .safe_label_max_bytes;
    if text.is_empty() || text.len() > maximum || !free_of_control(text) {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{field} is missing or too long"),
        ));
    }
    Ok(text.to_owned())
}

fn optional_label(payload: &Value, field: &str) -> std::result::Result<Option<String>, Diagnostic> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => safe_label(value, field).map(Some),
    }
}

/// Optional metadata that fails its check is dropped, not fatal: the event
/// still means what its name says. Each dropped field is remembered so one
/// diagnostic can name them all.
#[derive(Default)]
struct DroppedFields(Vec<&'static str>);

impl DroppedFields {
    fn keep(
        &mut self,
        field: &'static str,
        checked: std::result::Result<Option<String>, Diagnostic>,
    ) -> Option<String> {
        checked.unwrap_or_else(|_| {
            self.0.push(field);
            None
        })
    }

    fn diagnostic(self) -> Option<Diagnostic> {
        if self.0.is_empty() {
            return None;
        }
        Some(
            Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                format!("optional fields were dropped: {}", self.0.join(", ")),
            )
            .with("dropped_fields", serde_json::json!(self.0)),
        )
    }
}

fn optional_path(payload: &Value, field: &str) -> std::result::Result<Option<String>, Diagnostic> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(path) = value.as_str() else {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{field} must be absolute"),
        ));
    };
    let maximum = manifest()
        .map_err(|error| error.diagnostic)?
        .limits
        .path_max_bytes;
    if path.is_empty()
        || path.len() > maximum
        || !free_of_control(path)
        || !Path::new(path).is_absolute()
    {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{field} must be absolute"),
        ));
    }
    Ok(Some(path.to_owned()))
}

fn environment_path(
    env: &BTreeMap<String, String>,
    field: &str,
) -> std::result::Result<Option<String>, Diagnostic> {
    let Some(value) = env.get(field) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len()
        > manifest()
            .map_err(|error| error.diagnostic)?
            .limits
            .path_max_bytes
        || !free_of_control(value)
        || !Path::new(value).is_absolute()
    {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{field} must be absolute"),
        ));
    }
    Ok(Some(value.clone()))
}

fn parse_provider_common(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    env: &BTreeMap<String, String>,
) -> std::result::Result<ProviderEvent, Diagnostic> {
    let provider_session_id = safe_label(&payload["session_id"], "session_id")?;
    let transcript_field = if provider == Provider::Pi {
        "session_file"
    } else {
        "transcript_path"
    };
    let config_field = match provider {
        Provider::Claude => "CLAUDE_CONFIG_DIR",
        Provider::Codex => "CODEX_HOME",
        Provider::Pi => "PI_CODING_AGENT_DIR",
        Provider::Agy => unreachable!("agy uses parse_agy"),
    };
    let mut dropped = DroppedFields::default();
    // Claude Code 2.1.283 sends an empty `agent_type` for the agents it runs
    // for itself; that says no type, and is not malformed.
    let agent_type = match payload.get("agent_type") {
        Some(Value::String(text)) if text.is_empty() => None,
        _ => dropped.keep("agent_type", optional_label(payload, "agent_type")),
    };
    let transcript_path = dropped.keep(transcript_field, optional_path(payload, transcript_field));
    let cwd = dropped.keep("cwd", optional_path(payload, "cwd"));
    let config_dir = dropped.keep(config_field, environment_path(env, config_field));
    let model = dropped.keep("model", optional_label(payload, "model"));
    // Only Pi's bus carries a badge label.
    let label = if provider == Provider::Pi && event_name == "bus" {
        dropped.keep("label", optional_label(payload, "label"))
    } else {
        None
    };
    let event = ProviderEvent {
        provider_session_id: Some(provider_session_id),
        label,
        // Checked before dispatch: a bad child identity ignores the event.
        agent_id: payload
            .get("agent_id")
            .and_then(|value| safe_label(value, "agent_id").ok()),
        agent_type,
        transcript_path,
        cwd,
        config_dir,
        model,
        diagnostic: dropped.diagnostic(),
        ..ProviderEvent::inert(Some(provider))
    };
    Ok(event)
}

/// The ids of the tasks a Claude Code `Stop` lists in `background_tasks` as in
/// flight. Claude Code 2.1.284 sends the list, sub-agents under the id their
/// hooks carry, and its hooks reference has documented it since 2.1.145.
/// `None` unless the field is an array whose every entry has a string `id`: a
/// list that cannot be read in full says nothing about which children ended.
fn listed_task_ids(payload: &Value) -> Option<Vec<String>> {
    payload
        .get("background_tasks")?
        .as_array()?
        .iter()
        .map(|task| task.get("id")?.as_str().map(str::to_owned))
        .collect()
}

fn parse_claude_or_codex(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    env: &BTreeMap<String, String>,
) -> ProviderEvent {
    if provider == Provider::Claude
        && env
            .get("CLAUDE_JOB_DIR")
            .is_some_and(|value| !value.is_empty())
    {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::ClaimStale,
            "Claude background job is not pane authority",
        );
    }
    if provider == Provider::Claude
        && env
            .get("CURSOR_AGENT")
            .is_some_and(|value| !value.is_empty())
    {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::ClaimStale,
            "Cursor-owned Claude invocation is not pane authority",
        );
    }
    if !declared_hook(provider, event_name) {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::IntegrationVersionMismatch,
            "provider event is not supported",
        );
    }
    if payload.get("hook_event_name").and_then(Value::as_str) != Some(event_name) {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::RecordInvalid,
            "hook event name does not match the callback",
        );
    }
    let mut event = match parse_provider_common(provider, event_name, payload, env) {
        Ok(event) => event,
        Err(diagnostic) => {
            let mut event =
                ProviderEvent::ignored(Some(provider), diagnostic.code, &diagnostic.message);
            event.diagnostic = Some(diagnostic);
            return event;
        }
    };
    if provider == Provider::Codex
        && let Some(inherited) = env.get("CODEX_THREAD_ID").filter(|value| !value.is_empty())
        && event.provider_session_id.as_deref() != Some(inherited)
    {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::ClaimStale,
            "Codex hook identity differs from the inherited thread",
        );
    }
    // A lead prompt starts the turn, so it tints the pane the way Pi's agent_start
    // does. Without it a turn that answers without calling a tool shows nothing
    // between the prompt and its Stop. A child actor cannot write lead state, so
    // its prompt stays observation-only.
    if event_name == "UserPromptSubmit" && event.agent_id.is_none() {
        event.action = ProviderAction::Activity;
        event.activity_type = Some("thinking".to_owned());
        event.is_lead_prompt = true;
        return event;
    }
    // Claude sends StopFailure in place of Stop when an API error ends the
    // turn, and Codex runs no Stop after an interrupt. Either way the turn is
    // over and the prompt's thinking must not outlive it: a failed turn needs
    // the user, and an interrupted one has nothing left to report.
    if event.agent_id.is_none() {
        match event_name {
            "StopFailure" => {
                event.action = ProviderAction::Activity;
                event.activity_type = Some("notify".to_owned());
                return event;
            }
            "Interrupt" => {
                event.action = ProviderAction::Clear;
                return event;
            }
            _ => {}
        }
    }
    if matches!(
        event_name,
        "PostToolUse"
            | "UserPromptSubmit"
            | "PreCompact"
            | "PostCompact"
            | "StopFailure"
            | "Interrupt"
            | "PostToolUseFailure"
            | "PermissionDenied"
            | "Elicitation"
            | "ElicitationResult"
    ) {
        if event_name == "Interrupt" && event.agent_id.is_some() {
            return ProviderEvent::ignored(
                Some(provider),
                DiagnosticCode::RecordInvalid,
                "Interrupt is a root-turn observation",
            );
        }
        event.action = ProviderAction::Observation;
        return event;
    }
    if event_name == "SubagentStart" {
        if event.agent_id.is_none() {
            return ProviderEvent::ignored(
                Some(provider),
                DiagnosticCode::RecordInvalid,
                "SubagentStart requires a valid agent_id",
            );
        }
        event.action = ProviderAction::ChildActive;
        return event;
    }
    if event_name == "SubagentStop" {
        if event.agent_id.is_none() {
            return ProviderEvent::ignored(
                Some(provider),
                DiagnosticCode::RecordInvalid,
                "SubagentStop requires a valid agent_id",
            );
        }
        event.action = ProviderAction::ChildStopped;
        return event;
    }
    if event_name == "PreToolUse" && event.agent_id.is_some() {
        event.action = ProviderAction::ChildActive;
        return event;
    }
    // A child blocked on a permission prompt waits for the user as the lead
    // would, and Codex has no Notification hook to say so another way. The
    // lifecycle also marks the child waiting in the binding's child set, so
    // the lead's own tool calls do not repaint the notify while it waits.
    if event_name == "PermissionRequest" && event.agent_id.is_some() {
        event.action = ProviderAction::Activity;
        event.activity_type = Some("notify".to_owned());
        return event;
    }
    if event.agent_id.is_some() {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::ClaimStale,
            "child callback cannot write lead state",
        );
    }
    match event_name {
        "SessionStart" => {
            let Some(source) = payload.get("source").and_then(Value::as_str) else {
                return ProviderEvent::ignored(
                    Some(provider),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "SessionStart source is not supported",
                );
            };
            if !["startup", "resume", "clear", "compact", "fork"].contains(&source) {
                return ProviderEvent::ignored(
                    Some(provider),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "SessionStart source is not supported",
                );
            }
            event.action = ProviderAction::Binding;
            event.start_source = Some(source.to_owned());
        }
        "SessionEnd" => {
            let reason = payload.get("reason").and_then(Value::as_str);
            if ![
                "clear",
                "resume",
                "logout",
                "prompt_input_exit",
                "bypass_permissions_disabled",
                "other",
            ]
            .contains(&reason.unwrap_or(""))
            {
                return ProviderEvent::ignored(
                    Some(provider),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "SessionEnd reason is not supported",
                );
            }
            event.action = ProviderAction::End;
        }
        "PreToolUse" => {
            let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) else {
                return ProviderEvent::ignored(
                    Some(provider),
                    DiagnosticCode::RecordInvalid,
                    "PreToolUse requires tool_name",
                );
            };
            event.action = ProviderAction::Activity;
            event.activity_type = Some(
                if provider == Provider::Codex
                    && ["request_user_input", "request_permissions"].contains(&tool_name)
                {
                    "notify"
                } else {
                    "thinking"
                }
                .to_owned(),
            );
        }
        "PermissionRequest" => {
            event.action = ProviderAction::Activity;
            event.activity_type = Some("notify".to_owned());
        }
        "Notification" => {
            let notification = payload.get("notification_type").and_then(Value::as_str);
            if notification == Some("elicitation_url_dialog") {
                event.action = ProviderAction::Observation;
                return event;
            }
            if notification == Some("idle_prompt") {
                return ProviderEvent::skipped(
                    provider,
                    "idle prompt does not replace terminal activity",
                );
            }
            if !["permission_prompt", "auth_success", "elicitation_dialog"]
                .contains(&notification.unwrap_or(""))
            {
                return ProviderEvent::ignored(
                    Some(provider),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "notification type is not supported",
                );
            }
            event.action = ProviderAction::Activity;
            event.activity_type = Some("notify".to_owned());
        }
        "Stop" => {
            event.action = if provider == Provider::Codex {
                ProviderAction::ParentStop
            } else {
                ProviderAction::Activity
            };
            event.activity_type = Some("stop".to_owned());
            if provider == Provider::Claude && event.agent_id.is_none() {
                event.in_flight_task_ids = listed_task_ids(payload);
            }
        }
        _ => {
            return ProviderEvent::ignored(
                Some(provider),
                DiagnosticCode::IntegrationVersionMismatch,
                "provider event has no transition",
            );
        }
    }
    event
}

fn parse_pi(event_name: &str, payload: &Value, env: &BTreeMap<String, String>) -> ProviderEvent {
    if !declared_hook(Provider::Pi, event_name) {
        return ProviderEvent::ignored(
            Some(Provider::Pi),
            DiagnosticCode::IntegrationVersionMismatch,
            "Pi event is not supported",
        );
    }
    let mut event = match parse_provider_common(Provider::Pi, event_name, payload, env) {
        Ok(event) => event,
        Err(diagnostic) => {
            let mut event =
                ProviderEvent::ignored(Some(Provider::Pi), diagnostic.code, &diagnostic.message);
            event.diagnostic = Some(diagnostic);
            return event;
        }
    };
    match event_name {
        "input" | "session_before_compact" | "session_compact" => {
            event.action = ProviderAction::Observation
        }
        "tool_execution_end" => event.action = ProviderAction::Observation,
        "message_end" => match (
            payload.get("role").and_then(Value::as_str),
            payload.get("stop_reason").and_then(Value::as_str),
        ) {
            (Some("assistant"), Some("error" | "aborted")) => {
                event.action = ProviderAction::Observation
            }
            // Pi 0.85.1 sends message_end for every message, prompts and tool
            // results included, and its StopReason type lists these besides
            // error and aborted.
            (Some(role), stop)
                if role != "assistant"
                    || matches!(
                        stop,
                        Some("stop" | "length" | "toolUse" | "pending" | "deferred")
                    ) =>
            {
                return ProviderEvent::skipped(
                    Provider::Pi,
                    "Pi message is not a failed or aborted reply",
                );
            }
            _ => {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "Pi message role or stop reason is not supported",
                );
            }
        },
        "session_start" => {
            let source = payload
                .get("start_source")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !["startup", "reload", "new", "resume", "fork"].contains(&source) {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "Pi session start source is not supported",
                );
            }
            event.action = ProviderAction::Binding;
            event.start_source = Some(source.to_owned());
        }
        "session_shutdown" => {
            let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
            if reason == "reload" {
                return ProviderEvent::skipped(Provider::Pi, "Pi reload keeps the current binding");
            }
            if !["quit", "new", "resume", "fork"].contains(&reason) {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    DiagnosticCode::IntegrationVersionMismatch,
                    "Pi shutdown reason is not supported",
                );
            }
            event.action = ProviderAction::End;
        }
        "agent_start" | "tool_execution_start" => {
            event.action = ProviderAction::Activity;
            event.activity_type = Some("thinking".to_owned());
        }
        "agent_settled" => {
            event.action = ProviderAction::Activity;
            event.activity_type = Some("stop".to_owned());
        }
        "agent_end" => {
            return ProviderEvent::skipped(Provider::Pi, "Pi agent_end is not terminal");
        }
        "bus" => match payload.get("state").and_then(Value::as_str) {
            Some("review") => event.action = ProviderAction::Review,
            Some("clear") => event.action = ProviderAction::Clear,
            Some(state @ ("thinking" | "stop" | "notify")) => {
                event.action = ProviderAction::Activity;
                event.activity_type = Some(state.to_owned());
            }
            _ => {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    DiagnosticCode::RecordInvalid,
                    "Pi bus state is invalid",
                );
            }
        },
        _ => {}
    }
    event
}

fn nonempty_optional_label(
    value: Option<&Value>,
    field: &str,
) -> std::result::Result<Option<String>, Diagnostic> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(value) => safe_label(value, field).map(Some),
    }
}

fn optional_bool(
    payload: &Value,
    field: &str,
    message: &str,
) -> std::result::Result<Option<bool>, Diagnostic> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(Diagnostic::new(DiagnosticCode::RecordInvalid, message)),
    }
}

fn draft_observation(
    event_name: &str,
    observation_id: String,
    agent_id: Option<&str>,
    correlation: NativeCorrelation,
    body: ObservationBody,
) -> LifecycleObservation {
    LifecycleObservation {
        turn_end: None,
        observation_id,
        source_event: event_name.to_owned(),
        source_version: None,
        observed_mono_ns: "00000000000000000000".to_owned(),
        written_at_unix_ns: "00000000000000000000".to_owned(),
        actor: match agent_id {
            Some(id) => Actor::Child {
                agent_id: id.to_owned(),
                agent_key: crate::protocol::sha256_hex(id.as_bytes()),
            },
            None => Actor::Lead,
        },
        correlation: (correlation != NativeCorrelation::default()).then_some(correlation),
        body,
    }
}

fn agy_observation(
    event_name: &str,
    agent_id: Option<&str>,
    tool_call_id: Option<String>,
    turn_id: Option<String>,
    body: ObservationBody,
) -> LifecycleObservation {
    draft_observation(
        event_name,
        uuid::Uuid::new_v4().to_string(),
        agent_id,
        NativeCorrelation {
            tool_call_id,
            turn_id,
            ..NativeCorrelation::default()
        },
        body,
    )
}

struct AgyToolCall<'a> {
    object: &'a serde_json::Map<String, Value>,
    name: String,
    id: std::result::Result<Option<String>, Diagnostic>,
}

fn agy_tool_call<'a>(
    payload: &'a Value,
    event_name: &str,
) -> std::result::Result<AgyToolCall<'a>, Diagnostic> {
    let Some(tool_call) = payload.get("toolCall").and_then(Value::as_object) else {
        return Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            format!("{event_name} requires toolCall"),
        ));
    };
    let tool_name = match tool_call.get("name") {
        Some(value) => safe_label(value, "toolCall.name")?,
        None => {
            return Err(Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                format!("{event_name} requires toolCall.name"),
            ));
        }
    };
    let tool_call_id = nonempty_optional_label(tool_call.get("id"), "id");
    Ok(AgyToolCall {
        object: tool_call,
        name: tool_name,
        id: tool_call_id,
    })
}

fn agy_post_tool_error(payload: &Value) -> std::result::Result<Option<bool>, Diagnostic> {
    match payload.get("error") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(_)) => Ok(Some(true)),
        Some(_) => Err(Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            "PostToolUse error field is invalid",
        )),
    }
}

fn agy_manage_subagents_stops(
    tool_call: &serde_json::Map<String, Value>,
    is_lead: bool,
) -> (Option<Vec<String>>, Vec<String>) {
    let Some(args) = tool_call.get("args").and_then(Value::as_object) else {
        return (None, Vec::new());
    };
    match args.get("Action").and_then(Value::as_str) {
        Some("kill_all") if is_lead => (Some(Vec::new()), Vec::new()),
        Some("kill") => {
            let ids = args
                .get("ConversationIds")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| safe_label(item, "ConversationIds").ok())
                        .collect()
                })
                .unwrap_or_default();
            (None, ids)
        }
        _ => (None, Vec::new()),
    }
}

fn agy_ignored(diagnostic: Diagnostic) -> ProviderEvent {
    ProviderEvent {
        diagnostic: Some(diagnostic),
        ..ProviderEvent::inert(Some(Provider::Agy))
    }
}

fn parse_agy(event_name: &str, payload: &Value) -> ProviderEvent {
    if !declared_hook(Provider::Agy, event_name) {
        return ProviderEvent::ignored(
            Some(Provider::Agy),
            DiagnosticCode::IntegrationVersionMismatch,
            "provider event is not supported",
        );
    }
    if payload.get("hookEventName").and_then(Value::as_str) != Some(event_name) {
        return ProviderEvent::ignored(
            Some(Provider::Agy),
            DiagnosticCode::RecordInvalid,
            "hook event name does not match the callback",
        );
    }
    let conversation_id = match safe_label(&payload["conversationId"], "conversationId") {
        Ok(id) => id,
        Err(diagnostic) => return agy_ignored(diagnostic),
    };
    let mut dropped = DroppedFields::default();
    let transcript_checked = optional_path(payload, "transcriptPath");
    let transcript_provided = !matches!(transcript_checked, Ok(None));
    let transcript_path = dropped.keep("transcriptPath", transcript_checked);
    let cwd = dropped.keep("cwd", optional_path(payload, "cwd"));
    let model = dropped.keep("modelName", optional_label(payload, "modelName"));
    let artifact_dir = dropped.keep(
        "artifactDirectoryPath",
        optional_path(payload, "artifactDirectoryPath"),
    );

    // As of agy 1.5.16, subagent hooks omit parent/root ids on stdin and carry
    // an empty `lastUserInput`, while lead hooks carry the active prompt. Walk
    // `.system_generated/messages/*.json` upward from `artifactDirectoryPath`
    // to link subagents (including nested grandchildren) to their root lead.
    let has_user_input = payload
        .get("lastUserInput")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty());

    let (provider_session_id, agent_id, agent_type) = if has_user_input {
        (conversation_id, None, None)
    } else if let Some(lineage) = artifact_dir.as_deref().and_then(|dir| {
        crate::agy_status::resolve_subagent_lineage(&conversation_id, Path::new(dir))
    }) {
        (
            lineage.root_session_id,
            Some(conversation_id),
            lineage.role.or(lineage.type_name),
        )
    } else {
        // Skip unlinked empty-input events before binding admission so they
        // can never hijack the pane's active lead binding.
        return ProviderEvent::skipped(
            Provider::Agy,
            "agy event has empty lastUserInput and no resolved parent session",
        );
    };

    let is_child = agent_id.is_some();
    let execution_id = if is_child {
        Ok(None)
    } else {
        nonempty_optional_label(payload.get("executionId"), "executionId")
    };
    let mut event = ProviderEvent {
        provider_session_id: Some(provider_session_id),
        agent_id,
        agent_type,
        transcript_path,
        cwd,
        model,
        diagnostic: dropped.diagnostic(),
        ..ProviderEvent::inert(Some(Provider::Agy))
    };

    match event_name {
        "SessionStart" if is_child => {
            event.action = ProviderAction::ChildActive;
        }
        "SessionStart" => {
            event.action = ProviderAction::Binding;
            event.start_source = Some("unspecified".to_owned());
        }
        "PreInvocation" if is_child => {
            event.action = ProviderAction::ChildActive;
        }
        "PreInvocation" => {
            let Some(invocation_num) = payload.get("invocationNum").and_then(Value::as_u64) else {
                return ProviderEvent::ignored(
                    Some(Provider::Agy),
                    DiagnosticCode::RecordInvalid,
                    "PreInvocation requires invocationNum",
                );
            };
            event.initial_step_count = payload
                .get("initialNumSteps")
                .and_then(Value::as_i64)
                .filter(|count| *count >= 0);
            event.lazy_binding = event.transcript_path.is_some();
            event.action = ProviderAction::Activity;
            event.activity_type = Some("thinking".to_owned());
            if invocation_num == 0 {
                // Fail closed whenever the payload names a transcript path that
                // was rejected, unreadable, symlinked, or does not confirm a
                // fresh USER_INPUT turn; only truly absent transcriptPath
                // (synthetic payloads without a transcript) defaults to prompt.
                let prompt_check = if !transcript_provided {
                    Some(true)
                } else {
                    event.transcript_path.as_deref().and_then(|path| {
                        crate::agy_status::is_user_prompt_invocation(Path::new(path))
                    })
                };
                match prompt_check {
                    Some(true) => {
                        event.is_lead_prompt = true;
                        match execution_id {
                            Ok(turn_id) => {
                                event.observation = Some(agy_observation(
                                    event_name,
                                    None,
                                    None,
                                    turn_id,
                                    ObservationBody::PromptSubmitted { input_source: None },
                                ));
                            }
                            Err(problem) => event.observation_diagnostic = Some(problem),
                        }
                    }
                    Some(false) => {
                        event.continues_turn = true;
                    }
                    None => {}
                }
            } else {
                event.continues_turn = true;
            }
        }
        "PreToolUse" => {
            let AgyToolCall {
                name: tool_name,
                id: tool_call_id,
                ..
            } = match agy_tool_call(payload, event_name) {
                Ok(parsed) => parsed,
                Err(diagnostic) => return agy_ignored(diagnostic),
            };
            let (tool_class, question_mode) = classify_tool("agy", &tool_name);
            if question_mode == Some(QuestionMode::Blocking) {
                event.action = ProviderAction::Activity;
                event.activity_type = Some("notify".to_owned());
            } else if is_child {
                event.action = ProviderAction::ChildActive;
            } else {
                event.action = ProviderAction::Activity;
                event.activity_type = Some("thinking".to_owned());
            }
            match (tool_call_id, execution_id) {
                (Ok(tool_call_id), Ok(turn_id)) => {
                    event.observation = Some(agy_observation(
                        event_name,
                        event.agent_id.as_deref(),
                        tool_call_id,
                        turn_id,
                        ObservationBody::ToolPreflight {
                            tool_name,
                            tool_class,
                            question_mode,
                        },
                    ));
                }
                (Err(problem), _) | (_, Err(problem)) => {
                    event.observation_diagnostic = Some(problem);
                }
            }
        }
        "PostToolUse" => {
            let AgyToolCall {
                object: tool_call,
                name: tool_name,
                id: tool_call_id,
            } = match agy_tool_call(payload, event_name) {
                Ok(parsed) => parsed,
                Err(diagnostic) => return agy_ignored(diagnostic),
            };
            let (tool_class, question_mode) = classify_tool("agy", &tool_name);
            let error_flag = agy_post_tool_error(payload);
            if tool_name == "manage_subagents" && matches!(error_flag, Ok(None)) {
                let (in_flight, stopped) = agy_manage_subagents_stops(tool_call, !is_child);
                event.in_flight_task_ids = in_flight;
                event.stopped_child_ids = stopped;
            }
            let stops_children =
                event.in_flight_task_ids.is_some() || !event.stopped_child_ids.is_empty();
            if question_mode == Some(QuestionMode::Blocking) || stops_children {
                if is_child {
                    event.action = ProviderAction::ChildActive;
                } else {
                    event.action = ProviderAction::Activity;
                    event.activity_type = Some("thinking".to_owned());
                    if stops_children {
                        event.continues_turn = true;
                    }
                }
            } else {
                event.action = ProviderAction::Observation;
            }
            match (tool_call_id, execution_id, error_flag) {
                (Ok(tool_call_id), Ok(turn_id), Ok(is_error)) => {
                    event.observation = Some(agy_observation(
                        event_name,
                        event.agent_id.as_deref(),
                        tool_call_id,
                        turn_id,
                        ObservationBody::ToolResult {
                            tool_name,
                            tool_class,
                            question_mode,
                            result_surface: ResultSurface::PostHook,
                            is_error,
                            interrupted: None,
                        },
                    ));
                }
                (Err(problem), _, _) | (_, Err(problem), _) | (_, _, Err(problem)) => {
                    event.observation_diagnostic = Some(problem);
                }
            }
        }
        "Stop" => {
            let fully_idle =
                match optional_bool(payload, "fullyIdle", "Stop fullyIdle flag is invalid") {
                    Ok(flag) => flag,
                    Err(diagnostic) => return agy_ignored(diagnostic),
                };
            if is_child {
                event.action = if fully_idle == Some(false) {
                    ProviderAction::ChildActive
                } else {
                    ProviderAction::ChildStopped
                };
                return event;
            }
            // As of agy 1.5.16, `fullyIdle: false` fires when the lead pauses
            // while background subagents or tasks are still running; keep the
            // turn open as thinking with no turn-end observation.
            if fully_idle == Some(false) {
                event.action = ProviderAction::Activity;
                event.activity_type = Some("thinking".to_owned());
                event.continues_turn = true;
                return event;
            }
            let continuation_active = match optional_bool(
                payload,
                "continuationActive",
                "Stop continuationActive flag is invalid",
            ) {
                Ok(flag) => flag.unwrap_or(false),
                Err(diagnostic) => return agy_ignored(diagnostic),
            };
            let execution_num = match payload.get("executionNum") {
                None | Some(Value::Null) => 0,
                Some(value) => match value.as_u64() {
                    Some(num) => num,
                    None => {
                        return ProviderEvent::ignored(
                            Some(Provider::Agy),
                            DiagnosticCode::RecordInvalid,
                            "Stop executionNum is invalid",
                        );
                    }
                },
            };
            let reason = payload.get("terminationReason").and_then(Value::as_str);
            let body = match reason {
                Some("NO_TOOL_CALL" | "TERMINAL_STEP_TYPE" | "INJECTED_RESPONSE") => {
                    event.action = ProviderAction::Activity;
                    event.activity_type = Some("stop".to_owned());
                    ObservationBody::ResponseFinished {
                        // As of agy 1.5.16, `continuationActive` stayed false
                        // on a Stop hook continuation while `executionNum`
                        // incremented to 1.
                        stop_hook_active: Some(continuation_active || execution_num > 0),
                    }
                }
                Some("USER_CANCELED") => {
                    event.action = ProviderAction::Clear;
                    ObservationBody::UserInterrupt
                }
                Some(
                    code @ ("ERROR"
                    | "MAX_INVOCATIONS"
                    | "MAX_FORCED_INVOCATIONS"
                    | "MAX_TOKEN_BUDGET_EXCEEDED"
                    | "TERMINAL_CUSTOM_HOOK"),
                ) => {
                    event.action = ProviderAction::Activity;
                    event.activity_type = Some("notify".to_owned());
                    event.label = Some(code.to_owned());
                    ObservationBody::AttemptOutcome {
                        outcome: AttemptOutcome::Failed,
                        error_category: None,
                        error_code: Some(code.to_owned()),
                    }
                }
                _ => {
                    return ProviderEvent::ignored(
                        Some(Provider::Agy),
                        DiagnosticCode::RecordInvalid,
                        "Stop terminationReason is missing or unsupported",
                    );
                }
            };
            event.in_flight_task_ids = Some(Vec::new());
            match execution_id {
                Ok(turn_id) => {
                    event.observation =
                        Some(agy_observation(event_name, None, None, turn_id, body));
                }
                Err(problem) => event.observation_diagnostic = Some(problem),
            }
        }
        _ => {
            return ProviderEvent::ignored(
                Some(Provider::Agy),
                DiagnosticCode::IntegrationVersionMismatch,
                "provider event has no transition",
            );
        }
    }
    event
}

pub fn parse_provider_event(
    provider_name: &str,
    event_name: &str,
    payload: &Value,
    env: &BTreeMap<String, String>,
) -> ProviderEvent {
    let Some(provider) = Provider::parse(provider_name) else {
        return ProviderEvent::ignored(
            None,
            DiagnosticCode::IntegrationVersionMismatch,
            "provider is not supported",
        );
    };
    if !payload.is_object() {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::RecordInvalid,
            "provider payload is not an object",
        );
    }
    // Ownership is validated before the legacy dispatcher can choose lead state.
    if let Some(agent) = payload.get("agent_id")
        && safe_label(agent, "agent_id").is_err()
    {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::RecordInvalid,
            "child identity is invalid",
        );
    }
    if provider == Provider::Pi && payload.get("agent_id").is_some() {
        return ProviderEvent::ignored(
            Some(provider),
            DiagnosticCode::RecordInvalid,
            "Pi has no native child identity contract",
        );
    }
    let mut event = match provider {
        Provider::Pi => parse_pi(event_name, payload, env),
        Provider::Claude | Provider::Codex => {
            parse_claude_or_codex(provider, event_name, payload, env)
        }
        Provider::Agy => parse_agy(event_name, payload),
    };
    event.source_event = event_name.to_owned();
    if event.action != ProviderAction::Ignored && provider != Provider::Agy {
        let parsed = match event_name {
            "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "tool_execution_start"
            | "tool_execution_end" => {
                parse_tool_observation(provider, event_name, payload, &event).map(Some)
            }
            "PermissionRequest" | "PermissionDenied" | "Elicitation" | "ElicitationResult"
            | "Notification" => parse_request_observation(provider, event_name, payload, &event),
            "UserPromptSubmit" | "Stop" | "StopFailure" | "Interrupt" | "input" | "message_end"
            | "agent_settled" => {
                parse_run_observation(provider, event_name, payload, &event).map(Some)
            }
            "PreCompact" | "PostCompact" | "session_before_compact" | "session_compact" => {
                parse_compaction_observation(provider, event_name, payload, &event).map(Some)
            }
            _ => Ok(None),
        };
        match parsed {
            Ok(observation) => event.observation = observation,
            Err(problem) => event.observation_diagnostic = Some(problem),
        }
    }
    if provider == Provider::Codex
        && event.observation.is_some()
        && let crate::hook_content::HookContent::Available { text } =
            prompt_content(&event, payload, true)
    {
        event.answered_questions = answered_questions(&text);
    }
    event
}

/// The questions a Codex prompt answers. As of Codex 0.162.0 a person's
/// answer to a `request_user_input_async` question is submitted as a prompt
/// that is a `<send_user_message_question_reply>` wrapper around a JSON
/// array, after IDE context when there is any; each item's `questionItemId`
/// is the JSON text of `["request_user_input_async", <call id>, <question
/// index>]`. The wrapper is the whole request, matched by its outer tags,
/// because a question or answer inside it can hold the same tags. Anything
/// else answers nothing, and so does an index the record could not hold.
fn answered_questions(prompt: &str) -> Vec<crate::observations::AnsweredQuestion> {
    const OPEN: &str = "<send_user_message_question_reply>";
    const CLOSE: &str = "</send_user_message_question_reply>";
    let mut text = prompt.trim();
    if text.starts_with("# Context from my IDE setup:\n") {
        // JSON escapes newlines, so an answer cannot hold this separator.
        text = text
            .rsplit_once("\n## My request for Codex:\n")
            .map_or("", |(_, request)| request.trim());
    }
    let Some(body) = text
        .strip_prefix(OPEN)
        .and_then(|rest| rest.strip_suffix(CLOSE))
    else {
        return vec![];
    };
    let Ok(items) = serde_json::from_str::<Vec<Value>>(body) else {
        return vec![];
    };
    let Ok(protocol) = manifest() else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|item| {
            let id = item.get("questionItemId")?.as_str()?;
            let (tool, call, question_index): (String, Value, u64) =
                serde_json::from_str(id).ok()?;
            if classify_tool("codex", &tool).1 != Some(QuestionMode::Nonblocking)
                || question_index > protocol.limits.frame_max
            {
                return None;
            }
            Some(crate::observations::AnsweredQuestion {
                tool_call_id: safe_label(&call, "questionItemId").ok()?,
                question_index,
            })
        })
        .take(protocol.limits.lifecycle_pool_max_count)
        .collect()
}

pub fn reply_content(
    event: &ProviderEvent,
    payload: &Value,
    requested: bool,
) -> crate::hook_content::HookContent {
    reply_content_with_transcript_fallback(event, payload, requested, true)
}

/// Extract reply content for a transient hook delivery, gating mutable
/// on-disk transcript fallback (`agy` when `finalModelOutput` is absent/empty)
/// on `outcome.reply_transcript_attributable` so a replayed or superseded
/// `Stop` never attributes a newer turn's transcript tail to an earlier
/// observation id.
pub fn reply_content_for_outcome(
    event: &ProviderEvent,
    payload: &Value,
    requested: bool,
    outcome: &crate::lifecycle::HookOutcome,
) -> crate::hook_content::HookContent {
    reply_content_with_transcript_fallback(
        event,
        payload,
        requested,
        outcome.reply_transcript_attributable,
    )
}

fn reply_content_with_transcript_fallback(
    event: &ProviderEvent,
    payload: &Value,
    requested: bool,
    allow_transcript_fallback: bool,
) -> crate::hook_content::HookContent {
    use crate::hook_content::HookContent;
    if !requested {
        return HookContent::NotRequested;
    }
    if event.agent_id.is_some() || event.source_event != "Stop" {
        return HookContent::Unsupported;
    }
    match event.provider {
        Some(Provider::Claude | Provider::Codex) => {
            // Codex declares this field nullable and sends null when a turn ends
            // without final text; that is a turn with no reply, not a malformed one.
            if payload.get("last_assistant_message") == Some(&Value::Null) {
                return HookContent::Absent;
            }
            text_content(payload, "last_assistant_message")
        }
        Some(Provider::Agy) => {
            if event.activity_type.as_deref() != Some("stop") {
                return HookContent::Unsupported;
            }
            let max_bytes = match manifest() {
                Ok(protocol) => protocol.limits.max_json_bytes,
                Err(_) => return HookContent::Absent,
            };
            match payload.get("finalModelOutput") {
                Some(Value::String(text)) if !text.is_empty() => {
                    if text.len() <= max_bytes {
                        return HookContent::Available { text: text.clone() };
                    }
                    return HookContent::TooLarge;
                }
                None | Some(Value::Null) | Some(Value::String(_)) => {}
                Some(_) => return HookContent::Invalid,
            }
            if !allow_transcript_fallback {
                return HookContent::Absent;
            }
            let Some(path) = event.transcript_path.as_deref() else {
                return HookContent::Absent;
            };
            match crate::agy_status::last_reply_from_transcript(Path::new(path), max_bytes) {
                crate::agy_status::ReplyRead::Available(text) => HookContent::Available { text },
                crate::agy_status::ReplyRead::Absent => HookContent::Absent,
                crate::agy_status::ReplyRead::TooLarge => HookContent::TooLarge,
            }
        }
        Some(Provider::Pi) | None => HookContent::Unsupported,
    }
}

pub fn prompt_content(
    event: &ProviderEvent,
    payload: &Value,
    requested: bool,
) -> crate::hook_content::HookContent {
    use crate::hook_content::HookContent;
    if !requested {
        return HookContent::NotRequested;
    }
    if event.agent_id.is_some() {
        return HookContent::Unsupported;
    }
    match event.provider {
        Some(Provider::Claude | Provider::Codex) if event.source_event == "UserPromptSubmit" => {
            text_content(payload, "prompt")
        }
        Some(Provider::Agy) if event.is_lead_prompt => text_content(payload, "lastUserInput"),
        Some(Provider::Claude | Provider::Codex | Provider::Pi | Provider::Agy) | None => {
            HookContent::Unsupported
        }
    }
}

fn text_content(payload: &Value, field: &str) -> crate::hook_content::HookContent {
    use crate::hook_content::HookContent;
    match payload.get(field) {
        None => HookContent::Absent,
        Some(Value::String(text))
            if manifest().is_ok_and(|p| text.len() <= p.limits.max_json_bytes) =>
        {
            HookContent::Available { text: text.clone() }
        }
        Some(Value::String(_)) => HookContent::TooLarge,
        Some(_) => HookContent::Invalid,
    }
}

fn strict_optional_label(
    payload: &Value,
    name: &str,
) -> std::result::Result<Option<String>, Diagnostic> {
    payload
        .get(name)
        .map(|value| safe_label(value, name))
        .transpose()
}

fn parse_tool_observation(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    event: &ProviderEvent,
) -> std::result::Result<LifecycleObservation, Diagnostic> {
    let tool_name = safe_label(&payload["tool_name"], "tool_name")?;
    let (tool_class, question_mode) = classify_tool(provider.as_str(), &tool_name);
    let body = if matches!(event_name, "PreToolUse" | "tool_execution_start") {
        ObservationBody::ToolPreflight {
            tool_name,
            tool_class,
            question_mode,
        }
    } else {
        let is_error = if provider == Provider::Pi {
            Some(
                payload
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        Diagnostic::new(
                            DiagnosticCode::RecordInvalid,
                            "Pi tool result requires is_error",
                        )
                    })?,
            )
        } else if event_name == "PostToolUseFailure" {
            Some(true)
        } else {
            None
        };
        let interrupted = payload
            .get("is_interrupt")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    Diagnostic::new(
                        DiagnosticCode::RecordInvalid,
                        "tool interruption flag is invalid",
                    )
                })
            })
            .transpose()?;
        if question_mode == Some(QuestionMode::Nonblocking)
            && (event.agent_id.is_some()
                || interrupted == Some(true)
                || payload
                    .get("is_error")
                    .is_some_and(|value| value != &Value::Bool(false))
                || !accepted_async_receipt(&payload["tool_response"]))
        {
            return Err(Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                "async question publication receipt is invalid",
            ));
        }
        ObservationBody::ToolResult {
            tool_name,
            tool_class,
            question_mode,
            result_surface: if provider == Provider::Pi {
                ResultSurface::ExecutionEnd
            } else if provider == Provider::Claude && event_name == "PostToolUse" {
                ResultSurface::SuccessHook
            } else {
                ResultSurface::PostHook
            },
            is_error,
            interrupted,
        }
    };
    observation_for_body(provider, event_name, payload, event, body)
}

fn observation_for_body(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    event: &ProviderEvent,
    body: ObservationBody,
) -> std::result::Result<LifecycleObservation, Diagnostic> {
    let tool_event = matches!(
        event_name,
        "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "PermissionDenied"
            | "tool_execution_start"
            | "tool_execution_end"
    );
    let elicitation = matches!(event_name, "Elicitation" | "ElicitationResult");
    let correlation = NativeCorrelation {
        tool_call_id: if tool_event {
            strict_optional_label(payload, "tool_use_id")?
        } else {
            None
        },
        turn_id: match provider {
            Provider::Codex => strict_optional_label(payload, "turn_id")?,
            // As of Claude Code 2.1.310 a prompt typed while a turn runs, and
            // every Stop of that turn, carry the turn's `prompt_id`; any other
            // input starts a new one. Only the lead's prompts and turn ends
            // take it as their turn id. Request groups and stored compactions
            // are keyed by turn id too, and with it two compactions in one
            // turn would be stored as one.
            Provider::Claude
                if event.agent_id.is_none()
                    && matches!(
                        body,
                        ObservationBody::PromptSubmitted { .. }
                            | ObservationBody::ResponseFinished { .. }
                            | ObservationBody::AttemptOutcome { .. }
                    ) =>
            {
                strict_optional_label(payload, "prompt_id")?
            }
            Provider::Claude | Provider::Pi | Provider::Agy => None,
        },
        elicitation_id: if elicitation {
            strict_optional_label(payload, "elicitation_id")?
        } else {
            None
        },
        mcp_server_name: if elicitation {
            Some(safe_label(&payload["mcp_server_name"], "mcp_server_name")?)
        } else {
            None
        },
        ..NativeCorrelation::default()
    };
    let observation_id = if provider == Provider::Pi {
        payload
            .get("transport_id")
            .map(|value| {
                let id = safe_label(value, "transport_id")?;
                if !uuid::Uuid::parse_str(&id).is_ok_and(|parsed| parsed.to_string() == id) {
                    return Err(Diagnostic::new(
                        DiagnosticCode::RecordInvalid,
                        "Pi transport ID is invalid",
                    ));
                }
                Ok(id)
            })
            .transpose()?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    Ok(draft_observation(
        event_name,
        observation_id,
        event.agent_id.as_deref(),
        correlation,
        body,
    ))
}

fn parse_request_observation(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    event: &ProviderEvent,
) -> std::result::Result<Option<LifecycleObservation>, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            DiagnosticCode::RecordInvalid,
            "request observation metadata is invalid",
        )
    };
    let body = match event_name {
        "PermissionRequest" => ObservationBody::ApprovalRequested {
            tool_name: strict_optional_label(payload, "tool_name")?,
        },
        "PermissionDenied" => {
            if payload.get("permission_mode").and_then(Value::as_str) != Some("auto") {
                return Err(invalid());
            }
            ObservationBody::AutomaticDenial {
                tool_name: safe_label(&payload["tool_name"], "tool_name")?,
                policy_scope: "auto_mode".into(),
            }
        }
        "Elicitation" => ObservationBody::ElicitationRequested {
            mode: match payload.get("mode").and_then(Value::as_str) {
                Some("form") => ElicitationMode::Form,
                Some("url") => ElicitationMode::Url,
                _ => return Err(invalid()),
            },
        },
        "ElicitationResult" => ObservationBody::ElicitationActionSelected {
            action: match payload.get("action").and_then(Value::as_str) {
                Some("accept") => SelectionAction::Accept,
                Some("decline") => SelectionAction::Decline,
                Some("cancel") => SelectionAction::Cancel,
                _ => return Err(invalid()),
            },
        },
        "Notification" => ObservationBody::Notice {
            subtype: match payload.get("notification_type").and_then(Value::as_str) {
                Some("permission_prompt") => NoticeSubtype::PermissionPrompt,
                Some("elicitation_dialog") => NoticeSubtype::ElicitationDialog,
                Some("elicitation_url_dialog") => NoticeSubtype::ElicitationUrlDialog,
                _ => return Ok(None),
            },
        },
        _ => return Ok(None),
    };
    observation_for_body(provider, event_name, payload, event, body).map(Some)
}

// Codex 0.154.0 collapses one InputText item to FunctionCallOutputBody::Text.
// Native contact pins tool_response to a JSON string containing this receipt.
// It acknowledges publication, not a human answer; no output is retained.
fn accepted_async_receipt(value: &Value) -> bool {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Receipt {
        accepted: bool,
    }
    value.as_str().is_some_and(|text| {
        serde_json::from_str::<Receipt>(text).is_ok_and(|receipt| receipt.accepted)
    })
}

fn optional_observation_enum(
    payload: &Value,
    field: &str,
    vocabulary: &str,
) -> std::result::Result<Option<String>, Diagnostic> {
    let value = strict_optional_label(payload, field)?;
    let known = &manifest()
        .map_err(|error| error.diagnostic)?
        .lifecycle_enums[vocabulary];
    // Providers add values before this writer learns them. The observation is
    // still true without the detail, so an unknown value becomes "unknown"
    // where the vocabulary has that word and is left out where it does not.
    Ok(value.and_then(|value| {
        if known.contains(&value) {
            Some(value)
        } else {
            known.get("unknown").cloned()
        }
    }))
}

fn parse_run_observation(
    provider: Provider,
    name: &str,
    payload: &Value,
    event: &ProviderEvent,
) -> std::result::Result<LifecycleObservation, Diagnostic> {
    let body = match name {
        "UserPromptSubmit" => ObservationBody::PromptSubmitted { input_source: None },
        "input" => ObservationBody::PromptSubmitted {
            input_source: optional_observation_enum(payload, "source", "input_source")?,
        },
        "Stop" => ObservationBody::ResponseFinished {
            stop_hook_active: payload
                .get("stop_hook_active")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        Diagnostic::new(
                            DiagnosticCode::RecordInvalid,
                            "stop continuation flag is invalid",
                        )
                    })
                })
                .transpose()?,
        },
        // Among hook events, one kind with two meanings. Claude Code 2.1.284
        // sends StopFailure instead of Stop, so for Claude this ends the turn.
        // Pi 0.85.1 still sends agent_settled after a failed or aborted
        // message_end, and can retry an error first, so for Pi it does not. A
        // Codex errored end read from the rollout
        // (`ProviderEvent::codex_turn_failure`) also ends the turn. The
        // consumer guide lists the turn-end kinds.
        "StopFailure" => ObservationBody::AttemptOutcome {
            outcome: AttemptOutcome::Failed,
            error_category: optional_observation_enum(payload, "error", "error_category")?,
            error_code: None,
        },
        "message_end" => ObservationBody::AttemptOutcome {
            outcome: if payload["stop_reason"] == "error" {
                AttemptOutcome::Failed
            } else {
                AttemptOutcome::Aborted
            },
            error_category: None,
            error_code: None,
        },
        "Interrupt" => ObservationBody::UserInterrupt,
        "agent_settled" => ObservationBody::RunSettled,
        _ => unreachable!("run observation dispatcher is closed"),
    };
    observation_for_body(provider, name, payload, event, body)
}

fn parse_compaction_observation(
    provider: Provider,
    name: &str,
    payload: &Value,
    event: &ProviderEvent,
) -> std::result::Result<LifecycleObservation, Diagnostic> {
    let trigger = optional_observation_enum(
        payload,
        if provider == Provider::Pi {
            "reason"
        } else {
            "trigger"
        },
        "compaction_trigger",
    )?;
    if trigger.as_ref().is_some_and(|value| {
        if provider == Provider::Pi {
            !["manual", "threshold", "overflow"].contains(&value.as_str())
        } else {
            !["manual", "auto"].contains(&value.as_str())
        }
    }) {
        return Err(Diagnostic::new(
            DiagnosticCode::IntegrationVersionMismatch,
            "provider compaction trigger is unsupported",
        ));
    }
    let body = if matches!(name, "PreCompact" | "session_before_compact") {
        ObservationBody::CompactionAttempted { trigger }
    } else {
        ObservationBody::CompactionSucceeded { trigger }
    };
    observation_for_body(provider, name, payload, event, body)
}
