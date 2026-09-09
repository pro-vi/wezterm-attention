//! Legacy v1 projections derived from validated v2 records.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::identity::PaneAddress;
use crate::protocol::{AttentionError, Result};
use crate::records::{
    RecordIdentity, atomic_replace_if_different, launch_path, pane_path, read_record,
    remove_file_durable, with_lock,
};

fn matches_claim(record: &Value, address: &PaneAddress, launch_id: &str) -> bool {
    record.get("address") == serde_json::to_value(address).ok().as_ref()
        && record.get("launch_id").and_then(Value::as_str) == Some(launch_id)
}

fn current_binding(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> Result<bool> {
    let pointer = read_record(
        &launch_path(root, address, launch_id).join("current-binding.json"),
        Some("current_binding"),
        &RecordIdentity::launch(address, launch_id),
    )?;
    Ok(pointer.as_ref().is_some_and(|record| {
        matches_claim(record, address, launch_id)
            && record.get("binding_id").and_then(Value::as_str) == Some(binding_id)
    }))
}

fn current_claim(root: &Path, address: &PaneAddress, launch_id: &str) -> Result<bool> {
    Ok(read_record(
        &pane_path(root, address).join("claim.json"),
        Some("claim"),
        &RecordIdentity::pane(address),
    )?
    .as_ref()
    .is_some_and(|record| matches_claim(record, address, launch_id)))
}

fn projection_value(activity: &Value) -> Result<Value> {
    let written = activity
        .get("written_at_unix_ns")
        .and_then(Value::as_str)
        .ok_or_else(|| AttentionError::new("record_invalid", "activity wall time is invalid"))?
        .parse::<u128>()
        .map_err(|_| AttentionError::new("record_invalid", "activity wall time is invalid"))?;
    let mut projection = serde_json::Map::new();
    projection.insert("type".to_owned(), activity["type"].clone());
    projection.insert("source".to_owned(), activity["source"].clone());
    projection.insert("publication_id".to_owned(), activity["event_id"].clone());
    projection.insert(
        "updated_at".to_owned(),
        json!((written / 1_000_000_000) as u64),
    );
    projection.insert(
        "updated_at_ms".to_owned(),
        json!((written / 1_000_000) as u64),
    );
    projection.insert(
        "puppet".to_owned(),
        activity.get("puppet").cloned().unwrap_or(json!(false)),
    );
    for field in ["frame", "label", "ttl_ms"] {
        if let Some(value) = activity.get(field) {
            projection.insert(field.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(projection))
}

pub fn reconcile_activity(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    activity: Option<&Value>,
) -> Result<bool> {
    let pane = pane_path(root, address);
    with_lock(&pane.join(".claim.lock"), Duration::from_secs(2), || {
        if !current_claim(root, address, launch_id)?
            || !current_binding(root, address, launch_id, binding_id)?
        {
            return Ok(false);
        }
        let marker = root.join(&address.pane_id);
        match activity {
            Some(activity) => atomic_replace_if_different(&marker, &projection_value(activity)?),
            None => {
                let marker_removed = remove_file_durable(&marker)?;
                let acknowledgement_removed =
                    remove_file_durable(&root.join(format!("{}.ack", address.pane_id)))?;
                Ok(marker_removed || acknowledgement_removed)
            }
        }
    })
}

pub fn reconcile_activity_clear(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    clear_order: &str,
) -> Result<bool> {
    let pane = pane_path(root, address);
    with_lock(&pane.join(".claim.lock"), Duration::from_secs(2), || {
        reconcile_activity_clear_locked(root, address, launch_id, binding_id, clear_order)
    })
}

pub fn reconcile_activity_clear_locked(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    clear_order: &str,
) -> Result<bool> {
    if !current_claim(root, address, launch_id)?
        || !current_binding(root, address, launch_id, binding_id)?
    {
        return Ok(false);
    }
    let binding_dir = launch_path(root, address, launch_id)
        .join("bindings")
        .join(binding_id);
    let activity = read_record(
        &binding_dir.join("activity.json"),
        Some("activity"),
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?;
    if activity
        .as_ref()
        .is_some_and(|activity| activity["observed_mono_ns"].as_str().unwrap_or("") > clear_order)
    {
        return Ok(false);
    }
    let marker_removed = remove_file_durable(&root.join(&address.pane_id))?;
    let acknowledgement_removed =
        remove_file_durable(&root.join(format!("{}.ack", address.pane_id)))?;
    Ok(marker_removed || acknowledgement_removed)
}

pub fn reconcile_launch_activity(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    activity: &Value,
) -> Result<bool> {
    let pane = pane_path(root, address);
    with_lock(&pane.join(".claim.lock"), Duration::from_secs(2), || {
        if !current_claim(root, address, launch_id)? {
            return Ok(false);
        }
        atomic_replace_if_different(&root.join(&address.pane_id), &projection_value(activity)?)
    })
}

pub fn reconcile_agents(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    binding_dir: &Path,
) -> Result<bool> {
    let pane = pane_path(root, address);
    with_lock(&pane.join(".claim.lock"), Duration::from_secs(2), || {
        if !current_claim(root, address, launch_id)?
            || !current_binding(root, address, launch_id, binding_id)?
        {
            return Ok(false);
        }
        let clear = read_record(
            &binding_dir.join("agents-clear.json"),
            Some("subagent_clear"),
            &RecordIdentity::binding(address, launch_id, binding_id),
        )?;
        let floor = read_record(
            &binding_dir.join("agents-floor.json"),
            Some("subagent_retention_floor"),
            &RecordIdentity::binding(address, launch_id, binding_id),
        )?;
        let clear_order = clear
            .as_ref()
            .and_then(|record| record.get("observed_mono_ns"))
            .and_then(Value::as_str);
        let floor_order = floor
            .as_ref()
            .and_then(|record| record.get("floor_mono_ns"))
            .and_then(Value::as_str);
        let mut projected = BTreeMap::new();
        let agents = binding_dir.join("agents");
        if let Ok(entries) = fs::read_dir(&agents) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let agent_key = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                let Some(presence) = read_record(
                    &path,
                    Some("subagent_presence"),
                    &RecordIdentity::agent(address, launch_id, binding_id, agent_key),
                )?
                else {
                    continue;
                };
                if !matches_claim(&presence, address, launch_id)
                    || presence.get("binding_id").and_then(Value::as_str) != Some(binding_id)
                {
                    return Err(AttentionError::new(
                        "record_invalid",
                        "subagent presence identity mismatches its path",
                    ));
                }
                let order = presence
                    .get("observed_mono_ns")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if presence.get("status").and_then(Value::as_str) != Some("active")
                    || clear_order.is_some_and(|clear| order <= clear)
                    || floor_order.is_some_and(|floor| order <= floor)
                {
                    continue;
                }
                let written = presence["written_at_unix_ns"]
                    .as_str()
                    .unwrap_or("0")
                    .parse::<u128>()
                    .map_err(|_| {
                        AttentionError::new("record_invalid", "subagent wall time is invalid")
                    })?;
                let agent_id = presence["agent_id"].as_str().unwrap_or("").to_owned();
                projected.insert(
                    agent_id,
                    json!({
                        "type": presence["source"],
                        "last_ms": (written / 1_000_000) as u64,
                    }),
                );
            }
        }
        let sidecar = root.join(format!("{}.agents", address.pane_id));
        if projected.is_empty() {
            remove_file_durable(&sidecar)
        } else {
            atomic_replace_if_different(&sidecar, &json!({"agents": projected}))
        }
    })
}
