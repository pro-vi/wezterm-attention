//! The agent process a binding's lead hooks proved, kept beside the binding
//! in `host.json` and checked again each time `inspect` reports it.
//!
//! Only the writer makes the record, from the [`HostProof`] it made for a lead
//! hook it admitted. A reader learns from it which process, on which terminal,
//! is this binding's agent; nothing a caller supplies can stand in for it.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::identity::PaneAddress;
use crate::launch::{ClaimOwner, HostProof, OwnerState, owner_state, recorded_terminal};
use crate::protocol::{Provider, Result, manifest};
use crate::wezterm::{
    Arg0Locks, ControllingTerminal, ProcessInspector, ProcessRead, ProcessStart, TtyWriter,
    arg0_lock_home,
};

/// The host record a lead hook's `proof` leaves after meeting `stored`, the
/// binding's record as it stands, or `None` when it stays as it is.
///
/// A record naming another process gives way only once that process is shown
/// to have exited. While it runs, or while whether it runs cannot be read, it
/// is kept and marked contested, and readers refuse it until it has gone.
/// `admitted` names the hook whose proof wrote the record; a hook that proves
/// the same host on the same terminal rewrites nothing.
pub(crate) fn recorded_host(
    stored: Option<&Value>,
    proof: &HostProof,
    binding: (&PaneAddress, &str, &str),
    source_event: &str,
    written_at: &str,
    processes: &dyn ProcessInspector,
) -> Result<Option<Value>> {
    let proved = || host_record(proof, binding, source_event, written_at);
    let Some(stored) = stored else {
        return proved().map(Some);
    };
    if proof.owns(stored).unwrap_or(false) {
        return Ok(None);
    }
    let contested = stored.get("contested_at_unix_ns").cloned();
    match ClaimOwner::of(stored) {
        // The same process, on another terminal.
        Ok(Some(owner)) if owner == proof.owner => {
            let mut moved = proved()?;
            if let Some(contested) = contested {
                moved["contested_at_unix_ns"] = contested;
            }
            Ok(Some(moved))
        }
        Ok(Some(owner)) => match owner_state(&owner, processes) {
            OwnerState::Gone => proved().map(Some),
            OwnerState::Alive | OwnerState::Unknown if contested.is_some() => Ok(None),
            OwnerState::Alive | OwnerState::Unknown => {
                let mut marked = stored.clone();
                marked["contested_at_unix_ns"] = json!(written_at);
                Ok(Some(marked))
            }
        },
        // A stored record is valid, so it names its owner whole.
        Ok(None) | Err(()) => proved().map(Some),
    }
}

/// Whether `stored`, a binding's host record, names the host `proof` proved,
/// on the same terminal, while no other process contests it.
pub(crate) fn proves_recorded_host(proof: &HostProof, stored: &Value) -> bool {
    !is_contested(stored) && proof.owns(stored).unwrap_or(false)
}

/// Whether a second process was proved for the record's binding while the
/// recorded one ran.
fn is_contested(record: &Value) -> bool {
    record.get("contested_at_unix_ns").is_some()
}

fn host_record(
    proof: &HostProof,
    (address, launch_id, binding_id): (&PaneAddress, &str, &str),
    source_event: &str,
    written_at: &str,
) -> Result<Value> {
    let mut record = json!({
        "kind": "host",
        "schema": manifest()?.record_schema,
        "address": address,
        "launch_id": launch_id,
        "binding_id": binding_id,
        "tty_path": proof.tty_path,
        "tty_fingerprint": proof.tty_fingerprint,
        "source_event": source_event,
        "written_at_unix_ns": written_at,
    });
    proof.owner.write_into(&mut record);
    Ok(record)
}

/// What a host facet reads to check a recorded host again: the process
/// table, the boot, terminal devices and terminal fingerprints, and for a
/// Codex host its start-up `CODEX_HOME` and `HOME`, the names of its open
/// arg0 helper locks and its real user's home.
pub struct HostCheckPorts<'a> {
    pub processes: &'a dyn ProcessInspector,
    pub tty: &'a dyn TtyWriter,
}

/// Whether `inspect` vouches for the binding's host. Every value but
/// `available` refuses it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAvailability {
    Available,
    Absent,
    Unavailable,
    Invalid,
    Unsupported,
}

/// Why `inspect` does not vouch for the binding's host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRefusal {
    /// No lead hook of this binding proved a host.
    NoneRecorded,
    ScopeNotMatched,
    BindingEnded,
    /// No process has the recorded pid, or one that has exited holds it.
    HostExited,
    /// Another process holds the recorded pid: it started at another time, or
    /// runs as another user.
    HostReplaced,
    BootChanged,
    /// The host no longer runs on the recorded terminal, the terminal at the
    /// recorded path is another one, or the pane's server has exited.
    TerminalChanged,
    /// A second process was proved for this binding while the recorded one
    /// still ran, or could not be read.
    HostContested,
    /// The process table, the boot, a terminal, the host record or the
    /// binding record could not be read, or the caller gave no way to read
    /// them.
    ProbeUnavailable,
    /// The host record is invalid, or declares a newer schema.
    RecordInvalid,
    PlatformUnsupported,
}

impl HostRefusal {
    fn availability(self) -> HostAvailability {
        match self {
            Self::NoneRecorded => HostAvailability::Absent,
            Self::RecordInvalid => HostAvailability::Invalid,
            Self::PlatformUnsupported => HostAvailability::Unsupported,
            Self::ScopeNotMatched
            | Self::BindingEnded
            | Self::HostExited
            | Self::HostReplaced
            | Self::BootChanged
            | Self::TerminalChanged
            | Self::HostContested
            | Self::ProbeUnavailable => HostAvailability::Unavailable,
        }
    }
}

/// The `host` facet of `inspect`: the binding's recorded host as a check made
/// while answering found it, or why it is refused.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HostFacet {
    pub availability: HostAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<HostRefusal>,
    /// When the answer was read; absent where no check ran or the clock could
    /// not be read. It says nothing about whether the host still holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at_unix_ns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<CheckedHost>,
}

impl HostFacet {
    pub(crate) fn refused(reason: HostRefusal, checked_at: Option<String>) -> Self {
        Self {
            availability: reason.availability(),
            reason: Some(reason),
            checked_at_unix_ns: checked_at,
            record: None,
        }
    }
}

/// A host the check found as recorded and, for a Codex binding, the Codex
/// home it uses or why none is named.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckedHost {
    pub pid: i32,
    pub start: HostStart,
    pub boot_session_id: String,
    pub terminal: HostTerminal,
    pub admitted: HostAdmission,
    /// Whether the host's process group was its terminal's foreground group
    /// at the check.
    pub foreground: bool,
    /// For a Codex binding only: the Codex home the host uses, or why none
    /// is named.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub codex_home: Option<CodexHome>,
}

/// The Codex home of a Codex binding's host, written as `config_dir` or as
/// `config_dir_refusal`, so exactly one of the two is ever set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum CodexHome {
    /// The canonical Codex home the host uses, read while answering.
    #[serde(rename = "config_dir")]
    Named(String),
    #[serde(rename = "config_dir_refusal")]
    Refused(CodexHomeRefusal),
}

/// Why `inspect` names no Codex home for an available Codex host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexHomeRefusal {
    /// The host's start-up environment could not be read.
    EnvironmentUnreadable,
    /// `CODEX_HOME` is a UTF-8 value that is not absolute.
    CodexHomeRelative,
    /// `CODEX_HOME` names nothing, or no directory.
    CodexHomeUnresolvable,
    /// `CODEX_HOME` counts as unset and `HOME` is not UTF-8.
    HomeNotUtf8,
    /// `CODEX_HOME` counts as unset and `HOME` is not absolute.
    HomeRelative,
    /// There is no usable `HOME`, or `$HOME/.codex` is no directory.
    HomeUnresolvable,
    /// The host holds no Codex arg0 helper lock.
    HomeUnobserved,
    /// The host holds arg0 helper locks in more than one home.
    HomeAmbiguous,
    /// The host's open files could not all be read.
    HomeObservationUnavailable,
    /// The Codex home the host's lock lies in is not the one its start-up
    /// environment names, or the lock is no longer at its path.
    HomeChangedAfterStart,
    /// The host's start time differed when read again after the home.
    HostChangedDuringRead,
}

impl CodexHomeRefusal {
    /// Every refusal; the manifest's `codex_home_refusals` lists their names.
    pub const ALL: [Self; 11] = [
        Self::EnvironmentUnreadable,
        Self::CodexHomeRelative,
        Self::CodexHomeUnresolvable,
        Self::HomeNotUtf8,
        Self::HomeRelative,
        Self::HomeUnresolvable,
        Self::HomeUnobserved,
        Self::HomeAmbiguous,
        Self::HomeObservationUnavailable,
        Self::HomeChangedAfterStart,
        Self::HostChangedDuringRead,
    ];

    /// The name `inspect` writes.
    pub fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HostStart {
    pub seconds: String,
    pub microseconds: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HostTerminal {
    pub path: String,
    pub device: String,
    pub fingerprint: String,
}

/// The lead hook whose proof wrote the record, and when.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HostAdmission {
    pub source_event: String,
    pub written_at_unix_ns: String,
}

/// Check the host `stored` names, for a binding the scope matched and that
/// has not ended, the way [`HostProof::confirm`] does without the relation to
/// a hook: the same process, started at the same time on the same boot, as
/// this user, on the recorded terminal at the recorded path with the recorded
/// fingerprint. `stored` is the binding's host record, or why there is none to
/// check; `pane_gone` says the pane's server was shown to have exited. The
/// found host of a `provider` that is Codex also names its Codex home.
pub(crate) fn check_host(
    stored: std::result::Result<&Value, HostRefusal>,
    pane_gone: bool,
    ports: Option<&HostCheckPorts<'_>>,
    checked_at: Option<&str>,
    provider: Option<Provider>,
) -> HostFacet {
    let verdict = (|| -> std::result::Result<CheckedHost, HostRefusal> {
        let ports = ports.ok_or(HostRefusal::ProbeUnavailable)?;
        if !ports.processes.self_claim_supported() {
            return Err(HostRefusal::PlatformUnsupported);
        }
        let record = stored?;
        if pane_gone {
            return Err(HostRefusal::TerminalChanged);
        }
        confirm_recorded(record, ports, provider)
    })();
    let checked_at = checked_at.map(str::to_owned);
    match verdict {
        Ok(host) => HostFacet {
            availability: HostAvailability::Available,
            reason: None,
            checked_at_unix_ns: checked_at,
            record: Some(host),
        },
        Err(reason) => HostFacet::refused(reason, checked_at),
    }
}

fn confirm_recorded(
    record: &Value,
    ports: &HostCheckPorts<'_>,
    provider: Option<Provider>,
) -> std::result::Result<CheckedHost, HostRefusal> {
    let owner = ClaimOwner::of(record)
        .ok()
        .flatten()
        .ok_or(HostRefusal::RecordInvalid)?;
    let (path, fingerprint) = recorded_terminal(record).map_err(|_| HostRefusal::RecordInvalid)?;
    let text = |field: &str| {
        record
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(HostRefusal::RecordInvalid)
    };
    let admitted = HostAdmission {
        source_event: text("source_event")?,
        written_at_unix_ns: text("written_at_unix_ns")?,
    };
    let processes = ports.processes;
    match processes.boot_session() {
        None => return Err(HostRefusal::ProbeUnavailable),
        Some(boot) if boot != owner.boot_session_id => return Err(HostRefusal::BootChanged),
        Some(_) => {}
    }
    let ProcessRead::Found(reader) = processes.process(processes.own_pid()) else {
        return Err(HostRefusal::ProbeUnavailable);
    };
    let host = match processes.process(owner.pid) {
        ProcessRead::Found(host) if host.zombie => return Err(HostRefusal::HostExited),
        ProcessRead::Found(host) => host,
        ProcessRead::Gone => return Err(HostRefusal::HostExited),
        ProcessRead::Unknown => return Err(HostRefusal::ProbeUnavailable),
    };
    if host.start != owner.start || host.uid != reader.uid {
        return Err(HostRefusal::HostReplaced);
    }
    // An unchanged fingerprint is an unchanged device node at the path, so a
    // host on the device found there runs where it was proved.
    let device = match host.terminal {
        ControllingTerminal::Device(device) => device,
        ControllingTerminal::Unknown => return Err(HostRefusal::ProbeUnavailable),
        ControllingTerminal::Absent => return Err(HostRefusal::TerminalChanged),
    };
    if processes.terminal_device(&path) != Some(device)
        || ports.tty.fingerprint(&path).ok().as_ref() != Some(&fingerprint)
    {
        return Err(HostRefusal::TerminalChanged);
    }
    if is_contested(record) {
        return Err(HostRefusal::HostContested);
    }
    let codex_home = match provider {
        Some(Provider::Codex) => Some(
            match effective_codex_home(owner.pid, owner.start, processes) {
                Ok(home) => CodexHome::Named(home),
                Err(refusal) => CodexHome::Refused(refusal),
            },
        ),
        Some(Provider::Claude | Provider::Pi) | None => None,
    };
    Ok(CheckedHost {
        pid: owner.pid,
        start: HostStart {
            seconds: owner.start.seconds.to_string(),
            microseconds: owner.start.microseconds,
        },
        boot_session_id: owner.boot_session_id,
        terminal: HostTerminal {
            path,
            device: device.to_string(),
            fingerprint,
        },
        admitted,
        foreground: host.process_group == host.terminal_foreground_group,
        codex_home,
    })
}

/// The Codex home the host at `pid`, which started at `start`, uses: the home
/// its start-up environment names, read as Codex 0.162.0 and 0.162.1 resolve
/// it, only when the arg0 helper lock the host holds open lies in that same
/// directory, and only when the host still has that start time afterwards.
///
/// As of Codex 0.162.0 and 0.162.1, Codex loads the `.env` in the home its
/// start-up environment names after it starts and lets it set `HOME`, so that
/// environment alone can name a home Codex no longer uses; the lock is created
/// after the load (`codex-rs/arg0/src/lib.rs`).
fn effective_codex_home(
    pid: i32,
    start: ProcessStart,
    processes: &dyn ProcessInspector,
) -> std::result::Result<String, CodexHomeRefusal> {
    let home = start_home(pid, processes).and_then(|home| corroborated(home, pid, processes));
    match processes.process(pid) {
        ProcessRead::Found(now) if !now.zombie && now.start == start => home,
        ProcessRead::Found(_) | ProcessRead::Gone | ProcessRead::Unknown => {
            Err(CodexHomeRefusal::HostChangedDuringRead)
        }
    }
}

/// A directory by its canonical path, which is UTF-8, and its identity.
struct Directory {
    path: String,
    identity: (u64, u64),
}

impl Directory {
    fn at(path: &Path) -> Option<Self> {
        let canonical = std::fs::canonicalize(path).ok()?;
        let metadata = std::fs::metadata(&canonical).ok()?;
        if !metadata.is_dir() {
            return None;
        }
        Some(Self {
            path: canonical.into_os_string().into_string().ok()?,
            identity: (metadata.dev(), metadata.ino()),
        })
    }
}

/// The home the host's start-up environment names, as `find_codex_home` in
/// Codex 0.162.0 and 0.162.1 reads it (`codex-rs/utils/home-dir/src/lib.rs`,
/// with dirs-sys 0.5.0's `home_dir`): `CODEX_HOME` when it is set, not empty
/// and UTF-8, else `<HOME>/.codex`, else the user database's home.
fn start_home(
    pid: i32,
    processes: &dyn ProcessInspector,
) -> std::result::Result<Directory, CodexHomeRefusal> {
    let variables = processes
        .start_home_variables(pid)
        .ok_or(CodexHomeRefusal::EnvironmentUnreadable)?;
    // Codex 0.162.0 and 0.162.1 read CODEX_HOME with `std::env::var`, so a
    // value that is not UTF-8 counts as unset, like an empty one.
    let codex_home = variables
        .codex_home
        .filter(|value| !value.is_empty())
        .and_then(|value| String::from_utf8(value).ok());
    if let Some(codex_home) = codex_home {
        let path = PathBuf::from(codex_home);
        if !path.is_absolute() {
            return Err(CodexHomeRefusal::CodexHomeRelative);
        }
        return Directory::at(&path).ok_or(CodexHomeRefusal::CodexHomeUnresolvable);
    }
    let home = match variables.home.filter(|value| !value.is_empty()) {
        Some(home) => {
            let home = String::from_utf8(home).map_err(|_| CodexHomeRefusal::HomeNotUtf8)?;
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err(CodexHomeRefusal::HomeRelative);
            }
            home
        }
        None => processes
            .passwd_home(pid)
            .and_then(|home| String::from_utf8(home).ok())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(CodexHomeRefusal::HomeUnresolvable)?,
    };
    Directory::at(&home.join(".codex")).ok_or(CodexHomeRefusal::HomeUnresolvable)
}

/// `home`'s path, when the arg0 helper locks the host holds open all lie in
/// it and are still at their paths.
fn corroborated(
    home: Directory,
    pid: i32,
    processes: &dyn ProcessInspector,
) -> std::result::Result<String, CodexHomeRefusal> {
    let Arg0Locks::Complete(locks) = processes.arg0_locks(pid) else {
        return Err(CodexHomeRefusal::HomeObservationUnavailable);
    };
    let homes: BTreeSet<_> = locks
        .iter()
        .filter_map(|lock| arg0_lock_home(&lock.path))
        .collect();
    let mut homes = homes.into_iter();
    let observed = match (homes.next(), homes.next()) {
        (None, _) => return Err(CodexHomeRefusal::HomeUnobserved),
        (Some(observed), None) => observed,
        (Some(_), Some(_)) => return Err(CodexHomeRefusal::HomeAmbiguous),
    };
    let in_place = locks.iter().all(|lock| {
        std::fs::symlink_metadata(&lock.path).is_ok_and(|metadata| {
            metadata.is_file() && (metadata.dev(), metadata.ino()) == (lock.device, lock.inode)
        })
    });
    if !in_place {
        return Err(CodexHomeRefusal::HomeChangedAfterStart);
    }
    match Directory::at(observed) {
        Some(observed) if observed.identity == home.identity => Ok(home.path),
        Some(_) | None => Err(CodexHomeRefusal::HomeChangedAfterStart),
    }
}
