//! Bounded lifecycle evidence. No IO, clocks, provider control, or badge policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::identity::PaneAddress;
use crate::protocol::{AttentionError, Result, manifest, vocabulary};

// Defined in `protocol` because the manifest's tool classification is written in
// them: a closed vocabulary the contract declares belongs with the contract.
// Re-exported because the observation model is where callers expect to find them.
pub use crate::protocol::{QuestionMode, ToolClass};

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

vocabulary!(LifecycleAvailability {
    Available,
    Absent,
    Unavailable,
    Invalid,
    Unsupported
});
vocabulary!(Coverage { BoundedWindow });
vocabulary!(RequestKind {
    Question,
    Permission,
    Approval,
    Elicitation,
    Notice
});
vocabulary!(RelationKind {
    ToolResultObserved,
    ElicitationActionSelected,
    AutomaticDenialObserved
});

#[derive(Clone, Debug, Serialize)]
pub struct PooledObservation {
    pub pool: String,
    #[serde(flatten)]
    pub observation: LifecycleObservation,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestRelation {
    pub kind: RelationKind,
    pub observation_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestEvidence {
    pub kind: RequestKind,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<NativeCorrelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_mode: Option<QuestionMode>,
    pub request_observation_ids: Vec<String>,
    pub result_observation_ids: Vec<String>,
    pub selection_observation_ids: Vec<String>,
    pub denial_observation_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_observation_ids: Option<Vec<String>>,
    pub relations: Vec<RequestRelation>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LifecycleView {
    pub availability: LifecycleAvailability,
    pub coverage: Coverage,
    pub observations: Vec<PooledObservation>,
    pub requests: Vec<RequestEvidence>,
    pub retention_floors: std::collections::BTreeMap<String, String>,
    pub diagnostics: Vec<crate::protocol::Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge_acknowledgement: Option<Value>,
}

impl LifecycleView {
    pub fn empty(availability: LifecycleAvailability) -> Self {
        Self {
            availability,
            coverage: Coverage::BoundedWindow,
            observations: vec![],
            requests: vec![],
            retention_floors: Default::default(),
            diagnostics: vec![],
            snapshot_id: None,
            badge_acknowledgement: None,
        }
    }

    pub fn from_snapshot(snapshot: &LifecycleSnapshot, now: Option<&str>) -> Result<Self> {
        crate::protocol::validate_record(
            &serde_json::to_value(snapshot).map_err(AttentionError::record_json)?,
            Some("lifecycle_snapshot"),
        )?;
        let mut view = Self::empty(LifecycleAvailability::Available);
        view.snapshot_id = Some(snapshot.snapshot_id.clone());
        for (name, pool) in [
            ("requests", &snapshot.pools.requests),
            ("general", &snapshot.pools.general),
        ] {
            if let Some(floor) = &pool.retention_floor_mono_ns {
                view.retention_floors.insert(name.into(), floor.clone());
            }
            for observation in &pool.observations {
                if now.is_some_and(|now| now < observation.written_at_unix_ns.as_str())
                    && view.diagnostics.len() < 8
                {
                    view.diagnostics.push(
                        AttentionError::new("clock_skew", "lifecycle write time is ahead of UTC")
                            .diagnostic,
                    );
                }
                view.observations.push(PooledObservation {
                    pool: name.into(),
                    observation: observation.clone(),
                });
            }
        }
        view.observations.sort_by(|a, b| {
            (
                &a.observation.observed_mono_ns,
                &a.observation.observation_id,
            )
                .cmp(&(
                    &b.observation.observed_mono_ns,
                    &b.observation.observation_id,
                ))
        });
        view.requests = request_evidence(&view.observations, &snapshot.provider);
        Ok(view)
    }
}

fn request_evidence(observations: &[PooledObservation], provider: &str) -> Vec<RequestEvidence> {
    use std::collections::BTreeMap;
    #[derive(Clone, Copy)]
    enum Role {
        Request,
        Result,
        Publication,
        Selection,
        Denial,
    }
    let mut indices = BTreeMap::new();
    let mut groups: Vec<RequestEvidence> = Vec::new();
    for pooled in observations {
        let item = &pooled.observation;
        let c = item.correlation.clone().unwrap_or_default();
        let from_class = |class| match class {
            ToolClass::Question => RequestKind::Question,
            ToolClass::Permission => RequestKind::Permission,
            ToolClass::Generic => RequestKind::Approval,
        };
        let (kind, role, tool_name, mode, namespace, native_id) = match &item.body {
            ObservationBody::ToolPreflight {
                tool_name,
                tool_class,
                question_mode,
            } if *tool_class != ToolClass::Generic => (
                from_class(*tool_class),
                Role::Request,
                Some(tool_name.clone()),
                *question_mode,
                "tool",
                c.tool_call_id.as_deref(),
            ),
            ObservationBody::ToolResult {
                tool_name,
                tool_class,
                question_mode,
                ..
            } if *tool_class != ToolClass::Generic => (
                from_class(*tool_class),
                if *question_mode == Some(QuestionMode::Nonblocking) {
                    Role::Publication
                } else {
                    Role::Result
                },
                Some(tool_name.clone()),
                *question_mode,
                "tool",
                c.tool_call_id.as_deref(),
            ),
            ObservationBody::ApprovalRequested { tool_name } => (
                RequestKind::Approval,
                Role::Request,
                tool_name.clone(),
                None,
                "tool",
                c.tool_call_id.as_deref(),
            ),
            ObservationBody::AutomaticDenial { tool_name, .. } => {
                let (class, mode) = classify_tool(provider, tool_name);
                (
                    from_class(class),
                    Role::Denial,
                    Some(tool_name.clone()),
                    mode,
                    "tool",
                    c.tool_call_id.as_deref(),
                )
            }
            ObservationBody::ElicitationRequested { .. } => (
                RequestKind::Elicitation,
                Role::Request,
                None,
                None,
                "elicitation",
                c.elicitation_id.as_deref(),
            ),
            ObservationBody::ElicitationActionSelected { .. } => (
                RequestKind::Elicitation,
                Role::Selection,
                None,
                None,
                "elicitation",
                c.elicitation_id.as_deref(),
            ),
            ObservationBody::Notice { .. } => {
                (RequestKind::Notice, Role::Request, None, None, "", None)
            }
            _ => continue,
        };
        let (actor, agent) = match &item.actor {
            Actor::Lead => ("lead", ""),
            Actor::Child { agent_id, .. } => ("child", agent_id.as_str()),
        };
        let key = vec![
            format!("{kind:?}"),
            actor.into(),
            agent.into(),
            if native_id.is_some() {
                namespace.into()
            } else {
                "observation_id".into()
            },
            native_id.unwrap_or(&item.observation_id).into(),
            c.turn_id.clone().unwrap_or_default(),
            c.mcp_server_name.clone().unwrap_or_default(),
            tool_name.clone().unwrap_or_default(),
            format!("{mode:?}"),
        ];
        let index = *indices.entry(key).or_insert_with(|| {
            let index = groups.len();
            groups.push(RequestEvidence {
                kind,
                actor: item.actor.clone(),
                correlation: item.correlation.clone(),
                tool_name,
                question_mode: if kind == RequestKind::Question {
                    mode
                } else {
                    None
                },
                request_observation_ids: vec![],
                result_observation_ids: vec![],
                selection_observation_ids: vec![],
                denial_observation_ids: vec![],
                publication_observation_ids: (kind == RequestKind::Question).then(Vec::new),
                relations: vec![],
            });
            index
        });
        let group = &mut groups[index];
        let ids = match role {
            Role::Request => &mut group.request_observation_ids,
            Role::Result => &mut group.result_observation_ids,
            Role::Selection => &mut group.selection_observation_ids,
            Role::Denial => &mut group.denial_observation_ids,
            Role::Publication => group
                .publication_observation_ids
                .as_mut()
                .expect("validated nonblocking question"),
        };
        ids.push(item.observation_id.clone());
    }
    for group in &mut groups {
        if group.request_observation_ids.is_empty() {
            continue;
        }
        // Lexical kind order matches the Lua projection, independent of arrival.
        for (kind, ids) in [
            (
                RelationKind::AutomaticDenialObserved,
                &group.denial_observation_ids,
            ),
            (
                RelationKind::ElicitationActionSelected,
                &group.selection_observation_ids,
            ),
            (
                RelationKind::ToolResultObserved,
                &group.result_observation_ids,
            ),
        ] {
            let mut ids = ids.clone();
            ids.sort();
            group
                .relations
                .extend(ids.into_iter().map(|observation_id| RequestRelation {
                    kind,
                    observation_id,
                }));
        }
    }
    groups
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
        let mut key = String::new();
        for text in self.storage_key_parts() {
            key.push_str(&text.len().to_string());
            key.push(':');
            key.push_str(text);
        }
        key
    }

    // Equality of these borrowed parts is identical to equality of the
    // length-prefixed public key, without allocating a string on each scan.
    fn storage_key_parts(&self) -> [&str; 7] {
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
        [
            self.body.kind(),
            actor,
            actor_id,
            namespace,
            id,
            c.and_then(|c| c.turn_id.as_deref()).unwrap_or(""),
            c.and_then(|c| c.mcp_server_name.as_deref()).unwrap_or(""),
        ]
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
    manifest()
        .ok()
        .and_then(|protocol| protocol.tool_classification.get(provider))
        .and_then(|tools| tools.get(name))
        .map(|class| (class.tool_class, class.question_mode))
        .unwrap_or((ToolClass::Generic, None))
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
                let key = item.storage_key_parts();
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
        let key = candidate.storage_key_parts();
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
            .find(|item| item.storage_key_parts() == key);
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
        pool.observations
            .retain(|item| item.storage_key_parts() != key);
        next.written_at_unix_ns = candidate.written_at_unix_ns.clone();
        let candidate_id = candidate.observation_id.clone();
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
        // Older than everything a full pool keeps, so the insertion evicted
        // the candidate itself. Nothing was stored, and the pool as it was is
        // still within its bounds.
        if !pool
            .observations
            .iter()
            .any(|item| item.observation_id == candidate_id)
        {
            return Ok(false);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_keys_preserve_namespaces_and_public_encoding() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/lifecycle/observations.json"
        ))
        .unwrap();
        let snapshot: LifecycleSnapshot =
            serde_json::from_value(fixtures["cases"][1]["value"].clone()).unwrap();
        let original = snapshot.pools.general.observations[0].clone();
        assert_eq!(
            original.storage_key(),
            "14:tool_preflight4:lead0:14:observation_id36:00000000-0000-4000-8000-0000000000110:0:"
        );
        let mut native = original.clone();
        native.correlation = Some(NativeCorrelation {
            tool_call_id: Some(original.observation_id.clone()),
            turn_id: Some("turn:一".into()),
            ..Default::default()
        });
        assert_eq!(
            native.storage_key_parts(),
            [
                "tool_preflight",
                "lead",
                "",
                "tool_call_id",
                original.observation_id.as_str(),
                "turn:一",
                ""
            ]
        );
        assert_ne!(original.storage_key_parts(), native.storage_key_parts());
        let mut retry = native.clone();
        retry.observation_id = Uuid::new_v4().to_string();
        retry.observed_mono_ns = "00000000000000000999".into();
        assert_eq!(native.storage_key_parts(), retry.storage_key_parts());
        retry.actor = Actor::Child {
            agent_id: "child:一".into(),
            agent_key: crate::protocol::sha256_hex("child:一".as_bytes()),
        };
        assert_ne!(native.storage_key_parts(), retry.storage_key_parts());
        let mut server_a = native.clone();
        server_a.body = ObservationBody::ElicitationRequested {
            mode: ElicitationMode::Form,
        };
        server_a.correlation = Some(NativeCorrelation {
            elicitation_id: Some("request:1".into()),
            mcp_server_name: Some("server:a".into()),
            ..Default::default()
        });
        let mut server_b = server_a.clone();
        server_b.correlation.as_mut().unwrap().mcp_server_name = Some("server:b".into());
        assert_ne!(server_a.storage_key_parts(), server_b.storage_key_parts());

        let mut corpus = vec![original, native, retry, server_a, server_b];
        for case in fixtures["cases"].as_array().unwrap() {
            if case["expected"] == "valid" {
                let snapshot: LifecycleSnapshot =
                    serde_json::from_value(case["value"].clone()).unwrap();
                corpus.extend(snapshot.pools.requests.observations);
                corpus.extend(snapshot.pools.general.observations);
            }
        }
        for a in &corpus {
            for b in &corpus {
                assert_eq!(
                    a.storage_key_parts() == b.storage_key_parts(),
                    a.storage_key() == b.storage_key()
                );
            }
        }
    }

    #[test]
    fn a_candidate_evicted_by_its_own_insertion_is_not_reported_stored() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/lifecycle/observations.json"
        ))
        .unwrap();
        let mut snapshot: LifecycleSnapshot =
            serde_json::from_value(fixtures["cases"][1]["value"].clone()).unwrap();
        let template = snapshot.pools.general.observations[0].clone();
        snapshot.pools.general.observations.clear();
        let maximum = manifest().unwrap().limits.lifecycle_pool_max_count;
        for index in 0..maximum {
            let mut item = template.clone();
            item.observation_id = Uuid::new_v4().to_string();
            item.observed_mono_ns = format!("{:020}", 1000 + index);
            assert!(snapshot.reduce(item).unwrap());
        }
        assert!(snapshot.pools.general.retention_floor_mono_ns.is_none());
        let full = snapshot.clone();
        let mut late = template;
        late.observation_id = Uuid::new_v4().to_string();
        late.observed_mono_ns = format!("{:020}", 500);
        assert!(!snapshot.reduce(late).unwrap());
        assert_eq!(snapshot, full);
    }
}
