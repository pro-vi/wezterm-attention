//! Claiming a launch at a pane's tty, and publishing that claim to the terminal.
//!
//! One module because the two are one operation seen from two sides: the claim
//! is what the state directory records, the publication is what the terminal is
//! told. The crate root re-exports this surface, so these names can move within
//! the module without moving for a caller.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{PaneAddress, pane_address};
use crate::protocol::{AttentionError, Diagnostic, Disposition, Result, manifest};
use crate::records::{
    CommitPlan, RecordIdentity, Replacement, commit, mkdir_private, pane_path, read_record,
    state_root,
};
use crate::wezterm::{RuntimePorts, publication_bytes};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplyResult {
    pub disposition: Disposition,
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

fn manifests(
    address: &PaneAddress,
    metadata: &crate::identity::SocketMetadata,
) -> Result<(Value, Value)> {
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

/// Claim the pane for a launch from the shell running in it, at stdin's tty.
///
/// The claiming terminal is checked against the pane before anything is
/// written; see [`confirm_pane_tty`].
pub fn claim_launch(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<ApplyResult> {
    let tty_path = ports.tty.current_path()?;
    confirm_pane_tty(env, ports, &tty_path)?;
    claim_launch_at_tty(env, ports, &tty_path)
}

/// Refuse a claim from a terminal that is not the pane's own.
///
/// `WEZTERM_PANE` and `WEZTERM_UNIX_SOCKET` are inherited, so tmux, screen or
/// an editor's terminal started inside a pane carries them while running on
/// a terminal of its own. A claim from there takes over the pane's claim, and
/// the agent that held it then has its events refused as stale. When the mux
/// lists the pane with a tty, that tty decides. When it does not list the
/// pane at all, the pane is not in that mux, so the variables are left over
/// from somewhere else. When no tty can be compared -- the listing failed, or
/// the row has none -- a claim inside tmux or screen is refused, since there
/// the variables are known to be inherited, and any other claim proceeds.
///
/// One listing at most, under the listing's own deadline.
fn confirm_pane_tty(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    tty_path: &str,
) -> Result<()> {
    let (Some(socket), Some(pane_id)) = (env.get("WEZTERM_UNIX_SOCKET"), env.get("WEZTERM_PANE"))
    else {
        // The claim fails on the missing identity itself, with its own reason.
        return Ok(());
    };
    let pane_tty = match ports.panes.list(socket) {
        Ok(rows) => match rows.into_iter().find(|row| row.pane_id == *pane_id) {
            Some(row) => row.tty_name,
            None => {
                return Err(AttentionError::new(
                    "unsafe_tty",
                    "the mux does not list this pane; WEZTERM_PANE was inherited from elsewhere",
                ));
            }
        },
        Err(_) => None,
    };
    match pane_tty {
        Some(pane_tty) if pane_tty == tty_path => Ok(()),
        Some(_) => Err(AttentionError::new(
            "unsafe_tty",
            "this terminal is not the pane's terminal; WEZTERM_PANE was inherited",
        )),
        None if ["TMUX", "STY"]
            .iter()
            .any(|name| env.get(*name).is_some_and(|value| !value.is_empty())) =>
        {
            Err(AttentionError::new(
                "unsafe_tty",
                "inside tmux or screen, and the pane's terminal could not be confirmed",
            ))
        }
        None => Ok(()),
    }
}

/// Claim the pane at `tty_path`, which the caller has already confirmed is
/// the pane's own terminal.
pub fn claim_launch_at_tty(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    tty_path: &str,
) -> Result<ApplyResult> {
    let root = state_root(env)?;
    mkdir_private(&root)?;
    let (address, metadata) = pane_address(env)?;
    let launch_id = match env.get("WEZTERM_ATTENTION_LAUNCH_ID") {
        Some(value) => crate::identity::canonical_uuid(Some(value), "WEZTERM_ATTENTION_LAUNCH_ID")?,
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
                        (Disposition::Confirmed, current, Vec::new())
                    } else {
                        let current_order = current
                            .get("observed_mono_ns")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                AttentionError::new("record_invalid", "claim order is invalid")
                            })?;
                        if observation.as_str() < current_order {
                            (Disposition::Ignored, current, Vec::new())
                        } else if observation == current_order {
                            return Err(AttentionError::new(
                                "record_invalid",
                                "equal claim order has different content",
                            ));
                        } else {
                            (
                                Disposition::Applied,
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
                    Disposition::Applied,
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
                    disposition,
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

    let mut selected = selected;
    // The claim is committed by this point, so every later failure is a
    // publication failure and belongs on the result the commit already selected.
    // Returning it as an error instead throws away the launch id of a claim that
    // exists on disk, and the diagnostic names only the publication, so a caller
    // cannot tell a claim that never happened from one that did. The terminal
    // write already reported this way; the incarnation re-check and the
    // publication encoding did not.
    match publish_claim(
        env,
        ports,
        &address,
        tty_path,
        &fingerprint,
        &selected.launch_id,
    ) {
        Ok(()) => selected.publication = "published".to_owned(),
        Err(error) => selected.publication_diagnostic = Some(error.diagnostic),
    }
    Ok(selected)
}

/// Publish a committed claim to the terminal, leaving the caller to decide what
/// a failure means for the claim it already wrote.
fn publish_claim(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    address: &PaneAddress,
    tty_path: &str,
    fingerprint: &str,
    launch_id: &str,
) -> Result<()> {
    let (current_address, _) = pane_address(env)?;
    if current_address != *address {
        return Err(AttentionError::new(
            "incarnation_changed",
            "mux socket changed before publication",
        ));
    }
    let bytes = publication_bytes(address, Some(launch_id))?;
    ports.tty.write(tty_path, &bytes, fingerprint)
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
    let (realm_id, incarnation_id, _) = crate::identity::socket_identity(socket_path)?;
    let rows = ports.panes.list(socket_path)?;
    let (current_realm, current_incarnation, _) = crate::identity::socket_identity(socket_path)?;
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
            let pane_id = crate::identity::canonical_pane_id(&row.pane_id)?;
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
            let (current_realm, current_incarnation, _) =
                crate::identity::socket_identity(socket_path)?;
            if current_realm != realm_id || current_incarnation != incarnation_id {
                return Err(AttentionError::new(
                    "incarnation_changed",
                    "mux socket changed before pane publication",
                ));
            }
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
