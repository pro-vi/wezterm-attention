//! The agent process a binding's lead hooks proved, kept beside the binding
//! in `host.json` and checked again each time `inspect` reports it.
//!
//! Only the writer makes the record, from the [`HostProof`] it made for a lead
//! hook it admitted. A reader learns from it which process, on which terminal,
//! is this binding's agent; nothing a caller supplies can stand in for it.

use serde::Serialize;
use serde_json::{Value, json};

use crate::identity::PaneAddress;
use crate::launch::{ClaimOwner, HostProof, OwnerState, owner_state, recorded_terminal};
use crate::protocol::{Result, manifest};
use crate::wezterm::{ControllingTerminal, ProcessInspector, ProcessRead, TtyWriter};

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
/// table, the boot, terminal devices and terminal fingerprints.
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

/// A host the check found as recorded.
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
/// check; `pane_gone` says the pane's server was shown to have exited.
pub(crate) fn check_host(
    stored: std::result::Result<&Value, HostRefusal>,
    pane_gone: bool,
    ports: Option<&HostCheckPorts<'_>>,
    checked_at: Option<&str>,
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
        confirm_recorded(record, ports)
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
    if record.get("contested_at_unix_ns").is_some() {
        return Err(HostRefusal::HostContested);
    }
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
    })
}
