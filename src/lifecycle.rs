//! Pure lifecycle decisions and their record-application boundary.

pub mod outcome;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::identity::{PaneAddress, canonical_uuid, pane_address};
use crate::lifecycle::outcome::{
    AdmittedHook, BindingTarget, HookPersistence, HookScope, Persistence,
};
use crate::observations::{LifecycleSnapshot, ObservationPools};
use crate::protocol::{AttentionError, Diagnostic, Disposition, Result, free_of_control, manifest};
use crate::providers::{ProviderAction, ProviderEvent};
use crate::records::{
    CommitPlan, PreparedRecordWrite, RecordIdentity, RecordRead, Replacement, commit_nested_with,
    commit_triple_with, ends_binding, launch_path, pane_path, read_record, read_record_typed,
    session_entry, session_entry_path, state_root,
};
use crate::wezterm::RuntimePorts;

#[derive(Debug)]
pub struct HookOutcome {
    pub result: Result<LifecycleResult>,
    pub admission: Option<AdmittedHook>,
    pub persistence: HookPersistence,
    pub observation_id: Option<String>,
}

#[derive(Clone, Debug)]
struct HookEvidence {
    event: ProviderEvent,
    inherited: bool,
    persistence: HookPersistence,
    admission: Option<AdmittedHook>,
    planned_native: Option<bool>,
    pending_observation_id: Option<String>,
    observation_id: Option<String>,
}

fn accepted(result: &LifecycleResult) -> bool {
    matches!(
        result.disposition,
        Disposition::Applied
            | Disposition::Confirmed
            | Disposition::Replaced
            | Disposition::Skipped
            | Disposition::RepairedProjection
    )
}

// Called only by the native mutation's locked after-apply path. This records
// evidence; it never invokes consumers or resolves a later occupant for them.
fn confirm_native(resolved: &ResolvedLaunch, binding_id: &str, mutation: &Mutation) {
    let Some(cell) = &resolved.evidence else {
        return;
    };
    let mut evidence = cell.borrow_mut();
    let admitted = evidence
        .planned_native
        .unwrap_or_else(|| accepted(&mutation.result));
    if evidence.persistence.native_state != Persistence::NotRequested {
        evidence.persistence.native_state = if admitted {
            Persistence::Confirmed
        } else {
            Persistence::Rejected
        };
    }
    if evidence.persistence.activity != Persistence::NotRequested {
        evidence.persistence.activity = if admitted {
            Persistence::Confirmed
        } else {
            Persistence::Rejected
        };
    }
    if !evidence.inherited {
        return;
    }
    let context = (|| -> Result<Option<AdmittedHook>> {
        let claim = load_claim(&resolved.root, &resolved.address)?;
        if claim.as_ref().is_none_or(|claim| {
            !record_matches_launch(claim, &resolved.address, &resolved.launch_id)
        }) {
            return Ok(None);
        }
        let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
        let pointer = read_record(
            &launch.join("current-binding.json"),
            Some("current_binding"),
            &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        )?;
        let (_, binding) = read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
        let Some(binding) = binding.filter(|binding| {
            record_matches_binding(binding, &resolved.address, &resolved.launch_id, binding_id)
        }) else {
            return Ok(None);
        };
        let event = &evidence.event;
        if binding["provider"].as_str() != event.provider.map(|p| p.as_str())
            || binding["provider_session_id"].as_str() != event.provider_session_id.as_deref()
        {
            return Ok(None);
        }
        let actor = event
            .observation
            .as_ref()
            .map(|o| o.actor.clone())
            .unwrap_or_else(|| match &event.agent_id {
                Some(id) => crate::observations::Actor::Child {
                    agent_id: id.clone(),
                    agent_key: crate::protocol::sha256_hex(id.as_bytes()),
                },
                None => crate::observations::Actor::Lead,
            });
        Ok(Some(AdmittedHook {
            action: event.action,
            scope: HookScope {
                address: resolved.address.clone(),
                launch_id: resolved.launch_id.clone(),
                target: BindingTarget::Binding {
                    binding_id: binding_id.into(),
                },
            },
            provider: provider_name(event)?.into(),
            provider_session_id: event.provider_session_id.clone().unwrap_or_default(),
            source_event: event.source_event.clone(),
            actor,
            correlation: event
                .observation
                .as_ref()
                .and_then(|o| o.correlation.clone()),
        }))
    })();
    evidence.admission = context.ok().flatten();
}

#[derive(Clone, Debug, Serialize)]
pub struct LifecycleResult {
    pub disposition: Disposition,
    pub diagnostic: Option<Diagnostic>,
    pub event_id: Option<String>,
    pub repaired_projection: bool,
}

impl LifecycleResult {
    fn new(disposition: Disposition) -> Self {
        Self {
            disposition,
            diagnostic: None,
            event_id: None,
            repaired_projection: false,
        }
    }

    fn diagnosed(disposition: Disposition, code: &str, message: &str) -> Self {
        let mut result = Self::new(disposition);
        result.diagnostic = Some(AttentionError::new(code, message).diagnostic);
        result
    }

    fn ignored_error(error: AttentionError) -> Self {
        let mut result = Self::new(Disposition::Ignored);
        result.diagnostic = Some(error.diagnostic);
        result
    }
}

#[derive(Clone)]
struct ResolvedLaunch<'a> {
    evidence: Option<Rc<RefCell<HookEvidence>>>,
    root: PathBuf,
    address: PaneAddress,
    launch_id: String,
    /// The pane's claim as it stood when the event was resolved.
    claim: Value,
    /// For an event that carried no launch id, the proof of the agent process
    /// it came from, and what reads that process again.
    host: Option<HostCheck<'a>>,
    /// Why the claim this event made was not published to the terminal.
    publication_diagnostic: Option<Diagnostic>,
}

#[derive(Clone)]
struct HostCheck<'a> {
    proof: crate::launch::HostProof,
    env: &'a BTreeMap<String, String>,
    processes: &'a dyn crate::wezterm::ProcessInspector,
    tty: &'a dyn crate::wezterm::TtyWriter,
}

impl ResolvedLaunch<'_> {
    fn claim_lock(&self) -> PathBuf {
        pane_path(&self.root, &self.address).join(".claim.lock")
    }

    /// Why this event may no longer write, given the pane's claim as read
    /// under this launch's lock and then the claim lock.
    ///
    /// The claim must be exactly the one the event was resolved against, and
    /// an event that carried no launch id must still come from the agent
    /// process that proved it. Otherwise the event is refused outright: it
    /// writes nothing, and never continues into whatever launch holds the
    /// pane now.
    fn lapsed(&self, current: Option<&Value>) -> Option<LifecycleResult> {
        if current != Some(&self.claim) {
            return Some(LifecycleResult::diagnosed(
                Disposition::Ignored,
                "claim_stale",
                "the pane's claim changed after this event was resolved",
            ));
        }
        let host = self.host.as_ref()?;
        host.proof
            .confirm(host.env, host.processes, host.tty, &self.address)
            .err()
            .map(LifecycleResult::ignored_error)
    }

    /// [`Self::lapsed`] for a caller whose commit did not read the claim.
    fn lapsed_now(&self) -> Result<Option<LifecycleResult>> {
        Ok(self.lapsed(load_claim(&self.root, &self.address)?.as_ref()))
    }
}

/// A plan that writes nothing and reports `result`.
fn refusal(result: LifecycleResult) -> CommitPlan<Mutation> {
    CommitPlan {
        result: Mutation::plain(result),
        replacements: Vec::new(),
        removals: Vec::new(),
        private_dirs: Vec::new(),
    }
}

#[derive(Clone, Debug)]
struct Mutation {
    result: LifecycleResult,
    lifecycle_replacement: Option<PreparedRecordWrite>,
}

impl Mutation {
    fn plain(result: LifecycleResult) -> Self {
        Self {
            result,
            lifecycle_replacement: None,
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

/// Resolve the launch an agent event belongs to.
///
/// An inherited `WEZTERM_ATTENTION_LAUNCH_ID` decides alone: it matches the
/// pane's claim, or the event is refused, whatever else is true. Without one
/// the event resolves only against a claim its own agent process holds; see
/// [`crate::launch::self_owned_launch`]. Nothing here ever finds a launch by
/// the terminal alone.
fn resolve_launch<'a>(
    event: &ProviderEvent,
    env: &'a BTreeMap<String, String>,
    ports: &RuntimePorts<'a>,
) -> Result<ResolvedLaunch<'a>> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let claim = load_claim(&root, &address)?;
    if let Some(inherited) = env.get("WEZTERM_ATTENTION_LAUNCH_ID") {
        let inherited = canonical_uuid(Some(inherited), "WEZTERM_ATTENTION_LAUNCH_ID")?;
        return match claim {
            Some(claim) if record_matches_launch(&claim, &address, &inherited) => {
                Ok(ResolvedLaunch {
                    evidence: None,
                    root,
                    address,
                    launch_id: inherited,
                    claim,
                    host: None,
                    publication_diagnostic: None,
                })
            }
            _ => Err(AttentionError::new(
                "claim_stale",
                "inherited launch does not match the pane claim",
            )),
        };
    }
    let resolved = crate::launch::self_owned_launch(
        env,
        ports,
        &address,
        claim,
        event.action == ProviderAction::Binding,
    )?;
    let launch_id = resolved
        .claim
        .get("launch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AttentionError::new("record_invalid", "claim has no launch id"))?
        .to_owned();
    Ok(ResolvedLaunch {
        evidence: None,
        root,
        address,
        launch_id,
        claim: resolved.claim,
        host: Some(HostCheck {
            proof: resolved.proof,
            env,
            processes: ports.processes,
            tty: ports.tty,
        }),
        publication_diagnostic: resolved.publication_diagnostic,
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
    let (mutation, _) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(refusal(lapsed));
            }
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
                            Disposition::Ignored,
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
                            Disposition::Conflict,
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
                let current_ended = current_end
                    .as_ref()
                    .is_some_and(|end| ends_binding(end, current));
                let replace = match provider {
                    "claude" | "codex" => matches!(source, "resume" | "clear" | "fork"),
                    "pi" => matches!(source, "new" | "resume" | "fork"),
                    _ => false,
                };
                if matches!(source, "compact" | "reload") || (!current_ended && !replace) {
                    return Ok(CommitPlan {
                        result: Mutation::plain(LifecycleResult::diagnosed(
                            Disposition::Conflict,
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
                        Disposition::Ignored,
                        "binding_conflict",
                        "older binding observation was ignored",
                    )
                } else if observation == existing_order {
                    let conflicts = binding_facts(event).iter().any(|(field, value)| {
                        existing.get(*field).and_then(Value::as_str) != Some(value)
                    });
                    if conflicts {
                        LifecycleResult::diagnosed(
                            Disposition::Conflict,
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
                        let mut result = LifecycleResult::new(Disposition::Confirmed);
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
                    let mut result = LifecycleResult::new(Disposition::Confirmed);
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
                    Disposition::Replaced
                } else {
                    Disposition::Applied
                });
                result.event_id = Some(event_id);
                result
            };
            // The session index names every binding this writer keeps, in
            // the commit that keeps it. The entry is written first: a commit
            // that stops after it leaves an entry whose binding is missing,
            // which a reader takes as no record, never a binding the index
            // does not name.
            if matches!(
                result.disposition,
                Disposition::Applied | Disposition::Replaced | Disposition::Confirmed
            ) {
                replacements.insert(
                    0,
                    Replacement::if_different(
                        session_entry_path(
                            &resolved.root,
                            provider,
                            session,
                            &resolved.address,
                            &resolved.launch_id,
                            &binding_id,
                        ),
                        session_entry(&resolved.address, &resolved.launch_id, &binding_id)?,
                    ),
                );
            }
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements,
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            confirm_native(resolved, &binding_id, mutation);
            Ok(())
        },
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

// The plugin acknowledges a publication by naming its `event_id` in `ack.json`
// and shows nothing for that id again, so an acknowledged activity is no longer
// on screen: repeating the same semantic activity has to publish a fresh
// `event_id` rather than report the acknowledged one as still current. The
// plugin owns this record, so an unreadable one counts as no acknowledgement
// instead of failing the event.
fn acknowledged(activity_path: &Path, identity: &RecordIdentity, existing: &Value) -> bool {
    let RecordRead::Present(ack) = read_record_typed(
        &activity_path.with_file_name("ack.json"),
        Some("acknowledgement"),
        identity,
    ) else {
        return false;
    };
    let Some(event_id) = existing["event_id"].as_str() else {
        return false;
    };
    ack["target"] == existing["target"] && ack["activity_event_id"].as_str() == Some(event_id)
}

// Called inside the selected launch's lock. Rich rejection does not discard an
// independently valid legacy mutation, and the sidecar is always written last.
fn append_observation(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
    mut plan: CommitPlan<Mutation>,
) -> CommitPlan<Mutation> {
    if let Some(evidence) = &resolved.evidence {
        evidence.borrow_mut().planned_native = Some(accepted(&plan.result.result));
    }
    let prepared = (|| -> Result<Option<PreparedRecordWrite>> {
        if let Some(diagnostic) = &event.observation_diagnostic {
            return Err(AttentionError {
                diagnostic: diagnostic.clone(),
                exit_code: 3,
            });
        }
        let Some(draft) = &event.observation else {
            return Ok(None);
        };
        let binding_id = event_binding_id(event, &resolved.launch_id)?;
        let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
        let claim = load_claim(&resolved.root, &resolved.address)?;
        if claim.as_ref().is_none_or(|record| {
            !record_matches_launch(record, &resolved.address, &resolved.launch_id)
        }) {
            return Err(AttentionError::new(
                "claim_stale",
                "lifecycle claim changed",
            ));
        }
        let pointer = read_record(
            &launch.join("current-binding.json"),
            Some("current_binding"),
            &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        )?;
        let (_, binding) = read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
        if binding.as_ref().is_none_or(|record| {
            !record_matches_binding(record, &resolved.address, &resolved.launch_id, &binding_id)
        }) {
            return Err(AttentionError::new(
                "claim_stale",
                "lifecycle binding changed",
            ));
        }
        let path = launch
            .join("bindings")
            .join(&binding_id)
            .join("lifecycle.json");
        let existing = read_record(
            &path,
            Some("lifecycle_snapshot"),
            &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
        )?;
        let mut snapshot = match existing {
            Some(value) => serde_json::from_value::<LifecycleSnapshot>(value).map_err(|_| {
                AttentionError::new("record_invalid", "lifecycle snapshot is invalid")
            })?,
            None => LifecycleSnapshot {
                kind: "lifecycle_snapshot".to_owned(),
                schema: manifest()?.record_schema,
                address: resolved.address.clone(),
                launch_id: resolved.launch_id.clone(),
                binding_id,
                provider: provider_name(event)?.to_owned(),
                snapshot_id: Uuid::new_v4().to_string(),
                written_at_unix_ns: written_at.to_owned(),
                pools: ObservationPools::default(),
            },
        };
        if snapshot.provider != provider_name(event)? {
            return Err(AttentionError::new(
                "record_invalid",
                "lifecycle provider mismatches its binding",
            ));
        }
        let mut candidate = draft.clone();
        candidate.observed_mono_ns = observation.to_owned();
        candidate.written_at_unix_ns = written_at.to_owned();
        if !snapshot.reduce(candidate)? {
            if let Some(evidence) = &resolved.evidence {
                evidence.borrow_mut().persistence.lifecycle = Persistence::Rejected;
            }
            return Ok(None);
        }
        if let Some(evidence) = &resolved.evidence {
            evidence.borrow_mut().pending_observation_id = Some(draft.observation_id.clone());
        }
        let value = serde_json::to_value(snapshot).map_err(AttentionError::record_json)?;
        Ok(Some(PreparedRecordWrite::new(path, &value)?))
    })();
    match prepared {
        Ok(Some(replacement)) => plan.result.lifecycle_replacement = Some(replacement),
        Ok(None) => {}
        Err(error) => {
            if let Some(evidence) = &resolved.evidence {
                evidence.borrow_mut().persistence.lifecycle = Persistence::Rejected;
            }
            plan.result.result.disposition = Disposition::Partial;
            plan.result.result.diagnostic = Some(error.diagnostic);
        }
    }
    plan
}

fn apply_observation(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
) -> Result<LifecycleResult> {
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let (mutation, _) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &launch.join("current-binding.json"),
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |_| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(refusal(lapsed));
            }
            Ok(append_observation(
                resolved,
                event,
                observation,
                written_at,
                CommitPlan {
                    result: Mutation::plain(LifecycleResult::new(Disposition::Applied)),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                },
            ))
        },
        |mutation| {
            apply_observed_outputs(
                resolved,
                &event_binding_id(event, &resolved.launch_id)?,
                mutation,
            )
        },
    )?;
    Ok(mutation.result)
}

/// The presence source a child's permission request writes. The child's next
/// tool call or its stop replaces that presence, so while it stands the child
/// is waiting.
const WAITING_FOR_PERMISSION: &str = "permission";

/// Whether a child that asked for permission at or after `since` has not
/// emitted anything since. A parent clear or the retention floor ends the wait;
/// the presence TTL does not, because a child blocked on an approval prompt
/// sends nothing to refresh it. A fence or presence that cannot be read answers
/// no, which leaves the activity to its usual order.
fn child_still_waits(
    resolved: &ResolvedLaunch,
    binding_dir: &Path,
    binding_id: &str,
    since: &str,
) -> bool {
    let identity = RecordIdentity::binding(&resolved.address, &resolved.launch_id, binding_id);
    let fence = |name: &str, kind: &str, field: &str| match read_record_typed(
        &binding_dir.join(name),
        Some(kind),
        &identity,
    ) {
        RecordRead::Present(value) => Ok(value[field].as_str().map(str::to_owned)),
        RecordRead::Missing => Ok(None),
        _ => Err(()),
    };
    let (Ok(clear), Ok(floor)) = (
        fence("agents-clear.json", "subagent_clear", "observed_mono_ns"),
        fence(
            "agents-floor.json",
            "subagent_retention_floor",
            "floor_mono_ns",
        ),
    ) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(binding_dir.join("agents")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let Some(agent_key) = path.file_stem().and_then(|stem| stem.to_str()).filter(|_| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        }) else {
            return false;
        };
        let RecordRead::Present(presence) = read_record_typed(
            &path,
            Some("subagent_presence"),
            &RecordIdentity::agent(
                &resolved.address,
                &resolved.launch_id,
                binding_id,
                agent_key,
            ),
        ) else {
            return false;
        };
        let order = presence["observed_mono_ns"].as_str().unwrap_or("");
        presence["source"] == WAITING_FOR_PERMISSION
            && presence["status"] == "active"
            && order >= since
            && clear.as_deref().is_none_or(|clear| order > clear)
            && floor.as_deref().is_none_or(|floor| order > floor)
    })
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
    let (mutation, ()) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(refusal(lapsed));
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
                        Disposition::Ignored,
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
            }) && existing
                .as_ref()
                .is_none_or(|activity| !acknowledged(&activity_path, &identity, activity));
            // A lead that waits on a sub-agent keeps calling tools, and each
            // call is thinking. While a child that asked for permission is
            // still waiting, its notify is what the user needs to see. A
            // prompt, or anything but thinking, replaces it as usual.
            let held = event.agent_id.is_none()
                && base["type"] == "thinking"
                && event.source_event != "UserPromptSubmit"
                && visible
                && existing.as_ref().is_some_and(|activity| {
                    activity["type"] == "notify"
                        && child_still_waits(
                            resolved,
                            &binding_dir,
                            &binding_id,
                            activity["observed_mono_ns"].as_str().unwrap_or(""),
                        )
                });
            let mut replacements = Vec::new();
            let (result, activity) = if let Some(existing) = existing {
                if visible && semantic_activity(existing.clone()) == base {
                    let mut result = LifecycleResult::new(Disposition::Skipped);
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    (result, Some(existing))
                } else if held {
                    // The notify stands for a sub-agent still waiting on the
                    // user; the lead's next tool call is expected, not a conflict.
                    (LifecycleResult::new(Disposition::Ignored), Some(existing))
                } else {
                    let order = existing["observed_mono_ns"].as_str().unwrap_or("");
                    if observation < order {
                        (LifecycleResult::new(Disposition::Ignored), Some(existing))
                    } else if observation == order {
                        (
                            LifecycleResult::diagnosed(
                                Disposition::Conflict,
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
                                Disposition::Ignored,
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
                        let mut result = LifecycleResult::new(Disposition::Applied);
                        result.event_id = Some(event_id);
                        (result, Some(record))
                    }
                }
            } else if clear.as_ref().is_some_and(|clear| {
                observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                (
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
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
                let mut result = LifecycleResult::new(Disposition::Applied);
                result.event_id = Some(event_id);
                (result, Some(record))
            };
            // Only a child's permission request reaches here with an agent id.
            // Its presence marks the child as waiting until its next tool call
            // or its stop. The presence only holds the notify against the
            // lead's tool calls, so a presence that cannot be planned is
            // reported and the notify still applies.
            let mut presence_error = None;
            if let Some(agent_id) = event.agent_id.as_deref() {
                let mut presence = Vec::new();
                match plan_presence(
                    resolved,
                    event,
                    observation,
                    written_at,
                    &binding_id,
                    agent_id,
                    WAITING_FOR_PERMISSION,
                    "active",
                    &mut presence,
                ) {
                    Ok(_) => replacements.extend(presence),
                    Err(error) => presence_error = Some(error),
                }
            }
            if parent_stop && !matches!(result.disposition.as_str(), "ignored" | "conflict") {
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
                            Disposition::Conflict,
                            "record_invalid",
                            "equal parent-clear order has different content",
                        )),
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
            }
            let mut plan = append_observation(
                resolved,
                event,
                observation,
                written_at,
                CommitPlan {
                    result: Mutation {
                        result,
                        lifecycle_replacement: None,
                    },
                    replacements,
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                },
            );
            if let Some(error) = presence_error {
                let result = &mut plan.result.result;
                if accepted(result) {
                    result.disposition = Disposition::Partial;
                }
                if result.diagnostic.is_none() {
                    result.diagnostic = Some(error.diagnostic);
                }
            }
            Ok(plan)
        },
        |mutation| apply_observed_outputs(resolved, &binding_id, mutation),
    )?;
    Ok(mutation.result)
}

fn apply_observed_outputs(
    resolved: &ResolvedLaunch,
    binding_id: &str,
    mutation: &Mutation,
) -> Result<()> {
    apply_observed_outputs_with(resolved, binding_id, mutation, PreparedRecordWrite::apply)
}

fn apply_observed_outputs_with(
    resolved: &ResolvedLaunch,
    binding_id: &str,
    mutation: &Mutation,
    replace: impl FnOnce(&PreparedRecordWrite) -> Result<()>,
) -> Result<()> {
    confirm_native(resolved, binding_id, mutation);
    if let Some(replacement) = &mutation.lifecycle_replacement {
        replace(replacement).map_err(|mut error| {
            error
                .diagnostic
                .context
                .insert("lifecycle_write".into(), json!("unconfirmed"));
            error
        })?;
        if let Some(cell) = &resolved.evidence {
            let mut evidence = cell.borrow_mut();
            evidence.persistence.lifecycle = Persistence::Confirmed;
            evidence.observation_id = evidence.pending_observation_id.clone();
        }
    }
    Ok(())
}

fn safe_mark_text(value: &str, field: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum || !free_of_control(value) {
        return Err(AttentionError::usage(format!("{field} is invalid")));
    }
    Ok(())
}

/// The plugin's Alt+B owns the review named "user"; a CLI writer that used
/// the name would share that file and clear or forge the user's own flag.
const PLUGIN_REVIEW_OWNER: &str = "user";

fn safe_mark_source(source: &str) -> Result<()> {
    safe_mark_text(source, "source", manifest()?.limits.safe_label_max_bytes)?;
    if source == PLUGIN_REVIEW_OWNER {
        return Err(AttentionError::usage(
            "source \"user\" is reserved for the plugin's review key",
        ));
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
    safe_mark_source(source)?;
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
    let Some(claim) = load_claim(&root, &address)?
        .filter(|claim| record_matches_launch(claim, &address, &launch_id))
    else {
        return Err(AttentionError::new(
            "claim_stale",
            "current launch does not match claim",
        ));
    };
    let resolved = ResolvedLaunch {
        evidence: None,
        root,
        address,
        launch_id,
        claim,
        host: None,
        publication_diagnostic: None,
    };
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let pointer_path = launch.join("current-binding.json");
    let (mutation, ()) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&resolved.address, &resolved.launch_id),
        Duration::from_secs(2),
        |pointer| {
            if resolved.lapsed_now()?.is_some() {
                return Err(AttentionError::new(
                    "claim_stale",
                    "current launch does not match claim",
                ));
            }
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
            }) && existing
                .as_ref()
                .is_none_or(|activity| !acknowledged(&path, &activity_identity, activity));
            let mut replacements = Vec::new();
            let result = if let Some(existing) = existing {
                if visible && semantic_activity(existing.clone()) == base {
                    let mut result = LifecycleResult::new(Disposition::Skipped);
                    result.event_id = existing["event_id"].as_str().map(str::to_owned);
                    result
                } else {
                    let order = existing["observed_mono_ns"].as_str().unwrap_or("");
                    if observation < order {
                        LifecycleResult::new(Disposition::Ignored)
                    } else if observation == order {
                        LifecycleResult::diagnosed(
                            Disposition::Conflict,
                            "record_invalid",
                            "equal activity order has different content",
                        )
                    } else if clear.as_ref().is_some_and(|clear| {
                        observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
                    }) {
                        LifecycleResult::diagnosed(
                            Disposition::Ignored,
                            "binding_conflict",
                            "activity observation is covered by activity clear",
                        )
                    } else {
                        let event_id = Uuid::new_v4().to_string();
                        let mut record = base.clone();
                        record["event_id"] = json!(event_id);
                        record["observed_mono_ns"] = json!(observation);
                        record["written_at_unix_ns"] = json!(written_at);
                        replacements.push(Replacement::always(path.clone(), record));
                        let mut result = LifecycleResult::new(Disposition::Applied);
                        result.event_id = Some(event_id);
                        result
                    }
                }
            } else if clear.as_ref().is_some_and(|clear| {
                observation <= clear["observed_mono_ns"].as_str().unwrap_or("")
            }) {
                LifecycleResult::diagnosed(
                    Disposition::Ignored,
                    "binding_conflict",
                    "activity observation is covered by activity clear",
                )
            } else {
                let event_id = Uuid::new_v4().to_string();
                let mut record = base;
                record["event_id"] = json!(event_id);
                record["observed_mono_ns"] = json!(observation);
                record["written_at_unix_ns"] = json!(written_at);
                replacements.push(Replacement::always(path, record));
                let mut result = LifecycleResult::new(Disposition::Applied);
                result.event_id = Some(event_id);
                result
            };
            Ok(CommitPlan {
                result: Mutation {
                    result,
                    lifecycle_replacement: None,
                },
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
            apply_observed_outputs(&resolved, binding_id.as_deref().unwrap_or(""), mutation)
        },
    )?;
    Ok(mutation.result)
}

pub fn apply_mark_review(env: &BTreeMap<String, String>, source: &str) -> Result<LifecycleResult> {
    safe_mark_source(source)?;
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
            // Replaced whatever is there, but never over a review this
            // version cannot read.
            read_record(
                &review_path,
                Some("review"),
                &RecordIdentity::review(&address, &owner_key),
            )?;
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "review",
                "schema": manifest()?.record_schema,
                "address": address,
                "owner_id": source,
                "owner_key": owner_key,
                "event_id": event_id,
            });
            let mut result = LifecycleResult::new(Disposition::Applied);
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

/// Withdraws what `source` published in the current launch: its review, and
/// its activity, the way Pi's bus clear withdraws Pi's. Activity is withdrawn
/// where `mark` writes it. With a binding, the slot is shared by every writer
/// of the binding, so the activity-clear watermark is written only when the
/// activity in it is this source's. Without one, `mark` writes the launch's
/// own activity record, and that record is removed when it is this source's;
/// both readers treat an absent launch activity as no activity.
pub fn apply_mark_clear(
    env: &BTreeMap<String, String>,
    source: &str,
    observation: &str,
) -> Result<LifecycleResult> {
    safe_mark_source(source)?;
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    let owner_key = crate::protocol::sha256_hex(source.as_bytes());
    let pane = pane_path(&root, &address);
    let review_path = pane.join("reviews").join(format!("{owner_key}.json"));
    let launch = launch_path(&root, &address, &launch_id);
    let (mutation, ()) = commit_triple_with(
        &launch.join(".lock"),
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
            let review = read_record(
                &review_path,
                Some("review"),
                &RecordIdentity::review(&address, &owner_key),
            )?;
            let pointer = read_record(
                &launch.join("current-binding.json"),
                Some("current_binding"),
                &RecordIdentity::launch(&address, &launch_id),
            )?;
            let (_, current) = read_current(&launch, pointer, &address, &launch_id)?;
            let mut replacements = Vec::new();
            let mut removals = vec![review_path.clone()];
            let mut cleared = None;
            if current.is_none() {
                let activity_path = launch.join("activity.json");
                let activity = read_record(
                    &activity_path,
                    Some("activity"),
                    &RecordIdentity::launch(&address, &launch_id),
                )?;
                if activity.as_ref().is_some_and(|activity| {
                    activity["source"] == source && activity["target"] == json!({"kind":"launch"})
                }) {
                    removals.push(activity_path);
                    cleared = Some(LifecycleResult::new(Disposition::Applied));
                }
            }
            if let Some(binding_id) = current
                .as_ref()
                .and_then(|record| record["binding_id"].as_str())
            {
                let directory = launch.join("bindings").join(binding_id);
                let identity = RecordIdentity::binding(&address, &launch_id, binding_id);
                let clear_path = directory.join("activity-clear.json");
                let activity = read_record(
                    &directory.join("activity.json"),
                    Some("activity"),
                    &identity,
                )?;
                let clear = read_record(&clear_path, Some("activity_clear"), &identity)?;
                let published = activity.as_ref().is_some_and(|activity| {
                    activity["source"] == source
                        && clear.as_ref().is_none_or(|clear| {
                            activity["observed_mono_ns"].as_str()
                                > clear["observed_mono_ns"].as_str()
                        })
                });
                if published {
                    let (result, replacement) = activity_clear_plan(
                        &address,
                        &launch_id,
                        binding_id,
                        &clear_path,
                        observation,
                    )?;
                    replacements.extend(replacement);
                    cleared = Some(result);
                }
            }
            let mut result = cleared.unwrap_or_else(|| LifecycleResult::new(Disposition::Skipped));
            if review.is_some() && result.disposition == Disposition::Skipped {
                result.disposition = Disposition::Applied;
            }
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements,
                removals,
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
    let binding_path = launch
        .join("bindings")
        .join(&binding_id)
        .join("binding.json");
    let status = if event.action == ProviderAction::ChildActive {
        "active"
    } else {
        "stopped"
    };
    let (mutation, ()) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &binding_path,
        Some("binding"),
        &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
        Duration::from_secs(2),
        |binding| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(refusal(lapsed));
            }
            if binding.as_ref().is_none_or(|record| {
                !record_matches_binding(record, &resolved.address, &resolved.launch_id, &binding_id)
            }) {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "child event has no matching binding",
                    )),
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                });
            }
            let mut replacements = Vec::new();
            let result = plan_presence(
                resolved,
                event,
                observation,
                written_at,
                &binding_id,
                agent_id,
                source,
                status,
                &mut replacements,
            )?;
            Ok(append_observation(
                resolved,
                event,
                observation,
                written_at,
                CommitPlan {
                    result: Mutation {
                        lifecycle_replacement: None,
                        result,
                    },
                    replacements,
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                },
            ))
        },
        |mutation| apply_observed_outputs(resolved, &binding_id, mutation),
    )?;
    Ok(mutation.result)
}

/// Plans one child's presence write at `observation`, inside the launch lock.
/// A newer or equal record, the retention floor, and (for an active child) the
/// parent clear each keep what is stored.
#[allow(clippy::too_many_arguments)]
fn plan_presence(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
    binding_id: &str,
    agent_id: &str,
    source: &str,
    status: &str,
    replacements: &mut Vec<Replacement>,
) -> Result<LifecycleResult> {
    let binding_dir = launch_path(&resolved.root, &resolved.address, &resolved.launch_id)
        .join("bindings")
        .join(binding_id);
    let agent_key = crate::protocol::sha256_hex(agent_id.as_bytes());
    let presence_path = binding_dir.join("agents").join(format!("{agent_key}.json"));
    let identity = RecordIdentity::binding(&resolved.address, &resolved.launch_id, binding_id);
    let existing = read_record(
        &presence_path,
        Some("subagent_presence"),
        &RecordIdentity::agent(
            &resolved.address,
            &resolved.launch_id,
            binding_id,
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
    if let Some(existing) = existing {
        let existing_order = existing["observed_mono_ns"].as_str().unwrap_or("");
        if existing["status"] == "stopped" && status == "stopped" {
            let mut result = LifecycleResult::new(Disposition::Skipped);
            result.event_id = existing["event_id"].as_str().map(str::to_owned);
            return Ok(result);
        }
        if observation < existing_order {
            return Ok(LifecycleResult::new(Disposition::Ignored));
        }
        if observation == existing_order {
            if existing["status"] == status && existing["source"] == source {
                let mut result = LifecycleResult::new(Disposition::Skipped);
                result.event_id = existing["event_id"].as_str().map(str::to_owned);
                return Ok(result);
            }
            return Ok(LifecycleResult::diagnosed(
                Disposition::Conflict,
                "record_invalid",
                "equal child order has different content",
            ));
        }
    }
    child_replacement(
        resolved,
        event,
        observation,
        written_at,
        binding_id,
        agent_id,
        &agent_key,
        source,
        status,
        &presence_path,
        floor.as_ref(),
        clear.as_ref(),
        replacements,
    )
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
            Disposition::Ignored,
            "binding_conflict",
            "child observation is covered by retention floor",
        ));
    }
    if status == "active"
        && clear
            .is_some_and(|clear| observation <= clear["observed_mono_ns"].as_str().unwrap_or(""))
    {
        return Ok(LifecycleResult::diagnosed(
            Disposition::Ignored,
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
    let mut result = LifecycleResult::new(Disposition::Applied);
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
    let (mutation, ()) = commit_nested_with(
        &launch.join(".lock"),
        &resolved.claim_lock(),
        &binding_path,
        Some("binding"),
        &RecordIdentity::binding(&resolved.address, &resolved.launch_id, &binding_id),
        Duration::from_secs(2),
        |binding| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(refusal(lapsed));
            }
            let Some(binding) = binding else {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::diagnosed(
                        Disposition::Ignored,
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
                        Disposition::Ignored,
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
                    Disposition::Ignored
                } else if existing["reason"] == "session_end" && ends_binding(&existing, &binding) {
                    Disposition::Skipped
                } else if observation == order {
                    Disposition::Conflict
                } else {
                    Disposition::Applied
                };
                if disposition != Disposition::Applied {
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
                "binding_event_id": binding["event_id"],
                "observed_mono_ns": observation,
                "written_at_unix_ns": written_at,
            });
            let mut result = LifecycleResult::new(Disposition::Applied);
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements: vec![Replacement::always(end_path.clone(), record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            confirm_native(resolved, &binding_id, mutation);
            Ok(())
        },
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
            if let Some(lapsed) = resolved.lapsed(claim.as_ref()) {
                return Ok(refusal(lapsed));
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
                        Disposition::Ignored,
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
                        Disposition::Applied
                    } else {
                        Disposition::Skipped
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
            let mut result = LifecycleResult::new(Disposition::Applied);
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation::plain(result),
                replacements: vec![Replacement::always(review_path.clone(), record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            confirm_native(resolved, &binding_id, mutation);
            Ok(())
        },
    )?;
    Ok(mutation.result)
}

/// Plans the activity-clear watermark for one binding at `observation`. A
/// watermark already newer wins and an equal one is a replay, so neither
/// writes; either way the result names the stored watermark's event.
fn activity_clear_plan(
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    clear_path: &Path,
    observation: &str,
) -> Result<(LifecycleResult, Option<Replacement>)> {
    let existing = read_record(
        clear_path,
        Some("activity_clear"),
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?;
    if let Some(existing) = &existing {
        let order = existing["observed_mono_ns"].as_str().unwrap_or("");
        if observation <= order {
            let mut result = LifecycleResult::new(if observation < order {
                Disposition::Ignored
            } else {
                Disposition::Skipped
            });
            result.event_id = existing["event_id"].as_str().map(str::to_owned);
            return Ok((result, None));
        }
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
    let mut result = LifecycleResult::new(Disposition::Applied);
    result.event_id = Some(event_id);
    Ok((
        result,
        Some(Replacement::always(clear_path.to_owned(), record)),
    ))
}

// Pi's bus `clear` withdraws both its activity and its review. A Codex
// interrupt withdraws only the activity: the review belongs to another writer.
fn apply_clear_event(
    resolved: &ResolvedLaunch,
    event: &ProviderEvent,
    observation: &str,
    written_at: &str,
) -> Result<LifecycleResult> {
    let binding_id = event_binding_id(event, &resolved.launch_id)?;
    let launch = launch_path(&resolved.root, &resolved.address, &resolved.launch_id);
    let binding_dir = launch.join("bindings").join(&binding_id);
    let pointer_path = launch.join("current-binding.json");
    let clear_path = binding_dir.join("activity-clear.json");
    let pane = pane_path(&resolved.root, &resolved.address);
    let claim_path = pane.join("claim.json");
    let owner_key = crate::protocol::sha256_hex(b"pi-bus");
    let review_path = (event.provider == Some(crate::providers::Provider::Pi))
        .then(|| pane.join("reviews").join(format!("{owner_key}.json")));
    let decide = |pointer: Option<Value>| {
        let ignored = |message| {
            Ok(CommitPlan {
                result: Mutation::plain(LifecycleResult::diagnosed(
                    Disposition::Ignored,
                    "claim_stale",
                    message,
                )),
                replacements: Vec::new(),
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        };
        let claim = read_record(
            &claim_path,
            Some("claim"),
            &RecordIdentity::pane(&resolved.address),
        )?;
        if let Some(lapsed) = resolved.lapsed(claim.as_ref()) {
            return Ok(refusal(lapsed));
        }
        let (_, current) = read_current(&launch, pointer, &resolved.address, &resolved.launch_id)?;
        if current
            .as_ref()
            .and_then(|record| record.get("binding_id"))
            .and_then(Value::as_str)
            != Some(binding_id.as_str())
        {
            return ignored("clear event is not for the current binding");
        }
        let (mut result, replacement) = activity_clear_plan(
            &resolved.address,
            &resolved.launch_id,
            &binding_id,
            &clear_path,
            observation,
        )?;
        let mut removals = Vec::new();
        if result.disposition != Disposition::Ignored
            && let Some(review_path) = &review_path
        {
            if review_path.exists() && result.disposition == Disposition::Skipped {
                result.disposition = Disposition::Applied;
            }
            removals.push(review_path.clone());
        }
        Ok(append_observation(
            resolved,
            event,
            observation,
            written_at,
            CommitPlan {
                result: Mutation::plain(result),
                replacements: replacement.into_iter().collect(),
                removals,
                private_dirs: Vec::new(),
            },
        ))
    };
    let after_apply = |mutation: &Mutation| apply_observed_outputs(resolved, &binding_id, mutation);
    let identity = RecordIdentity::launch(&resolved.address, &resolved.launch_id);
    let (mutation, ()) = if review_path.is_some() {
        commit_triple_with(
            &launch.join(".lock"),
            &pane.join(".claim.lock"),
            &pane.join("reviews").join(format!(".{owner_key}.lock")),
            &pointer_path,
            Some("current_binding"),
            &identity,
            Duration::from_secs(2),
            decide,
            after_apply,
        )?
    } else {
        commit_nested_with(
            &launch.join(".lock"),
            &pane.join(".claim.lock"),
            &pointer_path,
            Some("current_binding"),
            &identity,
            Duration::from_secs(2),
            decide,
            after_apply,
        )?
    };
    Ok(mutation.result)
}

pub fn prompt_return(env: &BTreeMap<String, String>, observation: &str) -> Result<LifecycleResult> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    let Some(claim) = load_claim(&root, &address)?
        .filter(|claim| record_matches_launch(claim, &address, &launch_id))
    else {
        return Ok(LifecycleResult::diagnosed(
            Disposition::Ignored,
            "claim_stale",
            "prompt return has no matching claim",
        ));
    };
    clear_at_prompt(root, address, launch_id, claim, observation)
}

/// Prompt return in a pane whose shell carries no launch id, as the shell of
/// a pane an agent claimed for itself does.
///
/// That agent's activity is cleared only once its process is proven to have
/// exited, since nothing else ends it when the agent is killed or crashes. A
/// suspended agent, or one whose state cannot be read, keeps its activity.
/// Any other claim, or none, gives `None` and writes nothing.
pub fn prompt_return_after_agent_exit(
    env: &BTreeMap<String, String>,
    observation: &str,
    ports: &RuntimePorts<'_>,
) -> Result<Option<LifecycleResult>> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let Some(claim) = load_claim(&root, &address)? else {
        return Ok(None);
    };
    if !crate::launch::owner_proven_gone(&claim, ports.processes) {
        return Ok(None);
    }
    let launch_id = claim
        .get("launch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AttentionError::new("record_invalid", "claim has no launch id"))?
        .to_owned();
    clear_at_prompt(root, address, launch_id, claim, observation).map(Some)
}

/// Clear the lead activity of `launch_id`'s current binding at a prompt,
/// under the launch lock then the claim lock, while the pane's claim is still
/// `claim`.
fn clear_at_prompt(
    root: PathBuf,
    address: PaneAddress,
    launch_id: String,
    claim: Value,
    observation: &str,
) -> Result<LifecycleResult> {
    let launch = launch_path(&root, &address, &launch_id);
    let pointer_path = launch.join("current-binding.json");
    let (mutation, ()) = commit_nested_with(
        &launch.join(".lock"),
        &pane_path(&root, &address).join(".claim.lock"),
        &pointer_path,
        Some("current_binding"),
        &RecordIdentity::launch(&address, &launch_id),
        Duration::from_secs(2),
        |pointer| {
            if load_claim(&root, &address)?.as_ref() != Some(&claim) {
                return Ok(refusal(LifecycleResult::diagnosed(
                    Disposition::Ignored,
                    "claim_stale",
                    "prompt return has no matching claim",
                )));
            }
            let (_, current) = read_current(&launch, pointer, &address, &launch_id)?;
            let Some(current) = current else {
                return Ok(CommitPlan {
                    result: Mutation::plain(LifecycleResult::new(Disposition::Applied)),
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
                let mut result = LifecycleResult::new(Disposition::Ignored);
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
                let mut result = LifecycleResult::new(Disposition::Skipped);
                result.event_id = existing
                    .as_ref()
                    .and_then(|clear| clear["event_id"].as_str())
                    .map(str::to_owned);
                return Ok(CommitPlan {
                    result: Mutation {
                        lifecycle_replacement: None,
                        result,
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
            let mut result = LifecycleResult::new(Disposition::Applied);
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                result: Mutation {
                    lifecycle_replacement: None,
                    result,
                },
                replacements: vec![Replacement::always(clear_path, record)],
                removals: Vec::new(),
                private_dirs: Vec::new(),
            })
        },
        |mutation| {
            let pointer = read_record(
                &pointer_path,
                Some("current_binding"),
                &RecordIdentity::launch(&address, &launch_id),
            )?;
            let current_binding_id = pointer
                .as_ref()
                .and_then(|pointer| pointer["binding_id"].as_str())
                .unwrap_or("");
            apply_observed_outputs(
                &ResolvedLaunch {
                    evidence: None,
                    root: root.clone(),
                    address: address.clone(),
                    launch_id: launch_id.clone(),
                    claim: claim.clone(),
                    host: None,
                    publication_diagnostic: None,
                },
                current_binding_id,
                mutation,
            )
        },
    )?;
    Ok(mutation.result)
}

pub fn apply_provider_event(
    event: &ProviderEvent,
    env: &BTreeMap<String, String>,
    observation: &str,
    ports: &RuntimePorts<'_>,
) -> Result<LifecycleResult> {
    apply_provider_event_inner(event, env, observation, ports, None)
}

pub fn apply_provider_event_with_outcome(
    event: &ProviderEvent,
    env: &BTreeMap<String, String>,
    observation: &str,
    ports: &RuntimePorts<'_>,
) -> HookOutcome {
    let requested = |yes| {
        if yes {
            Persistence::Unconfirmed
        } else {
            Persistence::NotRequested
        }
    };
    let cell = Rc::new(RefCell::new(HookEvidence {
        event: event.clone(),
        inherited: env.contains_key("WEZTERM_ATTENTION_LAUNCH_ID"),
        admission: None,
        planned_native: None,
        pending_observation_id: None,
        observation_id: None,
        persistence: HookPersistence {
            native_state: requested(!matches!(
                event.action,
                ProviderAction::Observation | ProviderAction::Ignored
            )),
            activity: requested(matches!(
                event.action,
                ProviderAction::Activity | ProviderAction::ParentStop | ProviderAction::Clear
            )),
            compatibility: Persistence::NotRequested,
            lifecycle: requested(
                event.observation.is_some() || event.observation_diagnostic.is_some(),
            ),
        },
    }));
    let result = apply_provider_event_inner(event, env, observation, ports, Some(cell.clone()));
    let mut evidence = cell.borrow_mut();
    if result
        .as_ref()
        .is_ok_and(|r| matches!(r.disposition.as_str(), "ignored" | "conflict"))
    {
        let p = &mut evidence.persistence;
        for value in [
            &mut p.native_state,
            &mut p.activity,
            &mut p.compatibility,
            &mut p.lifecycle,
        ] {
            if *value == Persistence::Unconfirmed {
                *value = Persistence::Rejected;
            }
        }
    }
    HookOutcome {
        result,
        admission: evidence.admission.clone(),
        persistence: evidence.persistence.clone(),
        observation_id: evidence.observation_id.clone(),
    }
}

fn apply_provider_event_inner(
    event: &ProviderEvent,
    env: &BTreeMap<String, String>,
    observation: &str,
    ports: &RuntimePorts<'_>,
    evidence: Option<Rc<RefCell<HookEvidence>>>,
) -> Result<LifecycleResult> {
    if event.action == ProviderAction::Ignored {
        let mut result = LifecycleResult::new(Disposition::Ignored);
        result.diagnostic = event.diagnostic.clone();
        return Ok(result);
    }
    if observation.len() != 20 || !observation.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AttentionError::new(
            "record_invalid",
            "observation is invalid",
        ));
    }
    let mut resolved = match resolve_launch(event, env, ports) {
        Ok(resolved) => resolved,
        Err(error) => return Ok(LifecycleResult::ignored_error(error)),
    };
    resolved.evidence = evidence;
    let written_at = ports.clock.unix_ns20()?;
    let mut admitted = event.clone();
    // Lifecycle facts are kept only for an event whose process inherited its
    // launch id. An agent's own claim proves which process holds the pane,
    // not which execution an observation belongs to, so its events keep the
    // indicator and leave the lifecycle unwritten -- a property of the mode,
    // reported as not persisted rather than as a failed event.
    if resolved.host.is_some()
        && (admitted.observation.is_some() || admitted.observation_diagnostic.is_some())
    {
        admitted.observation = None;
        admitted.observation_diagnostic = None;
        if let Some(evidence) = &resolved.evidence {
            evidence.borrow_mut().persistence.lifecycle = Persistence::Rejected;
        }
    }
    let event = &admitted;
    let result = match event.action {
        ProviderAction::Observation => {
            apply_observation(&resolved, event, observation, &written_at)
        }
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
        ProviderAction::Clear => apply_clear_event(&resolved, event, observation, &written_at),
        ProviderAction::Ignored => unreachable!(),
    };
    // Metadata the parser dropped is reported unless the lifecycle has a
    // finding of its own, which says more about what happened to the event.
    // A claim this event made and could not publish says less than either.
    result.map(|mut result| {
        if result.diagnostic.is_none() {
            result.diagnostic = event
                .diagnostic
                .clone()
                .or_else(|| resolved.publication_diagnostic.clone());
        }
        result
    })
}

#[cfg(test)]
mod lifecycle_write_tests {
    use super::*;
    #[test]
    fn failure_of_snapshot_write_reports_partial_state() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/v2/protocol-cases.json")).unwrap();
        let samples = &fixture["record_samples"];
        let root = std::env::temp_dir().join(format!("attention-output-fault-{}", Uuid::new_v4()));
        let address: PaneAddress =
            serde_json::from_value(samples["claim"]["address"].clone()).unwrap();
        let launch_id = samples["claim"]["launch_id"].as_str().unwrap().to_owned();
        let binding_id = samples["binding"]["binding_id"].as_str().unwrap();
        let evidence = Rc::new(RefCell::new(HookEvidence {
            event: crate::providers::parse_provider_event(
                "claude",
                "Stop",
                &json!({"hook_event_name":"Stop", "session_id":samples["binding"]["provider_session_id"]}),
                &BTreeMap::new(),
            ),
            inherited: true,
            admission: None,
            planned_native: Some(true),
            pending_observation_id: None,
            observation_id: None,
            persistence: HookPersistence {
                native_state: Persistence::Unconfirmed,
                activity: Persistence::Unconfirmed,
                compatibility: Persistence::NotRequested,
                lifecycle: Persistence::Unconfirmed,
            },
        }));
        let resolved = ResolvedLaunch {
            evidence: Some(evidence.clone()),
            root: root.clone(),
            address: address.clone(),
            launch_id: launch_id.clone(),
            claim: samples["claim"].clone(),
            host: None,
            publication_diagnostic: None,
        };
        let launch = launch_path(&root, &address, &launch_id);
        crate::records::atomic_replace(
            &pane_path(&root, &address).join("claim.json"),
            &samples["claim"],
        )
        .unwrap();
        crate::records::atomic_replace(
            &launch.join("current-binding.json"),
            &samples["current_binding"],
        )
        .unwrap();
        let cases: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/lifecycle/observations.json"
        ))
        .unwrap();
        let mut snapshot = cases["cases"][1]["value"].clone();
        snapshot["provider"] = json!("claude");
        crate::protocol::validate_record(&snapshot, Some("lifecycle_snapshot")).unwrap();
        let path = launch
            .join("bindings")
            .join(binding_id)
            .join("lifecycle.json");
        crate::records::atomic_replace(&path.with_file_name("binding.json"), &samples["binding"])
            .unwrap();
        let mutation = Mutation {
            result: LifecycleResult::new(Disposition::Applied),
            lifecycle_replacement: Some(PreparedRecordWrite::new(path.clone(), &snapshot).unwrap()),
        };
        let error =
            crate::records::with_lock(&launch.join(".lock"), Duration::from_secs(2), || {
                apply_observed_outputs_with(&resolved, binding_id, &mutation, |_| {
                    assert!(
                        !root.join("42").exists(),
                        "writers do not project before a snapshot write"
                    );
                    Err(AttentionError::new(
                        "state_permissions",
                        "synthetic snapshot write failure",
                    ))
                })
            })
            .unwrap_err();
        assert!(!error.diagnostic.context.contains_key("legacy_applied"));
        assert_eq!(error.diagnostic.context["lifecycle_write"], "unconfirmed");
        assert!(!path.exists());
        let evidence = evidence.borrow();
        assert_eq!(evidence.persistence.native_state, Persistence::Confirmed);
        assert_eq!(evidence.persistence.activity, Persistence::Confirmed);
        assert_eq!(
            evidence.persistence.compatibility,
            Persistence::NotRequested
        );
        assert_eq!(evidence.persistence.lifecycle, Persistence::Unconfirmed);
        assert!(evidence.observation_id.is_none());
        drop(evidence);
        std::fs::remove_dir_all(root).unwrap();
    }
}
