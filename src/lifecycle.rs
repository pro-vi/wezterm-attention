//! Pure lifecycle decisions and their record-application boundary.

pub mod outcome;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
    CommitPlan, PreparedRecordWrite, RecordIdentity, RecordRead, Replacement, agents_dir,
    claim_lock, commit, ends_binding, launch_lock, read_claim, read_record, read_record_at,
    read_record_typed, read_record_typed_at, review_lock, session_entry, session_entry_path,
    state_root,
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
        // The claim reread here is never an agent's own with this launch id:
        // an agent's claim is always made with a fresh launch id, and no
        // shell exported that id for this event to inherit.
        if inherited_claim(&resolved.root, &resolved.address, &resolved.launch_id)?.is_none() {
            return Ok(None);
        }
        let pointer = read_record_at(&resolved.root, "current_binding", &resolved.launch())?;
        let Some(binding) = read_current(
            &resolved.root,
            pointer,
            &resolved.address,
            &resolved.launch_id,
        )?
        .filter(|binding| binding["binding_id"].as_str() == Some(binding_id)) else {
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
}

impl LifecycleResult {
    fn new(disposition: Disposition) -> Self {
        Self {
            disposition,
            diagnostic: None,
            event_id: None,
        }
    }

    fn diagnosed(disposition: Disposition, code: &str, message: &str) -> Self {
        let mut result = Self::new(disposition);
        result.diagnostic = Some(Diagnostic::new(code, message));
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
        claim_lock(&self.root, &self.address)
    }

    fn launch_lock(&self) -> PathBuf {
        launch_lock(&self.root, &self.address, &self.launch_id)
    }

    /// The identity of this launch's own records.
    fn launch(&self) -> RecordIdentity {
        RecordIdentity::launch(&self.address, &self.launch_id)
    }

    /// The identity of the records of this launch's binding `binding_id`.
    fn binding(&self, binding_id: &str) -> RecordIdentity {
        RecordIdentity::binding(&self.address, &self.launch_id, binding_id)
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
        Ok(self.lapsed(read_claim(&self.root, &self.address)?.as_ref()))
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

/// Whether `claim` is the pane's shell claim for an inherited `launch_id`.
/// An agent's own claim never matches: its launch id belongs to that agent's
/// process, and whatever else carries it inherited it by mistake.
fn inherited_claim_matches(claim: &Value, address: &PaneAddress, launch_id: &str) -> bool {
    record_matches_launch(claim, address, launch_id)
        && crate::launch::ClaimMode::of(claim)
            .is_ok_and(|mode| mode == crate::launch::ClaimMode::Shell)
}

/// The binding a launch's current-binding `pointer` selects, read by the
/// launch's own identity. A pointer whose binding is missing is an error.
fn read_current(
    root: &Path,
    pointer: Option<Value>,
    address: &PaneAddress,
    launch_id: &str,
) -> Result<Option<Value>> {
    let Some(pointer) = pointer else {
        return Ok(None);
    };
    let binding_id = pointer
        .get("binding_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AttentionError::new("record_invalid", "current binding pointer is invalid")
        })?;
    read_record_at(
        root,
        "binding",
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?
    .map(Some)
    .ok_or_else(|| AttentionError::new("record_invalid", "current binding record is missing"))
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

/// The pane's claim, when it is the shell claim an inherited `launch_id` came
/// from; see [`inherited_claim_matches`].
fn inherited_claim(root: &Path, address: &PaneAddress, launch_id: &str) -> Result<Option<Value>> {
    Ok(read_claim(root, address)?
        .filter(|claim| inherited_claim_matches(claim, address, launch_id)))
}

/// The state root, the pane and the launch id a process inherited from the
/// shell that claimed its pane, as its environment names them.
fn inherited(env: &BTreeMap<String, String>) -> Result<(PathBuf, PaneAddress, String)> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let launch_id = canonical_uuid(
        env.get("WEZTERM_ATTENTION_LAUNCH_ID").map(String::as_str),
        "WEZTERM_ATTENTION_LAUNCH_ID",
    )?;
    Ok((root, address, launch_id))
}

/// The launch a process inherited, resolved against the pane's claim; `None`
/// when that claim is not the shell claim the launch id came from.
fn inherited_launch<'a>(env: &BTreeMap<String, String>) -> Result<Option<ResolvedLaunch<'a>>> {
    let (root, address, launch_id) = inherited(env)?;
    let Some(claim) = inherited_claim(&root, &address, &launch_id)? else {
        return Ok(None);
    };
    Ok(Some(ResolvedLaunch {
        evidence: None,
        root,
        address,
        launch_id,
        claim,
        host: None,
        publication_diagnostic: None,
    }))
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
    let claim = read_claim(&root, &address)?;
    if let Some(inherited) = env.get("WEZTERM_ATTENTION_LAUNCH_ID") {
        let inherited = canonical_uuid(Some(inherited), "WEZTERM_ATTENTION_LAUNCH_ID")?;
        return match claim {
            Some(claim) if inherited_claim_matches(&claim, &address, &inherited) => {
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
    let launch_id = crate::launch::claim_launch_id(&resolved.claim)?;
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
    let binding_path = resolved
        .binding(&binding_id)
        .path(&resolved.root, "binding")?;
    let pointer_path = resolved.launch().path(&resolved.root, "current_binding")?;
    let (mutation, _) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "current_binding",
        &resolved.launch(),
        |pointer| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            let current = read_current(
                &resolved.root,
                pointer.clone(),
                &resolved.address,
                &resolved.launch_id,
            )?;
            let existing =
                read_record_at(&resolved.root, "binding", &resolved.binding(&binding_id))?;
            if let Some(current) = &current
                && current.get("binding_id").and_then(Value::as_str) != Some(binding_id.as_str())
            {
                let current_order = current["observed_mono_ns"].as_str().unwrap_or("");
                if observation < current_order {
                    return Ok(CommitPlan::reporting(Mutation::plain(
                        LifecycleResult::diagnosed(
                            Disposition::Ignored,
                            "binding_conflict",
                            "older binding selection was ignored",
                        ),
                    )));
                }
                if observation == current_order {
                    return Ok(CommitPlan::reporting(Mutation::plain(
                        LifecycleResult::diagnosed(
                            Disposition::Conflict,
                            "binding_conflict",
                            "equal binding order names a different binding",
                        ),
                    )));
                }
                let current_id = current["binding_id"].as_str().unwrap_or("");
                let current_end =
                    read_record_at(&resolved.root, "binding_end", &resolved.binding(current_id))?;
                let current_ended = current_end
                    .as_ref()
                    .is_some_and(|end| ends_binding(end, current));
                let replace = match provider {
                    "claude" | "codex" => matches!(source, "resume" | "clear" | "fork"),
                    "pi" => matches!(source, "new" | "resume" | "fork"),
                    _ => false,
                };
                if matches!(source, "compact" | "reload") || (!current_ended && !replace) {
                    return Ok(CommitPlan::reporting(Mutation::plain(
                        LifecycleResult::diagnosed(
                            Disposition::Conflict,
                            "binding_conflict",
                            "provider start cannot replace the active binding",
                        ),
                    )));
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
                replacements,
                ..CommitPlan::reporting(Mutation::plain(result))
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

// An acknowledgement names the `event_id` the user was shown in `ack.json`, and
// the plugin shows nothing for that id again, so an acknowledged activity is no
// longer on screen: repeating the same semantic activity has to publish a fresh
// `event_id` rather than report the acknowledged one as still current. The
// acknowledgement is the user's record, not a hook's, so an unreadable one
// counts as no acknowledgement instead of failing the event.
fn acknowledged(root: &Path, identity: &RecordIdentity, existing: &Value) -> bool {
    let RecordRead::Present(ack) = read_record_typed_at(root, "acknowledgement", identity) else {
        return false;
    };
    let Some(event_id) = existing["event_id"].as_str() else {
        return false;
    };
    ack["target"] == existing["target"] && ack["activity_event_id"].as_str() == Some(event_id)
}

/// Whether the activity `existing` is still on screen: newer than the clear
/// watermark of its slot, and not acknowledged by the user.
fn activity_visible(
    root: &Path,
    identity: &RecordIdentity,
    existing: Option<&Value>,
    clear: Option<&Value>,
) -> bool {
    clear.is_none_or(|clear| {
        existing.is_some_and(|activity| {
            activity["observed_mono_ns"].as_str().unwrap_or("")
                > clear["observed_mono_ns"].as_str().unwrap_or("")
        })
    }) && existing.is_none_or(|activity| !acknowledged(root, identity, activity))
}

/// What an activity does to the slot it is written in: its result, the
/// activity that stands in the slot afterwards, and the record written when
/// it takes the slot.
struct ActivityOutcome {
    result: LifecycleResult,
    standing: Option<Value>,
    written: Option<Value>,
}

/// The one ordering every activity writer applies. An activity with content
/// `base`, observed at `observation`, meets the activity `existing` in its
/// slot and the slot's `clear` watermark. The same content still `visible`
/// is a replay of it; a `held` activity keeps the slot; otherwise the later
/// observation takes the slot, unless the clear already covers it. An equal
/// observation with other content is a conflict.
fn plan_activity(
    existing: Option<Value>,
    clear: Option<&Value>,
    visible: bool,
    held: bool,
    base: &Value,
    observation: &str,
    written_at: &str,
) -> ActivityOutcome {
    let kept = |result, standing| ActivityOutcome {
        result,
        standing,
        written: None,
    };
    let covered = || {
        clear.is_some_and(|clear| observation <= clear["observed_mono_ns"].as_str().unwrap_or(""))
    };
    if let Some(existing) = existing {
        if visible && semantic_activity(existing.clone()) == *base {
            let mut result = LifecycleResult::new(Disposition::Skipped);
            result.event_id = existing["event_id"].as_str().map(str::to_owned);
            return kept(result, Some(existing));
        }
        if held {
            return kept(LifecycleResult::new(Disposition::Ignored), Some(existing));
        }
        let order = existing["observed_mono_ns"].as_str().unwrap_or("");
        if observation < order {
            return kept(LifecycleResult::new(Disposition::Ignored), Some(existing));
        }
        if observation == order {
            return kept(
                LifecycleResult::diagnosed(
                    Disposition::Conflict,
                    "record_invalid",
                    "equal activity order has different content",
                ),
                Some(existing),
            );
        }
        if covered() {
            return kept(
                LifecycleResult::diagnosed(
                    Disposition::Ignored,
                    "binding_conflict",
                    "activity observation is covered by activity clear",
                ),
                Some(existing),
            );
        }
    } else if covered() {
        return kept(
            LifecycleResult::diagnosed(
                Disposition::Ignored,
                "binding_conflict",
                "activity observation is covered by activity clear",
            ),
            None,
        );
    }
    let event_id = Uuid::new_v4().to_string();
    let mut record = base.clone();
    record["event_id"] = json!(event_id);
    record["observed_mono_ns"] = json!(observation);
    record["written_at_unix_ns"] = json!(written_at);
    let mut result = LifecycleResult::new(Disposition::Applied);
    result.event_id = Some(event_id);
    ActivityOutcome {
        result,
        standing: Some(record.clone()),
        written: Some(record),
    }
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
        // Only an inherited launch reaches here with an observation, and the
        // claim reread under the lock is never an agent's own with its launch
        // id: an agent's claim is always made with a fresh launch id, which no
        // shell exported for this event to inherit.
        if inherited_claim(&resolved.root, &resolved.address, &resolved.launch_id)?.is_none() {
            return Err(AttentionError::new(
                "claim_stale",
                "lifecycle claim changed",
            ));
        }
        let pointer = read_record_at(&resolved.root, "current_binding", &resolved.launch())?;
        let binding = read_current(
            &resolved.root,
            pointer,
            &resolved.address,
            &resolved.launch_id,
        )?;
        if binding
            .as_ref()
            .is_none_or(|record| record["binding_id"].as_str() != Some(binding_id.as_str()))
        {
            return Err(AttentionError::new(
                "claim_stale",
                "lifecycle binding changed",
            ));
        }
        let identity = resolved.binding(&binding_id);
        let path = identity.path(&resolved.root, "lifecycle_snapshot")?;
        let existing = read_record(&path, Some("lifecycle_snapshot"), &identity)?;
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
    let (mutation, _) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "current_binding",
        &resolved.launch(),
        |_| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            Ok(append_observation(
                resolved,
                event,
                observation,
                written_at,
                CommitPlan::reporting(Mutation::plain(LifecycleResult::new(Disposition::Applied))),
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
/// emitted anything since: the fence of
/// [`crate::protocol::eligible_subagent_presence`] without its TTL. A parent
/// clear or the retention floor ends the wait; the presence TTL does not,
/// because a child blocked on an approval prompt sends nothing to refresh it.
/// A fence or presence that cannot be read answers no, which leaves the
/// activity to its usual order.
fn child_still_waits(resolved: &ResolvedLaunch, binding_id: &str, since: &str) -> bool {
    let identity = resolved.binding(binding_id);
    let fence =
        |kind: &str, field: &str| match read_record_typed_at(&resolved.root, kind, &identity) {
            RecordRead::Present(value) => Ok(value[field].as_str().map(str::to_owned)),
            RecordRead::Missing => Ok(None),
            _ => Err(()),
        };
    let (Ok(clear), Ok(floor)) = (
        fence("subagent_clear", "observed_mono_ns"),
        fence("subagent_retention_floor", "floor_mono_ns"),
    ) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(agents_dir(
        &resolved.root,
        &resolved.address,
        &resolved.launch_id,
        binding_id,
    )) else {
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
    let identity = resolved.binding(&binding_id);
    let activity_path = identity.path(&resolved.root, "activity")?;
    let base = activity_base(resolved, event, &binding_id)?;
    let (mutation, ()) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "current_binding",
        &resolved.launch(),
        |pointer| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            let current = read_current(
                &resolved.root,
                pointer,
                &resolved.address,
                &resolved.launch_id,
            )?;
            if current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                != Some(binding_id.as_str())
            {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "activity event is not for the current binding",
                    ),
                )));
            }
            let existing = read_record(&activity_path, Some("activity"), &identity)?;
            let clear = read_record_at(&resolved.root, "activity_clear", &identity)?;
            let visible =
                activity_visible(&resolved.root, &identity, existing.as_ref(), clear.as_ref());
            // A lead that waits on a sub-agent keeps calling tools, and each
            // call is thinking. While a child that asked for permission is
            // still waiting, its notify is what the user needs to see, and
            // the lead's next tool call is expected, not a conflict. A
            // prompt, or anything but thinking, replaces it as usual.
            let held = event.agent_id.is_none()
                && base["type"] == "thinking"
                && event.source_event != "UserPromptSubmit"
                && visible
                && existing.as_ref().is_some_and(|activity| {
                    activity["type"] == "notify"
                        && child_still_waits(
                            resolved,
                            &binding_id,
                            activity["observed_mono_ns"].as_str().unwrap_or(""),
                        )
                });
            let ActivityOutcome {
                result,
                standing: activity,
                written,
            } = plan_activity(
                existing,
                clear.as_ref(),
                visible,
                held,
                &base,
                observation,
                written_at,
            );
            let mut replacements: Vec<_> = written
                .map(|record| Replacement::always(activity_path.clone(), record))
                .into_iter()
                .collect();
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
                let clear_path = identity.path(&resolved.root, "subagent_clear")?;
                let existing_clear = read_record(&clear_path, Some("subagent_clear"), &identity)?;
                if existing_clear.as_ref().is_none_or(|current| {
                    current["observed_mono_ns"].as_str().unwrap_or("") < surviving_order
                }) {
                    replacements.push(Replacement::always(clear_path, desired));
                } else if existing_clear.as_ref().is_some_and(|current| {
                    current["observed_mono_ns"].as_str().unwrap_or("") == surviving_order
                        && current != &desired
                }) {
                    return Ok(CommitPlan::reporting(Mutation::plain(
                        LifecycleResult::diagnosed(
                            Disposition::Conflict,
                            "record_invalid",
                            "equal parent-clear order has different content",
                        ),
                    )));
                }
            }
            let mut plan = append_observation(
                resolved,
                event,
                observation,
                written_at,
                CommitPlan {
                    replacements,
                    ..CommitPlan::reporting(Mutation::plain(result))
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
            error.diagnostic.set("lifecycle_write", "unconfirmed");
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

/// The plugin's review key owns the review named "user", and only
/// `attention plugin` writes it; a producer that used the name would share
/// that file and clear or forge the user's own flag.
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
    let Some(resolved) = inherited_launch(env)? else {
        return Err(AttentionError::new(
            "claim_stale",
            "current launch does not match claim",
        ));
    };
    let (mutation, ()) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "current_binding",
        &resolved.launch(),
        |pointer| {
            if resolved.lapsed_now()?.is_some() {
                return Err(AttentionError::new(
                    "claim_stale",
                    "current launch does not match claim",
                ));
            }
            let current = read_current(
                &resolved.root,
                pointer,
                &resolved.address,
                &resolved.launch_id,
            )?;
            let binding_id = current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let (activity_identity, target, clear) = match &binding_id {
                Some(binding_id) => (
                    resolved.binding(binding_id),
                    json!({"kind":"binding","binding_id":binding_id}),
                    read_record_at(
                        &resolved.root,
                        "activity_clear",
                        &resolved.binding(binding_id),
                    )?,
                ),
                None => (resolved.launch(), json!({"kind":"launch"}), None),
            };
            let path = activity_identity.path(&resolved.root, "activity")?;
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
            let existing = read_record(&path, Some("activity"), &activity_identity)?;
            let visible = activity_visible(
                &resolved.root,
                &activity_identity,
                existing.as_ref(),
                clear.as_ref(),
            );
            let ActivityOutcome {
                result, written, ..
            } = plan_activity(
                existing,
                clear.as_ref(),
                visible,
                false,
                &base,
                observation,
                written_at,
            );
            let replacements: Vec<_> = written
                .map(|record| Replacement::always(path, record))
                .into_iter()
                .collect();
            Ok(CommitPlan {
                replacements,
                ..CommitPlan::reporting(Mutation::plain(result))
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

pub fn apply_mark_review(env: &BTreeMap<String, String>, source: &str) -> Result<LifecycleResult> {
    safe_mark_source(source)?;
    let (root, address, launch_id) = inherited(env)?;
    let owner_key = crate::protocol::sha256_hex(source.as_bytes());
    let review = RecordIdentity::review(&address, &owner_key);
    let review_path = review.path(&root, "review")?;
    let (mutation, ()) = commit(
        &root,
        &[
            &claim_lock(&root, &address),
            &review_lock(&root, &address, &owner_key),
        ],
        "claim",
        &RecordIdentity::pane(&address),
        |claim| {
            if claim
                .as_ref()
                .is_none_or(|claim| !inherited_claim_matches(claim, &address, &launch_id))
            {
                return Err(AttentionError::new(
                    "claim_stale",
                    "current launch does not match claim",
                ));
            }
            // Replaced whatever is there, but never over a review this
            // version cannot read.
            read_record(&review_path, Some("review"), &review)?;
            let (record, result) = review_record(&address, source, &owner_key)?;
            Ok(CommitPlan {
                replacements: vec![Replacement::always(review_path.clone(), record)],
                ..CommitPlan::reporting(Mutation::plain(result))
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

/// A new review of the pane at `address` by the owner `owner_id`, whose key
/// is `owner_key`, and the result that reports writing it.
fn review_record(
    address: &PaneAddress,
    owner_id: &str,
    owner_key: &str,
) -> Result<(Value, LifecycleResult)> {
    let event_id = Uuid::new_v4().to_string();
    let record = json!({
        "kind": "review",
        "schema": manifest()?.record_schema,
        "address": address,
        "owner_id": owner_id,
        "owner_key": owner_key,
        "event_id": event_id,
    });
    let mut result = LifecycleResult::new(Disposition::Applied);
    result.event_id = Some(event_id);
    Ok((record, result))
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
    let (root, address, launch_id) = inherited(env)?;
    let owner_key = crate::protocol::sha256_hex(source.as_bytes());
    let review_identity = RecordIdentity::review(&address, &owner_key);
    let review_path = review_identity.path(&root, "review")?;
    let launch = RecordIdentity::launch(&address, &launch_id);
    let (mutation, ()) = commit(
        &root,
        &[
            &launch_lock(&root, &address, &launch_id),
            &claim_lock(&root, &address),
            &review_lock(&root, &address, &owner_key),
        ],
        "claim",
        &RecordIdentity::pane(&address),
        |claim| {
            if claim
                .as_ref()
                .is_none_or(|claim| !inherited_claim_matches(claim, &address, &launch_id))
            {
                return Err(AttentionError::new(
                    "claim_stale",
                    "current launch does not match claim",
                ));
            }
            let review = read_record(&review_path, Some("review"), &review_identity)?;
            let pointer = read_record_at(&root, "current_binding", &launch)?;
            let current = read_current(&root, pointer, &address, &launch_id)?;
            let mut replacements = Vec::new();
            let mut removals = vec![review_path.clone()];
            let mut cleared = None;
            if current.is_none() {
                let activity_path = launch.path(&root, "activity")?;
                let activity = read_record(&activity_path, Some("activity"), &launch)?;
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
                let identity = RecordIdentity::binding(&address, &launch_id, binding_id);
                let activity = read_record_at(&root, "activity", &identity)?;
                let clear = read_record_at(&root, "activity_clear", &identity)?;
                let published = activity.as_ref().is_some_and(|activity| {
                    activity["source"] == source
                        && clear.as_ref().is_none_or(|clear| {
                            activity["observed_mono_ns"].as_str()
                                > clear["observed_mono_ns"].as_str()
                        })
                });
                if published {
                    let (result, replacement) =
                        activity_clear_plan(&root, &address, &launch_id, binding_id, observation)?;
                    replacements.extend(replacement);
                    cleared = Some(result);
                }
            }
            let mut result = cleared.unwrap_or_else(|| LifecycleResult::new(Disposition::Skipped));
            if review.is_some() && result.disposition == Disposition::Skipped {
                result.disposition = Disposition::Applied;
            }
            Ok(CommitPlan {
                replacements,
                removals,
                ..CommitPlan::reporting(Mutation::plain(result))
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

/// The claim a write by the plugin needs: one that names the launch the pane
/// published, whichever kind of claim holds it. A reader shows nothing for a
/// pane whose claim names another launch, so a write then would show nothing.
/// The plugin is not a process in the pane, so the inherited-launch rule a
/// hook answers to is not its rule.
fn plugin_claim_matches(
    claim: Option<&Value>,
    address: &PaneAddress,
    launch_id: &str,
) -> Result<()> {
    if claim.is_some_and(|claim| record_matches_launch(claim, address, launch_id)) {
        Ok(())
    } else {
        Err(AttentionError::new(
            "claim_stale",
            "the pane's claim does not name the launch the pane published",
        ))
    }
}

/// Sets (`set`) or withdraws the review the plugin's review key owns, on the
/// pane at `address` whose published launch is `launch_id`. The plugin names
/// the pane itself, because it is not a process in it. Only that owner's
/// review is touched: another owner's review is that owner's to withdraw.
pub fn apply_user_review(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    set: bool,
) -> Result<LifecycleResult> {
    let owner_key = crate::protocol::sha256_hex(PLUGIN_REVIEW_OWNER.as_bytes());
    let review = RecordIdentity::review(address, &owner_key);
    let review_path = review.path(root, "review")?;
    let (mutation, ()) = commit(
        root,
        &[
            &claim_lock(root, address),
            &review_lock(root, address, &owner_key),
        ],
        "claim",
        &RecordIdentity::pane(address),
        |claim| {
            plugin_claim_matches(claim.as_ref(), address, launch_id)?;
            // Never over, or instead of, a review this version cannot read.
            let existing = read_record(&review_path, Some("review"), &review)?;
            if !set {
                let removed = existing.is_some();
                return Ok(CommitPlan {
                    removals: if removed {
                        vec![review_path.clone()]
                    } else {
                        Vec::new()
                    },
                    ..CommitPlan::reporting(Mutation::plain(LifecycleResult::new(if removed {
                        Disposition::Applied
                    } else {
                        Disposition::Skipped
                    })))
                });
            }
            let (record, result) = review_record(address, PLUGIN_REVIEW_OWNER, &owner_key)?;
            Ok(CommitPlan {
                replacements: vec![Replacement::always(review_path.clone(), record)],
                ..CommitPlan::reporting(Mutation::plain(result))
            })
        },
        |_| Ok(()),
    )?;
    Ok(mutation.result)
}

/// Records that the user has seen the activity `activity_event_id` of launch
/// `launch_id` in the pane at `address`, so the tab stops showing it. It is
/// decided under the locks every activity writer takes, and written only
/// while that activity is still the one the pane shows: a newer activity, or
/// a clear, is something the user has not seen, and the answer is `ignored`
/// with nothing written.
pub fn acknowledge_activity(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    activity_event_id: &str,
) -> Result<LifecycleResult> {
    let (mutation, ()) = commit(
        root,
        &[
            &launch_lock(root, address, launch_id),
            &claim_lock(root, address),
        ],
        "current_binding",
        &RecordIdentity::launch(address, launch_id),
        |pointer| {
            plugin_claim_matches(read_claim(root, address)?.as_ref(), address, launch_id)?;
            let current = read_current(root, pointer, address, launch_id)?;
            let binding_id = current
                .as_ref()
                .and_then(|record| record["binding_id"].as_str())
                .map(str::to_owned);
            let (target, identity) = match &binding_id {
                Some(binding_id) => (
                    json!({"kind":"binding","binding_id":binding_id}),
                    RecordIdentity::binding(address, launch_id, binding_id),
                ),
                None => (
                    json!({"kind":"launch"}),
                    RecordIdentity::launch(address, launch_id),
                ),
            };
            let activity = read_record_at(root, "activity", &identity)?;
            let clear = match binding_id {
                Some(_) => read_record_at(root, "activity_clear", &identity)?,
                None => None,
            };
            let shown = activity.as_ref().is_some_and(|activity| {
                activity["event_id"].as_str() == Some(activity_event_id)
                    && activity["target"] == target
                    && clear.as_ref().is_none_or(|clear| {
                        activity["observed_mono_ns"].as_str().unwrap_or("")
                            > clear["observed_mono_ns"].as_str().unwrap_or("")
                    })
            });
            if !shown {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::new(Disposition::Ignored),
                )));
            }
            let ack_path = identity.path(root, "acknowledgement")?;
            // Never over an acknowledgement this version cannot read.
            if let Some(existing) = read_record(&ack_path, Some("acknowledgement"), &identity)?
                && existing["activity_event_id"].as_str() == Some(activity_event_id)
            {
                let mut result = LifecycleResult::new(Disposition::Skipped);
                result.event_id = existing["event_id"].as_str().map(str::to_owned);
                return Ok(CommitPlan::reporting(Mutation::plain(result)));
            }
            let event_id = Uuid::new_v4().to_string();
            let record = json!({
                "kind": "acknowledgement",
                "schema": manifest()?.record_schema,
                "address": address,
                "launch_id": launch_id,
                "target": target,
                "activity_event_id": activity_event_id,
                "event_id": event_id,
            });
            let mut result = LifecycleResult::new(Disposition::Applied);
            result.event_id = Some(event_id);
            Ok(CommitPlan {
                replacements: vec![Replacement::always(ack_path, record)],
                ..CommitPlan::reporting(Mutation::plain(result))
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
    let status = if event.action == ProviderAction::ChildActive {
        "active"
    } else {
        "stopped"
    };
    let (mutation, ()) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "binding",
        &resolved.binding(&binding_id),
        |binding| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            if binding.is_none() {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "child event has no matching binding",
                    ),
                )));
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
                    replacements,
                    ..CommitPlan::reporting(Mutation::plain(result))
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
    let agent_key = crate::protocol::sha256_hex(agent_id.as_bytes());
    let presence = RecordIdentity::agent(
        &resolved.address,
        &resolved.launch_id,
        binding_id,
        &agent_key,
    );
    let presence_path = presence.path(&resolved.root, "subagent_presence")?;
    let identity = resolved.binding(binding_id);
    let existing = read_record(&presence_path, Some("subagent_presence"), &presence)?;
    let floor = read_record_at(&resolved.root, "subagent_retention_floor", &identity)?;
    let clear = read_record_at(&resolved.root, "subagent_clear", &identity)?;
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
    if floor
        .as_ref()
        .is_some_and(|floor| observation <= floor["floor_mono_ns"].as_str().unwrap_or(""))
    {
        return Ok(LifecycleResult::diagnosed(
            Disposition::Ignored,
            "binding_conflict",
            "child observation is covered by retention floor",
        ));
    }
    if status == "active"
        && clear
            .as_ref()
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
    replacements.push(Replacement::always(presence_path, record));
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
    let identity = resolved.binding(&binding_id);
    let end_path = identity.path(&resolved.root, "binding_end")?;
    let (mutation, ()) = commit(
        &resolved.root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "binding",
        &identity,
        |binding| {
            if let Some(lapsed) = resolved.lapsed_now()? {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            let Some(binding) = binding else {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "end event has no matching binding",
                    ),
                )));
            };
            if observation < binding["observed_mono_ns"].as_str().unwrap_or("") {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "binding_conflict",
                        "end observation predates binding",
                    ),
                )));
            }
            let existing = read_record(&end_path, Some("binding_end"), &identity)?;
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
                    return Ok(CommitPlan::reporting(Mutation::plain(result)));
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
                replacements: vec![Replacement::always(end_path.clone(), record)],
                ..CommitPlan::reporting(Mutation::plain(result))
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
    let owner_id = "pi-bus";
    let owner_key = crate::protocol::sha256_hex(owner_id.as_bytes());
    let review = RecordIdentity::review(&resolved.address, &owner_key);
    let review_path = review.path(&resolved.root, "review")?;
    let (mutation, ()) = commit(
        &resolved.root,
        &[
            &resolved.launch_lock(),
            &resolved.claim_lock(),
            &review_lock(&resolved.root, &resolved.address, &owner_key),
        ],
        "claim",
        &RecordIdentity::pane(&resolved.address),
        |claim| {
            if let Some(lapsed) = resolved.lapsed(claim.as_ref()) {
                return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
            }
            let pointer = read_record_at(&resolved.root, "current_binding", &resolved.launch())?;
            let current = read_current(
                &resolved.root,
                pointer,
                &resolved.address,
                &resolved.launch_id,
            )?;
            if current
                .as_ref()
                .and_then(|record| record.get("binding_id"))
                .and_then(Value::as_str)
                != Some(binding_id.as_str())
            {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "Pi review is not for the current binding",
                    ),
                )));
            }
            let existing = read_record(&review_path, Some("review"), &review)?;
            if clear {
                return Ok(CommitPlan {
                    removals: vec![review_path.clone()],
                    ..CommitPlan::reporting(Mutation::plain(LifecycleResult::new(
                        if existing.is_some() {
                            Disposition::Applied
                        } else {
                            Disposition::Skipped
                        },
                    )))
                });
            }
            let (record, result) = review_record(&resolved.address, owner_id, &owner_key)?;
            Ok(CommitPlan {
                replacements: vec![Replacement::always(review_path.clone(), record)],
                ..CommitPlan::reporting(Mutation::plain(result))
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
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    observation: &str,
) -> Result<(LifecycleResult, Option<Replacement>)> {
    let identity = RecordIdentity::binding(address, launch_id, binding_id);
    let clear_path = identity.path(root, "activity_clear")?;
    let existing = read_record(&clear_path, Some("activity_clear"), &identity)?;
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
    Ok((result, Some(Replacement::always(clear_path, record))))
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
    let owner_key = crate::protocol::sha256_hex(b"pi-bus");
    let review_path = (event.provider == Some(crate::providers::Provider::Pi))
        .then(|| {
            RecordIdentity::review(&resolved.address, &owner_key).path(&resolved.root, "review")
        })
        .transpose()?;
    let decide = |pointer: Option<Value>| {
        let ignored = |message| {
            Ok(CommitPlan::reporting(Mutation::plain(
                LifecycleResult::diagnosed(Disposition::Ignored, "claim_stale", message),
            )))
        };
        let claim = read_claim(&resolved.root, &resolved.address)?;
        if let Some(lapsed) = resolved.lapsed(claim.as_ref()) {
            return Ok(CommitPlan::reporting(Mutation::plain(lapsed)));
        }
        let current = read_current(
            &resolved.root,
            pointer,
            &resolved.address,
            &resolved.launch_id,
        )?;
        if current
            .as_ref()
            .and_then(|record| record.get("binding_id"))
            .and_then(Value::as_str)
            != Some(binding_id.as_str())
        {
            return ignored("clear event is not for the current binding");
        }
        let (mut result, replacement) = activity_clear_plan(
            &resolved.root,
            &resolved.address,
            &resolved.launch_id,
            &binding_id,
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
                replacements: replacement.into_iter().collect(),
                removals,
                ..CommitPlan::reporting(Mutation::plain(result))
            },
        ))
    };
    let after_apply = |mutation: &Mutation| apply_observed_outputs(resolved, &binding_id, mutation);
    let (launch_lock, claim_lock) = (resolved.launch_lock(), resolved.claim_lock());
    let review_lock = review_lock(&resolved.root, &resolved.address, &owner_key);
    let locks: &[&Path] = if review_path.is_some() {
        &[&launch_lock, &claim_lock, &review_lock]
    } else {
        &[&launch_lock, &claim_lock]
    };
    let (mutation, ()) = commit(
        &resolved.root,
        locks,
        "current_binding",
        &resolved.launch(),
        decide,
        after_apply,
    )?;
    Ok(mutation.result)
}

pub fn prompt_return(env: &BTreeMap<String, String>, observation: &str) -> Result<LifecycleResult> {
    let Some(resolved) = inherited_launch(env)? else {
        return Ok(LifecycleResult::diagnosed(
            Disposition::Ignored,
            "claim_stale",
            "prompt return has no matching claim",
        ));
    };
    clear_at_prompt(resolved, observation)
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
    let Some(claim) = read_claim(&root, &address)? else {
        return Ok(None);
    };
    if !crate::launch::owner_proven_gone(&claim, ports.processes) {
        return Ok(None);
    }
    let launch_id = crate::launch::claim_launch_id(&claim)?;
    let resolved = ResolvedLaunch {
        evidence: None,
        root,
        address,
        launch_id,
        claim,
        host: None,
        publication_diagnostic: None,
    };
    clear_at_prompt(resolved, observation).map(Some)
}

/// Clear the lead activity of `launch_id`'s current binding at a prompt,
/// under the launch lock then the claim lock, while the pane's claim is still
/// `claim`.
fn clear_at_prompt(resolved: ResolvedLaunch, observation: &str) -> Result<LifecycleResult> {
    let ResolvedLaunch {
        root,
        address,
        launch_id,
        claim,
        ..
    } = &resolved;
    let launch = resolved.launch();
    let (mutation, ()) = commit(
        root,
        &[&resolved.launch_lock(), &resolved.claim_lock()],
        "current_binding",
        &launch,
        |pointer| {
            if read_claim(root, address)?.as_ref() != Some(claim) {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::diagnosed(
                        Disposition::Ignored,
                        "claim_stale",
                        "prompt return has no matching claim",
                    ),
                )));
            }
            let current = read_current(root, pointer, address, launch_id)?;
            let Some(current) = current else {
                return Ok(CommitPlan::reporting(Mutation::plain(
                    LifecycleResult::new(Disposition::Applied),
                )));
            };
            let binding_id = current["binding_id"].as_str().unwrap_or("");
            let (result, replacement) =
                activity_clear_plan(root, address, launch_id, binding_id, observation)?;
            Ok(CommitPlan {
                replacements: replacement.into_iter().collect(),
                ..CommitPlan::reporting(Mutation::plain(result))
            })
        },
        |_| Ok(()),
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
        for value in [&mut p.native_state, &mut p.activity, &mut p.lifecycle] {
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
    if !crate::protocol::ns20_text(observation) {
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
    use crate::records::{launch_path, pane_path};
    use std::time::Duration;
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
