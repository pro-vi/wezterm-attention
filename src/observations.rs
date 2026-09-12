//! Bounded lifecycle evidence. No IO, clocks, provider control, or badge policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::identity::PaneAddress;
use crate::protocol::{AttentionError, Result, manifest};

macro_rules! vocabulary {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
vocabulary!(ToolClass {
    Generic,
    Question,
    Permission
});
vocabulary!(QuestionMode {
    Blocking,
    Nonblocking
});
vocabulary!(ResultSurface {
    SuccessHook,
    PostHook,
    ExecutionEnd
});
vocabulary!(AttemptOutcome { Failed, Aborted });
vocabulary!(SelectionAction {
    Accept,
    Decline,
    Cancel
});
vocabulary!(NoticeSubtype {
    PermissionPrompt,
    ElicitationDialog,
    ElicitationUrlDialog
});
vocabulary!(ElicitationMode { Form, Url });

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Actor {
    Lead,
    Child { agent_id: String, agent_key: String },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NativeCorrelation {
    /// Native namespace for elicitation IDs; separate servers may reuse an ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationBody {
    PromptSubmitted {
        #[serde(skip_serializing_if = "Option::is_none")]
        input_source: Option<String>,
    },
    ToolPreflight {
        tool_name: String,
        tool_class: ToolClass,
        #[serde(skip_serializing_if = "Option::is_none")]
        question_mode: Option<QuestionMode>,
    },
    ToolResult {
        tool_name: String,
        tool_class: ToolClass,
        #[serde(skip_serializing_if = "Option::is_none")]
        question_mode: Option<QuestionMode>,
        result_surface: ResultSurface,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupted: Option<bool>,
    },
    ApprovalRequested {
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    AutomaticDenial {
        tool_name: String,
        policy_scope: String,
    },
    ResponseFinished {
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_hook_active: Option<bool>,
    },
    RunSettled,
    AttemptOutcome {
        outcome: AttemptOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_category: Option<String>,
    },
    UserInterrupt,
    ElicitationRequested {
        mode: ElicitationMode,
    },
    ElicitationActionSelected {
        action: SelectionAction,
    },
    Notice {
        subtype: NoticeSubtype,
    },
    CompactionAttempted {
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
    CompactionSucceeded {
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct LifecycleObservation {
    pub observation_id: String,
    pub source_event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    pub observed_mono_ns: String,
    pub written_at_unix_ns: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<NativeCorrelation>,
    #[serde(flatten)]
    pub body: ObservationBody,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationPool {
    pub observations: Vec<LifecycleObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_floor_mono_ns: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationPools {
    pub requests: ObservationPool,
    pub general: ObservationPool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleSnapshot {
    pub kind: String,
    pub schema: u64,
    pub address: PaneAddress,
    pub launch_id: String,
    pub binding_id: String,
    pub provider: String,
    pub snapshot_id: String,
    pub written_at_unix_ns: String,
    pub pools: ObservationPools,
}

impl ObservationBody {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PromptSubmitted { .. } => "prompt_submitted",
            Self::ToolPreflight { .. } => "tool_preflight",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::AutomaticDenial { .. } => "automatic_denial",
            Self::ResponseFinished { .. } => "response_finished",
            Self::RunSettled => "run_settled",
            Self::AttemptOutcome { .. } => "attempt_outcome",
            Self::UserInterrupt => "user_interrupt",
            Self::ElicitationRequested { .. } => "elicitation_requested",
            Self::ElicitationActionSelected { .. } => "elicitation_action_selected",
            Self::Notice { .. } => "notice",
            Self::CompactionAttempted { .. } => "compaction_attempted",
            Self::CompactionSucceeded { .. } => "compaction_succeeded",
        }
    }
    pub fn pool(&self) -> &'static str {
        match self {
            Self::ToolPreflight { tool_class, .. } | Self::ToolResult { tool_class, .. } => {
                if *tool_class == ToolClass::Generic {
                    "general"
                } else {
                    "requests"
                }
            }
            Self::ApprovalRequested { .. }
            | Self::AutomaticDenial { .. }
            | Self::ElicitationRequested { .. }
            | Self::ElicitationActionSelected { .. }
            | Self::Notice { .. } => "requests",
            Self::PromptSubmitted { .. }
            | Self::ResponseFinished { .. }
            | Self::RunSettled
            | Self::AttemptOutcome { .. }
            | Self::UserInterrupt
            | Self::CompactionAttempted { .. }
            | Self::CompactionSucceeded { .. } => "general",
        }
    }

    fn tool(&self) -> Option<(&str, ToolClass, Option<QuestionMode>)> {
        match self {
            Self::ToolPreflight {
                tool_name,
                tool_class,
                question_mode,
            }
            | Self::ToolResult {
                tool_name,
                tool_class,
                question_mode,
                ..
            } => Some((tool_name, *tool_class, *question_mode)),
            _ => None,
        }
    }
}

impl LifecycleObservation {
    /// Kind is part of storage identity, not of cross-phase relation identity.
    pub fn storage_key(&self) -> String {
        let c = self.correlation.as_ref();
        let native = c.and_then(|c| match self.body {
            ObservationBody::ToolPreflight { .. }
            | ObservationBody::ToolResult { .. }
            | ObservationBody::AutomaticDenial { .. }
            | ObservationBody::ApprovalRequested { .. } => {
                c.tool_call_id.as_deref().map(|id| ("tool_call_id", id))
            }
            ObservationBody::ElicitationRequested { .. }
            | ObservationBody::ElicitationActionSelected { .. } => {
                c.elicitation_id.as_deref().map(|id| ("elicitation_id", id))
            }
            _ => c
                .message_id
                .as_deref()
                .map(|id| ("message_id", id))
                .or_else(|| c.turn_id.as_deref().map(|id| ("turn_id", id))),
        });
        let (actor, actor_id) = match &self.actor {
            Actor::Lead => ("lead", ""),
            Actor::Child { agent_id, .. } => ("child", agent_id.as_str()),
        };
        let (namespace, id) = native.unwrap_or(("observation_id", &self.observation_id));
        let mut key = String::new();
        for text in [
            self.body.kind(),
            actor,
            actor_id,
            namespace,
            id,
            c.and_then(|c| c.turn_id.as_deref()).unwrap_or(""),
            c.and_then(|c| c.mcp_server_name.as_deref()).unwrap_or(""),
        ] {
            key.push_str(&text.len().to_string());
            key.push(':');
            key.push_str(text);
        }
        key
    }

    fn semantic(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("typed observation serializes");
        let object = value.as_object_mut().expect("observation is object");
        for key in ["observation_id", "observed_mono_ns", "written_at_unix_ns"] {
            object.remove(key);
        }
        value
    }
}

pub fn classify_tool(provider: &str, name: &str) -> (ToolClass, Option<QuestionMode>) {
    match (provider, name) {
        ("claude", "AskUserQuestion") | ("codex", "request_user_input") => {
            (ToolClass::Question, Some(QuestionMode::Blocking))
        }
        ("codex", "request_user_input_async") => {
            (ToolClass::Question, Some(QuestionMode::Nonblocking))
        }
        ("codex", "request_permissions") => (ToolClass::Permission, None),
        _ => (ToolClass::Generic, None),
    }
}

fn invalid() -> AttentionError {
    AttentionError::new("record_invalid", "lifecycle evidence violates its contract")
}

impl LifecycleSnapshot {
    pub fn validate_semantics(&self) -> Result<()> {
        let limits = &manifest()?.limits;
        let mut keys = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut pools_size = 0;
        for (name, pool) in [
            ("requests", &self.pools.requests),
            ("general", &self.pools.general),
        ] {
            let pool_size = compact_size(pool)?;
            pools_size += pool_size;
            if pool.observations.len() > limits.lifecycle_pool_max_count
                || pool_size > limits.lifecycle_pool_max_bytes
            {
                return Err(invalid());
            }
            let mut prior = None;
            for item in &pool.observations {
                if !manifest()?
                    .lifecycle_sources
                    .get(&self.provider)
                    .and_then(|kinds| kinds.get(item.body.kind()))
                    .is_some_and(|events| events.contains(&item.source_event))
                {
                    return Err(invalid());
                }
                if item.correlation.as_ref().is_some_and(|c| {
                    c.mcp_server_name.is_some()
                        && !matches!(
                            item.body,
                            ObservationBody::ElicitationRequested { .. }
                                | ObservationBody::ElicitationActionSelected { .. }
                        )
                }) {
                    return Err(invalid());
                }
                let order = (&item.observed_mono_ns, &item.observation_id);
                if prior.is_some_and(|before| before > order)
                    || item.body.pool() != name
                    || compact_size(item)? > limits.lifecycle_observation_max_bytes
                    || pool
                        .retention_floor_mono_ns
                        .as_ref()
                        .is_some_and(|floor| item.observed_mono_ns <= *floor)
                    || !ids.insert(&item.observation_id)
                {
                    return Err(invalid());
                }
                prior = Some(order);
                if item
                    .correlation
                    .as_ref()
                    .is_some_and(|c| c.elicitation_id.is_some() && c.mcp_server_name.is_none())
                {
                    return Err(invalid());
                }
                let key = item.storage_key();
                if !keys.insert(key) {
                    return Err(invalid());
                }
                if let Actor::Child {
                    agent_id,
                    agent_key,
                } = &item.actor
                    && (self.provider == "pi"
                        || crate::protocol::sha256_hex(agent_id.as_bytes()) != *agent_key)
                {
                    return Err(invalid());
                }
                if let Some((tool, class, mode)) = item.body.tool() {
                    if classify_tool(&self.provider, tool) != (class, mode) {
                        return Err(invalid());
                    }
                    if mode == Some(QuestionMode::Nonblocking) {
                        if item.actor != Actor::Lead {
                            return Err(invalid());
                        }
                        if let ObservationBody::ToolResult {
                            result_surface,
                            is_error,
                            interrupted,
                            ..
                        } = &item.body
                            && (*result_surface != ResultSurface::PostHook
                                || *is_error == Some(true)
                                || *interrupted == Some(true)
                                || item.source_event != "PostToolUse")
                        {
                            return Err(invalid());
                        }
                    }
                }
            }
        }
        let size = compact_size(self)? + 1;
        let envelope = size - pools_size;
        if size > limits.lifecycle_max_json_bytes || envelope > limits.lifecycle_envelope_max_bytes
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Returns false on a duplicate/older/fenced receipt. Conflicts are errors.
    pub fn reduce(&mut self, candidate: LifecycleObservation) -> Result<bool> {
        self.validate_semantics()?;
        let limits = &manifest()?.limits;
        if compact_size(&candidate)? > limits.lifecycle_observation_max_bytes {
            return Err(invalid());
        }
        let key = candidate.storage_key();
        let requests = candidate.body.pool() == "requests";
        let pool = if requests {
            &self.pools.requests
        } else {
            &self.pools.general
        };
        if pool
            .retention_floor_mono_ns
            .as_ref()
            .is_some_and(|floor| candidate.observed_mono_ns <= *floor)
        {
            return Ok(false);
        }
        let existing = self
            .pools
            .requests
            .observations
            .iter()
            .chain(&self.pools.general.observations)
            .find(|item| item.storage_key() == key);
        if let Some(existing) = existing {
            if existing.body.tool() != candidate.body.tool() {
                return Err(invalid());
            }
            if existing.semantic() == candidate.semantic()
                || candidate.observed_mono_ns < existing.observed_mono_ns
            {
                return Ok(false);
            }
            if candidate.observed_mono_ns == existing.observed_mono_ns {
                return Err(invalid());
            }
        }
        // Work on a copy so validation failure cannot mutate the caller's snapshot.
        let mut next = self.clone();
        let pool = if requests {
            &mut next.pools.requests
        } else {
            &mut next.pools.general
        };
        pool.observations.retain(|item| item.storage_key() != key);
        next.written_at_unix_ns = candidate.written_at_unix_ns.clone();
        pool.observations.push(candidate);
        pool.observations.sort_by(|a, b| {
            (&a.observed_mono_ns, &a.observation_id).cmp(&(&b.observed_mono_ns, &b.observation_id))
        });
        while pool.observations.len() > limits.lifecycle_pool_max_count
            || compact_size(pool)? > limits.lifecycle_pool_max_bytes
        {
            let floor = pool
                .observations
                .first()
                .ok_or_else(invalid)?
                .observed_mono_ns
                .clone();
            pool.observations
                .retain(|item| item.observed_mono_ns > floor);
            pool.retention_floor_mono_ns = Some(floor);
        }
        next.snapshot_id = Uuid::new_v4().to_string();
        next.validate_semantics()?;
        *self = next;
        Ok(true)
    }
}

fn compact_size(value: &impl Serialize) -> Result<usize> {
    struct ByteCount(usize);
    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = ByteCount(0);
    serde_json::to_writer(&mut count, value).map_err(|_| invalid())?;
    Ok(count.0)
}
