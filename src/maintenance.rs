use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{PaneAddress, canonical_pane_id, socket_identity};
use crate::protocol::{
    AttentionError, Diagnostic, EMBEDDED_MANIFEST, Result, hex64_text, manifest, sha256_hex,
};
use crate::query::{
    FileStamp, ListOncePerSocket, PaneEvidence, ProbeOncePerAssembly, collect_binding_files,
    collect_state_files, kept_history_code, name_address, pane_evidence, read_bindings_with_ports,
    read_tab_publications, reader_presence, record_address, recorded_socket,
};
use crate::records::{
    CommitPlan, RecordIdentity, Replacement, atomic_replace_if_different, binding_session_entry,
    commit_nested_with, ends_binding, incarnation_path, launch_path, pane_path, read_record,
    realm_path, remove_file_durable, session_index_marker, session_index_path, with_lock,
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
    /// How many steps an apply set out to take and could not: a record it
    /// could not read to decide on, or a removal or write that failed. Any
    /// makes the answer incomplete. A preview counts none.
    #[serde(skip)]
    pub failed_steps: usize,
}

pub fn limit_sweep_preview(details: Vec<Value>, all_details: bool) -> (Vec<Value>, usize) {
    let total = details.len();
    if all_details {
        return (details, total);
    }
    let mut leftover = Vec::new();
    let mut rest = Vec::new();
    for detail in details {
        if matches!(
            detail.get("kind").and_then(Value::as_str),
            Some("projection_collection" | "tab_order_collection")
        ) {
            leftover.push(detail);
        } else {
            rest.push(detail);
        }
    }
    rest.truncate(50);
    leftover.extend(rest);
    (leftover, total)
}

fn diagnostic(code: &str, message: &str) -> Diagnostic {
    AttentionError::new(code, message).diagnostic
}

/// Every JSON file below `path`, with a diagnostic for each directory that
/// could not be read.
fn collect_json(
    root: &Path,
    path: &Path,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    collect_state_files(
        root,
        path,
        &|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"),
        output,
        diagnostics,
    );
}

fn collect_json_complete(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    collect_json_complete_at(path, output, true)
}

fn collect_json_complete_at(
    path: &Path,
    output: &mut Vec<PathBuf>,
    absent_is_empty: bool,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if absent_is_empty {
                return Ok(());
            }
            return Err(AttentionError::new(
                "probe_unavailable",
                "claim tree changed during walk",
            ));
        }
        Err(_) => {
            return Err(AttentionError::new(
                "probe_unavailable",
                "claim tree could not be enumerated",
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(AttentionError::new(
            "record_invalid",
            "claim tree contains a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(AttentionError::new(
            "probe_unavailable",
            "claim tree could not be enumerated",
        ));
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AttentionError::new(
                "probe_unavailable",
                "claim tree changed during walk",
            ));
        }
        Err(_) => {
            return Err(AttentionError::new(
                "probe_unavailable",
                "claim tree could not be enumerated",
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|_| {
            AttentionError::new("probe_unavailable", "claim tree entry is unavailable")
        })?;
        let file_type = entry.file_type().map_err(|_| {
            AttentionError::new("probe_unavailable", "claim tree entry type is unavailable")
        })?;
        if file_type.is_symlink() {
            return Err(AttentionError::new(
                "record_invalid",
                "claim tree contains a symlink",
            ));
        }
        let child = entry.path();
        if file_type.is_dir() {
            collect_json_complete_at(&child, output, false)?;
        } else if child.extension().and_then(|extension| extension.to_str()) == Some("json") {
            output.push(child);
        }
    }
    Ok(())
}

fn binding_files(root: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_binding_files(root, &mut files, diagnostics);
    files.sort();
    files
}

fn state_kind(path: &Path) -> Option<&'static str> {
    let ancestor = |up: usize| {
        path.ancestors()
            .nth(up)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
    };
    if ancestor(2) == Some("sessions") {
        return Some("session_binding");
    }
    if ancestor(1) == Some("sessions") && ancestor(0) == Some("complete.json") {
        return Some("session_index");
    }
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
        "lifecycle.json" => Some("lifecycle_snapshot"),
        "end.json" => Some("binding_end"),
        "ack.json" => Some("acknowledgement"),
        "absence-probe.json" => Some("absence_probe"),
        "agents-clear.json" => Some("subagent_clear"),
        "agents-floor.json" => Some("subagent_retention_floor"),
        _ => None,
    }
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
    let mut diagnostics = Vec::new();
    collect_json(root, &root.join("v2"), &mut files, &mut diagnostics);
    files.sort();
    let mut records = Vec::new();
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
    // Resolve the link before walking ancestors. macOS returns the symlink from
    // `current_exe()`, so a link on PATH would otherwise search the link's own
    // parents -- finding nothing, or worse, an unrelated `protocol/v2.json` that
    // merely sits beside it.
    let executable = fs::canonicalize(&executable).unwrap_or(executable);
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

/// `attention doctor` as run from a process with `environment`. Only
/// `WEZTERM_UNIX_SOCKET` and `WEZTERM_PANE` are read from it.
///
/// A probe that found nothing to check says `unobserved`, never `healthy`: a
/// setup with no state, no claims and no pane passes every check vacuously,
/// and that is the setup that cannot work.
pub fn doctor_with_environment(
    root: &Path,
    environment: &BTreeMap<String, String>,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(Value, Vec<Diagnostic>)> {
    let probed_once = processes.map(ProbeOncePerAssembly::new);
    let processes = probed_once.as_ref().map(|probe| probe as &dyn ProcessProbe);
    let mut diagnostics = Vec::new();
    let mut probes = Vec::new();
    let (audited_records, audit_diagnostics) = audit_state(root);
    let unreadable = audit_diagnostics
        .iter()
        .any(|item| item.code == "state_permissions");
    let permission_status = if !root.exists() {
        "unobserved"
    } else if unreadable {
        "finding"
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
    let (rows, binding_diagnostics) = read_bindings_with_ports(root, panes, processes)?;
    let mut state_diagnostics = audit_diagnostics;
    state_diagnostics.extend(binding_diagnostics);
    let state_status = if !state_diagnostics.is_empty() {
        "finding"
    } else if audited_records.is_empty() {
        "unobserved"
    } else {
        "healthy"
    };
    probes.push(json!({"name":"state_files","status":state_status}));
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
    let claimed: Vec<(&String, String)> = audited_records
        .iter()
        .filter(|record| record["kind"] == "claim")
        .filter_map(|claim| {
            let address = record_address(claim)?;
            Some((realm_sockets.get(&address.realm_id)?, address.pane_id))
        })
        .collect();
    let process_status = match processes {
        Some(processes) if processes.available() => {
            if claimed.is_empty() {
                "unobserved"
            } else if claimed
                .iter()
                .any(|(socket, pane)| processes.presence(socket, pane) == Presence::Unavailable)
            {
                "unavailable"
            } else {
                "healthy"
            }
        }
        _ => "unavailable",
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
    // A mux that did not answer leaves its panes unknown. A socket that is
    // gone, replaced or refusing is a finding about the recorded history.
    let socket_status = if state_diagnostics
        .iter()
        .any(|item| item.code == "realm_unavailable")
    {
        "unavailable"
    } else if state_diagnostics
        .iter()
        .any(|item| kept_history_code(&item.code))
    {
        "finding"
    } else if realm_sockets.is_empty() {
        "unobserved"
    } else {
        "healthy"
    };
    probes.push(json!({"name":"socket","status":socket_status}));
    let environment_status = environment_probe(root, environment, &mut diagnostics);
    probes.push(json!({"name":"environment","status":environment_status}));
    diagnostics.extend(state_diagnostics);
    let diagnostics = fold_kept_history(diagnostics);
    let mut unobserved = vec![json!("gui_user_vars")];
    unobserved.extend(
        probes
            .iter()
            .filter(|probe| probe["status"] == "unobserved")
            .map(|probe| probe["name"].clone()),
    );
    Ok((
        json!({
            "scope": ["state_files","socket","processes","permissions","versions","environment"],
            "unobserved": unobserved,
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

/// Whether the pane doctor runs in has a server identity anything can find.
/// Outside a pane there is nothing to check. Inside one, the socket must read
/// as a mux socket and its realm and incarnation must be published, or hooks
/// run and nothing they write is ever shown.
fn environment_probe(
    root: &Path,
    environment: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> &'static str {
    let (Some(socket), Some(_)) = (
        environment.get("WEZTERM_UNIX_SOCKET"),
        environment.get("WEZTERM_PANE"),
    ) else {
        return "unobserved";
    };
    let (realm_id, incarnation_id, _) = match socket_identity(socket) {
        Ok(identity) => identity,
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "finding";
        }
    };
    let published = read_record(
        &realm_path(root, &realm_id).join("realm.json"),
        Some("realm"),
        &RecordIdentity::realm(&realm_id),
    )
    .is_ok_and(|record| record.is_some())
        && read_record(
            &incarnation_path(root, &realm_id, &incarnation_id).join("incarnation.json"),
            Some("incarnation"),
            &RecordIdentity::incarnation(&realm_id, &incarnation_id),
        )
        .is_ok_and(|record| record.is_some());
    if published {
        "healthy"
    } else {
        diagnostics.push(diagnostic(
            "identity_unpublished",
            "this pane's mux server identity is not published",
        ));
        "finding"
    }
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
    root: &Path,
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
    if agents.exists() && !directory_confined(root, &agents) {
        return Ok(Compaction {
            action: "blocked",
            floor: None,
            candidates: Vec::new(),
            diagnostics: vec![diagnostic(
                "record_invalid",
                "subagent directory outside the state root is preserved",
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
        "lifecycle.json",
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
                if file_type.is_file() && write_leftover(&child.file_name().to_string_lossy()) {
                    continue;
                }
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
        if file_type.is_file() && write_leftover(&entry.file_name().to_string_lossy()) {
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

/// Whether `directory` and every directory between it and the state root is
/// a directory in its own right, not a symlink, so a removal there cannot
/// reach through a link to somewhere outside the root. The root itself may be
/// reached through a link: where it lives is the user's choice.
fn directory_confined(root: &Path, directory: &Path) -> bool {
    let Ok(relative) = directory.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => {}
            _ => return false,
        }
    }
    true
}

/// Whether removing `path` removes something inside the state root: its
/// directory is confined, and it names an entry of that directory.
fn removal_confined(root: &Path, path: &Path) -> bool {
    matches!(
        path.components().next_back(),
        Some(std::path::Component::Normal(_))
    ) && path
        .parent()
        .is_some_and(|parent| directory_confined(root, parent))
}

/// Whether a binding directory lies inside the state root, so removing it
/// removes nothing outside.
fn binding_confined(root: &Path, binding_dir: &Path, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let confined = directory_confined(root, binding_dir);
    if !confined {
        diagnostics.push(diagnostic(
            "record_invalid",
            "binding removal target is outside the state root",
        ));
    }
    confined
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
            // The monotonic clock restarts at boot, so a probe ahead of it was
            // taken before a restart and cannot be measured against: it is
            // replaced, and the count starts again.
            Ok(if prior > current {
                "first_absence"
            } else if current - prior < ABSENCE_INTERVAL_NS {
                "too_soon"
            } else {
                "end"
            })
        }
        _ => Ok("unavailable"),
    }
}

/// What [`absence_presence`] says of a pane whose server's socket no longer
/// serves its incarnation when nothing shows the server gone. The server may
/// still run with its socket removed, replaced or not accepting, so its
/// records are kept.
/// That is the state of the recorded history, not a probe that did not
/// answer: sweep reports it once for the whole run and decides nothing on it.
const SERVER_GONE: &str = "server_gone";

/// Why a tab order naming a pane whose realm or incarnation record this store
/// does not hold is kept: there is no socket to ask, and no probe failed.
const NOT_RECORDED: &str = "not_recorded";

/// A pane's presence as the absence rule reads it, with every diagnostic
/// taken on the way named by the pane and the binding that asked.
fn absence_presence(
    root: &Path,
    address: &PaneAddress,
    binding_id: &str,
    panes: &dyn PaneLister,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> String {
    let before = diagnostics.len();
    let presence = match pane_evidence(root, address, Some(panes), processes, diagnostics) {
        PaneEvidence::Observed(presence) => presence,
        PaneEvidence::ServerGone { diagnostic } => {
            diagnostics.push(diagnostic);
            SERVER_GONE.to_owned()
        }
    };
    name_pane(&mut diagnostics[before..], address, binding_id);
    presence
}

/// Names the pane and binding each diagnostic is about.
fn name_pane(items: &mut [Diagnostic], address: &PaneAddress, binding_id: &str) {
    name_address(items, address);
    for item in items {
        item.context.insert("binding_id".into(), json!(binding_id));
    }
}

/// The diagnostic for a pane whose absence could not be established, named
/// by the pane and binding it is about.
fn absence_unavailable(message: &str, address: &PaneAddress, binding_id: &str) -> Diagnostic {
    let mut item = diagnostic("probe_unavailable", message);
    name_pane(std::slice::from_mut(&mut item), address, binding_id);
    item
}

/// The diagnostics doctor and sweep report. Kept history, the panes whose
/// server's socket is gone, replaced or refusing with nothing to show the
/// server gone, is reported once per code for the run: one diagnostic naming
/// each such incarnation, the directory that holds it and how many of its
/// panes were looked at, so the amount of history never cuts the answer short. A
/// diagnostic that names what it is about is said once however often it was
/// found, as when an apply looks at a pane again before deciding.
fn fold_kept_history(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    /// The panes seen under each realm and incarnation.
    type Held = BTreeMap<(String, String), BTreeSet<String>>;
    let mut folded: Vec<Diagnostic> = Vec::new();
    // Per code, where its one diagnostic stands and what it holds.
    let mut held: BTreeMap<String, (usize, Held)> = BTreeMap::new();
    for item in diagnostics {
        let field = |name: &str| {
            item.context
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        if kept_history_code(&item.code)
            && let (Some(realm_id), Some(incarnation_id), Some(pane_id)) =
                (field("realm_id"), field("incarnation_id"), field("pane_id"))
        {
            let position = folded.len();
            let (_, incarnations) = held.entry(item.code.clone()).or_insert_with(|| {
                folded.push(item.clone());
                (position, BTreeMap::new())
            });
            incarnations
                .entry((realm_id, incarnation_id))
                .or_default()
                .insert(pane_id);
            continue;
        }
        if item.context.is_empty() || !folded.contains(&item) {
            folded.push(item);
        }
    }
    for (position, incarnations) in held.into_values() {
        let listed: Vec<Value> = incarnations
            .into_iter()
            .map(|((realm_id, incarnation_id), panes)| {
                json!({
                    "realm_id": realm_id,
                    "incarnation_id": incarnation_id,
                    "path": incarnation_path(Path::new(""), &realm_id, &incarnation_id),
                    "pane_count": panes.len(),
                })
            })
            .collect();
        folded[position].context = BTreeMap::from([("incarnations".to_owned(), json!(listed))]);
    }
    folded
}

fn claim_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_json_complete(&root.join("v2/realms"), &mut files)?;
    files.retain(|path| path.file_name().and_then(|name| name.to_str()) == Some("claim.json"));
    files.sort();
    Ok(files)
}

fn pane_address_from_claim_path(root: &Path, path: &Path) -> Result<PaneAddress> {
    RecordIdentity::from_state_path(root, path, "claim")?;
    let parts: Vec<_> = path
        .strip_prefix(root)
        .map_err(|_| AttentionError::new("record_invalid", "state path is outside its root"))?
        .iter()
        .filter_map(|part| part.to_str())
        .collect();
    if parts.len() != 8 {
        return Err(AttentionError::new(
            "record_invalid",
            "state record path has the wrong shape",
        ));
    }
    Ok(PaneAddress {
        realm_id: parts[2].to_owned(),
        incarnation_id: parts[4].to_owned(),
        pane_id: parts[6].to_owned(),
    })
}

fn flat_projection_stem(name: &str) -> Option<String> {
    if name.ends_with(".review") {
        return None;
    }
    let stem = name
        .strip_suffix(".agents")
        .or_else(|| name.strip_suffix(".ack"))
        .unwrap_or(name);
    canonical_pane_id(stem).ok()
}

struct FlatFile {
    path: PathBuf,
    identity: FileStamp,
}

fn regular_file_identity(path: &Path) -> Result<Option<FileStamp>> {
    FileStamp::regular_file(path)
        .map_err(|_| AttentionError::new("state_permissions", "flat marker could not be inspected"))
}

struct ClaimInventory {
    owners: BTreeMap<String, Vec<PaneAddress>>,
    undecidable: BTreeSet<String>,
}

fn inventory_claims(root: &Path) -> Result<ClaimInventory> {
    let mut owners: BTreeMap<String, Vec<PaneAddress>> = BTreeMap::new();
    let mut undecidable = BTreeSet::new();
    for path in claim_files(root)? {
        let address = pane_address_from_claim_path(root, &path)?;
        match read_record(&path, Some("claim"), &RecordIdentity::pane(&address)) {
            Ok(Some(claim)) => match record_address(&claim) {
                Some(claimed) if claimed.pane_id == address.pane_id => {
                    let entry = owners.entry(claimed.pane_id.clone()).or_default();
                    if !entry.contains(&claimed) {
                        entry.push(claimed);
                    }
                }
                _ => {
                    undecidable.insert(address.pane_id);
                }
            },
            Ok(None) => {}
            Err(_) => {
                undecidable.insert(address.pane_id);
            }
        }
    }
    Ok(ClaimInventory {
        owners,
        undecidable,
    })
}

type FlatCandidates = (BTreeMap<String, Vec<FlatFile>>, BTreeSet<String>);

fn enumerate_flat_candidates(root: &Path) -> Result<FlatCandidates> {
    let mut files: BTreeMap<String, Vec<FlatFile>> = BTreeMap::new();
    let mut malformed = BTreeSet::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((files, malformed));
        }
        Err(_) => {
            return Err(AttentionError::new(
                "probe_unavailable",
                "state root could not be enumerated",
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|_| {
            AttentionError::new("probe_unavailable", "state root entry is unavailable")
        })?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(stem) = flat_projection_stem(&name) else {
            continue;
        };
        let path = entry.path();
        match regular_file_identity(&path)? {
            Some(identity) => files
                .entry(stem)
                .or_default()
                .push(FlatFile { path, identity }),
            None => {
                malformed.insert(stem);
            }
        }
    }
    for paths in files.values_mut() {
        paths.sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok((files, malformed))
}

fn projection_collection_diagnostic(code: &str, message: &str, pane_id: &str) -> Diagnostic {
    let mut item = diagnostic(code, message);
    item.context.insert("pane_id".into(), json!(pane_id));
    item
}

fn apply_projection_collection(
    root: &Path,
    address: &PaneAddress,
    stem: &str,
    files: &[FlatFile],
) -> Result<bool> {
    let pane = pane_path(root, address);
    with_lock(&pane.join(".claim.lock"), Duration::from_secs(2), || {
        let claim = read_record(
            &pane.join("claim.json"),
            Some("claim"),
            &RecordIdentity::pane(address),
        )?;
        let Some(claim) = claim else {
            return Err(AttentionError::new(
                "claim_stale",
                "claim disappeared before collection",
            ));
        };
        let Some(locked) = record_address(&claim) else {
            return Err(AttentionError::new(
                "record_invalid",
                "claim address is invalid",
            ));
        };
        if locked.pane_id != stem {
            return Err(AttentionError::new(
                "claim_stale",
                "claim no longer names this pane id",
            ));
        }
        let inventory = inventory_claims(root)?;
        if inventory.undecidable.contains(stem) {
            return Err(AttentionError::new(
                "record_invalid",
                "flat marker cannot be attributed",
            ));
        }
        let Some(owners) = inventory.owners.get(stem) else {
            return Ok(false);
        };
        if owners.len() != 1 || !owners.iter().any(|owner| owner == address) {
            return Err(AttentionError::new(
                "binding_conflict",
                "flat marker is claimed at more than one pane address",
            ));
        }
        for file in files {
            match regular_file_identity(&file.path)? {
                Some(identity) if identity == file.identity => {}
                _ => {
                    return Err(AttentionError::new(
                        "record_invalid",
                        "flat marker changed before collection",
                    ));
                }
            }
        }
        let mut removed = false;
        for file in files {
            if remove_file_durable(&file.path)? {
                removed = true;
            }
        }
        Ok(removed)
    })
}

/// A tab order whose writer has exited stays on disk for good: the writer
/// withdraws its own files when a window closes, but nothing runs after the
/// last window of a WezTerm process. It is collected here once every pane it
/// names is verified absent, or once it names no tab at all: WezTerm closes a
/// window whose last tab closes, so an empty order is the bar's final draw. A
/// file naming a v1 marker id is kept, because a bare pane id has no realm to
/// ask; so is one naming a pane whose realm or incarnation is not recorded,
/// and one whose panes could not be probed. A GUI source is not the pane realm
/// a sweep selects, so a realm-filtered sweep leaves these files alone.
/// Returns how many steps failed.
fn collect_tab_orders(
    root: &Path,
    apply: bool,
    panes: &dyn PaneLister,
    processes: Option<&dyn ProcessProbe>,
    presence_cache: &mut BTreeMap<PaneAddress, String>,
    details: &mut Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> usize {
    let mut failed = 0;
    let (windows, read_diagnostics) = match read_tab_publications(root) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return 1;
        }
    };
    diagnostics.extend(read_diagnostics);
    for window in windows {
        let relative = window.relative_path.to_string_lossy().into_owned();
        let mut addresses: BTreeSet<PaneAddress> = BTreeSet::new();
        let mut without_address = false;
        for marker_id in window.tabs.iter().flat_map(|tab| tab.marker_ids.iter()) {
            match v2_marker_address(marker_id) {
                Some(address) => {
                    addresses.insert(address);
                }
                None => without_address = true,
            }
        }
        let mut keep = without_address.then_some("no_address");
        if keep.is_none() {
            for address in &addresses {
                let presence = match presence_cache.get(address) {
                    Some(cached) => cached.clone(),
                    None => {
                        let before = diagnostics.len();
                        // A pane whose realm or incarnation this store does
                        // not record has no socket to ask. Nothing failed to
                        // answer, and no later sweep can learn more, so it
                        // keeps the file without leaving the sweep undecided.
                        // Otherwise it is the evidence a binding's absence is
                        // decided on. A pane that leaves undecided leaves this
                        // file's fate undecided too; a server that may be
                        // gone is kept history, which decides nothing.
                        let observed = if matches!(recorded_socket(root, address), Ok(None)) {
                            NOT_RECORDED.to_owned()
                        } else {
                            let (observed, server_gone) =
                                reader_presence(root, address, Some(panes), processes, diagnostics);
                            if observed == "unavailable" && !server_gone {
                                diagnostics.push(diagnostic(
                                    "probe_unavailable",
                                    "tab order pane presence cannot be established",
                                ));
                            }
                            observed
                        };
                        // Named by the file that asked and the pane it asked
                        // about, as a bindings answer names its panes.
                        name_address(&mut diagnostics[before..], address);
                        for item in &mut diagnostics[before..] {
                            item.context.insert("path".into(), json!(relative));
                        }
                        presence_cache.insert(address.clone(), observed.clone());
                        observed
                    }
                };
                match presence.as_str() {
                    "verified_absent" => {}
                    "present" => {
                        keep = Some("present");
                        break;
                    }
                    NOT_RECORDED => {
                        keep.get_or_insert(NOT_RECORDED);
                    }
                    _ => keep = Some("unavailable"),
                }
            }
        }
        let detail = match keep {
            Some(reason) => json!({
                "kind": "tab_order_collection",
                "window_id": window.window_id,
                "path": relative,
                "action": "keep",
                "reason": reason,
            }),
            None if !apply => json!({
                "kind": "tab_order_collection",
                "window_id": window.window_id,
                "path": relative,
                "action": "collect",
            }),
            // The panes were probed after the file was read. One that changed
            // since is a newer draw from a live bar, not the one judged here.
            None if FileStamp::regular_file(&root.join(&relative))
                .ok()
                .flatten()
                .is_none_or(|now| Some(now) != window.stamp) =>
            {
                json!({
                    "kind": "tab_order_collection",
                    "window_id": window.window_id,
                    "path": relative,
                    "action": "keep",
                    "reason": "changed",
                })
            }
            None if !removal_confined(root, &root.join(&relative)) => {
                let mut item = diagnostic(
                    "record_invalid",
                    "tab order outside the state root is preserved",
                );
                item.context.insert("path".into(), json!(relative));
                diagnostics.push(item);
                continue;
            }
            None => match remove_file_durable(&root.join(&relative)) {
                Ok(_) => json!({
                    "kind": "tab_order_collection",
                    "window_id": window.window_id,
                    "path": relative,
                    "action": "collected",
                }),
                Err(error) => {
                    diagnostics.push(error.diagnostic);
                    failed += 1;
                    continue;
                }
            },
        };
        details.push(detail);
    }
    failed
}

/// The address a published `v2:<realm>:<incarnation>:<pane>` marker id names.
/// The reader has already checked the shape; a v1 decimal id has no address.
fn v2_marker_address(marker_id: &str) -> Option<PaneAddress> {
    let mut parts = marker_id.strip_prefix("v2:")?.splitn(3, ':');
    let realm_id = parts.next()?.to_owned();
    let incarnation_id = parts.next()?.to_owned();
    let pane_id = parts.next()?.to_owned();
    Some(PaneAddress {
        realm_id,
        incarnation_id,
        pane_id,
    })
}

/// Collects flat files a single claim owns. Returns how many steps failed.
fn collect_projection_orphans(
    root: &Path,
    realm_filter: Option<&str>,
    apply: bool,
    details: &mut Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> usize {
    let mut failed = 0;
    let inventory = match inventory_claims(root) {
        Ok(inventory) => inventory,
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return 1;
        }
    };
    let (files, malformed) = match enumerate_flat_candidates(root) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return 1;
        }
    };
    let mut stems: BTreeSet<String> = files.keys().cloned().collect();
    stems.extend(malformed.iter().cloned());
    for stem in stems {
        if malformed.contains(&stem) {
            diagnostics.push(projection_collection_diagnostic(
                "record_invalid",
                "flat marker is not a regular file",
                &stem,
            ));
            continue;
        }
        if inventory.undecidable.contains(&stem) {
            diagnostics.push(projection_collection_diagnostic(
                "record_invalid",
                "flat marker cannot be attributed",
                &stem,
            ));
            continue;
        }
        let Some(owners) = inventory.owners.get(&stem) else {
            continue;
        };
        if owners.len() > 1 {
            diagnostics.push(projection_collection_diagnostic(
                "binding_conflict",
                "flat marker is claimed at more than one pane address",
                &stem,
            ));
            continue;
        }
        let Some(address) = owners.iter().next() else {
            continue;
        };
        if realm_filter.is_some_and(|realm| realm != address.realm_id) {
            continue;
        }
        let Some(candidates) = files.get(&stem) else {
            continue;
        };
        let relative: Vec<String> = candidates
            .iter()
            .filter_map(|file| file.path.file_name()?.to_str().map(str::to_owned))
            .collect();
        if apply {
            match apply_projection_collection(root, address, &stem, candidates) {
                Ok(true) => details.push(json!({
                    "kind": "projection_collection",
                    "pane_id": stem,
                    "paths": relative,
                })),
                Ok(false) => {}
                Err(error) => {
                    diagnostics.push(error.diagnostic);
                    failed += 1;
                }
            }
        } else {
            details.push(json!({
                "kind": "projection_collection",
                "pane_id": stem,
                "paths": relative,
            }));
        }
    }
    failed
}

/// Gives every binding in `files` its session index entry, and marks the index
/// complete when `files` is every binding in the store and each one now has
/// its entry. Readers walk every binding until then. Returns whether a write
/// failed.
fn complete_session_index(
    root: &Path,
    files: &[PathBuf],
    walked_every_directory: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut complete = walked_every_directory;
    let mut failed = false;
    for path in files {
        let binding = RecordIdentity::from_state_path(root, path, "binding")
            .and_then(|identity| read_record(path, Some("binding"), &identity));
        let Ok(Some(binding)) = binding else {
            // Reported by the binding step. Until it reads, readers walk.
            complete = false;
            continue;
        };
        let written = binding_session_entry(root, &binding)
            .and_then(|(path, entry)| atomic_replace_if_different(&path, &entry));
        if let Err(error) = written {
            diagnostics.push(error.diagnostic);
            complete = false;
            failed = true;
        }
    }
    if complete
        && let Err(error) = session_index_marker()
            .and_then(|marker| atomic_replace_if_different(&session_index_path(root), &marker))
    {
        diagnostics.push(error.diagnostic);
        failed = true;
    }
    failed
}

/// The session index entries naming the bindings below `dir`, which go with
/// `dir` when sweep removes it. An entry is taken only when it names exactly
/// that binding and removing it stays inside the state root.
fn session_entries_below(root: &Path, dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_state_files(
        root,
        dir,
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("binding.json"),
        &mut files,
        &mut Vec::new(),
    );
    files
        .into_iter()
        .filter_map(|path| {
            let identity = RecordIdentity::from_state_path(root, &path, "binding").ok()?;
            let binding = read_record(&path, Some("binding"), &identity).ok()??;
            let (entry_path, entry) = binding_session_entry(root, &binding).ok()?;
            let found = read_record(
                &entry_path,
                Some("session_binding"),
                &RecordIdentity::from_record(&entry),
            )
            .ok()??;
            (found == entry && removal_confined(root, &entry_path)).then_some(entry_path)
        })
        .collect()
}

/// An apply's pane lister: a listing that failed answers every later ask
/// about that socket for the rest of the run, and one that answered is taken
/// fresh each time. A failed listing leaves a pane undecided and so removes
/// nothing, while each ask against a mux that accepts and never answers
/// waits out the listing deadline; asking once per pane made an apply's wait
/// grow with the panes on that socket.
struct FailedListingOncePerSocket<'a> {
    inner: &'a dyn PaneLister,
    failed: std::sync::Mutex<BTreeMap<String, AttentionError>>,
}

impl PaneLister for FailedListingOncePerSocket<'_> {
    fn list(&self, socket_path: &str) -> Result<Vec<crate::wezterm::PaneRow>> {
        if let Some(error) = self
            .failed
            .lock()
            .ok()
            .and_then(|failed| failed.get(socket_path).cloned())
        {
            return Err(error);
        }
        let answer = self.inner.list(socket_path);
        if let (Err(error), Ok(mut failed)) = (&answer, self.failed.lock()) {
            failed.insert(socket_path.to_owned(), error.clone());
        }
        answer
    }
}

/// What one sweep run decides with, for the steps that take it whole.
struct SweepRun<'a> {
    root: &'a Path,
    apply: bool,
    operation_id: Option<&'a str>,
    observation: &'a str,
    now: &'a str,
    panes: &'a dyn PaneLister,
    processes: Option<&'a dyn ProcessProbe>,
}

/// An absence probe counts toward removing a pane only if it was taken after
/// the binding ended; one from before belongs to the binding's own absence,
/// and the pane may have been seen since without anything clearing it. A
/// probe ahead of this run's clock predates a restart and does not count
/// either. An end ahead of the clock predates a restart too, so no probe from
/// this boot can be shown to follow it: such a probe is taken again, which
/// only delays removal.
fn retention_probe(probe: Option<Value>, end: &Value, observation: &str) -> Option<Value> {
    let ended = end["observed_mono_ns"].as_str().unwrap_or("");
    probe.filter(|probe| {
        let observed = probe["observed_mono_ns"].as_str().unwrap_or("");
        observed <= observation && observed > ended
    })
}

/// Removes a closed pane's whole tree -- claim, launches, bindings, reviews --
/// once its current binding has been over for the retention age. The pane's
/// absence is established by the same two-observation rule that ends a
/// binding, counted afresh after the end, and nothing is removed without
/// `--apply`. A tree holding anything sweep does not recognise is kept.
/// Returns whether a step failed.
fn pane_retention(
    run: &SweepRun<'_>,
    binding_path: &Path,
    binding: &Value,
    address: &PaneAddress,
    end: &Value,
    details: &mut Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<bool> {
    let root = run.root;
    let launch_id = binding["launch_id"].as_str().unwrap_or("");
    let binding_id = binding["binding_id"].as_str().unwrap_or("");
    let written = end["written_at_unix_ns"].as_str().unwrap_or("");
    match wall_age_exceeds(run.now, written, RETENTION_AGE_NS) {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return Ok(true);
        }
    }
    let pane = pane_path(root, address);
    let probe_path = pane.join("absence-probe.json");
    let probe_identity = RecordIdentity::pane(address);
    let probe = match read_record(&probe_path, Some("absence_probe"), &probe_identity) {
        Ok(probe) => retention_probe(probe, end, run.observation),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return Ok(true);
        }
    };
    let presence = absence_presence(
        root,
        address,
        binding_id,
        run.panes,
        run.processes,
        diagnostics,
    );
    if presence == SERVER_GONE {
        return Ok(false);
    }
    let action = absence_action(&presence, probe.as_ref(), run.operation_id, run.observation)?;
    let detail =
        |action: &str| json!({"kind":"pane_retention","binding_id":binding_id,"action":action});
    if action == "unavailable" {
        diagnostics.push(absence_unavailable(
            "pane absence cannot be established",
            address,
            binding_id,
        ));
    }
    if !run.apply {
        details.push(if action != "end" {
            detail(action)
        } else if pane_tree_prunable(root, &pane, diagnostics) {
            detail("prune")
        } else {
            detail("keep")
        });
        return Ok(false);
    }
    let operation = run.operation_id.expect("apply operation id");
    let binding_identity = RecordIdentity::binding(address, launch_id, binding_id);
    let end_path = binding_path
        .parent()
        .expect("binding parent")
        .join("end.json");
    // The presence above was taken before the locks, as for a binding's own
    // absence. Under them only the records are checked again.
    let applied = commit_nested_with(
        &launch_path(root, address, launch_id).join(".lock"),
        &pane.join(".claim.lock"),
        binding_path,
        Some("binding"),
        &binding_identity,
        Duration::from_secs(2),
        |locked_binding| {
            let plan = |action: &str, removals: Vec<PathBuf>, diagnostics: Vec<Diagnostic>| {
                Ok(CommitPlan {
                    result: (action.to_owned(), diagnostics),
                    replacements: Vec::new(),
                    removals,
                    private_dirs: Vec::new(),
                })
            };
            let locked_end = read_record(&end_path, Some("binding_end"), &binding_identity)?;
            if locked_binding.as_ref() != Some(binding)
                || locked_end.as_ref() != Some(end)
                || binding_selection(root, address, launch_id, binding_id)? != Some(true)
            {
                return plan(
                    "changed",
                    Vec::new(),
                    vec![diagnostic(
                        "record_invalid",
                        "binding changed before pane retention",
                    )],
                );
            }
            let locked_probe = retention_probe(
                read_record(&probe_path, Some("absence_probe"), &probe_identity)?,
                end,
                run.observation,
            );
            let action = absence_action(
                &presence,
                locked_probe.as_ref(),
                Some(operation),
                run.observation,
            )?;
            match action {
                "clear_absence" if !removal_confined(root, &probe_path) => plan(
                    "keep",
                    Vec::new(),
                    vec![diagnostic(
                        "record_invalid",
                        "absence probe outside the state root is preserved",
                    )],
                ),
                "clear_absence" => plan(action, vec![probe_path.clone()], Vec::new()),
                "first_absence" => Ok(CommitPlan {
                    result: (action.to_owned(), Vec::new()),
                    replacements: vec![Replacement::always(
                        probe_path.clone(),
                        json!({"kind":"absence_probe","schema":manifest()?.record_schema,"address":address,"operation_id":operation,"observed_mono_ns":run.observation}),
                    )],
                    removals: Vec::new(),
                    private_dirs: Vec::new(),
                }),
                "end" => {
                    let mut kept = Vec::new();
                    if !directory_confined(root, &pane) {
                        kept.push(diagnostic(
                            "record_invalid",
                            "pane removal target is outside the state root",
                        ));
                        return plan("keep", Vec::new(), kept);
                    }
                    if !pane_tree_prunable(root, &pane, &mut kept) {
                        return plan("keep", Vec::new(), kept);
                    }
                    // The tree first: an entry left behind names a binding
                    // that is gone, which a reader skips.
                    let mut removals = vec![pane.clone()];
                    removals.extend(session_entries_below(root, &pane));
                    plan("prune", removals, kept)
                }
                other => plan(other, Vec::new(), Vec::new()),
            }
        },
        |_| Ok(()),
    );
    match applied {
        Ok(((action, locked_diagnostics), ())) => {
            diagnostics.extend(locked_diagnostics);
            if action != "changed" {
                details.push(detail(&action));
            }
            Ok(false)
        }
        Err(error) => {
            diagnostics.push(error.diagnostic);
            Ok(true)
        }
    }
}

/// Whether every entry under a pane directory is state sweep recognises, so
/// removing the tree removes nothing else: records that read as valid for
/// their path, the two lock files, the lock a review writer leaves beside
/// the reviews, and the temporary files an interrupted write leaves beside a
/// record. A symlink anywhere keeps the tree.
fn pane_tree_prunable(root: &Path, pane: &Path, diagnostics: &mut Vec<Diagnostic>) -> bool {
    pane_entries_prunable(root, pane, pane, diagnostics)
}

/// The lock a review writer takes beside the review it writes and leaves in
/// place: `reviews/.<owner key>.lock`, where the owner key is the SHA-256 of
/// the review's source in lowercase hex.
fn review_lock_name(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|name| name.strip_suffix(".lock"))
        .is_some_and(hex64_text)
}

fn pane_entries_prunable(
    root: &Path,
    pane: &Path,
    directory: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let preserved = |diagnostics: &mut Vec<Diagnostic>, message: &str, path: &Path| {
        let mut item = diagnostic("record_invalid", message);
        item.context.insert(
            "path".into(),
            json!(path.strip_prefix(root).unwrap_or(path).to_string_lossy()),
        );
        diagnostics.push(item);
        false
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return preserved(diagnostics, "pane state could not be read", directory);
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return preserved(diagnostics, "pane state could not be read", directory);
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => {
                return preserved(diagnostics, "symlinked pane state is preserved", &path);
            }
            Ok(kind) if kind.is_dir() => {
                if !pane_entries_prunable(root, pane, &path, diagnostics) {
                    return false;
                }
            }
            Ok(kind) if kind.is_file() => {
                if matches!(name.as_str(), ".lock" | ".claim.lock")
                    || write_leftover(&name)
                    || (directory == pane.join("reviews") && review_lock_name(&name))
                {
                    continue;
                }
                let recognised = state_kind(&path).is_some_and(|kind| {
                    RecordIdentity::from_state_path(root, &path, kind)
                        .and_then(|identity| read_record(&path, Some(kind), &identity))
                        .is_ok_and(|record| record.is_some())
                });
                if !recognised {
                    return preserved(diagnostics, "unknown pane state is preserved", &path);
                }
            }
            _ => return preserved(diagnostics, "unknown pane state is preserved", &path),
        }
    }
    true
}

/// A file an interrupted write left beside a record: the writer here names
/// its temporary `.<record>.<uuid>`, the plugin `<record>.<session>.tmp`, and
/// the plugin's review clear moves the review aside to
/// `<record>.<session>.<ms>.clear` before it removes it.
fn write_leftover(name: &str) -> bool {
    name.contains(".json.")
        && (name.starts_with('.') || name.ends_with(".tmp") || name.ends_with(".clear"))
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
    let operation_id = operation_id
        .map(|value| {
            Uuid::parse_str(value)
                .ok()
                .filter(|parsed| parsed.to_string() == value)
                .map(|_| value.to_owned())
                .ok_or_else(|| AttentionError::usage("operation id is not canonical"))
        })
        .transpose()?;
    // An operation id lets a retried apply recognise its own earlier work, and
    // a repeat under the same id changes nothing. An apply with nothing to
    // retry gets a fresh id, so running it again is a new observation; the
    // result reports the id it used.
    let operation_id = operation_id.or_else(|| apply.then(|| Uuid::new_v4().to_string()));
    if let Some(realm) = realm_filter
        && !hex64_text(realm)
    {
        return Err(AttentionError::usage(
            "realm id must be 64 lowercase hex characters",
        ));
    }
    let observation = clock.monotonic_ns20()?;
    let now = clock.unix_ns20()?;
    ns20(&observation, "record_invalid")?;
    ns20(&now, "record_invalid")?;
    // A preview decides nothing, so one pane listing per socket and one
    // process listing answer all its steps. An apply acts on each answer and
    // takes a fresh look for each decision, except where a socket's listing
    // already failed.
    let listed_once = (!apply).then(|| ListOncePerSocket::new(panes));
    let failed_once = apply.then(|| FailedListingOncePerSocket {
        inner: panes,
        failed: std::sync::Mutex::new(BTreeMap::new()),
    });
    let probed_once = processes.filter(|_| !apply).map(ProbeOncePerAssembly::new);
    let panes = match (&listed_once, &failed_once) {
        (Some(lister), _) => lister as &dyn PaneLister,
        (None, Some(lister)) => lister as &dyn PaneLister,
        (None, None) => panes,
    };
    let processes = probed_once
        .as_ref()
        .map(|probe| probe as &dyn ProcessProbe)
        .or(processes);
    let mut details = Vec::new();
    let mut diagnostics = Vec::new();
    let files = binding_files(root, &mut diagnostics);
    // A directory the walk could not read hides the bindings below it.
    let mut failed = diagnostics.len();
    if apply && realm_filter.is_none() {
        let walked_every_directory = failed == 0;
        failed += usize::from(complete_session_index(
            root,
            &files,
            walked_every_directory,
            &mut diagnostics,
        ));
    }
    failed += collect_projection_orphans(root, realm_filter, apply, &mut details, &mut diagnostics);
    let mut ended: BTreeMap<String, Vec<(String, PathBuf, bool)>> = BTreeMap::new();
    let mut presence_cache: BTreeMap<PaneAddress, String> = BTreeMap::new();
    // Kept apart from the readers' view above, which reads a pane of kept
    // history as unavailable where the absence rule reads it as
    // `SERVER_GONE`.
    let mut absence_cache: BTreeMap<PaneAddress, String> = BTreeMap::new();
    if realm_filter.is_none() {
        failed += collect_tab_orders(
            root,
            apply,
            panes,
            processes,
            &mut presence_cache,
            &mut details,
            &mut diagnostics,
        );
    }
    for binding_path in &files {
        let identity = match RecordIdentity::from_state_path(root, binding_path, "binding") {
            Ok(identity) => identity,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
                continue;
            }
        };
        let binding = match read_record(binding_path, Some("binding"), &identity) {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
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
                failed += 1;
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
                failed += 1;
                continue;
            }
        };
        let ended_now = end.as_ref().is_some_and(|end| ends_binding(end, &binding));
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
                Err(error) => {
                    diagnostics.push(error.diagnostic);
                    failed += 1;
                }
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
                            root,
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
                        }
                    }
                    Err(error) => {
                        diagnostics.push(error.diagnostic);
                        failed += 1;
                    }
                }
            } else {
                match compaction_plan(
                    root,
                    binding_dir,
                    &address,
                    launch_id,
                    binding_id,
                    &now,
                    None,
                ) {
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
            if let Some(end) = &end {
                let run = SweepRun {
                    root,
                    apply,
                    operation_id: operation_id.as_deref(),
                    observation: &observation,
                    now: &now,
                    panes,
                    processes,
                };
                failed += usize::from(pane_retention(
                    &run,
                    binding_path,
                    &binding,
                    &address,
                    end,
                    &mut details,
                    &mut diagnostics,
                )?);
            }
            continue;
        }
        let presence = if let Some(cached) = absence_cache.get(&address) {
            cached.clone()
        } else {
            let observed = absence_presence(
                root,
                &address,
                binding_id,
                panes,
                processes,
                &mut diagnostics,
            );
            absence_cache.insert(address.clone(), observed.clone());
            observed
        };
        // Kept history: reported once for the run, and nothing to decide.
        if presence == SERVER_GONE {
            continue;
        }
        let probe_path = pane.join("absence-probe.json");
        let probe = match read_record(
            &probe_path,
            Some("absence_probe"),
            &RecordIdentity::pane(&address),
        ) {
            Ok(probe) => probe,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
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
                diagnostics.push(absence_unavailable(
                    "binding absence cannot be established",
                    &address,
                    binding_id,
                ));
            }
            continue;
        }
        let operation = operation_id.as_deref().expect("apply operation id");
        // A fresh look for the decision, taken before the locks: listing panes
        // can take seconds, and a hook writer gives up on these locks after
        // two. Under the locks only the records are checked again.
        let fresh_presence = absence_presence(
            root,
            &address,
            binding_id,
            panes,
            processes,
            &mut diagnostics,
        );
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
                let locked_probe = read_record(
                    &probe_path,
                    Some("absence_probe"),
                    &RecordIdentity::pane(&address),
                )?;
                let mut action = absence_action(
                    &fresh_presence,
                    locked_probe.as_ref(),
                    Some(operation),
                    &observation,
                )?;
                let mut replacements = Vec::new();
                let mut removals = Vec::new();
                if action == "clear_absence" {
                    if !removal_confined(root, &probe_path) {
                        return Ok(CommitPlan {
                            result: AbsenceOutcome {
                                action: "keep".to_owned(),
                                diagnostic: Some(diagnostic(
                                    "record_invalid",
                                    "absence probe outside the state root is preserved",
                                )),
                            },
                            replacements,
                            removals,
                            private_dirs: Vec::new(),
                        });
                    }
                    removals.push(probe_path.clone());
                } else if action == "first_absence" {
                    replacements.push(Replacement::always(
                        probe_path.clone(),
                        json!({"kind":"absence_probe","schema":manifest()?.record_schema,"address":address,"operation_id":operation,"observed_mono_ns":observation}),
                    ));
                } else if action == "end" {
                    let locked_end =
                        read_record(&end_path, Some("binding_end"), &binding_identity)?;
                    // An end that arrived since the look above already ends
                    // this binding, and keeps its own reason.
                    if locked_end
                        .as_ref()
                        .is_some_and(|end| ends_binding(end, &binding))
                    {
                        action = "already_ended";
                    } else {
                        replacements.push(Replacement::always(
                            end_path.clone(),
                            json!({"kind":"binding_end","schema":manifest()?.record_schema,"address":address,"launch_id":launch_id,"binding_id":binding_id,"reason":"sweep_absent","operation_id":operation,"event_id":Uuid::new_v4().to_string(),"binding_event_id":binding["event_id"],"observed_mono_ns":observation,"written_at_unix_ns":clock.unix_ns20()?}),
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
        match applied {
            Ok((outcome, ())) => {
                diagnostics.extend(outcome.diagnostic);
                if outcome.action != "changed" {
                    details.push(
                        json!({"kind":"absence","binding_id":binding_id,"action":outcome.action}),
                    );
                    if outcome.action == "unavailable" && fresh_presence != SERVER_GONE {
                        diagnostics.push(absence_unavailable(
                            "binding absence cannot be established",
                            &address,
                            binding_id,
                        ));
                    }
                }
            }
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
            }
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
            // The preview runs the checks apply runs on the tree itself, so it
            // says keep where apply would keep.
            let mut kept = Vec::new();
            let binding_path = binding_dir.join("binding.json");
            let binding = RecordIdentity::from_state_path(root, &binding_path, "binding")
                .and_then(|identity| read_record(&binding_path, Some("binding"), &identity));
            let prunable = match binding {
                Ok(Some(binding)) => {
                    binding_confined(root, &binding_dir, &mut kept)
                        && binding_known_and_prunable(&binding_dir, &binding, &mut kept)
                }
                Ok(None) => false,
                Err(error) => {
                    kept.push(error.diagnostic);
                    false
                }
            };
            diagnostics.extend(kept);
            let action = if prunable { "prune" } else { "keep" };
            details.push(json!({"kind":"binding_retention","binding_id":binding_id_for_detail,"action":action}));
            continue;
        }
        let binding_path = binding_dir.join("binding.json");
        let identity = match RecordIdentity::from_state_path(root, &binding_path, "binding") {
            Ok(identity) => identity,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
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
                    let end_is_current = end
                        .as_ref()
                        .is_some_and(|end| ends_binding(end, &locked_binding));
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
                    if !end_is_current || (!still_old && !cap_paths.contains(&binding_dir)) {
                        local_diagnostics.push(diagnostic(
                            "record_invalid",
                            "binding changed before retention apply",
                        ));
                    } else if binding_confined(root, &binding_dir, &mut local_diagnostics)
                        && binding_known_and_prunable(
                            &binding_dir,
                            &locked_binding,
                            &mut local_diagnostics,
                        )
                    {
                        let mut removals = vec![binding_dir.clone()];
                        removals.extend(session_entries_below(root, &binding_dir));
                        return Ok(CommitPlan {
                            result: RetentionOutcome {
                                pruned: true,
                                diagnostics: local_diagnostics,
                            },
                            replacements: Vec::new(),
                            removals,
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
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
            }
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
            failed_steps: if apply { failed } else { 0 },
        },
        fold_kept_history(diagnostics),
    ))
}

#[cfg(test)]
mod projection_collection_tests {
    use super::*;

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn address(realm: char) -> PaneAddress {
        PaneAddress {
            realm_id: realm.to_string().repeat(64),
            incarnation_id: "b".repeat(64),
            pane_id: "42".to_owned(),
        }
    }

    /// Writes a claim for `address`, with `schema` in place of the current
    /// record schema when they differ.
    fn claim(root: &Path, address: &PaneAddress, schema: u64) {
        let path = pane_path(root, address).join("claim.json");
        crate::records::atomic_replace(
            &path,
            &json!({
                "kind":"claim","schema":manifest().expect("manifest").record_schema,
                "address":address,"launch_id":"00000000-0000-4000-8000-000000000701",
                "tty_path":"/dev/ttys888","tty_fingerprint":"f".repeat(64),
                "observed_mono_ns":"00000000000000000100"
            }),
        )
        .expect("claim");
        let mut value: Value =
            serde_json::from_slice(&fs::read(&path).expect("claim")).expect("claim JSON");
        value["schema"] = json!(schema);
        fs::write(&path, serde_json::to_vec(&value).expect("JSON")).expect("rewrite claim");
    }

    fn current_schema() -> u64 {
        manifest().expect("manifest").record_schema
    }

    /// A state root holding pane 42's claim at realm `a` and its three flat
    /// files, enumerated as collection first sees them.
    fn enumerated() -> (Root, PaneAddress, BTreeMap<String, Vec<FlatFile>>) {
        let root = Root(
            std::env::temp_dir().join(format!("attention-collect-{}", Uuid::new_v4().simple())),
        );
        let address = address('a');
        claim(&root.0, &address, current_schema());
        for name in ["42", "42.agents", "42.ack"] {
            fs::write(root.0.join(name), "stop\n").expect("flat file");
        }
        let (files, malformed) = enumerate_flat_candidates(&root.0).expect("enumerate");
        assert!(malformed.is_empty());
        (root, address, files)
    }

    fn assert_nothing_collected(root: &Path) {
        for name in ["42", "42.agents", "42.ack"] {
            assert!(root.join(name).exists(), "{name} was collected");
        }
    }

    /// Collection enumerates the flat files, then takes the pane's claim lock
    /// and checks each file is still the one it enumerated. Calling the two
    /// steps in turn puts a replacement exactly between them, which a test
    /// racing a sweep thread against a sleep could only hope to do.
    #[test]
    fn a_marker_replaced_after_enumeration_is_refused_not_collected() {
        let (root, address, files) = enumerated();
        let next = root.0.join("42.agents.next");
        fs::write(&next, "thinking\n").expect("replacement");
        fs::rename(&next, root.0.join("42.agents")).expect("replace agents");

        let error = apply_projection_collection(&root.0, &address, "42", &files["42"])
            .expect_err("a replaced file is refused");
        assert_eq!(error.diagnostic.code, "record_invalid");
        assert!(error.diagnostic.message.contains("changed"));
        assert_eq!(
            fs::read_to_string(root.0.join("42")).expect("marker"),
            "stop\n"
        );
        assert_eq!(
            fs::read_to_string(root.0.join("42.agents")).expect("agents"),
            "thinking\n"
        );
        assert!(root.0.join("42.ack").exists());
    }

    /// Collection decided the markers belong to one claimed pane. A claim for
    /// the same pane id that appears before the lock and cannot be read makes
    /// them unattributable, and the check under the lock sees it.
    #[test]
    fn a_claim_that_turns_unreadable_before_the_lock_stops_collection() {
        let (root, owner, files) = enumerated();
        claim(&root.0, &address('c'), 999);
        let error = apply_projection_collection(&root.0, &owner, "42", &files["42"])
            .expect_err("unattributable markers are refused");
        assert_eq!(error.diagnostic.code, "record_invalid");
        assert_nothing_collected(&root.0);
    }

    /// A second claim for the same pane id at another address, appearing
    /// before the lock, makes the markers ambiguous.
    #[test]
    fn a_second_owner_that_appears_before_the_lock_stops_collection() {
        let (root, owner, files) = enumerated();
        claim(&root.0, &address('c'), current_schema());
        let error = apply_projection_collection(&root.0, &owner, "42", &files["42"])
            .expect_err("ambiguous markers are refused");
        assert_eq!(error.diagnostic.code, "binding_conflict");
        assert_nothing_collected(&root.0);
    }
}
