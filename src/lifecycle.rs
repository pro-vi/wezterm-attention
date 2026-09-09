//! Pure lifecycle decisions and their record-application boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::compat::{
    reconcile_activity, reconcile_activity_clear, reconcile_activity_clear_locked,
    reconcile_agents, reconcile_launch_activity,
};
use crate::identity::{PaneAddress, canonical_uuid, pane_address};
use crate::protocol::{AttentionError, Diagnostic, Result, manifest};
use crate::providers::{ProviderAction, ProviderEvent};
use crate::records::{
    CommitPlan, RecordIdentity, Replacement, commit_nested_with, commit_triple_with, commit_with,
    launch_path, pane_path, read_record, state_root,
};
use crate::wezterm::RuntimePorts;

#[derive(Clone, Debug, Serialize)]
pub struct LifecycleResult {
    pub disposition: String,
    pub diagnostic: Option<Diagnostic>,
    pub event_id: Option<String>,
    pub repaired_projection: bool,
}

impl LifecycleResult {
    fn new(disposition: &str) -> Self {
        Self {
            disposition: disposition.to_owned(),
            diagnostic: None,
            event_id: None,
            repaired_projection: false,
        }
    }

    fn diagnosed(disposition: &str, code: &str, message: &str) -> Self {
        let mut result = Self::new(disposition);
        result.diagnostic = Some(AttentionError::new(code, message).diagnostic);
        result
    }

    fn ignored_error(error: AttentionError) -> Self {
        let mut result = Self::new("ignored");
        result.diagnostic = Some(error.diagnostic);
        result
    }
}

#[derive(Clone, Debug)]
struct ResolvedLaunch {
    root: PathBuf,
    address: PaneAddress,
    launch_id: String,
}

#[derive(Clone, Debug)]
enum Projection {
    None,
    LaunchActivity(Value),
    Activity(Option<Value>),
    ActivityClear(String),
    Agents(PathBuf),
    ActivityAndAgents(Option<Value>, PathBuf),
}

#[derive(Clone, Debug)]
struct Mutation {
    result: LifecycleResult,
    projection: Projection,
}

impl Mutation {
    fn plain(result: LifecycleResult) -> Self {
        Self {
            result,
            projection: Projection::None,
        }
    }
}

pub fn binding_id(provider: &str, provider_session_id: &str, launch_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(provider.as_bytes());
    digest.update([0]);
    digest.update(provider_session_id.as_bytes());
    digest.update([0]);
    digest.update(launch_id.as_bytes());
    format!("{:x}", digest.finalize())
}

fn record_matches_launch(record: &Value, address: &PaneAddress, launch_id: &str) -> bool {
    record.get("address") == serde_json::to_value(address).ok().as_ref()
        && record.get("launch_id").and_then(Value::as_str) == Some(launch_id)
}

fn record_matches_binding(
    record: &Value,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> bool {
    record_matches_launch(record, address, launch_id)
        && record.get("binding_id").and_then(Value::as_str) == Some(binding_id)
}

fn read_current(
    launch: &Path,
    pointer: Option<Value>,
    address: &PaneAddress,
    launch_id: &str,
) -> Result<(Option<Value>, Option<Value>)> {
    let Some(pointer) = pointer else {
        return Ok((None, None));
    };
    let binding_id = pointer
        .get("binding_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AttentionError::new("record_invalid", "current binding pointer is invalid")
        })?;
    if !record_matches_binding(&pointer, address, launch_id, binding_id) {
        return Err(AttentionError::new(
            "record_invalid",
            "current binding pointer identity mismatches its path",
        ));
    }
    let binding = read_record(
        &launch
            .join("bindings")
            .join(binding_id)
            .join("binding.json"),
        Some("binding"),
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?
    .ok_or_else(|| AttentionError::new("record_invalid", "current binding record is missing"))?;
    if !record_matches_binding(&binding, address, launch_id, binding_id) {
        return Err(AttentionError::new(
            "record_invalid",
            "current binding record identity mismatches its path",
        ));
    }
    Ok((Some(pointer), Some(binding)))
}

fn provider_name(event: &ProviderEvent) -> Result<&'static str> {
    event
        .provider
        .map(|provider| provider.as_str())
        .ok_or_else(|| AttentionError::new("record_invalid", "provider is missing"))
}

fn event_binding_id(event: &ProviderEvent, launch_id: &str) -> Result<String> {
    let session = event
        .provider_session_id
        .as_deref()
        .ok_or_else(|| AttentionError::new("record_invalid", "provider session id is missing"))?;
    Ok(binding_id(provider_name(event)?, session, launch_id))
}

fn load_claim(root: &Path, address: &PaneAddress) -> Result<Option<Value>> {
    read_record(
        &pane_path(root, address).join("claim.json"),
        Some("claim"),
        &RecordIdentity::pane(address),
    )
}

fn resolve_launch(
    event: &ProviderEvent,
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<ResolvedLaunch> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let claim = load_claim(&root, &address)?;
    if let Some(inherited) = env.get("WEZTERM_ATTENTION_LAUNCH_ID") {
        let inherited = canonical_uuid(Some(inherited), "WEZTERM_ATTENTION_LAUNCH_ID")?;
        if claim
            .as_ref()
            .is_some_and(|record| record_matches_launch(record, &address, &inherited))
        {
            return Ok(ResolvedLaunch {
                root,
                address,
                launch_id: inherited,
            });
        }
        return Err(AttentionError::new(
            "claim_stale",
            "inherited launch does not match the pane claim",
        ));
    }

    let controlling = ports.tty.controlling_path()?;
    let fingerprint = ports.tty.fingerprint(&controlling)?;
    if let Some(claim) = claim
        && claim.get("tty_path").and_then(Value::as_str) == Some(controlling.as_str())
        && claim.get("tty_fingerprint").and_then(Value::as_str) == Some(fingerprint.as_str())
    {
        let launch_id = claim["launch_id"].as_str().unwrap_or_default().to_owned();
        return Ok(ResolvedLaunch {
            root,
            address,
            launch_id,
        });
    }

    if event.action != ProviderAction::Binding {
        return Err(AttentionError::new(
            "claim_stale",
            "provider event has no matching pane claim",
        ));
    }
    if env
        .get("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM")
        .map(String::as_str)
        != Some("1")
    {
        return Err(AttentionError::new(
            "claim_stale",
            "provider self-claim is disabled pending contact verification",
        ));
    }
    let socket = env.get("WEZTERM_UNIX_SOCKET").ok_or_else(|| {
        AttentionError::new("identity_unpublished", "WEZTERM_UNIX_SOCKET is missing")
    })?;
    let rows = ports.panes.list(socket)?;
    let matching = rows.iter().any(|row| {
        row.pane_id == address.pane_id && row.tty_name.as_deref() == Some(controlling.as_str())
    });
    if !matching {
        return Err(AttentionError::new(
            "unsafe_tty",
            "controlling terminal does not match the enumerated pane",
        ));
    }
    let launch_id = Uuid::new_v4().to_string();
    let mut claimed_env = env.clone();
    claimed_env.insert("WEZTERM_ATTENTION_LAUNCH_ID".to_owned(), launch_id.clone());
    crate::claim_launch_at_tty(&claimed_env, ports, &controlling)?;
    Ok(ResolvedLaunch {
        root,
        address,
        launch_id,
    })
}

fn binding_facts(event: &ProviderEvent) -> Vec<(&'static str, String)> {
    [
        ("expected_session_id", event.expected_session_id.as_ref()),
        ("transcript_path", event.transcript_path.as_ref()),
        ("cwd", event.cwd.as_ref()),
        ("config_dir", event.config_dir.as_ref()),
        ("model", event.model.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name, value.clone())))
    .collect()
}

fn binding_mutation(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
) -> Result<Mutation> {
    let provider = provider_name(event)?;
    let session = event.provider_session_id.as_deref().ok_or_else(|| {
        AttentionError::new("record_invalid", "binding event has no provider session")
    })?;
    let source = event.start_source.as_deref().ok_or_else(|| {
        AttentionError::new("record_invalid", "binding event has no start source")
    })?;
    let binding_id = binding_id(provider, session, &resolved.launch_id);
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_path = launch
        .join("bindings")
        .join(&binding_id)
        .join("binding.json");
    let pointer_path = launch.join("current-binding.json");
    let (mutation, ()) = commit_with(
        &launch.join(".lock"),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            let (_, current) = read_current(
                &launch,
                pointer.clone(),
                &resolved.address,
                &resolved.launch_id,
            )?;
            let existing = read_record(
                &binding_path,
                Some("binding"),
                &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
            )?;
            if let Some(existing) = &existing
                && !record_matches_binding(
                    existing,
                    &resolved.address,
                    &resolved.launch_id,
                    &binding_id,
                )
            {
                return Err(AttentionError::new(
                    "record_invalid",
                    "binding identity mismatches its path",
                ));
            }
            if let Some(current) = &current
                && current.get("binding_id").and_then(Value::as_str) != Some(binding_id.as_str())
            {
                let current_order = current["observed_mono_ns"].as_str().unwrap_or("");
                if observation < current_order {
                    return Ok(CommitPlan {
                        result: Mutation::plain(LifecycleResult::diagnosed(
                            "ignored",
                            "binding_conflict",
                            "older binding selection was ignored",
                        )),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
                if observation == current_order {
                    return Ok(CommitPlan {
                        result: Mutation::plain(LifecycleResult::diagnosed(
                            "conflict",
                            "binding_conflict",
                            "equal binding order names a different binding",
                        )),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
                let current_id = current["binding_id"].as_str().unwrap_or("");
                let current_end = read_record(
                    &launch.join("bindings").join(current_id).join("end.json"),
                    Some("binding_end"),
                    &RecordIdentity::binding(&resolved.address, &resolved.launch_id, current_id),
                )?;
                let current_ended = current_end.as_ref().is_some_and(|end| {
                    end["observed_mono_ns"].as_str().unwrap_or("") >= current_order
                });
                let replace = match provider {
                    "claude" | "codex" => matches!(source, "resume" | "clear"),
                    "pi" => matches!(source, "new" | "resume" | "fork"),
                    _ => false,
                };
                if matches!(source, "compact" | "reload") || (!current_ended && !replace) {
                    return Ok(CommitPlan {
                        result: Mutation::plain(LifecycleResult::diagnosed(
                            "conflict",
                            "binding_conflict",
                            "provider start cannot replace the active binding",
                        )),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
            }

            let pointer_record = json!({
                "kind": "current_binding",
                "schema": manifest()?.record_schema,
                "address": resolved.address,
                "launch_id": resolved.launch_id,
                "binding_id": binding_id,
            });
            let mut replacements = Vec::new();
            let result = if let Some(mut existing) = existing {
                let existing_order = existing["observed_mono_ns"].as_str().unwrap_or("");
                if observation < existing_order {
                    LifecycleResult::diagnosed(
                        "ignored",
                        "binding_conflict",
                        "older binding observation was ignored",
                    )
                } else if observation == existing_order {
                    let conflicts = binding_facts(event).iter().any(|(field, value)| {
                        existing.get(*field).and_then(Value::as_str) != Some(value)
                    });
                    if conflicts {
                        LifecycleResult::diagnosed(
                            "conflict",
                            "binding_conflict",
                            "equal binding order has different facts",
                        )
                    } else {
                        if pointer
                            .as_ref()
                            .and_then(|value| value.get("binding_id"))
                            .and_then(Value::as_str)
                            != Some(binding_id.as_str())
                        {
                            replacements
                                .push(Replacement::always(pointer_path.clone(), pointer_record));
                        }
                        let mut result = LifecycleResult::new("confirmed");
                        result.event_id = existing["event_id"].as_str().map(str::to_owned);
                        result
                    }
                } else {
                    let event_id = Uuid::new_v4().to_string();
                    existing["event_id"] = json!(event_id);
                    existing["observed_mono_ns"] = json!(observation);
                    existing["written_at_unix_ns"] = json!(written_at);
                    replacements.push(Replacement::always(binding_path.clone(), existing));
                    replacements.push(Replacement::if_different(
                        pointer_path.clone(),
                        pointer_record,
                    ));
                    let mut result = LifecycleResult::new("confirmed");
                    result.event_id = Some(event_id);
                    result
                }
            } else {
                let event_id = Uuid::new_v4().to_string();
                let mut record = json!({
                    "kind": "binding",
                    "schema": manifest()?.record_schema,
                    "address": resolved.address,
                    "launch_id": resolved.launch_id,
                    "binding_id": binding_id,
                    "event_id": event_id,
                    "provider": provider,
                    "provider_session_id": session,
                    "start_source": source,
                    "observed_mono_ns": observation,
                    "written_at_unix_ns": written_at,
                    "writer_version": manifest()?.writer_version,
                });
                for (field, value) in binding_facts(event) {
                    record[field] = json!(value);
                }
                replacements.push(Replacement::always(binding_path.clone(), record));
                replacements.push(Replacement::always(pointer_path.clone(), pointer_record));
                let mut result = LifecycleResult::new(if current.is_some() {
                    "replaced"
                } else {
                    "applied"
                });
                result.event_id = Some(event_id);
                result
            };
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements,
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation)
}

fn activity_base(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    binding_id: &str,
) -> Result<Value> {
    let activity_type = event.activity_type.as_deref().ok_or_else(|| {
        AttentionError::new("record_invalid", "provider activity type is missing")
    })?;
    if !manifest()?.enums.activity_types.contains(activity_type) {
        return Err(AttentionError::new(
            "record_invalid",
            "provider activity type is invalid",
        ));
    }
    let mut value = json!({
        "kind": "activity",
        "schema": manifest()?.record_schema,
        "address": resolved.address,
        "launch_id": resolved.launch_id,
        "target": {"kind": "binding", "binding_id": binding_id},
        "type": activity_type,
        "source": provider_name(event)?,
        "puppet": false,
    });
    if let Some(label) = &event.label {
        value["label"] = json!(label);
    }
    Ok(value)
}

fn semantic_activity(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        for field in [
            "event_id",
            "observed_mono_ns",
            "written_at_unix_ns",
            "publication_id",
        ] {
            object.remove(field);
        }
    }
    value
}

fn apply_activity(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
    parent_stop: bool,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    let activity_path = binding_dir.join("activity.json");
    let pointer_path = launch.join("current-binding.json");
    let base = activity_base(resolved, event, &binding_id)?;
    let (mutation, projected) = commit_with(
        &launch.join(".lock"),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            let (_, current) =
                read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
            if current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                != Some(binding_id.as_str())
            {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "activity event is not for the current binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let identity =
                RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id);
            let existing = read_record(&activity_path, Some("activity"), &identity)?;
            let clear = read_record(
                &binding_dir.join("activity-clear.json"),
                Some("activity_clear"),
                &identity,
            )?;
            let visible = clear.as_ref().is_none_or(|clear| {
                existing.as_ref().is_some_and(|activity| {
                    activity["observed_mono_ns"].as_str().unwrap_or("")
                        > clear["observed_mono_ns"].as_str().unwrap_or("")
                })
            });
            let mut replacements = Vec::new();
            let (result, activity) = if let Some(existing) = existing {
                if visible && semantic_activity(existing.clone()) == base {
                    let mut result = LifecycleResult::new("skipped");
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    (result, Some(existing))
                } else {
                    let order = existing["observed_mono_ns"].as_str().unwrap_or("");
                    if observation < order {
                        (LifecycleResult::new("ignored"), Some(existing))
                    } else if observation == order {
                        (
                            LifecycleResult::diagnosed(
                                "conflict",
                                "record_invalid",
                                "equal activity order has different content",
                            ),
                            Some(existing),
                        )
                    } else if clear.as_ref().is_some_and(|clear| {
                        observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
                    }) {
                        (
                            LifecycleResult::diagnosed(
                                "ignored",
                                "binding_conflict",
                                "activity observation is covered by activity clear",
                            ),
                            Some(existing),
                        )
                    } else {
                        let event_id = Uuid::new_v4().to_string();
                        let mut record = base.clone();
                        record["event_id"] = json!(event_id);
                        record["observed_mono_ns"] = json!(observation);
                        record["written_at_unix_ns"] = json!(written_at);
                        replacements
                            .push(Replacement::always(activity_path.clone(), record.clone()));
                        let mut result = LifecycleResult::new("applied");
                        result.event_id = Some(event_id);
                        (result, Some(record))
                    }
                }
            } else if clear.as_ref().is_some_and(|clear| {
                observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                (
                    LifecycleResult::diagnosed(
                        "ignored",
                        "binding_conflict",
                        "activity observation is covered by activity clear",
                    ),
                    None,
                )
            } else {
                let event_id = Uuid::new_v4().to_string();
                let mut record = base.clone();
                record["event_id"] = json!(event_id);
                record["observed_mono_ns"] = json!(observation);
                record["written_at_unix_ns"] = json!(written_at);
                replacements.push(Replacement::always(activity_path.clone(), record.clone()));
                let mut result = LifecycleResult::new("applied");
                result.event_id = Some(event_id);
                (result, Some(record))
            };
            let projection_activity = activity.clone().filter(|activity| {
                clear.as_ref().is_none_or(|clear| {
                    activity["observed_mono_ns"].as_str().unwrap_or("")
                        > clear["observed_mono_ns"].as_str().unwrap_or("")
                })
            });
            let projection = if parent_stop
                && !matches!(result.disposition.as_str(), "ignored" | "conflict")
            {
                let surviving_activity = activity.as_ref().ok_or_else(|| {
                    AttentionError::new("record_invalid", "parent stop has no surviving activity")
                })?;
                let surviving_order = surviving_activity["observed_mono_ns"]
                    .as_str()
                    .unwrap_or("");
                let desired = json!({
                    "kind": "subagent_clear",
                    "schema": manifest()?.record_schema,
                    "address": resolved.address,
                    "launch_id": resolved.launch_id,
                    "binding_id": binding_id,
                    "event_id": surviving_activity["event_id"],
                    "observed_mono_ns": surviving_order,
                });
                let clear_path = binding_dir.join("agents-clear.json");
                let existing_clear = read_record(&clear_path, Some("subagent_clear"), &identity)?;
                if existing_clear.as_ref().is_none_or(|current| {
                    current["observed_mono_ns"].as_str().unwrap_or("") < surviving_order
                }) {
                    replacements.push(Replacement::always(clear_path, desired));
                } else if existing_clear.as_ref().is_some_and(|current| {
                    current["observed_mono_ns"].as_str().unwrap_or("") == surviving_order
                        && current != &desired
                }) {
                    return Ok(CommitPlan {
                        result: Mutation::plain(LifecycleResult::diagnosed(
                            "conflict",
                            "record_invalid",
                            "equal parent-clear order has different content",
                        )),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
                Projection::ActivityAndAgents(projection_activity, binding_dir.clone())
            } else {
                Projection::Activity(projection_activity)
            };
            Ok(CommitPlan {
                result: Mutation { result, projection },
                replacements,
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| match &mutation.projection {
            Projection::ActivityClear(clear_order) => reconcile_activity_clear_locked(
                &resolved.root,
                &resolved.address,
                &resolved.launch_id,
                &binding_id,
                clear_order,
            ),
            _ => apply_projection(resolved, &binding_id, mutation),
        },
    )?;
    let mut result = mutation.result;
    if projected && result.disposition == "skipped" {
        result.disposition = "repaired_projection".to_owned();
        result.repaired_projection = true;
    }
    Ok(result)
}

fn apply_projection(
    resolved: &ResolvedLaunch,
    binding_id: &str,
    mutation: &Mutation,
) -> Result<bool> {
    match &mutation.projection {
        Projection::None => Ok(false),
        Projection::LaunchActivity(activity) => reconcile_launch_activity(
            &resolved.root,
            &resolved.address,
            &resolved.launch_id,
            activity,
        ),
        Projection::Activity(activity) => reconcile_activity(
            &resolved.root,
            &resolved.address,
            &resolved.launch_id,
            binding_id,
            activity.as_ref(),
        ),
        Projection::ActivityClear(clear_order) => reconcile_activity_clear(
            &resolved.root,
            &resolved.address,
            &resolved.launch_id,
            binding_id,
            clear_order,
        ),
        Projection::Agents(binding_dir) => reconcile_agents(
            &resolved.root,
            &resolved.address,
            &resolved.launch_id,
            binding_id,
            binding_dir,
        ),
        Projection::ActivityAndAgents(activity, binding_dir) => {
            let activity_changed = reconcile_activity(
                &resolved.root,
                &resolved.address,
                &resolved.launch_id,
                binding_id,
                activity.as_ref(),
            )?;
            let agents_changed = reconcile_agents(
                &resolved.root,
                &resolved.address,
                &resolved.launch_id,
                binding_id,
                binding_dir,
            )?;
            Ok(activity_changed || agents_changed)
        }
    }
}

fn safe_mark_text(value: &str, field: &str, maximum: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || value
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
    {
        return Err(AttentionError::usage(format!("{field} is invalid")));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn apply_mark_activity(
    env: &BTreeMap<String, String>,
    activity_type: &str,
    source: &str,
    frame: Option<u64>,
    label: Option<&str>,
    ttl_ms: Option<u64>,
    observation: &str,
    written_at: &str,
) -> Result<LifecycleResult> {
    if !manifest()?.enums.activity_types.contains(activity_type) {
        return Err(AttentionError::usage("mark state is invalid"));
    }
    safe_mark_text(source, "source", manifest()?.limits.safe_label_max_bytes)?;
    if let Some(label) = label {
        safe_mark_text(label, "label", manifest()?.limits.safe_label_max_bytes)?;
    }
    if frame.is_some_and(|frame| frame > manifest().expect("manifest loaded").limits.frame_max) {
        return Err(AttentionError::usage("frame is out of range"));
    }
    if ttl_ms
        .is_some_and(|ttl| ttl == 0 || ttl > manifest().expect("manifest loaded").limits.ttl_ms_max)
    {
        return Err(AttentionError::usage("ttl_ms must be positive"));
    }
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    let claim = load_claim(&root, &address)?;
    if claim
        .as_ref()
        .is_none_or(|claim| !record_matches_launch(claim, &address, &launch_id))
    {
        return Err(AttentionError::new(
            "claim_stale",
            "current launch does not match claim",
        ));
    }
    let resolved = ResolvedLaunch {
        root,
        address,
        launch_id,
    };
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let pointer_path = launch.join("current-binding.json");
    let (mutation, projected) = commit_with(
        &launch.join(".lock"),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            let (_, current) =
                read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
            let binding_id = current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let (path, target, clear) = match &binding_id {
                Some(binding_id) => {
                    let directory = launch.join("bindings").join(binding_id);
                    (
                        directory.join("activity.json"),
                        json!({"kind":"binding","binding_id":binding_id}),
                        read_record(
                            &directory.join("activity-clear.json"),
                            Some("activity_clear"),
                            &RecordIdentity::binding(
                                &resolved.address,
                                &resolved.launch_id,
                                binding_id,
                            ),
                        )?,
                    )
                }
                None => (launch.join("activity.json"), json!({"kind":"launch"}), None),
            };
            let mut base = json!({
                "kind": "activity",
                "schema": manifest()?.record_schema,
                "address": resolved.address,
                "launch_id": resolved.launch_id,
                "target": target,
                "type": activity_type,
                "source": source,
                "puppet": false,
            });
            if let Some(frame) = frame {
                base["frame"] = json!(frame);
            }
            if let Some(label) = label {
                base["label"] = json!(label);
            }
            if let Some(ttl_ms) = ttl_ms {
                base["ttl_ms"] = json!(ttl_ms);
            }
            let activity_identity = match binding_id.as_deref() {
                Some(binding_id) => {
                    RecordIdentity::binding(&resolved.address, &resolved.launch_id, binding_id)
                }
                None => RecordIdentity::launch(&resolved.address, &resolved.launch_id),
            };
            let existing = read_record(&path, Some("activity"), &activity_identity)?;
            let visible = clear.as_ref().is_none_or(|clear| {
                existing.as_ref().is_some_and(|activity| {
                    activity["observed_mono_ns"].as_str().unwrap_or("")
                        > clear["observed_mono_ns"].as_str().unwrap_or("")
                })
            });
            let mut replacements = Vec::new();
            let (result, activity) = if let Some(existing) = existing {
                if visible && semantic_activity(existing.clone()) == base {
                    let mut result = LifecycleResult::new("skipped");
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    (result, existing)
                } else {
                    let order = existing["observed_mono_ns"].as_str().unwrap_or("");
                    if observation < order {
                        (LifecycleResult::new("ignored"), existing)
                    } else if observation == order {
                        (
                            LifecycleResult::diagnosed(
                                "conflict",
                                "record_invalid",
                                "equal activity order has different content",
                            ),
                            existing,
                        )
                    } else if clear.as_ref().is_some_and(|clear| {
                        observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
                    }) {
                        (
                            LifecycleResult::diagnosed(
                                "ignored",
                                "binding_conflict",
                                "activity observation is covered by activity clear",
                            ),
                            existing,
                        )
                    } else {
                        let event_id = Uuid::new_v4().to_string();
                        let mut record = base.clone();
                        record["event_id"] = json!(event_id);
                        record["observed_mono_ns"] = json!(observation);
                        record["written_at_unix_ns"] = json!(written_at);
                        replacements.push(Replacement::always(path.clone(), record.clone()));
                        let mut result = LifecycleResult::new("applied");
                        result.event_id = Some(event_id);
                        (result, record)
                    }
                }
            } else if clear.as_ref().is_some_and(|clear| {
                observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                (
                    LifecycleResult::diagnosed(
                        "ignored",
                        "binding_conflict",
                        "activity observation is covered by activity clear",
                    ),
                    base,
                )
            } else {
                let event_id = Uuid::new_v4().to_string();
                let mut record = base;
                record["event_id"] = json!(event_id);
                record["observed_mono_ns"] = json!(observation);
                record["written_at_unix_ns"] = json!(written_at);
                replacements.push(Replacement::always(path, record.clone()));
                let mut result = LifecycleResult::new("applied");
                result.event_id = Some(event_id);
                (result, record)
            };
            let projection = if matches!(result.disposition.as_str(), "ignored" | "conflict") {
                Projection::None
            } else {
                match binding_id {
                    Some(_) => Projection::Activity(Some(activity)),
                    None => Projection::LaunchActivity(activity),
                }
            };
            Ok(CommitPlan {
                result: Mutation { result, projection },
                replacements,
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            let binding_id = read_record(
                &pointer_path,
                Some("current_binding"),
                &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
            )?
            .and_then(|pointer| pointer["binding_id"].as_str().map(str::to_owned));
            apply_projection(&resolved, binding_id.as_deref().unwrap_or(""), mutation)
        },
    )?;
    let mut result = mutation.result;
    if projected && result.disposition == "skipped" {
        result.disposition = "repaired_projection".to_owned();
        result.repaired_projection = true;
    }
    Ok(result)
}

pub fn apply_mark_review(
    env: &BTreeMap<String, String>,
    source: &str,
    clear: bool,
) -> Result<LifecycleResult> {
    safe_mark_text(source, "source", manifest()?.limits.safe_label_max_bytes)?;
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    let owner_key = crate::protocol::sha256_hex(source.as_bytes());
    let pane = pane_path(&root, &address);
    let review_path = pane.join("reviews").join(format!("{owner_key}.json"));
    let (mutation, ()) = commit_nested_with(
        &pane.join(".claim.lock"),
        &pane.join("reviews").join(format!(".{owner_key}.lock")),
        &pane.join("claim.json"),
        Some("claim"),
        &RecordIdentity::pane(&address),
        Duration::from_secs(2),
        |claim| {
            if claim
                .as_ref()
                .is_none_or(|claim| !record_matches_launch(claim, &address, &launch_id))
            {
                return Err(AttentionError::new(
                    "claim_stale",
                    "current launch does not match claim",
                ));
            }
            let existing = read_record(
                &review_path,
                Some("review"),
                &RecordIdentity::review(&address, &owner_key),
            )?;
            if clear {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::new(if existing.is_some() {
                        "applied"
                    } else {
                        "skipped"
                    })),
                    replacements: Vec::new(),
                    removals: vec![review_path.clone()],
                    private_dirs: Vec::new(),
                });
            }
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "review",
                "schema": manifest()?.record_schema,
                "address": address,
                "owner_id": source,
                "owner_key": owner_key,
                "event_id": event_id,
            });
            let mut result = LifecycleResult::new("applied");
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements: vec![Replacement::always(review_path.clone(), record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

fn apply_child(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let agent_id = event
        .agent_id
        .as_deref()
        .ok_or_else(|| AttentionError::new("record_invalid", "child event has no agent id"))?;
    let source = event
        .agent_type
        .as_deref()
        .or(event.child_source.as_deref())
        .ok_or_else(|| AttentionError::new("record_invalid", "child event has no source"))?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    let binding_path = binding_dir.join("binding.json");
    let agent_key = crate::protocol::sha256_hex(agent_id.as_bytes());
    let presence_path = binding_dir.join("agents").join(format!("{agent_key}.json"));
    let status = if event.action == ProviderAction::ChildActive {
        "active"
    } else {
        "stopped"
    };
    let (mutation, projected) = commit_with(
        &launch.join(".lock"),
        &binding_path,
        Some("binding"),
        &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
        Duration::from_secs(2),
        |binding| {
            if binding.as_ref().is_none_or(|record| {
                !record_matches_binding(record, &resolved.address, &resolved.launch_id, &binding_id)
            }) {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "child event has no matching binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let identity =
                RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id);
            let existing = read_record(
                &presence_path,
                Some("subagent_presence"),
                &RecordIdentity::agent(
                    &resolved.address,
                    &resolved.launch_id,
                    &binding_id,
                    &agent_key,
                ),
            )?;
            let floor = read_record(
                &binding_dir.join("agents-floor.json"),
                Some("subagent_retention_floor"),
                &identity,
            )?;
            let clear = read_record(
                &binding_dir.join("agents-clear.json"),
                Some("subagent_clear"),
                &identity,
            )?;
            let mut replacements = Vec::new();
            let result = if let Some(existing) = existing {
                let existing_order = existing["observed_mono_ns"].as_str().unwrap_or("");
                if existing["status"] == "stopped" && status == "stopped" {
                    let mut result = LifecycleResult::new("skipped");
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    result
                } else if observation < existing_order {
                    LifecycleResult::new("ignored")
                } else if observation == existing_order {
                    if existing["status"] == status && existing["source"] == source {
                        let mut result = LifecycleResult::new("skipped");
                        result.event_id = existing["event_id"].as_str().map(str::to_owned);
                        result
                    } else {
                        LifecycleResult::diagnosed(
                            "conflict",
                            "record_invalid",
                            "equal child order has different content",
                        )
                    }
                } else {
                    child_replacement(
                        resolved,
                        event,
                        observation,
                        written_at,
                        &binding_id,
                        agent_id,
                        &agent_key,
                        source,
                        status,
                        &presence_path,
                        floor.as_ref(),
                        clear.as_ref(),
                        &mut replacements,
                    )?
                }
            } else {
                child_replacement(
                    resolved,
                    event,
                    observation,
                    written_at,
                    &binding_id,
                    agent_id,
                    &agent_key,
                    source,
                    status,
                    &presence_path,
                    floor.as_ref(),
                    clear.as_ref(),
                    &mut replacements,
                )?
            };
            Ok(CommitPlan {
                result: Mutation {
                    result,
                    projection: Projection::Agents(binding_dir.clone()),
                },
                replacements,
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| match &mutation.projection {
            Projection::ActivityClear(clear_order) => reconcile_activity_clear_locked(
                &resolved.root,
                &resolved.address,
                &resolved.launch_id,
                &binding_id,
                clear_order,
            ),
            _ => apply_projection(resolved, &binding_id, mutation),
        },
    )?;
    let mut result = mutation.result;
    if projected && result.disposition == "skipped" {
        result.disposition = "repaired_projection".to_owned();
        result.repaired_projection = true;
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn child_replacement(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
    binding_id: &str,
    agent_id: &str,
    agent_key: &str,
    source: &str,
    status: &str,
    presence_path: &Path,
    floor: Option<&Value>,
    clear: Option<&Value>,
    replacements: &mut Vec<Replacement>,
) -> Result<LifecycleResult> {
    if floor.is_some_and(|floor| observation <= floor["floor_mono_ns"].as_str().unwrap_or("")) {
        return Ok(LifecycleResult::diagnosed(
            "ignored",
            "binding_conflict",
            "child observation is covered by retention floor",
        ));
    }
    if status == "active"
        && clear
            .is_some_and(|clear| observation <= clear["observed_mono_ns"].as_str().unwrap_or(""))
    {
        return Ok(LifecycleResult::diagnosed(
            "ignored",
            "binding_conflict",
            "child observation is covered by parent clear",
        ));
    }
    let event_id = Uuid::new_v4().to_string();
    let record = json!({
        "kind": "subagent_presence",
        "schema": manifest()?.record_schema,
        "address": resolved.address,
        "launch_id": resolved.launch_id,
        "binding_id": binding_id,
        "provider": provider_name(event)?,
        "agent_id": agent_id,
        "agent_key": agent_key,
        "source": source,
        "status": status,
        "event_id": event_id,
        "observed_mono_ns": observation,
        "written_at_unix_ns": written_at,
        "ttl_ms": manifest()?.limits.subagent_ttl_ms,
    });
    replacements.push(Replacement::always(presence_path.to_path_buf(), record));
    let mut result = LifecycleResult::new("applied");
    result.event_id = Some(event_id);
    Ok(result)
}

fn apply_end(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    let binding_path = binding_dir.join("binding.json");
    let end_path = binding_dir.join("end.json");
    let (mutation, ()) = commit_with(
        &launch.join(".lock"),
        &binding_path,
        Some("binding"),
        &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
        Duration::from_secs(2),
        |binding| {
            let Some(binding) = binding else {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "end event has no matching binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            };
            if observation < binding["observed_mono_ns"].as_str().unwrap_or("") {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "binding_conflict",
                        "end observation predates binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let existing = read_record(
                &end_path,
                Some("binding_end"),
                &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
            )?;
            if let Some(existing) = existing {
                let order = existing["observed_mono_ns"].as_str().unwrap_or("");
                let disposition = if observation < order {
                    "ignored"
                } else if existing["reason"] == "session_end"
                    && order >= binding["observed_mono_ns"].as_str().unwrap_or("")
                {
                    "skipped"
                } else if observation == order {
                    "conflict"
                } else {
                    "applied"
                };
                if disposition != "applied" {
                    let mut result = LifecycleResult::new(disposition);
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    return Ok(CommitPlan {
                        result: Mutation::plain(result),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
            }
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "binding_end",
                "schema": manifest()?.record_schema,
                "address": resolved.address,
                "launch_id": resolved.launch_id,
                "binding_id": binding_id,
                "reason": "session_end",
                "event_id": event_id,
                "observed_mono_ns": observation,
                "written_at_unix_ns": written_at,
            });
            let mut result = LifecycleResult::new("applied");
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements: vec![Replacement::always(end_path.clone(), record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

fn apply_review_event(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    clear: bool,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let claim_path = pane_path(&resolved.root, &resolved.address).join("claim.json");
    let owner_id = "pi-bus";
    let owner_key = crate::protocol::sha256_hex(owner_id.as_bytes());
    let review_path = pane_path(&resolved.root, &resolved.address)
        .join("reviews")
        .join(format!("{owner_key}.json"));
    let (mutation, ()) = commit_triple_with(
        &launch.join(".lock"),
        &pane_path(&resolved.root, &resolved.address).join(".claim.lock"),
        &pane_path(&resolved.root, &resolved.address)
            .join("reviews")
            .join(format!(".{owner_key}.lock")),
        &claim_path,
        Some("claim"),
        &RecordIdentity::pane(&resolved.address),
        Duration::from_secs(2),
        |claim| {
            if claim.as_ref().is_none_or(|claim| {
                !record_matches_launch(claim, &resolved.address, &resolved.launch_id)
            }) {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "Pi review launch is no longer current",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let pointer = read_record(
                &launch.join("current-binding.json"),
                Some("current_binding"),
                &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
            )?;
            let (_, current) =
                read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
            if current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                != Some(binding_id.as_str())
            {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "Pi review is not for the current binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let existing = read_record(
                &review_path,
                Some("review"),
                &RecordIdentity::review(&resolved.address, &owner_key),
            )?;
            if let Some(existing) = &existing
                && (existing.get("address")
                    != serde_json::to_value(&resolved.address).ok().as_ref()
                    || existing.get("owner_key").and_then(Value::as_str)
                        != Some(owner_key.as_str()))
            {
                return Err(AttentionError::new(
                    "record_invalid",
                    "review record identity mismatches its path",
                ));
            }
            if clear {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::new(if existing.is_some() {
                        "applied"
                    } else {
                        "skipped"
                    })),
                    replacements: Vec::new(),
                    removals: vec![review_path.clone()],
                    private_dirs: Vec::new(),
                });
            }
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "review",
                "schema": manifest()?.record_schema,
                "address": resolved.address,
                "owner_id": owner_id,
                "owner_key": owner_key,
                "event_id": event_id,
            });
            let mut result = LifecycleResult::new("applied");
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements: vec![Replacement::always(review_path.clone(), record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

fn apply_clear_event(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    let pointer_path = launch.join("current-binding.json");
    let clear_path = binding_dir.join("activity-clear.json");
    let claim_path = pane_path(&resolved.root, &resolved.address).join("claim.json");
    let owner_key = crate::protocol::sha256_hex(b"pi-bus");
    let review_path = pane_path(&resolved.root, &resolved.address)
        .join("reviews")
        .join(format!("{owner_key}.json"));
    let (mutation, projected) = commit_triple_with(
        &launch.join(".lock"),
        &pane_path(&resolved.root, &resolved.address).join(".claim.lock"),
        &pane_path(&resolved.root, &resolved.address)
            .join("reviews")
            .join(format!(".{owner_key}.lock")),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            let claim = read_record(
                &claim_path,
                Some("claim"),
                &RecordIdentity::pane(&resolved.address),
            )?;
            if claim.as_ref().is_none_or(|claim| {
                !record_matches_launch(claim, &resolved.address, &resolved.launch_id)
            }) {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "clear event is not for the current launch",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let (_, current) =
                read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
            if current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                != Some(binding_id.as_str())
            {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        "ignored",
                        "claim_stale",
                        "clear event is not for the current binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let existing = read_record(
                &clear_path,
                Some("activity_clear"),
                &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
            )?;
            let mut replacements = Vec::new();
            if let Some(existing) = &existing
                && observation < existing["observed_mono_ns"].as_str().unwrap_or("")
            {
                let mut result = LifecycleResult::new("ignored");
                result.event_id = existing["event_id"].as_str().map(str::to_owned);
                return Ok(CommitPlan {
                    result: Mutation::plain(result),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let (mut result, clear_order) = if existing.as_ref().is_some_and(|clear| {
                observation == clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                let mut result = LifecycleResult::new("skipped");
                result.event_id = existing
                    .as_ref()
                    .and_then(|clear| clear["event_id"].as_str())
                    .map(str::to_owned);
                (
                    result,
                    existing
                        .as_ref()
                        .and_then(|clear| clear["observed_mono_ns"].as_str())
                        .unwrap_or("")
                        .to_owned(),
                )
            } else {
                let event_id = Uuid::new_v4().to_string();
                let record = json!({
                    "kind": "activity_clear",
                    "schema": manifest()?.record_schema,
                    "address": resolved.address,
                    "launch_id": resolved.launch_id,
                    "binding_id": binding_id,
                    "event_id": event_id,
                    "observed_mono_ns": observation,
                });
                replacements.push(Replacement::always(clear_path.clone(), record));
                let mut result = LifecycleResult::new("applied");
                result.event_id = Some(event_id);
                (result, observation.to_owned())
            };
            let review_existed = review_path.exists();
            if review_existed && result.disposition == "skipped" {
                result.disposition = "applied".to_owned();
            }
            Ok(CommitPlan {
                result: Mutation {
                    result,
                    projection: Projection::ActivityClear(clear_order),
                },
                replacements,
                removals: vec![review_path.clone()],
                private_dirs: Vec::new(),
            })
        },
        |mutation| match &mutation.projection {
            Projection::ActivityClear(clear_order) => reconcile_activity_clear_locked(
                &resolved.root,
                &resolved.address,
                &resolved.launch_id,
                &binding_id,
                clear_order,
            ),
            _ => apply_projection(resolved, &binding_id, mutation),
        },
    )?;
    let mut result = mutation.result;
    if projected && result.disposition == "skipped" {
        result.disposition = "repaired_projection".to_owned();
        result.repaired_projection = true;
    }
    Ok(result)
}

pub fn prompt_return(env: &BTreeMap<String, String>, observation: &str) -> Result<LifecycleResult> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    let claim = load_claim(&root, &address)?;
    if claim
        .as_ref()
        .is_none_or(|claim| !record_matches_launch(claim, &address, &launch_id))
    {
        return Ok(LifecycleResult::diagnosed(
            "ignored",
            "claim_stale",
            "prompt return has no matching claim",
        ));
    }
    let launch = launch_path(&root, &address, &launch_id);
    let pointer_path = launch.join("current-binding.json");
    let (mutation, projected) = commit_with(
        &launch.join(".lock"),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&address, &launch_id),
        Duration::from_secs(2),
        |pointer| {
            let (_, current) = read_current(&launch, pointer, &address, &launch_id)?;
            let Some(current) = current else {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::new("applied")),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            };
            let binding_id = current["binding_id"].as_str().unwrap_or("").to_owned();
            let binding_dir = launch.join("bindings").join(&binding_id);
            let clear_path = binding_dir.join("activity-clear.json");
            let existing = read_record(
                &clear_path,
                Some("activity_clear"),
                &RecordIdentity::binding(&address, &launch_id, &binding_id),
            )?;
            if let Some(existing) = &existing
                && observation < existing["observed_mono_ns"].as_str().unwrap_or("")
            {
                let mut result = LifecycleResult::new("ignored");
                result.event_id = existing["event_id"].as_str().map(str::to_owned);
                return Ok(CommitPlan {
                    result: Mutation::plain(result),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            if existing.as_ref().is_some_and(|clear| {
                observation == clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                let mut result = LifecycleResult::new("skipped");
                result.event_id = existing
                    .as_ref()
                    .and_then(|clear| clear["event_id"].as_str())
                    .map(str::to_owned);
                return Ok(CommitPlan {
                    result: Mutation {
                        result,
                        projection: Projection::ActivityClear(observation.to_owned()),
                    },
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "activity_clear",
                "schema": manifest()?.record_schema,
                "address": address,
                "launch_id": launch_id,
                "binding_id": binding_id,
                "event_id": event_id,
                "observed_mono_ns": observation,
            });
            let mut result = LifecycleResult::new("applied");
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation {
                    result,
                    projection: Projection::ActivityClear(observation.to_owned()),
                },
                replacements: vec![Replacement::always(clear_path, record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            let binding_id = mutation.result.event_id.as_deref();
            let pointer = read_record(
                &pointer_path,
                Some("current_binding"),
                &RecordIdentity::launch(&address, &launch_id),
            )?;
            let current_binding_id = pointer
                .as_ref()
                .and_then(|pointer| pointer["binding_id"].as_str())
                .unwrap_or("");
            if binding_id.is_none() && current_binding_id.is_empty() {
                return Ok(false);
            }
            apply_projection(
                &ResolvedLaunch {
                    root: root.clone(),
                    address: address.clone(),
                    launch_id: launch_id.clone(),
                },
                current_binding_id,
                mutation,
            )
        },
    )?;
    let mut result = mutation.result;
    if projected && result.disposition == "skipped" {
        result.disposition = "repaired_projection".to_owned();
        result.repaired_projection = true;
    }
    Ok(result)
}

pub fn apply_provider_event(
    event: &ProviderEvent,
    env: &BTreeMap<String, String>,
    observation: &str,
    ports: &RuntimePorts<'_>,
) -> Result<LifecycleResult> {
    if event.action == ProviderAction::Ignored {
        let mut result = LifecycleResult::new("ignored");
        result.diagnostic = event.diagnostic.clone();
        return Ok(result);
    }
    if observation.len() != 20 || !observation.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AttentionError::new(
            "record_invalid",
            "observation is invalid",
        ));
    }
    let resolved = match resolve_launch(event, env, ports) {
        Ok(resolved) => resolved,
        Err(error) => return Ok(LifecycleResult::ignored_error(error)),
    };
    let written_at = ports.clock.unix_ns20()?;
    match event.action {
        ProviderAction::Binding => {
            Ok(binding_mutation(&resolved, event, observation, &written_at)?.result)
        }
        ProviderAction::Activity => {
            apply_activity(&resolved, event, observation, &written_at, false)
        }
        ProviderAction::ParentStop => {
            apply_activity(&resolved, event, observation, &written_at, true)
        }
        ProviderAction::ChildActive | ProviderAction::ChildStopped => {
            apply_child(&resolved, event, observation, &written_at)
        }
        ProviderAction::End => apply_end(&resolved, event, observation, &written_at),
        ProviderAction::Review => apply_review_event(&resolved, event, false),
        ProviderAction::Clear => apply_clear_event(&resolved, event, observation),
        ProviderAction::Ignored => unreachable!(),
    }
}
