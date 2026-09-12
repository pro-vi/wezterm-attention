use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::observations::{
    Actor, AttemptOutcome, ElicitationMode, LifecycleObservation, NativeCorrelation, NoticeSubtype,
    ObservationBody, QuestionMode, ResultSurface, SelectionAction, classify_tool,
};
use crate::protocol::{AttentionError, Diagnostic, manifest};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Claude,
    Codex,
    Pi,
}

impl Provider {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAction {
    Binding,
    Activity,
    ParentStop,
    ChildActive,
    ChildStopped,
    End,
    Review,
    Clear,
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

#[derive(Clone, Debug)]
pub struct ProviderEvent {
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
    pub child_source: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub config_dir: Option<String>,
    pub model: Option<String>,
    pub expected_session_id: Option<String>,
    pub diagnostic: Option<Diagnostic>,
}

impl ProviderEvent {
    fn ignored(provider: Option<Provider>, code: &str, message: &str) -> Self {
        Self {
            observation: None,
            observation_diagnostic: None,
            action: ProviderAction::Ignored,
            provider,
            provider_session_id: None,
            start_source: None,
            activity_type: None,
            label: None,
            agent_id: None,
            agent_type: None,
            child_source: None,
            transcript_path: None,
            cwd: None,
            config_dir: None,
            model: None,
            expected_session_id: None,
            diagnostic: Some(AttentionError::new(code, message).diagnostic),
        }
    }
}

fn safe_label(value: &Value, field: &str) -> std::result::Result<String, Diagnostic> {
    let maximum = manifest()
        .map_err(|error| error.diagnostic)?
        .limits
        .safe_label_max_bytes;
    let Some(text) = value.as_str() else {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{field} is missing or too long"),
        )
        .diagnostic);
    };
    if text.is_empty()
        || text.len() > maximum
        || text
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
    {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{field} is missing or too long"),
        )
        .diagnostic);
    }
    Ok(text.to_owned())
}

fn optional_label(payload: &Value, field: &str) -> std::result::Result<Option<String>, Diagnostic> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => safe_label(value, field).map(Some),
    }
}

fn optional_lenient_label(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|value| safe_label(value, field).ok())
}

fn optional_path(payload: &Value, field: &str) -> std::result::Result<Option<String>, Diagnostic> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(path) = value.as_str() else {
        return Err(
            AttentionError::new("record_invalid", format!("{field} must be absolute")).diagnostic,
        );
    };
    let maximum = manifest()
        .map_err(|error| error.diagnostic)?
        .limits
        .path_max_bytes;
    if path.is_empty()
        || path.len() > maximum
        || path
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
        || !Path::new(path).is_absolute()
    {
        return Err(
            AttentionError::new("record_invalid", format!("{field} must be absolute")).diagnostic,
        );
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
        || value
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
        || !Path::new(value).is_absolute()
    {
        return Err(
            AttentionError::new("record_invalid", format!("{field} must be absolute")).diagnostic,
        );
    }
    Ok(Some(value.clone()))
}

fn parse_provider_common(
    provider: Provider,
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
    };
    let expected_session_id = match env
        .get("WEZTERM_ATTENTION_EXPECTED_SESSION_ID")
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(safe_label(
            &Value::String(value.clone()),
            "WEZTERM_ATTENTION_EXPECTED_SESSION_ID",
        )?),
        None => None,
    };
    let event = ProviderEvent {
        observation: None,
        observation_diagnostic: None,
        action: ProviderAction::Ignored,
        provider: Some(provider),
        provider_session_id: Some(provider_session_id),
        start_source: None,
        activity_type: None,
        label: None,
        agent_id: optional_lenient_label(payload, "agent_id"),
        agent_type: optional_lenient_label(payload, "agent_type"),
        child_source: None,
        transcript_path: optional_path(payload, transcript_field)?,
        cwd: optional_path(payload, "cwd")?,
        config_dir: environment_path(env, config_field)?,
        model: optional_label(payload, "model")?,
        expected_session_id,
        diagnostic: None,
    };
    Ok(event)
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
            "claim_stale",
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
            "claim_stale",
            "Cursor-owned Claude invocation is not pane authority",
        );
    }
    let supported = [
        "SessionStart",
        "UserPromptSubmit",
        "PreCompact",
        "PostCompact",
        "StopFailure",
        "Interrupt",
        "SessionEnd",
        "PreToolUse",
        "PermissionRequest",
        "Notification",
        "Stop",
        "SubagentStop",
        "SubagentStart",
        "PostToolUse",
        "PostToolUseFailure",
        "PermissionDenied",
        "Elicitation",
        "ElicitationResult",
    ];
    if !supported.contains(&event_name)
        || (provider == Provider::Claude && event_name == "Interrupt")
        || (provider == Provider::Codex
            && matches!(
                event_name,
                "Notification"
                    | "StopFailure"
                    | "PostToolUseFailure"
                    | "PermissionDenied"
                    | "Elicitation"
                    | "ElicitationResult"
            ))
    {
        return ProviderEvent::ignored(
            Some(provider),
            "integration_version_mismatch",
            "provider event is not supported",
        );
    }
    if payload.get("hook_event_name").and_then(Value::as_str) != Some(event_name) {
        return ProviderEvent::ignored(
            Some(provider),
            "record_invalid",
            "hook event name does not match the callback",
        );
    }
    let mut event = match parse_provider_common(provider, payload, env) {
        Ok(event) => event,
        Err(diagnostic) => {
            let mut event =
                ProviderEvent::ignored(Some(provider), &diagnostic.code, &diagnostic.message);
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
            "claim_stale",
            "Codex hook identity differs from the inherited thread",
        );
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
                "record_invalid",
                "Interrupt is a root-turn observation",
            );
        }
        event.action = ProviderAction::Observation;
        return event;
    }
    if event_name == "SubagentStart" {
        return ProviderEvent::ignored(
            Some(provider),
            "integration_version_mismatch",
            "SubagentStart does not establish presence",
        );
    }
    if event_name == "SubagentStop" {
        if event.agent_id.is_none() {
            return ProviderEvent::ignored(
                Some(provider),
                "record_invalid",
                "SubagentStop requires a valid agent_id",
            );
        }
        event.action = ProviderAction::ChildStopped;
        event.child_source = Some("subagent_stop".to_owned());
        return event;
    }
    if matches!(event_name, "PreToolUse" | "PermissionRequest") && event.agent_id.is_some() {
        event.action = ProviderAction::ChildActive;
        event.child_source = Some(
            if event_name == "PreToolUse" {
                "tool"
            } else {
                "permission"
            }
            .to_owned(),
        );
        return event;
    }
    if event.agent_id.is_some() {
        return ProviderEvent::ignored(
            Some(provider),
            "claim_stale",
            "child callback cannot write lead state",
        );
    }
    match event_name {
        "SessionStart" => {
            let Some(source) = payload.get("source").and_then(Value::as_str) else {
                return ProviderEvent::ignored(
                    Some(provider),
                    "integration_version_mismatch",
                    "SessionStart source is not supported",
                );
            };
            if !["startup", "resume", "clear", "compact"].contains(&source) {
                return ProviderEvent::ignored(
                    Some(provider),
                    "integration_version_mismatch",
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
                    "integration_version_mismatch",
                    "SessionEnd reason is not supported",
                );
            }
            event.action = ProviderAction::End;
        }
        "PreToolUse" => {
            let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) else {
                return ProviderEvent::ignored(
                    Some(provider),
                    "record_invalid",
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
                return ProviderEvent::ignored(
                    Some(provider),
                    "integration_version_mismatch",
                    "idle prompt does not replace terminal activity",
                );
            }
            if !["permission_prompt", "auth_success", "elicitation_dialog"]
                .contains(&notification.unwrap_or(""))
            {
                return ProviderEvent::ignored(
                    Some(provider),
                    "integration_version_mismatch",
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
        }
        _ => {
            return ProviderEvent::ignored(
                Some(provider),
                "integration_version_mismatch",
                "provider event has no transition",
            );
        }
    }
    event
}

fn parse_pi(event_name: &str, payload: &Value, env: &BTreeMap<String, String>) -> ProviderEvent {
    let supported = [
        "session_start",
        "session_shutdown",
        "agent_start",
        "tool_execution_start",
        "tool_execution_end",
        "agent_settled",
        "agent_end",
        "bus",
        "input",
        "message_end",
        "session_before_compact",
        "session_compact",
    ];
    if !supported.contains(&event_name) {
        return ProviderEvent::ignored(
            Some(Provider::Pi),
            "integration_version_mismatch",
            "Pi event is not supported",
        );
    }
    let mut event = match parse_provider_common(Provider::Pi, payload, env) {
        Ok(event) => event,
        Err(diagnostic) => {
            let mut event =
                ProviderEvent::ignored(Some(Provider::Pi), &diagnostic.code, &diagnostic.message);
            event.diagnostic = Some(diagnostic);
            return event;
        }
    };
    let bus_label = if event_name == "bus" {
        match optional_label(payload, "label") {
            Ok(label) => label,
            Err(diagnostic) => {
                let mut ignored = ProviderEvent::ignored(
                    Some(Provider::Pi),
                    &diagnostic.code,
                    &diagnostic.message,
                );
                ignored.diagnostic = Some(diagnostic);
                return ignored;
            }
        }
    } else {
        None
    };
    match event_name {
        "input" | "session_before_compact" | "session_compact" => {
            event.action = ProviderAction::Observation
        }
        "tool_execution_end" => event.action = ProviderAction::Observation,
        "message_end" => {
            if payload.get("role").and_then(Value::as_str) != Some("assistant")
                || !matches!(
                    payload.get("stop_reason").and_then(Value::as_str),
                    Some("error" | "aborted")
                )
            {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    "integration_version_mismatch",
                    "Pi message has no supported attempt outcome",
                );
            }
            event.action = ProviderAction::Observation;
        }
        "session_start" => {
            let source = payload
                .get("start_source")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !["startup", "reload", "new", "resume", "fork"].contains(&source) {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    "integration_version_mismatch",
                    "Pi session start source is not supported",
                );
            }
            event.action = ProviderAction::Binding;
            event.start_source = Some(source.to_owned());
        }
        "session_shutdown" => {
            let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
            if reason == "reload" {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    "integration_version_mismatch",
                    "Pi reload keeps the current binding",
                );
            }
            if !["quit", "new", "resume", "fork"].contains(&reason) {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    "integration_version_mismatch",
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
            return ProviderEvent::ignored(
                Some(Provider::Pi),
                "integration_version_mismatch",
                "Pi agent_end is not terminal",
            );
        }
        "bus" => match payload.get("state").and_then(Value::as_str) {
            Some("review") => event.action = ProviderAction::Review,
            Some("clear") => event.action = ProviderAction::Clear,
            Some(state @ ("thinking" | "stop" | "notify")) => {
                event.action = ProviderAction::Activity;
                event.activity_type = Some(state.to_owned());
                event.label = bus_label;
            }
            _ => {
                return ProviderEvent::ignored(
                    Some(Provider::Pi),
                    "record_invalid",
                    "Pi bus state is invalid",
                );
            }
        },
        _ => {}
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
            "integration_version_mismatch",
            "provider is not supported",
        );
    };
    if !payload.is_object() {
        return ProviderEvent::ignored(
            Some(provider),
            "record_invalid",
            "provider payload is not an object",
        );
    }
    // Ownership is validated before the legacy dispatcher can choose lead state.
    if let Some(agent) = payload.get("agent_id")
        && safe_label(agent, "agent_id").is_err()
    {
        return ProviderEvent::ignored(
            Some(provider),
            "record_invalid",
            "child identity is invalid",
        );
    }
    if provider == Provider::Pi && payload.get("agent_id").is_some() {
        return ProviderEvent::ignored(
            Some(provider),
            "record_invalid",
            "Pi has no native child identity contract",
        );
    }
    let mut event = match provider {
        Provider::Pi => parse_pi(event_name, payload, env),
        Provider::Claude | Provider::Codex => {
            parse_claude_or_codex(provider, event_name, payload, env)
        }
    };
    if event.action != ProviderAction::Ignored {
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
    event
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
                        AttentionError::new("record_invalid", "Pi tool result requires is_error")
                            .diagnostic
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
                    AttentionError::new("record_invalid", "tool interruption flag is invalid")
                        .diagnostic
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
            return Err(AttentionError::new(
                "record_invalid",
                "async question publication receipt is invalid",
            )
            .diagnostic);
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
        turn_id: if provider == Provider::Codex {
            strict_optional_label(payload, "turn_id")?
        } else {
            None
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
                    return Err(AttentionError::new(
                        "record_invalid",
                        "Pi transport ID is invalid",
                    )
                    .diagnostic);
                }
                Ok(id)
            })
            .transpose()?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    Ok(LifecycleObservation {
        observation_id,
        source_event: event_name.to_owned(),
        source_version: None,
        observed_mono_ns: "00000000000000000000".to_owned(),
        written_at_unix_ns: "00000000000000000000".to_owned(),
        actor: match &event.agent_id {
            Some(id) => Actor::Child {
                agent_id: id.clone(),
                agent_key: crate::protocol::sha256_hex(id.as_bytes()),
            },
            None => Actor::Lead,
        },
        correlation: if correlation == NativeCorrelation::default() {
            None
        } else {
            Some(correlation)
        },
        body,
    })
}

fn parse_request_observation(
    provider: Provider,
    event_name: &str,
    payload: &Value,
    event: &ProviderEvent,
) -> std::result::Result<Option<LifecycleObservation>, Diagnostic> {
    let invalid = || {
        AttentionError::new("record_invalid", "request observation metadata is invalid").diagnostic
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
    if value.as_ref().is_some_and(|value| {
        !manifest().expect("manifest was validated").lifecycle_enums[vocabulary].contains(value)
    }) {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "native lifecycle enum is unsupported",
        )
        .diagnostic);
    }
    Ok(value)
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
                        AttentionError::new("record_invalid", "stop continuation flag is invalid")
                            .diagnostic
                    })
                })
                .transpose()?,
        },
        "StopFailure" => ObservationBody::AttemptOutcome {
            outcome: AttemptOutcome::Failed,
            error_category: optional_observation_enum(payload, "error", "error_category")?,
        },
        "message_end" => ObservationBody::AttemptOutcome {
            outcome: if payload["stop_reason"] == "error" {
                AttemptOutcome::Failed
            } else {
                AttemptOutcome::Aborted
            },
            error_category: None,
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
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "provider compaction trigger is unsupported",
        )
        .diagnostic);
    }
    let body = if matches!(name, "PreCompact" | "session_before_compact") {
        ObservationBody::CompactionAttempted { trigger }
    } else {
        ObservationBody::CompactionSucceeded { trigger }
    };
    observation_for_body(provider, name, payload, event, body)
}
