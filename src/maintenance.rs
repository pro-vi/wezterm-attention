use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{PaneAddress, socket_identity};
use crate::presence::{
    FailedListingOncePerSocket, ListOncePerSocket, PaneEvidence, ProbeOncePerAssembly,
    kept_history_code, pane_evidence, reader_presence, recorded_socket,
};
use crate::protocol::{
    AttentionError, Diagnostic, EMBEDDED_MANIFEST, Result, eligible_subagent_presence, hex64_text,
    manifest, sha256_hex,
};
use crate::query::{
    FileStamp, collect_binding_files, collect_state_files, name_address, naming_record,
    read_bindings_with_ports, read_tab_publications, record_address, state_relative,
};
use crate::records::{
    BINDING_FILE, BindingState, CommitPlan, FileRecords, RecordIdentity, RecordRead, Replacement,
    agents_dir, atomic_replace_if_different, binding_record_kind, binding_session_entry,
    claim_lock, commit, directory_confined, ends_binding, incarnation_path, launch_lock, lock_file,
    pane_path, read_record, read_record_at, removal_confined, remove_file_durable,
    session_index_marker, session_index_path,
};
use crate::wezterm::{Clock, PaneLister, Presence, ProcessInspector, ProcessProbe};

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
            Some("tab_order_collection")
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

fn binding_files(root: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_binding_files(root, &mut files, diagnostics);
    files.sort();
    files
}

/// A binding's state as sweep reads it, from the files themselves.
fn binding_state(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> BindingState {
    BindingState::read(&FileRecords, root, address, launch_id, binding_id)
}

/// Whether a binding is its pane's current one, as sweep takes it from the
/// binding's `state`: `None` when the pane has no claim, and an error, named
/// by its file, when the claim or the pointer could not be read, since
/// sweep decides nothing on a binding it cannot place.
fn binding_selection(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    state: &BindingState,
) -> Result<Option<bool>> {
    let named = |read: &RecordRead, kind: &str, identity: RecordIdentity| {
        let path = identity.path(root, kind)?;
        read.clone()
            .into_result()
            .map_err(|error| naming_record(root, &path, error))
    };
    if named(&state.claim, "claim", RecordIdentity::pane(address))?.is_none() {
        return Ok(None);
    }
    named(
        &state.pointer,
        "current_binding",
        RecordIdentity::launch(address, launch_id),
    )?;
    Ok(Some(state.current(launch_id, binding_id)))
}

fn audit_state(root: &Path) -> (Vec<Value>, Vec<Diagnostic>) {
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_json(root, &root.join("v2"), &mut files, &mut diagnostics);
    files.sort();
    let mut records = Vec::new();
    for path in files {
        let Some((kind, identity)) = RecordIdentity::locate(root, &path) else {
            let error =
                AttentionError::new("record_invalid", "unknown v2 state file is uninspected");
            diagnostics.push(naming_record(root, &path, error).diagnostic);
            continue;
        };
        let read = identity.and_then(|identity| read_record(&path, Some(kind), &identity));
        match read {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(error) => diagnostics.push(naming_record(root, &path, error).diagnostic),
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
    inspector: &dyn ProcessInspector,
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
                diagnostics.push(Diagnostic::new(
                    "state_permissions",
                    "state directory is accessible to other users",
                ));
                "finding"
            }
            Err(_) => {
                diagnostics.push(Diagnostic::new(
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
        diagnostics.push(Diagnostic::new(
            "probe_unavailable",
            "identity-scoped process evidence is unavailable",
        ));
    }
    probes.push(json!({"name":"processes","status":process_status}));
    let embedded_digest = sha256_hex(EMBEDDED_MANIFEST.as_bytes());
    let disk_bytes = match runtime_manifest_bytes() {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => {
            diagnostics.push(Diagnostic::new(
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
        diagnostics.push(Diagnostic::new(
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
    let environment_status = environment_probe(root, environment, inspector, &mut diagnostics);
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
/// run and nothing they write is ever shown. Where an agent can claim its own
/// pane, its hook publishes them at its first session start, so until then
/// there is nothing to check.
fn environment_probe(
    root: &Path,
    environment: &BTreeMap<String, String>,
    inspector: &dyn ProcessInspector,
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
    let published = read_record_at(root, "realm", &RecordIdentity::realm(&realm_id))
        .is_ok_and(|record| record.is_some())
        && read_record_at(
            root,
            "incarnation",
            &RecordIdentity::incarnation(&realm_id, &incarnation_id),
        )
        .is_ok_and(|record| record.is_some());
    if published {
        "healthy"
    } else if crate::launch::self_claim_refusal(environment, inspector).is_none() {
        "unobserved"
    } else {
        diagnostics.push(Diagnostic::new(
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

/// Whether a child's presence still counts, by the rule every reader
/// applies, [`eligible_subagent_presence`]. Compaction keeps a presence it
/// cannot judge, so a clock that went back is an error here.
fn presence_eligible(
    presence: &Value,
    clear: Option<&Value>,
    floor: Option<&Value>,
    now: &str,
) -> Result<bool> {
    let (eligible, problem) = eligible_subagent_presence(
        presence,
        clear.and_then(|clear| clear["observed_mono_ns"].as_str()),
        floor.and_then(|floor| floor["floor_mono_ns"].as_str()),
        Some(now),
        manifest()?,
    );
    match problem {
        None => Ok(eligible),
        Some("clock_skew") => Err(AttentionError::new(
            "clock_skew",
            "retention timestamp is newer than current UTC",
        )),
        Some(code) => Err(AttentionError::new(
            code,
            "subagent presence cannot be judged",
        )),
    }
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
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
    now: &str,
    operation_id: Option<&str>,
) -> Result<Compaction> {
    let identity = RecordIdentity::binding(address, launch_id, binding_id);
    let floor = read_record_at(root, "subagent_retention_floor", &identity)?;
    let clear = read_record_at(root, "subagent_clear", &identity)?;
    let mut records = Vec::new();
    let agents = agents_dir(root, address, launch_id, binding_id);
    if fs::symlink_metadata(&agents).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Ok(Compaction {
            action: "blocked",
            floor: None,
            candidates: Vec::new(),
            diagnostics: vec![Diagnostic::new(
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
            diagnostics: vec![Diagnostic::new(
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
                    diagnostics: vec![Diagnostic::new(
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
                    diagnostics: vec![Diagnostic::new(
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
                diagnostics.push(Diagnostic::new(
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
        diagnostics.push(Diagnostic::new(
            "record_invalid",
            "binding address is invalid",
        ));
        return false;
    };
    let Some(launch_id) = binding.get("launch_id").and_then(Value::as_str) else {
        return false;
    };
    let Some(binding_id) = binding.get("binding_id").and_then(Value::as_str) else {
        return false;
    };
    let identity = RecordIdentity::binding(&address, launch_id, binding_id);
    let Ok(entries) = fs::read_dir(binding_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            diagnostics.push(Diagnostic::new(
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
                    diagnostics.push(Diagnostic::new(
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
                    diagnostics.push(Diagnostic::new(
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
        let kind = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(binding_record_kind);
        let (true, Some(kind)) = (path.is_file(), kind) else {
            diagnostics.push(Diagnostic::new(
                "record_invalid",
                "unknown binding state is preserved",
            ));
            return false;
        };
        match read_record(&path, Some(kind), &identity) {
            Ok(Some(_)) => {}
            _ => {
                diagnostics.push(Diagnostic::new(
                    "record_invalid",
                    "binding child identity mismatch",
                ));
                return false;
            }
        }
    }
    true
}

/// Whether a binding directory lies inside the state root, so removing it
/// removes nothing outside.
fn binding_confined(root: &Path, binding_dir: &Path, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let confined = directory_confined(root, binding_dir);
    if !confined {
        diagnostics.push(Diagnostic::new(
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
        item.set("binding_id", binding_id);
    }
}

/// The diagnostic for a pane whose absence could not be established, named
/// by the pane and binding it is about.
fn absence_unavailable(message: &str, address: &PaneAddress, binding_id: &str) -> Diagnostic {
    let mut item = Diagnostic::new("probe_unavailable", message);
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

/// A tab order whose writer has exited stays on disk for good: the writer
/// withdraws its own files when a window closes, but nothing runs after the
/// last window of a WezTerm process. It is collected here once every pane it
/// names is verified absent, or once it names no tab at all: WezTerm closes a
/// window whose last tab closes, so an empty order is the bar's final draw. A
/// file naming a bare decimal pane id is kept, because it has no realm to
/// ask; so is one naming a pane whose realm or incarnation is not recorded,
/// and one whose panes could not be probed. A GUI source is not the pane realm
/// a sweep selects, so a realm-filtered sweep leaves these files alone.
/// Returns how many steps failed.
fn collect_tab_orders(
    root: &Path,
    apply: bool,
    panes: &dyn PaneLister,
    processes: Option<&dyn ProcessProbe>,
    details: &mut Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> usize {
    let mut failed = 0;
    let mut presence_cache: BTreeMap<PaneAddress, String> = BTreeMap::new();
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
                                diagnostics.push(Diagnostic::new(
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
                            item.set("path", relative.as_str());
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
                diagnostics.push(
                    Diagnostic::new(
                        "record_invalid",
                        "tab order outside the state root is preserved",
                    )
                    .with("path", relative.as_str()),
                );
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
/// The reader has already checked the shape; a bare decimal id has no address.
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
    let probe_identity = RecordIdentity::pane(address);
    let probe_path = probe_identity.path(root, "absence_probe")?;
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
    // The presence above was taken before the locks, as for a binding's own
    // absence. Under them only the records are checked again.
    let applied = commit(
        root,
        &[
            &launch_lock(root, address, launch_id),
            &claim_lock(root, address),
        ],
        "binding",
        &binding_identity,
        |locked_binding| {
            let plan = |action: &str, removals: Vec<PathBuf>, diagnostics: Vec<Diagnostic>| {
                Ok(CommitPlan {
                    removals,
                    ..CommitPlan::reporting((action.to_owned(), diagnostics))
                })
            };
            let state = binding_state(root, address, launch_id, binding_id);
            let locked_end = state.end.clone().into_result()?;
            if locked_binding.as_ref() != Some(binding)
                || locked_end.as_ref() != Some(end)
                || binding_selection(root, address, launch_id, binding_id, &state)? != Some(true)
            {
                return plan(
                    "changed",
                    Vec::new(),
                    vec![Diagnostic::new(
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
                    vec![Diagnostic::new(
                        "record_invalid",
                        "absence probe outside the state root is preserved",
                    )],
                ),
                "clear_absence" => plan(action, vec![probe_path.clone()], Vec::new()),
                "first_absence" => Ok(CommitPlan {
                    replacements: vec![Replacement::always(
                        probe_path.clone(),
                        json!({"kind":"absence_probe","schema":manifest()?.record_schema,"address":address,"operation_id":operation,"observed_mono_ns":run.observation}),
                    )],
                    ..CommitPlan::reporting((action.to_owned(), Vec::new()))
                }),
                "end" => {
                    let mut kept = Vec::new();
                    if !directory_confined(root, &pane) {
                        kept.push(Diagnostic::new(
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

fn pane_entries_prunable(
    root: &Path,
    pane: &Path,
    directory: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let preserved = |diagnostics: &mut Vec<Diagnostic>, message: &str, path: &Path| {
        diagnostics.push(
            Diagnostic::new("record_invalid", message).with("path", state_relative(root, path)),
        );
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
                if lock_file(pane, directory, &name) || write_leftover(&name) {
                    continue;
                }
                let recognised =
                    RecordIdentity::locate(root, &path).is_some_and(|(kind, identity)| {
                        identity
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
/// its temporary `.<record>.<uuid>`. Plugin builds that wrote pane records
/// themselves left `<record>.<session>.tmp`, and a review they were clearing
/// moved aside to `<record>.<session>.<ms>.clear`; nothing reads either, so
/// one a crash left goes with its tree.
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
    let listed_once = ListOncePerSocket::new(panes);
    let failed_once = FailedListingOncePerSocket::new(panes);
    let probed_once = processes.filter(|_| !apply).map(ProbeOncePerAssembly::new);
    let panes: &dyn PaneLister = if apply { &failed_once } else { &listed_once };
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
    let mut ended: BTreeMap<String, Vec<(String, PathBuf, bool)>> = BTreeMap::new();
    // The absence rule's view of each pane. The tab-order step keeps its
    // own, which reads a pane of kept history as unavailable where this one
    // reads it as `SERVER_GONE`.
    let mut absence_cache: BTreeMap<PaneAddress, String> = BTreeMap::new();
    if realm_filter.is_none() {
        failed += collect_tab_orders(
            root,
            apply,
            panes,
            processes,
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
        let state = binding_state(root, &address, launch_id, binding_id);
        let current = match binding_selection(root, &address, launch_id, binding_id, &state) {
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
        let binding_identity = RecordIdentity::binding(&address, launch_id, binding_id);
        let end_path = binding_identity.path(root, "binding_end")?;
        let end = match state.end.clone().into_result() {
            Ok(end) => end,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                failed += 1;
                continue;
            }
        };
        let ended_now = state.ended(&binding);
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
                let applied = commit(
                    root,
                    &[
                        &launch_lock(root, &address, launch_id),
                        &claim_lock(root, &address),
                    ],
                    "binding",
                    &binding_identity,
                    |locked_binding| {
                        if locked_binding.as_ref() != Some(&binding)
                            || binding_selection(
                                root,
                                &address,
                                launch_id,
                                binding_id,
                                &binding_state(root, &address, launch_id, binding_id),
                            )? != Some(true)
                        {
                            return Ok(CommitPlan::reporting(CompactionOutcome {
                                action: "changed".to_owned(),
                                floor: None,
                                covered: 0,
                                deleted: 0,
                                diagnostics: vec![Diagnostic::new(
                                    "record_invalid",
                                    "binding changed before sweep apply",
                                )],
                            }));
                        }
                        let plan = compaction_plan(
                            root,
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
                                    binding_identity.path(root, "subagent_retention_floor")?,
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
                        let deleted = removals.len();
                        Ok(CommitPlan {
                            replacements,
                            removals,
                            ..CommitPlan::reporting(CompactionOutcome {
                                action: plan.action.to_owned(),
                                floor: plan.floor,
                                covered,
                                deleted,
                                diagnostics: plan.diagnostics,
                            })
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
                match compaction_plan(root, &address, launch_id, binding_id, &now, None) {
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
        let probe_identity = RecordIdentity::pane(&address);
        let probe_path = probe_identity.path(root, "absence_probe")?;
        let probe = match read_record(&probe_path, Some("absence_probe"), &probe_identity) {
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
        let applied = commit(
            root,
            &[
                &launch_lock(root, &address, launch_id),
                &claim_lock(root, &address),
            ],
            "binding",
            &binding_identity,
            |locked_binding| {
                if locked_binding.as_ref() != Some(&binding)
                    || binding_selection(
                        root,
                        &address,
                        launch_id,
                        binding_id,
                        &binding_state(root, &address, launch_id, binding_id),
                    )? != Some(true)
                {
                    return Ok(CommitPlan::reporting(AbsenceOutcome {
                        action: "changed".to_owned(),
                        diagnostic: Some(Diagnostic::new(
                            "record_invalid",
                            "binding changed before sweep apply",
                        )),
                    }));
                }
                let locked_probe =
                    read_record(&probe_path, Some("absence_probe"), &probe_identity)?;
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
                        return Ok(CommitPlan::reporting(AbsenceOutcome {
                            action: "keep".to_owned(),
                            diagnostic: Some(Diagnostic::new(
                                "record_invalid",
                                "absence probe outside the state root is preserved",
                            )),
                        }));
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
                    replacements,
                    removals,
                    ..CommitPlan::reporting(AbsenceOutcome {
                        action: action.to_owned(),
                        diagnostic: None,
                    })
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
            let binding_path = binding_dir.join(BINDING_FILE);
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
        let binding_path = binding_dir.join(BINDING_FILE);
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
        let outcome = commit(
            root,
            &[
                &launch_lock(root, &address, launch_id),
                &claim_lock(root, &address),
            ],
            "binding",
            &identity,
            |locked_binding| {
                let mut local_diagnostics = Vec::new();
                let Some(locked_binding) = locked_binding else {
                    local_diagnostics.push(Diagnostic::new(
                        "record_invalid",
                        "binding changed before retention apply",
                    ));
                    return Ok(CommitPlan::reporting(RetentionOutcome {
                        pruned: false,
                        diagnostics: local_diagnostics,
                    }));
                };
                if locked_binding != binding {
                    local_diagnostics.push(Diagnostic::new(
                        "record_invalid",
                        "binding changed before retention apply",
                    ));
                } else {
                    let state = binding_state(root, &address, launch_id, binding_id);
                    match binding_selection(root, &address, launch_id, binding_id, &state)? {
                        None => {
                            local_diagnostics.push(Diagnostic::new(
                                "record_invalid",
                                "pane claim changed before retention apply",
                            ));
                            return Ok(CommitPlan::reporting(RetentionOutcome {
                                pruned: false,
                                diagnostics: local_diagnostics,
                            }));
                        }
                        Some(true) => {
                            local_diagnostics.push(Diagnostic::new(
                                "binding_conflict",
                                "current binding was preserved during retention",
                            ));
                            return Ok(CommitPlan::reporting(RetentionOutcome {
                                pruned: false,
                                diagnostics: local_diagnostics,
                            }));
                        }
                        Some(false) => {}
                    }
                    let end = state.end.clone().into_result()?;
                    let end_is_current = state.ended(&locked_binding);
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
                        local_diagnostics.push(Diagnostic::new(
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
                            removals,
                            ..CommitPlan::reporting(RetentionOutcome {
                                pruned: true,
                                diagnostics: local_diagnostics,
                            })
                        });
                    }
                }
                Ok(CommitPlan::reporting(RetentionOutcome {
                    pruned: false,
                    diagnostics: local_diagnostics,
                }))
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
