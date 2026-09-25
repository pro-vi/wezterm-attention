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
    CommitPlan, RecordIdentity, Replacement, commit_with, incarnation_path, mkdir_private,
    pane_path, read_record, realm_path, session_index_marker, session_index_path, state_root,
};
use crate::wezterm::{
    ControllingTerminal, ProcessFacts, ProcessInspector, ProcessRead, ProcessStart, RuntimePorts,
    TtyWriter, publication_bytes,
};

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
    // A store this claim starts holds no binding, so its session index is
    // complete from the first record, and every binding writer keeps it so.
    let new_store = !root.join("v2").exists();
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
    let realm_path = realm_path(&root, &address.realm_id);
    let incarnation_path = incarnation_path(&root, &address.realm_id, &address.incarnation_id);
    let reviews_path = pane.join("reviews");

    let (mut selected, published) = commit_with(
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
            let mut replacements = replacements;
            if new_store {
                replacements.push(Replacement::if_different(
                    session_index_path(&root),
                    session_index_marker()?,
                ));
            }
            Ok(CommitPlan {
                result: ApplyResult {
                    disposition,
                    launch_id: selected_launch,
                    publication: "pending".to_owned(),
                    publication_diagnostic: None,
                },
                replacements,
                removals: absence_probe_removal(&root, &pane),
                private_dirs: vec![reviews_path],
            })
        },
        // Published before the claim lock is released, so no later claim can
        // be published first and then covered by this one. The claim is
        // committed by this point, so a failure here is a publication failure
        // and belongs on the result the commit already selected: returned as
        // an error it would throw away the launch id of a claim that exists
        // on disk.
        |selected| {
            Ok(publish_claim(
                env,
                ports,
                &address,
                tty_path,
                &fingerprint,
                &selected.launch_id,
            ))
        },
    )?;
    match published {
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

/// Hold the pane's claim lock, when the pane has state, from reading its
/// claim through the whole terminal write, so a claim made meanwhile is
/// published after this publication and never covered by it.
///
/// A pane with no state directory has no claim and no lock to take, and this
/// creates neither: publishing to every listed pane must not leave a
/// directory for each. Such a publication names the pane and no launch, and
/// the terminal keeps the launch a claim publishes whatever order the two
/// land in, since naming the pane again changes nothing else.
fn with_pane_claim<T>(
    root: &std::path::Path,
    address: &PaneAddress,
    publish: impl FnOnce(Option<Value>) -> Result<T>,
) -> Result<T> {
    let pane = pane_path(root, address);
    let read = || {
        read_record(
            &pane.join("claim.json"),
            Some("claim"),
            &RecordIdentity::pane(address),
        )
    };
    if !pane.is_dir() {
        return publish(None);
    }
    crate::records::with_lock(
        &pane.join(".claim.lock"),
        std::time::Duration::from_secs(2),
        || publish(read()?),
    )
}

/// The pane's absence probe, which a new claim makes stale, when removing it
/// cannot reach outside the state root. A pane directory reached through a
/// link keeps whatever is there; the claim goes ahead either way.
fn absence_probe_removal(
    root: &std::path::Path,
    pane: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let probe = pane.join("absence-probe.json");
    if crate::maintenance::removal_confined(root, &probe) {
        vec![probe]
    } else {
        Vec::new()
    }
}

/// The launch a stored claim publishes to `tty_path`: its own, when it names
/// this pane at that terminal.
fn claimed_launch(
    claim: Option<&Value>,
    address: &PaneAddress,
    tty_path: &str,
    fingerprint: &str,
) -> Option<String> {
    let record = claim?;
    let matches = record.get("address") == serde_json::to_value(address).ok().as_ref()
        && record.get("tty_path").and_then(Value::as_str) == Some(tty_path)
        && record.get("tty_fingerprint").and_then(Value::as_str) == Some(fingerprint);
    matches
        .then(|| record.get("launch_id").and_then(Value::as_str))
        .flatten()
        .map(str::to_owned)
}

pub fn publish_current(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<PublishReport> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let tty_path = ports.tty.current_path()?;
    let fingerprint = ports.tty.fingerprint(&tty_path)?;
    let launch_id = with_pane_claim(&root, &address, |claim| {
        let launch_id = claimed_launch(claim.as_ref(), &address, &tty_path, &fingerprint);
        let (current_address, _) = pane_address(env)?;
        if current_address != address {
            return Err(AttentionError::new(
                "incarnation_changed",
                "mux socket changed before publication",
            ));
        }
        ports.tty.write(
            &tty_path,
            &publication_bytes(&address, launch_id.as_deref())?,
            &fingerprint,
        )?;
        Ok(launch_id)
    })?;
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
            with_pane_claim(&root, &address, |claim| {
                let launch_id = claimed_launch(claim.as_ref(), &address, tty_name, &fingerprint);
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
                    &publication_bytes(&address, launch_id.as_deref())?,
                    &fingerprint,
                )?;
                Ok(launch_id.is_some())
            })
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

/// The process a self-owned claim belongs to: one process lifetime on one
/// boot. A pid alone is not one, since the kernel reuses pids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClaimOwner {
    pub(crate) pid: i32,
    pub(crate) start: ProcessStart,
    pub(crate) boot_session_id: String,
}

/// Which kind of claim a pane holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClaimMode {
    /// Made by a shell for the commands it starts, which inherit its launch id.
    Shell,
    /// Made by an agent's own hook, for that agent's process.
    SelfOwned(ClaimOwner),
}

impl ClaimMode {
    /// The mode of a claim record that has already been validated as one.
    pub(crate) fn of(claim: &Value) -> Result<Self> {
        let text = |field: &str| claim.get(field).and_then(Value::as_str);
        let invalid = || AttentionError::new("record_invalid", "claim owner is invalid");
        match (
            text("owner_pid"),
            text("owner_started_sec"),
            text("owner_started_usec"),
            text("owner_boot_session_id"),
        ) {
            (None, None, None, None) => Ok(Self::Shell),
            (Some(pid), Some(seconds), Some(microseconds), Some(boot)) => {
                Ok(Self::SelfOwned(ClaimOwner {
                    pid: pid.parse().map_err(|_| invalid())?,
                    start: ProcessStart {
                        seconds: seconds.parse().map_err(|_| invalid())?,
                        microseconds: microseconds.parse().map_err(|_| invalid())?,
                    },
                    boot_session_id: boot.to_owned(),
                }))
            }
            _ => Err(invalid()),
        }
    }
}

fn parent_unverified(message: &str) -> AttentionError {
    AttentionError::new("self_claim_parent_unverified", message)
}

/// The pid `WEZTERM_ATTENTION_HOST_PID` names: decimal digits with no sign or
/// leading zero, naming a positive pid.
fn asserted_host(env: &BTreeMap<String, String>) -> Result<i32> {
    let value = env.get("WEZTERM_ATTENTION_HOST_PID").ok_or_else(|| {
        parent_unverified(
            "WEZTERM_ATTENTION_HOST_PID is missing; register the hook as \
             `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event ...`",
        )
    })?;
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(parent_unverified("WEZTERM_ATTENTION_HOST_PID is not a pid"));
    }
    value
        .parse::<i32>()
        .map_err(|_| parent_unverified("WEZTERM_ATTENTION_HOST_PID is not a pid"))
}

/// What one reading of the process table says about the agent a hook runs
/// under.
#[derive(Clone, Debug, Eq, PartialEq)]
struct HostReading {
    parent_pid: i32,
    parent_start: ProcessStart,
    /// The device of the terminal the hook or, when the hook has none, its
    /// parent runs on.
    terminal: u64,
    /// Whether the parent leads the terminal's foreground job.
    foreground: bool,
}

fn found(read: ProcessRead, whose: &str) -> Result<ProcessFacts> {
    match read {
        ProcessRead::Found(facts) => Ok(facts),
        ProcessRead::Gone => Err(parent_unverified(&format!("{whose} has exited"))),
        ProcessRead::Unknown => Err(parent_unverified(&format!(
            "{whose} could not be read from the kernel"
        ))),
    }
}

/// Read the hook and its direct parent, and require that parent to be the
/// process `expected_parent` names, to be this user's, and to be alive.
///
/// The hook's own terminal decides where it runs. Only when the hook has
/// none -- a hook its agent started in a session of its own -- does the
/// parent's decide, and nothing further up is consulted.
fn read_host(processes: &dyn ProcessInspector, expected_parent: i32) -> Result<HostReading> {
    let hook = found(processes.process(processes.own_pid()), "this hook")?;
    if hook.traced {
        return Err(parent_unverified("a debugger is attached to this hook"));
    }
    if hook.parent_pid == 1 {
        return Err(parent_unverified(
            "this hook's parent exited before it could be checked",
        ));
    }
    if hook.parent_pid != expected_parent {
        return Err(parent_unverified(
            "this hook's parent is not the process WEZTERM_ATTENTION_HOST_PID names; \
             it was run by a relay or a shell that stayed, not by the agent itself",
        ));
    }
    let parent = found(processes.process(hook.parent_pid), "this hook's parent")?;
    if parent.zombie {
        return Err(parent_unverified("this hook's parent has exited"));
    }
    if parent.uid != hook.uid {
        return Err(parent_unverified(
            "this hook's parent belongs to another user",
        ));
    }
    let unknown = || {
        AttentionError::new(
            "probe_unavailable",
            "the agent's controlling terminal could not be read",
        )
    };
    let (terminal, foreground_group) = match hook.terminal {
        ControllingTerminal::Device(device) => (device, hook.terminal_foreground_group),
        ControllingTerminal::Unknown => return Err(unknown()),
        ControllingTerminal::Absent => match parent.terminal {
            ControllingTerminal::Device(device) => (device, parent.terminal_foreground_group),
            ControllingTerminal::Unknown => return Err(unknown()),
            ControllingTerminal::Absent => {
                return Err(AttentionError::new(
                    "unsafe_tty",
                    "neither this hook nor its agent has a controlling terminal",
                ));
            }
        },
    };
    Ok(HostReading {
        parent_pid: hook.parent_pid,
        parent_start: parent.start,
        terminal,
        foreground: parent.process_group == foreground_group,
    })
}

/// What an agent's hook proved about where it runs: the process that owns
/// it, and the pane terminal that process runs on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostProof {
    pub(crate) owner: ClaimOwner,
    pub(crate) tty_path: String,
    pub(crate) tty_fingerprint: String,
    terminal: u64,
    pub(crate) foreground: bool,
}

impl HostProof {
    /// Prove which agent process started this hook and that it runs on the
    /// terminal the mux lists for `WEZTERM_PANE`.
    ///
    /// The parent must be the process `WEZTERM_ATTENTION_HOST_PID` names:
    /// the agent's hook entry sets it once, from its own `$PPID`, and execs
    /// this command, so a relay that forwards the value or a shell that stays
    /// behind leaves a different parent, and one that drops it leaves none.
    /// The listing has to succeed and name the pane exactly once, with a
    /// terminal whose device is the agent's. The parent is read again at the
    /// end, and one that changed in between proves nothing.
    pub(crate) fn establish(
        env: &BTreeMap<String, String>,
        ports: &RuntimePorts<'_>,
        address: &PaneAddress,
    ) -> Result<Self> {
        let asserted = asserted_host(env)?;
        let reading = read_host(ports.processes, asserted)?;
        let boot_session_id = ports.processes.boot_session().ok_or_else(|| {
            AttentionError::new("probe_unavailable", "the boot session id could not be read")
        })?;
        let socket = env.get("WEZTERM_UNIX_SOCKET").ok_or_else(|| {
            AttentionError::new("identity_unpublished", "WEZTERM_UNIX_SOCKET is missing")
        })?;
        let rows = ports.panes.list(socket)?;
        let mut listed = rows.iter().filter(|row| row.pane_id == address.pane_id);
        let (Some(row), None) = (listed.next(), listed.next()) else {
            return Err(AttentionError::new(
                "unsafe_tty",
                "the mux does not list this pane exactly once",
            ));
        };
        let tty_path = row.tty_name.clone().ok_or_else(|| {
            AttentionError::new("unsafe_tty", "the mux lists no terminal for this pane")
        })?;
        if ports.processes.terminal_device(&tty_path) != Some(reading.terminal) {
            return Err(AttentionError::new(
                "unsafe_tty",
                "the agent runs on a terminal that is not the pane's",
            ));
        }
        let tty_fingerprint = ports.tty.fingerprint(&tty_path)?;
        let (current, _) = pane_address(env)?;
        if current != *address {
            return Err(AttentionError::new(
                "incarnation_changed",
                "mux socket changed while the agent was being checked",
            ));
        }
        let proof = Self {
            owner: ClaimOwner {
                pid: reading.parent_pid,
                start: reading.parent_start,
                boot_session_id,
            },
            tty_path,
            tty_fingerprint,
            terminal: reading.terminal,
            foreground: reading.foreground,
        };
        proof.confirm(env, ports.processes, ports.tty, address)?;
        Ok(proof)
    }

    /// Read the same host again and return whether it still leads the
    /// terminal's foreground job. The same parent, started at the same time on
    /// the same boot, on the same terminal at the same path, under the same
    /// socket; any difference refuses, and no other process is ever put in
    /// the parent's place.
    pub(crate) fn confirm(
        &self,
        env: &BTreeMap<String, String>,
        processes: &dyn ProcessInspector,
        tty: &dyn TtyWriter,
        address: &PaneAddress,
    ) -> Result<bool> {
        let reading = read_host(processes, self.owner.pid)?;
        if reading.parent_start != self.owner.start
            || processes.boot_session().as_deref() != Some(self.owner.boot_session_id.as_str())
        {
            return Err(parent_unverified(
                "this hook's parent changed while it was being checked",
            ));
        }
        if reading.terminal != self.terminal
            || processes.terminal_device(&self.tty_path) != Some(self.terminal)
            || tty.fingerprint(&self.tty_path)? != self.tty_fingerprint
        {
            return Err(AttentionError::new(
                "unsafe_tty",
                "the agent's terminal changed while it was being checked",
            ));
        }
        let (current, _) = pane_address(env)?;
        if current != *address {
            return Err(AttentionError::new(
                "incarnation_changed",
                "mux socket changed while the agent was being checked",
            ));
        }
        Ok(reading.foreground)
    }

    /// Whether `claim` is this host's own: self-owned by the same process, at
    /// the same terminal.
    pub(crate) fn owns(&self, claim: &Value) -> Result<bool> {
        Ok(
            ClaimMode::of(claim)? == ClaimMode::SelfOwned(self.owner.clone())
                && claim.get("tty_path").and_then(Value::as_str) == Some(self.tty_path.as_str())
                && claim.get("tty_fingerprint").and_then(Value::as_str)
                    == Some(self.tty_fingerprint.as_str()),
        )
    }
}

/// A launch resolved for an agent event that carries no launch id: the claim
/// it resolved against, the proof of the host that sent it, and why a claim
/// this event made could not be published to the terminal.
pub(crate) struct SelfOwnedLaunch {
    pub(crate) claim: Value,
    pub(crate) proof: HostProof,
    pub(crate) publication_diagnostic: Option<Diagnostic>,
}

/// Whether the process a self-owned claim names still runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerState {
    Alive,
    /// Proven to have exited: the machine rebooted since, or its pid names no
    /// process, or names one that started at another time.
    Gone,
    /// Neither could be shown. A zombie counts here: it has exited but its
    /// pid is still held, and nothing else it proves is needed.
    Unknown,
}

fn owner_state(owner: &ClaimOwner, processes: &dyn ProcessInspector) -> OwnerState {
    let Some(boot) = processes.boot_session() else {
        return OwnerState::Unknown;
    };
    if boot != owner.boot_session_id {
        return OwnerState::Gone;
    }
    match processes.process(owner.pid) {
        ProcessRead::Gone => OwnerState::Gone,
        ProcessRead::Unknown => OwnerState::Unknown,
        ProcessRead::Found(facts) if facts.zombie => OwnerState::Unknown,
        ProcessRead::Found(facts) if facts.start != owner.start => OwnerState::Gone,
        ProcessRead::Found(_) => OwnerState::Alive,
    }
}

fn self_owned_claim_record(
    address: &PaneAddress,
    launch_id: &str,
    proof: &HostProof,
    observation: &str,
) -> Value {
    let mut record = claim_record(
        address,
        launch_id,
        &proof.tty_path,
        &proof.tty_fingerprint,
        observation,
    );
    record["owner_pid"] = json!(proof.owner.pid.to_string());
    record["owner_started_sec"] = json!(proof.owner.start.seconds.to_string());
    record["owner_started_usec"] = json!(proof.owner.start.microseconds.to_string());
    record["owner_boot_session_id"] = json!(proof.owner.boot_session_id);
    record
}

/// Claim the pane for the agent process `proof` names, at a session start.
///
/// Under the pane's claim lock, and no other lock: the same host is read
/// again and must still prove itself, then the claim is decided. No claim
/// gives a new one with a new launch id. The same process's own claim is kept
/// as it stands, so a callback already resolved against it stays valid. A
/// claim of another process proven gone is replaced with a new launch id;
/// one still running, or one whose state cannot be read, keeps the pane, and
/// so does a shell claim. Only a new or replacing claim needs the agent to
/// lead the terminal's foreground job. The claim is published to the proven
/// terminal before the lock is released.
fn claim_for_host(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    address: &PaneAddress,
    proof: &HostProof,
) -> Result<(Value, Option<Diagnostic>)> {
    let root = state_root(env)?;
    mkdir_private(&root)?;
    let new_store = !root.join("v2").exists();
    let (_, metadata) = pane_address(env)?;
    let (realm_record, incarnation_record) = manifests(address, &metadata)?;
    let pane = pane_path(&root, address);
    let claim_path = pane.join("claim.json");
    let observation = ports.clock.monotonic_ns20()?;
    let (selected, published) = commit_with(
        &pane.join(".claim.lock"),
        &claim_path,
        Some("claim"),
        &RecordIdentity::pane(address),
        std::time::Duration::from_secs(2),
        |existing| {
            let foreground = proof.confirm(env, ports.processes, ports.tty, address)?;
            let kept = |claim: Value| CommitPlan {
                result: claim,
                replacements: Vec::new(),
                removals: Vec::new(),
                private_dirs: Vec::new(),
            };
            if let Some(current) = &existing {
                if current.get("address")
                    != Some(&serde_json::to_value(address).map_err(AttentionError::record_json)?)
                {
                    return Err(AttentionError::new(
                        "record_invalid",
                        "claim interior address mismatches its path",
                    ));
                }
                match ClaimMode::of(current)? {
                    ClaimMode::Shell => {
                        return Err(AttentionError::new(
                            "claim_stale",
                            "the pane holds a shell claim, which an agent without its launch id cannot use",
                        ));
                    }
                    ClaimMode::SelfOwned(owner) if owner == proof.owner => {
                        if !proof.owns(current)? {
                            return Err(AttentionError::new(
                                "unsafe_tty",
                                "the agent's claim names a terminal the agent no longer runs on",
                            ));
                        }
                        return Ok(kept(current.clone()));
                    }
                    ClaimMode::SelfOwned(owner) => match owner_state(&owner, ports.processes) {
                        OwnerState::Gone => {}
                        OwnerState::Alive => {
                            return Err(AttentionError::new(
                                "claim_stale",
                                "another agent process that still runs holds the pane's claim",
                            ));
                        }
                        OwnerState::Unknown => {
                            return Err(AttentionError::new(
                                "probe_unavailable",
                                "whether the agent holding the pane's claim still runs could not be read",
                            ));
                        }
                    },
                }
            }
            if !foreground {
                return Err(AttentionError::new(
                    "claim_stale",
                    "the agent does not lead the pane's foreground job, so it cannot claim the pane",
                ));
            }
            let claim =
                self_owned_claim_record(address, &Uuid::new_v4().to_string(), proof, &observation);
            let mut replacements = vec![
                Replacement::if_different(
                    realm_path(&root, &address.realm_id).join("realm.json"),
                    realm_record.clone(),
                ),
                Replacement::if_different(
                    incarnation_path(&root, &address.realm_id, &address.incarnation_id)
                        .join("incarnation.json"),
                    incarnation_record.clone(),
                ),
                Replacement::always(claim_path.clone(), claim.clone()),
            ];
            if new_store {
                replacements.push(Replacement::if_different(
                    session_index_path(&root),
                    session_index_marker()?,
                ));
            }
            Ok(CommitPlan {
                result: claim,
                replacements,
                removals: absence_probe_removal(&root, &pane),
                private_dirs: vec![pane.join("reviews")],
            })
        },
        |claim| {
            let launch_id = claim
                .get("launch_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(publish_claim(
                env,
                ports,
                address,
                &proof.tty_path,
                &proof.tty_fingerprint,
                launch_id,
            )
            .err()
            .map(|error| error.diagnostic))
        },
    )?;
    Ok((selected, published))
}

/// Resolve an agent event that carries no launch id against a claim its own
/// agent process holds.
///
/// Self-claim is on unless `WEZTERM_ATTENTION_ENABLE_SELF_CLAIM` says
/// otherwise, and only where the platform supports it. A pane holding a
/// shell claim refuses every such event: that claim belongs to the commands
/// its shell starts, which carry its launch id.
pub(crate) fn self_owned_launch(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    address: &PaneAddress,
    claim: Option<Value>,
    starts_session: bool,
) -> Result<SelfOwnedLaunch> {
    if !ports.processes.self_claim_supported() {
        return Err(AttentionError::new(
            "claim_stale",
            "an agent cannot claim its own pane on this platform; start it from a claiming shell",
        ));
    }
    match env
        .get("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM")
        .map(String::as_str)
    {
        None | Some("1") => {}
        Some(_) => {
            return Err(AttentionError::new(
                "claim_stale",
                "an agent claiming its own pane is switched off by WEZTERM_ATTENTION_ENABLE_SELF_CLAIM",
            ));
        }
    }
    asserted_host(env)?;
    match &claim {
        Some(claim) if ClaimMode::of(claim)? == ClaimMode::Shell => {
            return Err(AttentionError::new(
                "claim_stale",
                "the pane holds a shell claim, which an agent without its launch id cannot use",
            ));
        }
        // Nothing but a session start can make a claim, so there is nothing
        // to prove this against, and no reason to ask the mux.
        None if !starts_session => {
            return Err(AttentionError::new(
                "claim_stale",
                "provider event has no matching pane claim",
            ));
        }
        _ => {}
    }
    let proof = HostProof::establish(env, ports, address)?;
    if starts_session {
        let (claim, publication_diagnostic) = claim_for_host(env, ports, address, &proof)?;
        return Ok(SelfOwnedLaunch {
            claim,
            proof,
            publication_diagnostic,
        });
    }
    let Some(claim) = claim else {
        return Err(AttentionError::new(
            "claim_stale",
            "provider event has no matching pane claim",
        ));
    };
    if !proof.owns(&claim)? {
        return Err(AttentionError::new(
            "claim_stale",
            "the pane's claim belongs to another agent process",
        ));
    }
    Ok(SelfOwnedLaunch {
        claim,
        proof,
        publication_diagnostic: None,
    })
}
