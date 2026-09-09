use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::compat::reconcile_agents;
use crate::identity::PaneAddress;
use crate::protocol::{
    AttentionError, Diagnostic, EMBEDDED_MANIFEST, Result, manifest, sha256_hex,
};
use crate::query::{pane_presence, read_bindings_with_ports};
use crate::records::{
    CommitPlan, RecordIdentity, Replacement, commit_nested_with, launch_path, pane_path,
    read_record,
};
use crate::wezterm::{Clock, PaneLister, Presence, ProcessProbe};

pub const ABSENCE_INTERVAL_NS: u128 = 60_000_000_000;
pub const RETENTION_AGE_NS: u128 = 30 * 24 * 60 * 60 * 1_000_000_000;
pub const RETENTION_CAP: usize = 500;

#[derive(Clone, Debug, Serialize)]
pub struct SweepResult {
    pub apply: bool,
    pub operation_id: Option<String>,
    pub scanned: usize,
    pub detail_count: usize,
    pub total_detail_count: usize,
    pub details: Vec<Value>,
}

fn diagnostic(code: &str, message: &str) -> Diagnostic {
    AttentionError::new(code, message).diagnostic
}

fn collect_json(path: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            collect_json(&path, output);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            output.push(path);
        }
    }
}

fn binding_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_json(&root.join("v2/realms"), &mut files);
    files.retain(|path| path.file_name().and_then(|name| name.to_str()) == Some("binding.json"));
    files.sort();
    files
}

fn state_kind(path: &Path) -> Option<&'static str> {
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("reviews")
    {
        return Some("review");
    }
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("agents")
    {
        return Some("subagent_presence");
    }
    match path.file_name().and_then(|name| name.to_str())? {
        "realm.json" => Some("realm"),
        "incarnation.json" => Some("incarnation"),
        "claim.json" => Some("claim"),
        "current-binding.json" => Some("current_binding"),
        "binding.json" => Some("binding"),
        "activity.json" => Some("activity"),
        "activity-clear.json" => Some("activity_clear"),
        "end.json" => Some("binding_end"),
        "ack.json" => Some("acknowledgement"),
        "absence-probe.json" => Some("absence_probe"),
        "agents-clear.json" => Some("subagent_clear"),
        "agents-floor.json" => Some("subagent_retention_floor"),
        _ => None,
    }
}

fn record_address(record: &Value) -> Option<PaneAddress> {
    serde_json::from_value(record.get("address")?.clone()).ok()
}

fn binding_selection(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> Result<Option<bool>> {
    let claim = read_record(
        &pane_path(root, address).join("claim.json"),
        Some("claim"),
        &RecordIdentity::pane(address),
    )?;
    let Some(claim) = claim else { return Ok(None) };
    let pointer = read_record(
        &launch_path(root, address, launch_id).join("current-binding.json"),
        Some("current_binding"),
        &RecordIdentity::launch(address, launch_id),
    )?;
    Ok(Some(
        claim.get("launch_id").and_then(Value::as_str) == Some(launch_id)
            && pointer
                .as_ref()
                .and_then(|pointer| pointer.get("binding_id"))
                .and_then(Value::as_str)
                == Some(binding_id),
    ))
}

fn audit_state(root: &Path) -> (Vec<Value>, Vec<Diagnostic>) {
    let mut files = Vec::new();
    collect_json(&root.join("v2"), &mut files);
    files.sort();
    let mut records = Vec::new();
    let mut diagnostics = Vec::new();
    for path in files {
        let Some(kind) = state_kind(&path) else {
            diagnostics.push(diagnostic(
                "record_invalid",
                "unknown v2 state file is uninspected",
            ));
            continue;
        };
        let identity = match RecordIdentity::from_state_path(root, &path, kind) {
            Ok(identity) => identity,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        match read_record(&path, Some(kind), &identity) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(error) => diagnostics.push(error.diagnostic),
        }
    }
    (records, diagnostics)
}

fn runtime_manifest_bytes() -> Result<Option<Vec<u8>>> {
    let executable = std::env::current_exe().map_err(|_| {
        AttentionError::new(
            "probe_unavailable",
            "current executable path is unavailable",
        )
    })?;
    let Some(path) = executable.ancestors().find_map(|ancestor| {
        let candidate = ancestor.join("protocol/v2.json");
        candidate.is_file().then_some(candidate)
    }) else {
        return Ok(None);
    };
    let file = fs::File::open(path).map_err(|_| {
        AttentionError::new("probe_unavailable", "installed manifest is unreadable")
    })?;
    let maximum = manifest()?.limits.max_json_bytes;
    let mut bytes = Vec::new();
    file.take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            AttentionError::new("probe_unavailable", "installed manifest is unreadable")
        })?;
    if bytes.len() > maximum {
        return Err(AttentionError::new(
            "probe_unavailable",
            "installed manifest exceeds its size bound",
        ));
    }
    Ok(Some(bytes))
}

pub fn doctor(
    root: &Path,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(Value, Vec<Diagnostic>)> {
    let mut diagnostics = Vec::new();
    let mut probes = Vec::new();
    let permission_status = if !root.exists() {
        "healthy"
    } else {
        match fs::metadata(root) {
            Ok(metadata) if metadata.permissions().mode() & 0o077 == 0 => "healthy",
            Ok(_) => {
                diagnostics.push(diagnostic(
                    "state_permissions",
                    "state directory is accessible to other users",
                ));
                "finding"
            }
            Err(_) => {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "state directory cannot be inspected",
                ));
                "unavailable"
            }
        }
    };
    probes.push(json!({"name":"permissions","status":permission_status}));
    let (audited_records, audit_diagnostics) = audit_state(root);
    let (rows, binding_diagnostics) = read_bindings_with_ports(root, panes, processes)?;
    let mut state_diagnostics = audit_diagnostics;
    state_diagnostics.extend(binding_diagnostics);
    probes.push(json!({"name":"state_files","status":if state_diagnostics.is_empty(){"healthy"}else{"finding"}}));
    let realm_sockets: BTreeMap<_, _> = audited_records
        .iter()
        .filter(|record| record["kind"] == "realm")
        .filter_map(|record| {
            Some((
                record["realm_id"].as_str()?.to_owned(),
                record["socket_path"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let process_unavailable = match processes {
        Some(processes) if processes.available() => audited_records
            .iter()
            .filter(|record| record["kind"] == "claim")
            .filter_map(|claim| {
                let address = record_address(claim)?;
                let socket = realm_sockets.get(&address.realm_id)?;
                Some(processes.presence(socket, &address.pane_id))
            })
            .any(|presence| presence == Presence::Unavailable),
        _ => true,
    };
    let process_status = if process_unavailable {
        "unavailable"
    } else {
        "healthy"
    };
    if process_status == "unavailable"
        && !diagnostics
            .iter()
            .any(|item| item.code == "probe_unavailable")
    {
        diagnostics.push(diagnostic(
            "probe_unavailable",
            "identity-scoped process evidence is unavailable",
        ));
    }
    probes.push(json!({"name":"processes","status":process_status}));
    let embedded_digest = sha256_hex(EMBEDDED_MANIFEST.as_bytes());
    let disk_bytes = match runtime_manifest_bytes() {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => {
            diagnostics.push(diagnostic(
                "probe_unavailable",
                "installed manifest was not found",
            ));
            None
        }
        Err(error) => {
            diagnostics.push(error.diagnostic);
            None
        }
    };
    let disk_digest = disk_bytes.as_ref().map(|bytes| sha256_hex(bytes));
    let manifest_matches = disk_bytes
        .as_deref()
        .is_some_and(|bytes| bytes == EMBEDDED_MANIFEST.as_bytes());
    if disk_bytes.is_some() && !manifest_matches {
        diagnostics.push(diagnostic(
            "integration_version_mismatch",
            "on-disk manifest differs from the embedded manifest",
        ));
    }
    let version_finding = state_diagnostics
        .iter()
        .any(|item| item.code == "future_schema")
        || diagnostics
            .iter()
            .any(|item| item.code == "integration_version_mismatch");
    probes.push(json!({"name":"versions","status":if version_finding{"finding"}else{"healthy"}}));
    probes.push(json!({"name":"socket","status":if state_diagnostics.iter().any(|item| matches!(item.code.as_str(),"realm_unavailable"|"incarnation_changed")){"finding"}else{"healthy"}}));
    diagnostics.extend(state_diagnostics);
    Ok((
        json!({
            "scope": ["state_files","socket","processes","permissions","versions"],
            "unobserved": ["gui_user_vars"],
            "probes": probes,
            "bindings_scanned": rows.len(),
            "manifest": {
                "embedded_sha256": embedded_digest,
                "on_disk_sha256": disk_digest,
                "matches": manifest_matches,
            },
            "diagnostic_codes": manifest()?.enums.diagnostic_codes,
        }),
        diagnostics,
    ))
}

fn ns20(value: &str, code: &str) -> Result<u128> {
    if value.len() != 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AttentionError::new(code, "timestamp is invalid"));
    }
    value
        .parse()
        .map_err(|_| AttentionError::new(code, "timestamp is invalid"))
}

fn wall_age_exceeds(now: &str, written: &str, interval: u128) -> Result<bool> {
    let now = ns20(now, "record_invalid")?;
    let written = ns20(written, "record_invalid")?;
    if now < written {
        return Err(AttentionError::new(
            "clock_skew",
            "retention timestamp is newer than current UTC",
        ));
    }
    Ok(now > written + interval)
}

fn presence_eligible(
    presence: &Value,
    clear: Option<&Value>,
    floor: Option<&Value>,
    now: &str,
) -> Result<bool> {
    let order = presence["observed_mono_ns"].as_str().unwrap_or("");
    if presence["status"] != "active"
        || clear.is_some_and(|clear| order <= clear["observed_mono_ns"].as_str().unwrap_or(""))
        || floor.is_some_and(|floor| order <= floor["floor_mono_ns"].as_str().unwrap_or(""))
    {
        return Ok(false);
    }
    let ttl = presence["ttl_ms"].as_u64().unwrap_or(0) as u128 * 1_000_000;
    Ok(!wall_age_exceeds(
        now,
        presence["written_at_unix_ns"].as_str().unwrap_or(""),
        ttl,
    )?)
}

#[derive(Clone)]
struct ChildRecord {
    path: PathBuf,
    value: Value,
}

struct Compaction {
    action: &'static str,
    floor: Option<String>,
    candidates: Vec<ChildRecord>,
    diagnostics: Vec<Diagnostic>,
}

struct CompactionOutcome {
    action: String,
    floor: Option<String>,
    covered: usize,
    deleted: usize,
    diagnostics: Vec<Diagnostic>,
}

struct AbsenceOutcome {
    action: String,
    diagnostic: Option<Diagnostic>,
}

struct RetentionOutcome {
    pruned: bool,
    diagnostics: Vec<Diagnostic>,
}

fn compaction_plan(
    binding_dir: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    now: &str,
    operation_id: Option<&str>,
) -> Result<Compaction> {
    let floor = read_record(
        &binding_dir.join("agents-floor.json"),
        Some("subagent_retention_floor"),
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?;
    let clear = read_record(
        &binding_dir.join("agents-clear.json"),
        Some("subagent_clear"),
        &RecordIdentity::binding(address, launch_id, binding_id),
    )?;
    let mut records = Vec::new();
    let agents = binding_dir.join("agents");
    if fs::symlink_metadata(&agents).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Ok(Compaction {
            action: "blocked",
            floor: None,
            candidates: Vec::new(),
            diagnostics: vec![diagnostic(
                "record_invalid",
                "symlinked subagent directory is preserved",
            )],
        });
    }
    if let Ok(entries) = fs::read_dir(&agents) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("json")
            {
                return Ok(Compaction {
                    action: "blocked",
                    floor: None,
                    candidates: Vec::new(),
                    diagnostics: vec![diagnostic(
                        "record_invalid",
                        "unknown subagent state file is preserved",
                    )],
                });
            }
            let agent_key = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let Some(value) = read_record(
                &path,
                Some("subagent_presence"),
                &RecordIdentity::agent(address, launch_id, binding_id, agent_key),
            )?
            else {
                continue;
            };
            if path.file_stem().and_then(|name| name.to_str()) != value["agent_key"].as_str() {
                return Ok(Compaction {
                    action: "blocked",
                    floor: None,
                    candidates: Vec::new(),
                    diagnostics: vec![diagnostic(
                        "record_invalid",
                        "subagent path identity mismatch",
                    )],
                });
            }
            records.push(ChildRecord { path, value });
        }
    }
    records.sort_by(|left, right| {
        (
            left.value["observed_mono_ns"].as_str().unwrap_or(""),
            left.value["agent_key"].as_str().unwrap_or(""),
        )
            .cmp(&(
                right.value["observed_mono_ns"].as_str().unwrap_or(""),
                right.value["agent_key"].as_str().unwrap_or(""),
            ))
    });
    if let Some(floor) = &floor
        && operation_id.is_some()
        && floor.get("operation_id").and_then(Value::as_str) == operation_id
    {
        let floor_order = floor["floor_mono_ns"].as_str().unwrap_or("").to_owned();
        let candidates = records
            .into_iter()
            .filter(|child| {
                child.value["observed_mono_ns"].as_str().unwrap_or("") <= floor_order.as_str()
            })
            .collect();
        return Ok(Compaction {
            action: "replay_floor",
            floor: Some(floor_order),
            candidates,
            diagnostics: Vec::new(),
        });
    }
    let excess = records.len().saturating_sub(RETENTION_CAP);
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let order = records[index].value["observed_mono_ns"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let start = index;
        while index < records.len() && records[index].value["observed_mono_ns"] == order {
            index += 1;
        }
        if floor
            .as_ref()
            .is_some_and(|floor| order.as_str() <= floor["floor_mono_ns"].as_str().unwrap_or(""))
        {
            continue;
        }
        let group = &records[start..index];
        let mut eligible = false;
        let mut old = true;
        for child in group {
            eligible |= presence_eligible(&child.value, clear.as_ref(), floor.as_ref(), now)?;
            old &= wall_age_exceeds(
                now,
                child.value["written_at_unix_ns"].as_str().unwrap_or(""),
                RETENTION_AGE_NS,
            )?;
        }
        let cap_candidate = candidates.len() < excess;
        if eligible {
            if cap_candidate {
                diagnostics.push(diagnostic(
                    "binding_conflict",
                    "eligible active child blocks unsafe cap pruning",
                ));
            }
            break;
        }
        if old || cap_candidate {
            candidates.extend_from_slice(group);
        } else {
            break;
        }
    }
    if candidates.is_empty() {
        return Ok(Compaction {
            action: "none",
            floor: None,
            candidates,
            diagnostics,
        });
    }
    let floor_order = candidates
        .last()
        .and_then(|child| child.value["observed_mono_ns"].as_str())
        .map(str::to_owned);
    Ok(Compaction {
        action: "advance_floor",
        floor: floor_order,
        candidates,
        diagnostics,
    })
}

fn binding_known_and_prunable(
    binding_dir: &Path,
    binding: &Value,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let Some(address) = record_address(binding) else {
        diagnostics.push(diagnostic("record_invalid", "binding address is invalid"));
        return false;
    };
    let Some(launch_id) = binding.get("launch_id").and_then(Value::as_str) else {
        return false;
    };
    let Some(binding_id) = binding.get("binding_id").and_then(Value::as_str) else {
        return false;
    };
    let identity = RecordIdentity::binding(&address, launch_id, binding_id);
    let known: BTreeSet<_> = [
        "binding.json",
        "activity.json",
        "activity-clear.json",
        "end.json",
        "ack.json",
        "agents-clear.json",
        "agents-floor.json",
    ]
    .into_iter()
    .collect();
    let Ok(entries) = fs::read_dir(binding_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            diagnostics.push(diagnostic(
                "record_invalid",
                "symlinked binding state is preserved",
            ));
            return false;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("agents") && file_type.is_dir() {
            let Ok(children) = fs::read_dir(&path) else {
                return false;
            };
            for child in children.flatten() {
                let child_path = child.path();
                let Ok(file_type) = child.file_type() else {
                    return false;
                };
                if !file_type.is_file()
                    || child_path.extension().and_then(|value| value.to_str()) != Some("json")
                {
                    diagnostics.push(diagnostic(
                        "record_invalid",
                        "unknown binding state is preserved",
                    ));
                    return false;
                }
                let agent_key = child_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                if read_record(
                    &child_path,
                    Some("subagent_presence"),
                    &RecordIdentity::agent(&address, launch_id, binding_id, agent_key),
                )
                .ok()
                .flatten()
                .is_none()
                {
                    diagnostics.push(diagnostic(
                        "record_invalid",
                        "subagent path identity mismatch",
                    ));
                    return false;
                }
            }
            continue;
        }
        if !path.is_file()
            || !known.contains(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
            )
        {
            diagnostics.push(diagnostic(
                "record_invalid",
                "unknown binding state is preserved",
            ));
            return false;
        }
        let Some(kind) = state_kind(&path) else {
            return false;
        };
        match read_record(&path, Some(kind), &identity) {
            Ok(Some(_)) => {}
            _ => {
                diagnostics.push(diagnostic(
                    "record_invalid",
                    "binding child identity mismatch",
                ));
                return false;
            }
        }
    }
    true
}

pub fn binding_cap_paths_by_realm(
    candidates: &BTreeMap<String, Vec<(String, PathBuf, bool)>>,
) -> BTreeSet<PathBuf> {
    let mut selected = BTreeSet::new();
    for rows in candidates.values() {
        let mut rows = rows.clone();
        rows.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        let excess = rows.len().saturating_sub(RETENTION_CAP);
        selected.extend(rows.into_iter().take(excess).map(|row| row.1));
    }
    selected
}

fn absence_action(
    presence: &str,
    probe: Option<&Value>,
    operation_id: Option<&str>,
    observation: &str,
) -> Result<&'static str> {
    match presence {
        "present" => Ok(if probe.is_some() {
            "clear_absence"
        } else {
            "present"
        }),
        "verified_absent" => {
            if probe.is_some_and(|probe| probe["operation_id"].as_str() == operation_id) {
                return Ok("replay_first");
            }
            let Some(probe) = probe else {
                return Ok("first_absence");
            };
            let prior = ns20(
                probe["observed_mono_ns"].as_str().unwrap_or(""),
                "record_invalid",
            )?;
            let current = ns20(observation, "record_invalid")?;
            Ok(if current.saturating_sub(prior) < ABSENCE_INTERVAL_NS {
                "too_soon"
            } else {
                "end"
            })
        }
        _ => Ok("unavailable"),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn sweep(
    root: &Path,
    realm_filter: Option<&str>,
    apply: bool,
    operation_id: Option<&str>,
    clock: &dyn Clock,
    panes: &dyn PaneLister,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(SweepResult, Vec<Diagnostic>)> {
    if apply && operation_id.is_none() {
        return Err(AttentionError::usage("--apply requires --operation-id"));
    }
    let operation_id = operation_id
        .map(|value| {
            Uuid::parse_str(value)
                .ok()
                .filter(|parsed| parsed.to_string() == value)
                .map(|_| value.to_owned())
                .ok_or_else(|| AttentionError::usage("operation id is not canonical"))
        })
        .transpose()?;
    if let Some(realm) = realm_filter
        && (realm.len() != 64
            || !realm
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(AttentionError::usage(
            "realm id must be 64 lowercase hex characters",
        ));
    }
    let observation = clock.monotonic_ns20()?;
    let now = clock.unix_ns20()?;
    ns20(&observation, "record_invalid")?;
    ns20(&now, "record_invalid")?;
    let files = binding_files(root);
    let mut details = Vec::new();
    let mut diagnostics = Vec::new();
    let mut ended: BTreeMap<String, Vec<(String, PathBuf, bool)>> = BTreeMap::new();
    let mut presence_cache: BTreeMap<(String, String, String), String> = BTreeMap::new();
    for binding_path in &files {
        let identity = match RecordIdentity::from_state_path(root, binding_path, "binding") {
            Ok(identity) => identity,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let binding = match read_record(binding_path, Some("binding"), &identity) {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let Some(address) = record_address(&binding) else {
            continue;
        };
        if realm_filter.is_some_and(|realm| realm != address.realm_id) {
            continue;
        }
        let launch_id = binding["launch_id"].as_str().unwrap_or("");
        let binding_id = binding["binding_id"].as_str().unwrap_or("");
        let binding_dir = binding_path.parent().expect("binding parent");
        let launch = launch_path(root, &address, launch_id);
        let pane = pane_path(root, &address);
        let current = match binding_selection(root, &address, launch_id, binding_id) {
            Ok(Some(current)) => current,
            Ok(None) => {
                details.push(json!({"kind":"binding_selection","binding_id":binding_id,"action":"unavailable"}));
                continue;
            }
            Err(error) => {
                diagnostics.push(error.diagnostic);
                details.push(json!({"kind":"binding_selection","binding_id":binding_id,"action":"unavailable"}));
                continue;
            }
        };
        let end_path = binding_dir.join("end.json");
        let binding_identity = RecordIdentity::binding(&address, launch_id, binding_id);
        let end = match read_record(&end_path, Some("binding_end"), &binding_identity) {
            Ok(end) => end,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let ended_now = end.as_ref().is_some_and(|end| {
            end["observed_mono_ns"].as_str().unwrap_or("")
                >= binding["observed_mono_ns"].as_str().unwrap_or("")
        });
        if let Some(end) = &end
            && ended_now
            && !current
        {
            match wall_age_exceeds(
                &now,
                end["written_at_unix_ns"].as_str().unwrap_or(""),
                RETENTION_AGE_NS,
            ) {
                Ok(old) => ended.entry(address.realm_id.clone()).or_default().push((
                    end["written_at_unix_ns"].as_str().unwrap_or("").to_owned(),
                    binding_dir.to_path_buf(),
                    old,
                )),
                Err(error) => diagnostics.push(error.diagnostic),
            }
        }
        if current {
            if apply {
                let operation = operation_id.as_deref().expect("apply operation id");
                let applied = commit_nested_with(
                    &launch.join(".lock"),
                    &pane.join(".claim.lock"),
                    binding_path,
                    Some("binding"),
                    &binding_identity,
                    Duration::from_secs(2),
                    |locked_binding| {
                        if locked_binding.as_ref() != Some(&binding)
                            || binding_selection(root, &address, launch_id, binding_id)?
                                != Some(true)
                        {
                            return Ok(CommitPlan {
                                result: CompactionOutcome {
                                    action: "changed".to_owned(),
                                    floor: None,
                                    covered: 0,
                                    deleted: 0,
                                    diagnostics: vec![diagnostic(
                                        "record_invalid",
                                        "binding changed before sweep apply",
                                    )],
                                },
                                replacements: Vec::new(),
                                removals: Vec::new(),
                                private_dirs: Vec::new(),
                            });
                        }
                        let plan = compaction_plan(
                            binding_dir,
                            &address,
                            launch_id,
                            binding_id,
                            &now,
                            Some(operation),
                        )?;
                        let mut replacements = Vec::new();
                        let mut removals = Vec::new();
                        if let Some(floor) = &plan.floor {
                            if plan.action == "advance_floor" {
                                replacements.push(Replacement::always(
                                    binding_dir.join("agents-floor.json"),
                                    json!({
                                        "kind":"subagent_retention_floor","schema":manifest()?.record_schema,
                                        "address":address,"launch_id":launch_id,"binding_id":binding_id,
                                        "floor_mono_ns":floor,"operation_id":operation,
                                    }),
                                ));
                            }
                            for child in &plan.candidates {
                                if child.value["observed_mono_ns"].as_str().unwrap_or("") <= floor {
                                    removals.push(child.path.clone());
                                }
                            }
                        }
                        let covered = if plan.action == "replay_floor" {
                            0
                        } else {
                            plan.candidates.len()
                        };
                        Ok(CommitPlan {
                            result: CompactionOutcome {
                                action: plan.action.to_owned(),
                                floor: plan.floor,
                                covered,
                                deleted: removals.len(),
                                diagnostics: plan.diagnostics,
                            },
                            replacements,
                            removals,
                            private_dirs: Vec::new(),
                        })
                    },
                    |_| Ok(()),
                );
                match applied {
                    Ok((outcome, ())) => {
                        diagnostics.extend(outcome.diagnostics);
                        if outcome.action != "none" && outcome.action != "changed" {
                            details.push(json!({"kind":"subagent_compaction","binding_id":binding_id,"action":outcome.action,"floor_mono_ns":outcome.floor,"covered":outcome.covered,"deleted":outcome.deleted}));
                            if matches!(outcome.action.as_str(), "advance_floor" | "replay_floor")
                                && let Err(error) = reconcile_agents(
                                    root,
                                    &address,
                                    launch_id,
                                    binding_id,
                                    binding_dir,
                                )
                            {
                                diagnostics.push(error.diagnostic);
                            }
                        }
                    }
                    Err(error) => diagnostics.push(error.diagnostic),
                }
            } else {
                match compaction_plan(binding_dir, &address, launch_id, binding_id, &now, None) {
                    Ok(plan) => {
                        diagnostics.extend(plan.diagnostics.clone());
                        if plan.action != "none" {
                            details.push(json!({"kind":"subagent_compaction","binding_id":binding_id,"action":plan.action,"floor_mono_ns":plan.floor,"covered":plan.candidates.len()}));
                        }
                    }
                    Err(error) => diagnostics.push(error.diagnostic),
                }
            }
        }
        if !current {
            continue;
        }
        if ended_now {
            let action = if operation_id.as_deref()
                == end.as_ref().and_then(|end| end["operation_id"].as_str())
            {
                "replay_end"
            } else {
                "already_ended"
            };
            details.push(json!({"kind":"absence","binding_id":binding_id,"action":action}));
            continue;
        }
        let presence_key = (
            address.realm_id.clone(),
            address.incarnation_id.clone(),
            address.pane_id.clone(),
        );
        let presence = if let Some(cached) = presence_cache.get(&presence_key) {
            cached.clone()
        } else {
            let observed = pane_presence(root, &address, Some(panes), processes, &mut diagnostics);
            presence_cache.insert(presence_key, observed.clone());
            observed
        };
        let probe_path = pane.join("absence-probe.json");
        let probe = match read_record(
            &probe_path,
            Some("absence_probe"),
            &RecordIdentity::pane(&address),
        ) {
            Ok(probe) => probe,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let preview_action = absence_action(
            &presence,
            probe.as_ref(),
            operation_id.as_deref(),
            &observation,
        )?;
        if !apply {
            details.push(json!({"kind":"absence","binding_id":binding_id,"action":preview_action}));
            if preview_action == "unavailable" {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "binding absence cannot be established",
                ));
            }
            continue;
        }
        let operation = operation_id.as_deref().expect("apply operation id");
        let mut locked_diagnostics = Vec::new();
        let applied = commit_nested_with(
            &launch.join(".lock"),
            &pane.join(".claim.lock"),
            binding_path,
            Some("binding"),
            &binding_identity,
            Duration::from_secs(2),
            |locked_binding| {
                if locked_binding.as_ref() != Some(&binding)
                    || binding_selection(root, &address, launch_id, binding_id)? != Some(true)
                {
                    return Ok(CommitPlan {
                        result: AbsenceOutcome {
                            action: "changed".to_owned(),
                            diagnostic: Some(diagnostic(
                                "record_invalid",
                                "binding changed before sweep apply",
                            )),
                        },
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                }
                let locked_presence = pane_presence(
                    root,
                    &address,
                    Some(panes),
                    processes,
                    &mut locked_diagnostics,
                );
                let locked_probe = read_record(
                    &probe_path,
                    Some("absence_probe"),
                    &RecordIdentity::pane(&address),
                )?;
                let action = absence_action(
                    &locked_presence,
                    locked_probe.as_ref(),
                    Some(operation),
                    &observation,
                )?;
                let mut replacements = Vec::new();
                let mut removals = Vec::new();
                if action == "clear_absence" {
                    removals.push(probe_path.clone());
                } else if action == "first_absence" {
                    replacements.push(Replacement::always(
                        probe_path.clone(),
                        json!({"kind":"absence_probe","schema":manifest()?.record_schema,"address":address,"operation_id":operation,"observed_mono_ns":observation}),
                    ));
                } else if action == "end" {
                    let locked_end =
                        read_record(&end_path, Some("binding_end"), &binding_identity)?;
                    if locked_end.as_ref().is_none_or(|end| {
                        end["observed_mono_ns"].as_str().unwrap_or("") < observation.as_str()
                    }) {
                        replacements.push(Replacement::always(
                            end_path.clone(),
                            json!({"kind":"binding_end","schema":manifest()?.record_schema,"address":address,"launch_id":launch_id,"binding_id":binding_id,"reason":"sweep_absent","operation_id":operation,"event_id":Uuid::new_v4().to_string(),"observed_mono_ns":observation,"written_at_unix_ns":clock.unix_ns20()?}),
                        ));
                    }
                }
                Ok(CommitPlan {
                    result: AbsenceOutcome {
                        action: action.to_owned(),
                        diagnostic: None,
                    },
                    replacements,
                    removals,
                    private_dirs: Vec::new(),
                })
            },
            |_| Ok(()),
        );
        diagnostics.extend(locked_diagnostics);
        match applied {
            Ok((outcome, ())) => {
                if let Some(diagnostic) = outcome.diagnostic {
                    diagnostics.push(diagnostic);
                } else {
                    details.push(
                        json!({"kind":"absence","binding_id":binding_id,"action":outcome.action}),
                    );
                    if outcome.action == "unavailable" {
                        diagnostics.push(diagnostic(
                            "probe_unavailable",
                            "binding absence cannot be established",
                        ));
                    }
                }
            }
            Err(error) => diagnostics.push(error.diagnostic),
        }
    }
    let cap_paths = binding_cap_paths_by_realm(&ended);
    let mut ordered_candidates: Vec<_> = ended.values().flatten().cloned().collect();
    ordered_candidates.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    for (_, binding_dir, old) in ordered_candidates {
        if !old && !cap_paths.contains(&binding_dir) {
            continue;
        }
        let binding_id_for_detail = binding_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_owned();
        if !apply {
            details.push(json!({"kind":"binding_retention","binding_id":binding_id_for_detail,"action":"prune"}));
            continue;
        }
        let binding_path = binding_dir.join("binding.json");
        let identity = match RecordIdentity::from_state_path(root, &binding_path, "binding") {
            Ok(identity) => identity,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let Some(binding) = read_record(&binding_path, Some("binding"), &identity)? else {
            continue;
        };
        let Some(address) = record_address(&binding) else {
            continue;
        };
        let launch_id = binding["launch_id"].as_str().unwrap_or("");
        let binding_id = binding["binding_id"].as_str().unwrap_or("");
        let launch = launch_path(root, &address, launch_id);
        let pane = pane_path(root, &address);
        let outcome = commit_nested_with(
            &launch.join(".lock"),
            &pane.join(".claim.lock"),
            &binding_path,
            Some("binding"),
            &identity,
            Duration::from_secs(2),
            |locked_binding| {
                let mut local_diagnostics = Vec::new();
                let Some(locked_binding) = locked_binding else {
                    local_diagnostics.push(diagnostic(
                        "record_invalid",
                        "binding changed before retention apply",
                    ));
                    return Ok(CommitPlan {
                        result: RetentionOutcome {
                            pruned: false,
                            diagnostics: local_diagnostics,
                        },
                        replacements: Vec::new(),
                        removals: Vec::new(),
                        private_dirs: Vec::new(),
                    });
                };
                if locked_binding != binding {
                    local_diagnostics.push(diagnostic(
                        "record_invalid",
                        "binding changed before retention apply",
                    ));
                } else {
                    match binding_selection(root, &address, launch_id, binding_id)? {
                        None => {
                            local_diagnostics.push(diagnostic(
                                "record_invalid",
                                "pane claim changed before retention apply",
                            ));
                            return Ok(CommitPlan {
                                result: RetentionOutcome {
                                    pruned: false,
                                    diagnostics: local_diagnostics,
                                },
                                replacements: Vec::new(),
                                removals: Vec::new(),
                                private_dirs: Vec::new(),
                            });
                        }
                        Some(true) => {
                            local_diagnostics.push(diagnostic(
                                "binding_conflict",
                                "current binding was preserved during retention",
                            ));
                            return Ok(CommitPlan {
                                result: RetentionOutcome {
                                    pruned: false,
                                    diagnostics: local_diagnostics,
                                },
                                replacements: Vec::new(),
                                removals: Vec::new(),
                                private_dirs: Vec::new(),
                            });
                        }
                        Some(false) => {}
                    }
                    let end = read_record(
                        &binding_dir.join("end.json"),
                        Some("binding_end"),
                        &RecordIdentity::binding(&address, launch_id, binding_id),
                    )?;
                    let end_is_current = end.as_ref().is_some_and(|end| {
                        end["observed_mono_ns"].as_str().unwrap_or("")
                            >= locked_binding["observed_mono_ns"].as_str().unwrap_or("")
                    });
                    let still_old = end
                        .as_ref()
                        .map(|end| {
                            wall_age_exceeds(
                                &now,
                                end["written_at_unix_ns"].as_str().unwrap_or(""),
                                RETENTION_AGE_NS,
                            )
                        })
                        .transpose()?
                        .unwrap_or(false);
                    let confined = fs::canonicalize(&binding_dir)
                        .ok()
                        .zip(fs::canonicalize(root).ok())
                        .is_some_and(|(target, root)| target.starts_with(root));
                    if !end_is_current || (!still_old && !cap_paths.contains(&binding_dir)) {
                        local_diagnostics.push(diagnostic(
                            "record_invalid",
                            "binding changed before retention apply",
                        ));
                    } else if !confined {
                        local_diagnostics.push(diagnostic(
                            "record_invalid",
                            "binding removal target is outside the state root",
                        ));
                    } else if !binding_known_and_prunable(
                        &binding_dir,
                        &locked_binding,
                        &mut local_diagnostics,
                    ) {
                    } else {
                        return Ok(CommitPlan {
                            result: RetentionOutcome {
                                pruned: true,
                                diagnostics: local_diagnostics,
                            },
                            replacements: Vec::new(),
                            removals: vec![binding_dir.clone()],
                            private_dirs: Vec::new(),
                        });
                    }
                }
                Ok(CommitPlan {
                    result: RetentionOutcome {
                        pruned: false,
                        diagnostics: local_diagnostics,
                    },
                    replacements: Vec::new(),
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                })
            },
            |_| Ok(()),
        );
        match outcome {
            Ok((outcome, ())) => {
                diagnostics.extend(outcome.diagnostics);
                if outcome.pruned {
                    details.push(json!({"kind":"binding_retention","binding_id":binding_id_for_detail,"action":"prune"}));
                }
            }
            Err(error) => diagnostics.push(error.diagnostic),
        }
    }
    let detail_count = details.len();
    Ok((
        SweepResult {
            apply,
            operation_id,
            scanned: files.len(),
            detail_count,
            total_detail_count: detail_count,
            details,
        },
        diagnostics,
    ))
}
