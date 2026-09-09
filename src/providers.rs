use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

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
}

impl ProviderAction {
    pub const ALL: [Self; 9] = [
        Self::Binding,
        Self::Activity,
        Self::ParentStop,
        Self::ChildActive,
        Self::ChildStopped,
        Self::End,
        Self::Review,
        Self::Clear,
        Self::Ignored,
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
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderEvent {
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
        "SessionEnd",
        "PreToolUse",
        "PermissionRequest",
        "Notification",
        "Stop",
        "SubagentStop",
        "SubagentStart",
    ];
    if !supported.contains(&event_name)
        || (provider == Provider::Codex && event_name == "Notification")
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
        "agent_settled",
        "agent_end",
        "bus",
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
    match provider {
        Provider::Pi => parse_pi(event_name, payload, env),
        Provider::Claude | Provider::Codex => {
            parse_claude_or_codex(provider, event_name, payload, env)
        }
    }
}
