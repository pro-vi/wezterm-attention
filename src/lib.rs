pub mod compat;
pub mod identity;
pub mod lifecycle;
pub mod maintenance;
pub mod protocol;
pub mod providers;
pub mod query;
pub mod records;
pub mod wezterm;

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{PaneAddress, pane_address};
use crate::protocol::{AttentionError, Diagnostic, Result, manifest};
use crate::records::{
    CommitPlan, RecordIdentity, Replacement, commit, mkdir_private, pane_path, read_record,
    state_root,
};
use crate::wezterm::{RuntimePorts, publication_bytes};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplyResult {
    pub disposition: String,
    pub launch_id: String,
    pub publication: String,
    #[serde(skip)]
    pub publication_diagnostic: Option<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublishReport {
    pub attempted: usize,
    pub published: usize,
    pub v2_published: usize,
    pub skipped: usize,
    pub diagnostics: Vec<Diagnostic>,
}

fn claim_record(
    address: &PaneAddress,
    launch_id: &str,
    tty_path: &str,
    tty_fingerprint: &str,
    observation: &str,
) -> Value {
    json!({
        "kind": "claim",
        "schema": manifest().expect("embedded manifest must load").record_schema,
        "address": address,
        "launch_id": launch_id,
        "tty_path": tty_path,
        "tty_fingerprint": tty_fingerprint,
        "observed_mono_ns": observation,
    })
}

fn manifests(address: &PaneAddress, metadata: &identity::SocketMetadata) -> Result<(Value, Value)> {
    let protocol = manifest()?;
    Ok((
        json!({
            "kind": "realm",
            "schema": protocol.record_schema,
            "realm_id": address.realm_id,
            "socket_path": metadata.socket_path,
            "writer_version": protocol.writer_version,
        }),
        json!({
            "kind": "incarnation",
            "schema": protocol.record_schema,
            "realm_id": address.realm_id,
            "incarnation_id": address.incarnation_id,
            "socket_path": metadata.socket_path,
            "socket_device": metadata.socket_device,
            "socket_inode": metadata.socket_inode,
            "socket_ctime_ns": metadata.socket_ctime_ns,
            "writer_version": protocol.writer_version,
        }),
    ))
}

pub fn claim_launch(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<ApplyResult> {
    let tty_path = ports.tty.current_path()?;
    claim_launch_at_tty(env, ports, &tty_path)
}

pub fn claim_launch_at_tty(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    tty_path: &str,
) -> Result<ApplyResult> {
    let root = state_root(env)?;
    mkdir_private(&root)?;
    let (address, metadata) = pane_address(env)?;
    let launch_id = match env.get("WEZTERM_ATTENTION_LAUNCH_ID") {
        Some(value) => identity::canonical_uuid(Some(value), "WEZTERM_ATTENTION_LAUNCH_ID")?,
        None => Uuid::new_v4().to_string(),
    };
    let fingerprint = ports.tty.fingerprint(tty_path)?;
    let observation = ports.clock.monotonic_ns20()?;
    let pane = pane_path(&root, &address);
    let claim_path = pane.join("claim.json");
    let proposed = claim_record(&address, &launch_id, tty_path, &fingerprint, &observation);
    let (realm_record, incarnation_record) = manifests(&address, &metadata)?;
    let realm_path = root.join("v2/realms").join(&address.realm_id);
    let incarnation_path = realm_path
        .join("incarnations")
        .join(&address.incarnation_id);
    let reviews_path = pane.join("reviews");

    let selected = commit(
        &pane.join(".claim.lock"),
        &claim_path,
        Some("claim"),
        &RecordIdentity::pane(&address),
        std::time::Duration::from_secs(2),
        |existing| {
            let (locked_address, _) = pane_address(env)?;
            if locked_address != address {
                return Err(AttentionError::new(
                    "incarnation_changed",
                    "mux socket changed before claim commit",
                ));
            }
            let (disposition, selected, replacements) = match existing {
                Some(current) => {
                    if current.get("address")
                        != Some(
                            &serde_json::to_value(&address).map_err(AttentionError::record_json)?,
                        )
                    {
                        return Err(AttentionError::new(
                            "record_invalid",
                            "claim interior address mismatches its path",
                        ));
                    }
                    let same_identity = [
                        "kind",
                        "schema",
                        "address",
                        "launch_id",
                        "tty_path",
                        "tty_fingerprint",
                    ]
                    .iter()
                    .all(|field| current.get(*field) == proposed.get(*field));
                    if same_identity {
                        ("confirmed", current, Vec::new())
                    } else {
                        let current_order = current
                            .get("observed_mono_ns")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                AttentionError::new("record_invalid", "claim order is invalid")
                            })?;
                        if observation.as_str() < current_order {
                            ("ignored", current, Vec::new())
                        } else if observation == current_order {
                            return Err(AttentionError::new(
                                "record_invalid",
                                "equal claim order has different content",
                            ));
                        } else {
                            (
                                "applied",
                                proposed.clone(),
                                vec![
                                    Replacement::if_different(
                                        realm_path.join("realm.json"),
                                        realm_record.clone(),
                                    ),
                                    Replacement::if_different(
                                        incarnation_path.join("incarnation.json"),
                                        incarnation_record.clone(),
                                    ),
                                    Replacement::always(claim_path.clone(), proposed.clone()),
                                ],
                            )
                        }
                    }
                }
                None => (
                    "applied",
                    proposed.clone(),
                    vec![
                        Replacement::if_different(realm_path.join("realm.json"), realm_record),
                        Replacement::if_different(
                            incarnation_path.join("incarnation.json"),
                            incarnation_record,
                        ),
                        Replacement::always(claim_path.clone(), proposed),
                    ],
                ),
            };
            let selected_launch = selected
                .get("launch_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AttentionError::new("record_invalid", "selected claim has no launch id")
                })?
                .to_owned();
            Ok(CommitPlan {
                result: ApplyResult {
                    disposition: disposition.to_owned(),
                    launch_id: selected_launch,
                    publication: "pending".to_owned(),
                    publication_diagnostic: None,
                },
                replacements,
                removals: vec![pane.join("absence-probe.json")],
                private_dirs: vec![reviews_path],
            })
        },
    )?;

    let (current_address, _) = pane_address(env)?;
    if current_address != address {
        return Err(AttentionError::new(
            "incarnation_changed",
            "mux socket changed before publication",
        ));
    }
    let bytes = publication_bytes(&address, Some(&selected.launch_id))?;
    let mut selected = selected;
    match ports.tty.write(tty_path, &bytes, &fingerprint) {
        Ok(()) => selected.publication = "published".to_owned(),
        Err(error) => selected.publication_diagnostic = Some(error.diagnostic),
    }
    Ok(selected)
}

pub fn publish_current(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<PublishReport> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let tty_path = ports.tty.current_path()?;
    let fingerprint = ports.tty.fingerprint(&tty_path)?;
    let claim = read_record(
        &pane_path(&root, &address).join("claim.json"),
        Some("claim"),
        &RecordIdentity::pane(&address),
    )?;
    let launch_id = claim.as_ref().and_then(|record| {
        let matches = record.get("address") == serde_json::to_value(&address).ok().as_ref()
            && record.get("tty_path").and_then(Value::as_str) == Some(tty_path.as_str())
            && record.get("tty_fingerprint").and_then(Value::as_str) == Some(fingerprint.as_str());
        matches
            .then(|| record.get("launch_id").and_then(Value::as_str))
            .flatten()
    });
    let (current_address, _) = pane_address(env)?;
    if current_address != address {
        return Err(AttentionError::new(
            "incarnation_changed",
            "mux socket changed before publication",
        ));
    }
    ports.tty.write(
        &tty_path,
        &publication_bytes(&address, launch_id)?,
        &fingerprint,
    )?;
    Ok(PublishReport {
        attempted: 1,
        published: 1,
        v2_published: usize::from(launch_id.is_some()),
        skipped: 0,
        diagnostics: Vec::new(),
    })
}

pub fn publish_realm(
    socket_path: &str,
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<PublishReport> {
    let root = state_root(env)?;
    let (realm_id, incarnation_id, _) = identity::socket_identity(socket_path)?;
    let rows = ports.panes.list(socket_path)?;
    let (current_realm, current_incarnation, _) = identity::socket_identity(socket_path)?;
    if (current_realm, current_incarnation) != (realm_id.clone(), incarnation_id.clone()) {
        return Err(AttentionError::new(
            "incarnation_changed",
            "mux socket changed during enumeration",
        ));
    }
    let mut report = PublishReport {
        attempted: rows.len(),
        published: 0,
        v2_published: 0,
        skipped: 0,
        diagnostics: Vec::new(),
    };
    for row in rows {
        let result = (|| {
            let pane_id = identity::canonical_pane_id(&row.pane_id)?;
            let tty_name = row
                .tty_name
                .as_deref()
                .ok_or_else(|| AttentionError::new("unsafe_tty", "pane has no publishable tty"))?;
            let fingerprint = ports.tty.fingerprint(tty_name)?;
            let address = PaneAddress {
                realm_id: realm_id.clone(),
                incarnation_id: incarnation_id.clone(),
                pane_id,
            };
            let claim = read_record(
                &pane_path(&root, &address).join("claim.json"),
                Some("claim"),
                &RecordIdentity::pane(&address),
            )?;
            let launch_id = claim.as_ref().and_then(|record| {
                let matches = record.get("address") == serde_json::to_value(&address).ok().as_ref()
                    && record.get("tty_path").and_then(Value::as_str) == Some(tty_name)
                    && record.get("tty_fingerprint").and_then(Value::as_str)
                        == Some(fingerprint.as_str());
                matches
                    .then(|| record.get("launch_id").and_then(Value::as_str))
                    .flatten()
            });
            ports.tty.write(
                tty_name,
                &publication_bytes(&address, launch_id)?,
                &fingerprint,
            )?;
            Ok::<bool, AttentionError>(launch_id.is_some())
        })();
        match result {
            Ok(has_v2) => {
                report.published += 1;
                report.v2_published += usize::from(has_v2);
            }
            Err(error) => report.diagnostics.push(error.diagnostic),
        }
    }
    report.skipped = report.attempted - report.published;
    Ok(report)
}

pub fn environment() -> BTreeMap<String, String> {
    env::vars().collect()
}

pub fn state_root_from_environment() -> Result<PathBuf> {
    state_root(&environment())
}
