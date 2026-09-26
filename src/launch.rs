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

use crate::identity::{PaneAddress, pane_address, pane_socket};
use crate::protocol::{AttentionError, Diagnostic, Disposition, Result, manifest};
use crate::records::{
    CommitPlan, LOCK_TIMEOUT, RecordIdentity, Replacement, claim_lock, commit, mkdir_private,
    pane_dir, read_claim, reviews_dir, session_index_marker, session_index_path, state_root,
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

/// What writing a pane's claim keeps in step with it: the realm and
/// incarnation manifests, the session index of a store the claim starts, the
/// pane's reviews directory, and no absence probe left from before.
struct ClaimWrite {
    root: std::path::PathBuf,
    address: PaneAddress,
    realm_record: Value,
    incarnation_record: Value,
    /// A store this claim starts holds no binding, so its session index is
    /// complete from the first record, and every binding writer keeps it so.
    new_store: bool,
}

impl ClaimWrite {
    fn new(
        root: &std::path::Path,
        address: &PaneAddress,
        metadata: &crate::identity::SocketMetadata,
    ) -> Result<Self> {
        let (realm_record, incarnation_record) = manifests(address, metadata)?;
        Ok(Self {
            root: root.to_path_buf(),
            address: address.clone(),
            realm_record,
            incarnation_record,
            new_store: !root.join("v2").exists(),
        })
    }

    fn lock_path(&self) -> std::path::PathBuf {
        claim_lock(&self.root, &self.address)
    }

    fn identity(&self) -> RecordIdentity {
        RecordIdentity::pane(&self.address)
    }

    /// Refuse a stored claim whose interior address is not this pane's.
    fn check_address(&self, current: &Value) -> Result<()> {
        if current.get("address")
            != Some(&serde_json::to_value(&self.address).map_err(AttentionError::record_json)?)
        {
            return Err(AttentionError::new(
                "record_invalid",
                "claim interior address mismatches its path",
            ));
        }
        Ok(())
    }

    /// The plan that writes `claim`, or with `None` keeps the stored one, and
    /// reports `result`.
    fn plan<T>(&self, result: T, claim: Option<&Value>) -> Result<CommitPlan<T>> {
        let (realm_id, incarnation_id) = (&self.address.realm_id, &self.address.incarnation_id);
        let mut replacements = match claim {
            Some(claim) => vec![
                Replacement::if_different(
                    RecordIdentity::realm(realm_id).path(&self.root, "realm")?,
                    self.realm_record.clone(),
                ),
                Replacement::if_different(
                    RecordIdentity::incarnation(realm_id, incarnation_id)
                        .path(&self.root, "incarnation")?,
                    self.incarnation_record.clone(),
                ),
                Replacement::always(self.identity().path(&self.root, "claim")?, claim.clone()),
            ],
            None => Vec::new(),
        };
        if self.new_store {
            replacements.push(Replacement::if_different(
                session_index_path(&self.root),
                session_index_marker()?,
            ));
        }
        Ok(CommitPlan {
            result,
            replacements,
            removals: vec![self.identity().path(&self.root, "absence_probe")?],
            private_dirs: vec![reviews_dir(&self.root, &self.address)],
        })
    }
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
    let proposed = claim_record(&address, &launch_id, tty_path, &fingerprint, &observation);
    let write = ClaimWrite::new(&root, &address, &metadata)?;

    let (mut selected, published) = commit(
        &root,
        &[&write.lock_path()],
        "claim",
        &write.identity(),
        |existing| {
            same_incarnation(
                pane_socket(env)?,
                &address.realm_id,
                &address.incarnation_id,
                "before claim commit",
            )?;
            let (disposition, selected, writes) = match existing {
                Some(current) => {
                    write.check_address(&current)?;
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
                        (Disposition::Confirmed, current, false)
                    } else {
                        let current_order = current
                            .get("observed_mono_ns")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                AttentionError::new("record_invalid", "claim order is invalid")
                            })?;
                        if observation.as_str() < current_order {
                            (Disposition::Ignored, current, false)
                        } else if observation == current_order {
                            return Err(AttentionError::new(
                                "record_invalid",
                                "equal claim order has different content",
                            ));
                        } else {
                            (Disposition::Applied, proposed.clone(), true)
                        }
                    }
                }
                None => (Disposition::Applied, proposed.clone(), true),
            };
            // The shell exports the launch id this returns to every command it
            // starts, and an agent's own claim belongs to that agent's process
            // alone, so a kept claim has to be a shell's.
            if ClaimMode::of(&selected)? != ClaimMode::Shell {
                return Err(AttentionError::new(
                    "claim_stale",
                    "the pane holds an agent's own claim, which a shell cannot share",
                ));
            }
            write.plan(
                ApplyResult {
                    disposition,
                    launch_id: claim_launch_id(&selected)?,
                    publication: "pending".to_owned(),
                    publication_diagnostic: None,
                },
                writes.then_some(&selected),
            )
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

/// Refuse to go on once the mux socket at `socket_path` no longer carries the
/// incarnation `realm_id` and `incarnation_id` name: a server that took the
/// socket's place is another server, and what was checked or decided for the
/// old one says nothing of it. `moment` says when the change was found.
fn same_incarnation(
    socket_path: &str,
    realm_id: &str,
    incarnation_id: &str,
    moment: &str,
) -> Result<()> {
    let (current_realm, current_incarnation, _) = crate::identity::socket_identity(socket_path)?;
    if current_realm != realm_id || current_incarnation != incarnation_id {
        return Err(AttentionError::new(
            "incarnation_changed",
            format!("mux socket changed {moment}"),
        ));
    }
    Ok(())
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
    same_incarnation(
        pane_socket(env)?,
        &address.realm_id,
        &address.incarnation_id,
        "before publication",
    )?;
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
    if !pane_dir(root, address).is_dir() {
        return publish(None);
    }
    crate::records::with_lock(&claim_lock(root, address), LOCK_TIMEOUT, || {
        publish(read_claim(root, address)?)
    })
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

/// Publish the pane at `address` to its terminal at `tty_path`: the pane's
/// address, and the launch of its claim when that
/// claim was made at this terminal. The pane's claim lock is held from
/// reading the claim through the write, and the mux socket must still carry
/// the pane's incarnation. Returns the launch published, if any.
fn publish_pane(
    root: &std::path::Path,
    address: &PaneAddress,
    socket_path: &str,
    tty_path: &str,
    fingerprint: &str,
    tty: &dyn TtyWriter,
    moment: &str,
) -> Result<Option<String>> {
    with_pane_claim(root, address, |claim| {
        let launch_id = claimed_launch(claim.as_ref(), address, tty_path, fingerprint);
        same_incarnation(
            socket_path,
            &address.realm_id,
            &address.incarnation_id,
            moment,
        )?;
        tty.write(
            tty_path,
            &publication_bytes(address, launch_id.as_deref())?,
            fingerprint,
        )?;
        Ok(launch_id)
    })
}

pub fn publish_current(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
) -> Result<PublishReport> {
    let root = state_root(env)?;
    let (address, _) = pane_address(env)?;
    let tty_path = ports.tty.current_path()?;
    let fingerprint = ports.tty.fingerprint(&tty_path)?;
    let launch_id = publish_pane(
        &root,
        &address,
        pane_socket(env)?,
        &tty_path,
        &fingerprint,
        ports.tty,
        "before publication",
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
    same_incarnation(
        socket_path,
        &realm_id,
        &incarnation_id,
        "during enumeration",
    )?;
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
            publish_pane(
                &root,
                &address,
                socket_path,
                tty_name,
                &fingerprint,
                ports.tty,
                "before pane publication",
            )
            .map(|launch_id| launch_id.is_some())
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
        match crate::protocol::claim_owner(claim) {
            Ok(None) => Ok(Self::Shell),
            Ok(Some((pid, seconds, microseconds, boot))) => Ok(Self::SelfOwned(ClaimOwner {
                pid,
                start: ProcessStart {
                    seconds,
                    microseconds,
                },
                boot_session_id: boot.to_owned(),
            })),
            Err(()) => Err(AttentionError::new(
                "record_invalid",
                "claim owner is invalid",
            )),
        }
    }
}

/// The launch a claim record names.
pub(crate) fn claim_launch_id(claim: &Value) -> Result<String> {
    claim
        .get("launch_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AttentionError::new("record_invalid", "claim has no launch id"))
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
    if !crate::protocol::pid_text(value) {
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
    /// Whether the parent is in the terminal's foreground process group. It
    /// need not lead it: a launcher can lead the group it runs its agent in.
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
            "this hook's parent is not the process WEZTERM_ATTENTION_HOST_PID names: \
             a relay or a shell that stayed ran it, or it inherited the value; \
             register the hook as `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event ...`, \
             with nothing after it",
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

/// The agent process `WEZTERM_ATTENTION_HOST_PID` asserts, read as the
/// hook's parent, with the boot it runs on.
fn read_owner(
    env: &BTreeMap<String, String>,
    processes: &dyn ProcessInspector,
) -> Result<(ClaimOwner, HostReading)> {
    let reading = read_host(processes, asserted_host(env)?)?;
    let boot_session_id = processes.boot_session().ok_or_else(|| {
        AttentionError::new("probe_unavailable", "the boot session id could not be read")
    })?;
    let owner = ClaimOwner {
        pid: reading.parent_pid,
        start: reading.parent_start,
        boot_session_id,
    };
    Ok((owner, reading))
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
        let (owner, reading) = read_owner(env, ports.processes)?;
        let rows = ports.panes.list(pane_socket(env)?)?;
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
        same_incarnation(
            pane_socket(env)?,
            &address.realm_id,
            &address.incarnation_id,
            "while the agent was being checked",
        )?;
        let proof = Self {
            owner,
            tty_path,
            tty_fingerprint,
            terminal: reading.terminal,
            foreground: reading.foreground,
        };
        proof.confirm(env, ports.processes, ports.tty, address)?;
        Ok(proof)
    }

    /// Prove that this hook's agent is the process `claim` names, still on the
    /// terminal the claim recorded, without asking the mux.
    ///
    /// Making the claim listed the pane and proved that terminal is its own.
    /// While the owner, the same process on the same boot, still runs on that
    /// terminal device at that path, with the fingerprint the claim recorded,
    /// under the socket incarnation the claim was made on, nothing the
    /// listing would add has changed. A different owner refuses as
    /// `claim_stale`; the rest refuses as [`Self::establish`] does. The parent
    /// is read again at the end, as there.
    pub(crate) fn of_claim(
        env: &BTreeMap<String, String>,
        ports: &RuntimePorts<'_>,
        address: &PaneAddress,
        claim: &Value,
    ) -> Result<Self> {
        let (owner, reading) = read_owner(env, ports.processes)?;
        if ClaimMode::of(claim)? != ClaimMode::SelfOwned(owner.clone()) {
            return Err(AttentionError::new(
                "claim_stale",
                "the pane's claim belongs to another agent process",
            ));
        }
        let recorded = |field: &str| {
            claim
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| AttentionError::new("record_invalid", "claim has no terminal"))
        };
        let proof = Self {
            owner,
            tty_path: recorded("tty_path")?,
            tty_fingerprint: recorded("tty_fingerprint")?,
            terminal: reading.terminal,
            foreground: reading.foreground,
        };
        proof.confirm(env, ports.processes, ports.tty, address)?;
        Ok(proof)
    }

    /// Read the same host again and return whether it is still in the
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
                "the agent no longer runs on the terminal the pane was proven to use",
            ));
        }
        same_incarnation(
            pane_socket(env)?,
            &address.realm_id,
            &address.incarnation_id,
            "while the agent was being checked",
        )?;
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

/// Rests on the kernel fixing a process's start time when it forks: an
/// `exec` keeps it, so a process that still runs reads the same start at
/// every check, and a different start at its pid is another process.
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

/// Whether `claim` is an agent's own claim whose process is proven to have
/// exited.
pub(crate) fn owner_proven_gone(claim: &Value, processes: &dyn ProcessInspector) -> bool {
    match ClaimMode::of(claim) {
        Ok(ClaimMode::SelfOwned(owner)) => owner_state(&owner, processes) == OwnerState::Gone,
        Ok(ClaimMode::Shell) | Err(_) => false,
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
    let owner = &proof.owner;
    for (field, value) in crate::protocol::CLAIM_OWNER_FIELDS.into_iter().zip([
        owner.pid.to_string(),
        owner.start.seconds.to_string(),
        owner.start.microseconds.to_string(),
        owner.boot_session_id.clone(),
    ]) {
        record[field] = json!(value);
    }
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
/// be in the terminal's foreground job. The claim is published to the proven
/// terminal before the lock is released.
fn claim_for_host(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    address: &PaneAddress,
    proof: &HostProof,
) -> Result<(Value, Option<Diagnostic>)> {
    let root = state_root(env)?;
    mkdir_private(&root)?;
    let (_, metadata) = pane_address(env)?;
    let write = ClaimWrite::new(&root, address, &metadata)?;
    let observation = ports.clock.monotonic_ns20()?;
    let (selected, published) = commit(
        &root,
        &[&write.lock_path()],
        "claim",
        &write.identity(),
        |existing| {
            let foreground = proof.confirm(env, ports.processes, ports.tty, address)?;
            if let Some(current) = &existing {
                write.check_address(current)?;
                match ClaimMode::of(current)? {
                    ClaimMode::Shell => return Err(shell_claim_refusal()),
                    ClaimMode::SelfOwned(owner) if owner == proof.owner => {
                        if !proof.owns(current)? {
                            return Err(AttentionError::new(
                                "unsafe_tty",
                                "the agent's claim names a terminal the agent no longer runs on",
                            ));
                        }
                        return write.plan(current.clone(), None);
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
                    "the agent is not in the pane's foreground job, so it cannot claim the pane",
                ));
            }
            let claim =
                self_owned_claim_record(address, &Uuid::new_v4().to_string(), proof, &observation);
            write.plan(claim.clone(), Some(&claim))
        },
        |claim| {
            Ok(publish_claim(
                env,
                ports,
                address,
                &proof.tty_path,
                &proof.tty_fingerprint,
                &claim_launch_id(claim)?,
            )
            .err()
            .map(|error| error.diagnostic))
        },
    )?;
    Ok((selected, published))
}

fn shell_claim_refusal() -> AttentionError {
    AttentionError::new(
        "claim_stale",
        "the pane holds a shell claim, which an agent without its launch id cannot use",
    )
}

/// Why an agent cannot claim its own pane in `env`, or `None` when it can.
/// Self-claim is on unless `WEZTERM_ATTENTION_ENABLE_SELF_CLAIM` says
/// otherwise, and only where the platform supports it.
pub(crate) fn self_claim_refusal(
    env: &BTreeMap<String, String>,
    processes: &dyn ProcessInspector,
) -> Option<AttentionError> {
    if !processes.self_claim_supported() {
        return Some(AttentionError::new(
            "claim_stale",
            "an agent cannot claim its own pane on this platform; start it from a claiming shell",
        ));
    }
    match env
        .get("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM")
        .map(String::as_str)
    {
        None | Some("1") => None,
        Some(_) => Some(AttentionError::new(
            "claim_stale",
            "an agent claiming its own pane is switched off by WEZTERM_ATTENTION_ENABLE_SELF_CLAIM",
        )),
    }
}

/// Resolve an agent event that carries no launch id against a claim its own
/// agent process holds.
///
/// Only where [`self_claim_refusal`] allows it. A pane holding a shell claim
/// refuses every such event: that claim belongs to the commands its shell
/// starts, which carry its launch id.
pub(crate) fn self_owned_launch(
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    address: &PaneAddress,
    claim: Option<Value>,
    starts_session: bool,
) -> Result<SelfOwnedLaunch> {
    if let Some(refusal) = self_claim_refusal(env, ports.processes) {
        return Err(refusal);
    }
    asserted_host(env)?;
    if let Some(claim) = &claim
        && ClaimMode::of(claim)? == ClaimMode::Shell
    {
        return Err(shell_claim_refusal());
    }
    if starts_session {
        let proof = HostProof::establish(env, ports, address)?;
        let (claim, publication_diagnostic) = claim_for_host(env, ports, address, &proof)?;
        return Ok(SelfOwnedLaunch {
            claim,
            proof,
            publication_diagnostic,
        });
    }
    // Nothing but a session start can make a claim, so there is nothing to
    // prove this against.
    let Some(claim) = claim else {
        return Err(AttentionError::new(
            "claim_stale",
            "provider event has no matching pane claim",
        ));
    };
    let proof = HostProof::of_claim(env, ports, address, &claim)?;
    Ok(SelfOwnedLaunch {
        claim,
        proof,
        publication_diagnostic: None,
    })
}
