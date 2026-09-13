use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::identity::PaneAddress;
use crate::identity::socket_identity;
use crate::protocol::{AttentionError, Diagnostic, Result};
use crate::records::{RecordIdentity, RecordRead, read_record, read_record_typed};
use crate::wezterm::{PaneLister, Presence, ProcessProbe};

#[derive(Clone, Debug, Serialize)]
pub struct BindingRow {
    pub address: PaneAddress,
    pub launch_id: String,
    pub binding_id: String,
    pub provider: String,
    pub provider_session_id: String,
    pub binding_phase: String,
    pub pane_presence: String,
    pub reader_confidence: String,
    pub binding_health: String,
    pub current: bool,
    pub expected_session_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_source: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BindingQueryScope {
    pub realm_id: String,
    pub incarnation_id: String,
}

pub fn validate_socket_selector(socket: &str) -> Result<()> {
    if !Path::new(socket).is_absolute()
        || socket.len() > crate::protocol::manifest()?.limits.path_max_bytes
        || socket.chars().any(|c| c < ' ' || c == '\u{7f}')
    {
        return Err(AttentionError::usage(
            "--socket must be an absolute path within the path bound",
        ));
    }
    Ok(())
}

pub fn read_bindings_for_socket_with_ports(
    root: &Path,
    socket: &str,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(BindingQueryScope, Vec<BindingRow>, Vec<Diagnostic>)> {
    validate_socket_selector(socket)?;
    let (realm_id, incarnation_id, _) = socket_identity(socket)?;
    let scope = BindingQueryScope {
        realm_id,
        incarnation_id,
    };
    let selected = root
        .join("v2/realms")
        .join(&scope.realm_id)
        .join("incarnations")
        .join(&scope.incarnation_id);
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_selected_binding_files(&selected, &mut files, &mut diagnostics, true);
    let (rows, mut read_diagnostics) = assemble_bindings(root, files, panes, processes, true)?;
    diagnostics.append(&mut read_diagnostics);
    let after = socket_identity(socket)?;
    if after.0 != scope.realm_id || after.1 != scope.incarnation_id {
        let mut error = AttentionError::new(
            "incarnation_changed",
            "selected socket identity changed during discovery",
        );
        error.exit_code = 1;
        return Err(error);
    }
    Ok((scope, rows, diagnostics))
}

fn collect_selected_binding_files(
    path: &Path,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
    missing_ok: bool,
) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding directory could not be enumerated",
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                diagnostics.push(diagnostic(
                    "record_invalid",
                    "selected binding directory entry could not be read",
                ));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if entry.file_name() == "binding.json" => {
                if kind.is_symlink() {
                    diagnostics.push(diagnostic(
                        "record_invalid",
                        "selected binding record is a symlink",
                    ));
                } else {
                    output.push(entry.path());
                }
            }
            Ok(kind) if kind.is_dir() => {
                collect_selected_binding_files(&entry.path(), output, diagnostics, false)
            }
            Ok(kind) if kind.is_symlink() => diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding directory contains a symlink that was not traversed",
            )),
            Err(_) => diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding entry type is unavailable",
            )),
            _ => {}
        }
    }
}

fn collect_binding_files(path: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            collect_binding_files(&candidate, output);
        } else if candidate.file_name().and_then(|name| name.to_str()) == Some("binding.json") {
            output.push(candidate);
        }
    }
}

fn string(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn path_identity(root: &Path, path: &Path) -> Option<(String, String, String, String, String)> {
    let parts: Vec<_> = path
        .strip_prefix(root)
        .ok()?
        .iter()
        .map(|part| part.to_str())
        .collect();
    if parts.len() != 12
        || parts[0] != Some("v2")
        || parts[1] != Some("realms")
        || parts[3] != Some("incarnations")
        || parts[5] != Some("panes")
        || parts[7] != Some("launches")
        || parts[9] != Some("bindings")
        || parts[11] != Some("binding.json")
    {
        return None;
    }
    Some((
        parts[2]?.to_owned(),
        parts[4]?.to_owned(),
        parts[6]?.to_owned(),
        parts[8]?.to_owned(),
        parts[10]?.to_owned(),
    ))
}

fn diagnostic(code: &str, message: &str) -> Diagnostic {
    AttentionError::new(code, message).diagnostic
}

pub fn read_bindings(root: &Path) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    read_bindings_with_ports(root, None, None)
}

pub(crate) fn pane_presence(
    root: &Path,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> String {
    let Some(panes) = panes else {
        return "unavailable".to_owned();
    };
    let realm_path = root
        .join("v2/realms")
        .join(&address.realm_id)
        .join("realm.json");
    let incarnation_path = root
        .join("v2/realms")
        .join(&address.realm_id)
        .join("incarnations")
        .join(&address.incarnation_id)
        .join("incarnation.json");
    let realm = match read_record(
        &realm_path,
        Some("realm"),
        &RecordIdentity::realm(&address.realm_id),
    ) {
        Ok(Some(record)) => record,
        Ok(None) => return "unavailable".to_owned(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    };
    match read_record(
        &incarnation_path,
        Some("incarnation"),
        &RecordIdentity::incarnation(&address.realm_id, &address.incarnation_id),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => return "unavailable".to_owned(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    }
    let Some(socket_path) = realm.get("socket_path").and_then(Value::as_str) else {
        return "unavailable".to_owned();
    };
    match socket_identity(socket_path) {
        Ok((realm_id, incarnation_id, _))
            if realm_id == address.realm_id && incarnation_id == address.incarnation_id => {}
        Ok(_) => {
            diagnostics.push(diagnostic(
                "incarnation_changed",
                "realm socket identity changed",
            ));
            return "unavailable".to_owned();
        }
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    }
    match panes.list(socket_path) {
        Ok(rows) if rows.iter().any(|row| row.pane_id == address.pane_id) => "present".to_owned(),
        Ok(_) => match processes.map(|probe| probe.presence(socket_path, &address.pane_id)) {
            Some(Presence::Present) => "present".to_owned(),
            Some(Presence::Absent) => "verified_absent".to_owned(),
            _ => {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "identity-scoped process probe is unavailable",
                ));
                "unavailable".to_owned()
            }
        },
        Err(error) => {
            diagnostics.push(error.diagnostic);
            "unavailable".to_owned()
        }
    }
}

pub fn read_bindings_with_ports(
    root: &Path,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    let mut files = Vec::new();
    collect_binding_files(&root.join("v2/realms"), &mut files);
    assemble_bindings(root, files, panes, processes, false)
}

fn assemble_bindings(
    root: &Path,
    mut files: Vec<PathBuf>,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    typed: bool,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    let read_record = |path: &Path, kind: Option<&str>, identity: &RecordIdentity| {
        if !typed {
            return read_record(path, kind, identity);
        }
        match read_record_typed(path, kind, identity) {
            RecordRead::Present(value) => Ok(Some(value)),
            RecordRead::Missing => Ok(None),
            RecordRead::Unavailable(mut error) => {
                error.diagnostic.message = "selected state record I/O is unavailable".into();
                Err(error)
            }
            RecordRead::Invalid(error) | RecordRead::Unsupported(error) => Err(error),
        }
    };
    files.sort();
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    let mut presence_cache: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut claim_cache: BTreeMap<PathBuf, (Option<Value>, Option<String>)> = BTreeMap::new();
    for path in files {
        let Some((path_realm, path_incarnation, path_pane, path_launch, path_binding)) =
            path_identity(root, &path)
        else {
            diagnostics.push(diagnostic(
                "record_invalid",
                "binding path has the wrong shape",
            ));
            continue;
        };
        let path_address = PaneAddress {
            realm_id: path_realm,
            incarnation_id: path_incarnation,
            pane_id: path_pane,
        };
        let binding = match read_record(
            &path,
            Some("binding"),
            &RecordIdentity::binding(&path_address, &path_launch, &path_binding),
        ) {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
        };
        let Some(address_value) = binding.get("address") else {
            continue;
        };
        let address: PaneAddress = serde_json::from_value(address_value.clone()).map_err(|_| {
            crate::protocol::AttentionError::new("record_invalid", "binding address is invalid")
        })?;
        let Some(launch_id) = string(&binding, "launch_id") else {
            continue;
        };
        let Some(binding_id) = string(&binding, "binding_id") else {
            continue;
        };
        let binding_dir = path.parent().expect("binding file has parent");
        let launch_dir = binding_dir
            .parent()
            .and_then(Path::parent)
            .expect("binding path has launch parent");
        let pane_dir = launch_dir
            .parent()
            .and_then(Path::parent)
            .expect("launch path has pane parent");
        let identity = RecordIdentity::binding(&address, &launch_id, &binding_id);
        let mut end_health = None;
        let end = match read_record(
            &binding_dir.join("end.json"),
            Some("binding_end"),
            &identity,
        ) {
            Ok(value) => value,
            Err(error) => {
                end_health = Some(if error.diagnostic.code == "future_schema" {
                    "future_schema"
                } else {
                    "invalid"
                });
                diagnostics.push(error.diagnostic);
                None
            }
        };
        let mut pointer_health = None;
        let pointer = match read_record(
            &launch_dir.join("current-binding.json"),
            Some("current_binding"),
            &RecordIdentity::launch(&address, &launch_id),
        ) {
            Ok(value) => value,
            Err(error) => {
                pointer_health = Some(if error.diagnostic.code == "future_schema" {
                    "future_schema"
                } else {
                    "invalid"
                });
                diagnostics.push(error.diagnostic);
                None
            }
        };
        let claim_path = pane_dir.join("claim.json");
        let (claim, claim_health) = if let Some(cached) = claim_cache.get(&claim_path) {
            cached.clone()
        } else {
            let loaded =
                match read_record(&claim_path, Some("claim"), &RecordIdentity::pane(&address)) {
                    Ok(value) => (value, None),
                    Err(error) => {
                        let health = Some(if error.diagnostic.code == "future_schema" {
                            "future_schema".to_owned()
                        } else {
                            "invalid".to_owned()
                        });
                        diagnostics.push(error.diagnostic);
                        (None, health)
                    }
                };
            claim_cache.insert(claim_path.clone(), loaded.clone());
            loaded
        };
        let current = claim
            .as_ref()
            .and_then(|value| string(value, "launch_id"))
            .as_deref()
            == Some(&launch_id)
            && pointer
                .as_ref()
                .and_then(|value| string(value, "binding_id"))
                .as_deref()
                == Some(&binding_id);
        let binding_order = string(&binding, "observed_mono_ns").unwrap_or_default();
        let ended = end
            .as_ref()
            .and_then(|value| string(value, "observed_mono_ns"))
            .is_some_and(|order| order >= binding_order);
        let expected_session_match = string(&binding, "expected_session_id").map(|expected| {
            string(&binding, "provider_session_id").is_some_and(|actual| actual == expected)
        });
        let presence_key = (
            address.realm_id.clone(),
            address.incarnation_id.clone(),
            address.pane_id.clone(),
        );
        let presence = if let Some(cached) = presence_cache.get(&presence_key) {
            cached.clone()
        } else {
            let observed = pane_presence(root, &address, panes, processes, &mut diagnostics);
            presence_cache.insert(presence_key, observed.clone());
            observed
        };
        if typed && presence == "unavailable" && diagnostics.is_empty() {
            diagnostics.push(diagnostic(
                "probe_unavailable",
                "selected binding presence is unavailable",
            ));
        }
        let binding_health = end_health
            .map(str::to_owned)
            .or(claim_health)
            .or_else(|| pointer_health.map(str::to_owned))
            .unwrap_or_else(|| "valid".to_owned());
        rows.push(BindingRow {
            address,
            launch_id,
            binding_id,
            provider: string(&binding, "provider").unwrap_or_default(),
            provider_session_id: string(&binding, "provider_session_id").unwrap_or_default(),
            binding_phase: if ended { "ended" } else { "active" }.to_owned(),
            pane_presence: presence.clone(),
            reader_confidence: if current && presence == "present" {
                "confirmed"
            } else {
                "unconfirmed"
            }
            .to_owned(),
            binding_health,
            current,
            expected_session_match,
            expected_session_id: string(&binding, "expected_session_id"),
            transcript_path: string(&binding, "transcript_path"),
            cwd: string(&binding, "cwd"),
            config_dir: string(&binding, "config_dir"),
            model: string(&binding, "model"),
            start_source: string(&binding, "start_source"),
        });
    }
    let mut duplicates: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        duplicates
            .entry((row.provider.clone(), row.provider_session_id.clone()))
            .or_default()
            .push(index);
    }
    for indices in duplicates.values() {
        let addresses: BTreeSet<_> = indices
            .iter()
            .map(|index| {
                let address = &rows[*index].address;
                (&address.realm_id, &address.incarnation_id, &address.pane_id)
            })
            .collect();
        if addresses.len() > 1 {
            for index in indices {
                rows[*index].binding_health = "conflicted".to_owned();
            }
            diagnostics.push(diagnostic(
                "binding_conflict",
                "provider session is bound to multiple pane addresses",
            ));
        }
    }
    rows.sort_by(|left, right| {
        (
            &left.address.realm_id,
            &left.address.incarnation_id,
            &left.address.pane_id,
            &left.launch_id,
            &left.binding_id,
        )
            .cmp(&(
                &right.address.realm_id,
                &right.address.incarnation_id,
                &right.address.pane_id,
                &right.launch_id,
                &right.binding_id,
            ))
    });
    Ok((rows, diagnostics))
}
