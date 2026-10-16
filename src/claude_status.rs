//! Claude Code's per-process status file, read for one thing: whether the lead
//! turn of the process that writes it is still working. As of Claude Code
//! 2.1.300 a turn the user stops with Esc runs no hook, and this file shows the
//! stop.
//!
//! The file is Claude Code's own, private and undocumented, and all of it is
//! untrusted. This module is the only code that knows where it is, which of
//! its fields are read and what their values mean; what leaves it is a
//! [`Reading`], which carries none of the file's field names or state values,
//! only the Claude Code version it names when that is a safe label. A file it
//! does not understand is never read as a working or an ended turn.

use std::fs::OpenOptions;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::providers::safe_text;
use crate::records::read_bounded;

/// The `source_event` of a turn end read from the status file; see
/// [`crate::turn_follower::FOLLOWER_SOURCES`].
pub const SESSION_STATUS: &str = "session_status";

/// The most of the file read; a longer one is not read. As of Claude Code
/// 2.1.300 one holds under 1 KiB.
const MAX_BYTES: usize = 64 << 10;

/// Where Claude Code keeps the status file of its process `pid`. As of Claude
/// Code 2.1.300 its configuration directory, `CLAUDE_CONFIG_DIR` when that is
/// set and `~/.claude` otherwise, holds one for each process it runs. `None`
/// when the directory that applies is not an absolute path.
pub fn status_path(config_dir: Option<&str>, home: Option<&str>, pid: i32) -> Option<PathBuf> {
    let directory = match config_dir {
        Some(directory) => PathBuf::from(directory),
        None => Path::new(home?).join(".claude"),
    };
    directory
        .is_absolute()
        .then(|| directory.join("sessions").join(format!("{pid}.json")))
}

/// What one read of the file says. `updated_at` is when Claude Code last
/// changed what the file says about the turn, in its own milliseconds; only
/// two of them are ever compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reading {
    /// A turn is working, or Claude Code waits on the user: at an approval,
    /// or in a dialog such as the one `/exit` opens.
    Working { updated_at: u64 },
    /// No turn is working: the process waits for the next prompt.
    Ended {
        updated_at: u64,
        version: Option<String>,
    },
    /// This read was not understood. A read that lands while Claude Code
    /// rewrites the file looks the same, so a later read may be.
    Unreadable {
        reason: Unreadable,
        version: Option<String>,
    },
    /// The file is not the one to read for this session and process.
    Foreign(Foreign),
    /// There is no file: the process is exiting, or never wrote one there.
    Gone,
}

/// Why a read was not understood.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unreadable {
    /// The open or the read failed, as one that runs out of descriptors does.
    ReadFailed,
    TooLarge,
    /// Not a JSON object, a truncated one included.
    NotJsonObject,
    /// A field this module reads is missing or holds another type.
    InvalidField,
    /// The file's status is one this module does not know.
    UnknownStatus,
}

/// Why a file is not the one to read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Foreign {
    Link,
    NotRegularFile,
    NotOwned,
    OtherSession,
    OtherProcess,
}

impl Unreadable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadFailed => "read_failed",
            Self::TooLarge => "too_large",
            Self::NotJsonObject => "not_json_object",
            Self::InvalidField => "invalid_field",
            Self::UnknownStatus => "unknown_status",
        }
    }
}

impl Foreign {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::NotRegularFile => "not_regular_file",
            Self::NotOwned => "not_owned",
            Self::OtherSession => "other_session",
            Self::OtherProcess => "other_process",
        }
    }
}

/// Read the status file at `path` once, for Claude Code session `session_id`
/// in process `pid`.
pub fn read(path: &Path, session_id: &str, pid: i32) -> Reading {
    let unreadable = |reason| Reading::Unreadable {
        reason,
        version: None,
    };
    let file = match OpenOptions::new()
        .read(true)
        // Without O_NONBLOCK a FIFO would hold the open until a writer came.
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Reading::Gone,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Reading::Foreign(Foreign::Link);
        }
        Err(_) => return unreadable(Unreadable::ReadFailed),
    };
    let Ok(metadata) = file.metadata() else {
        return unreadable(Unreadable::ReadFailed);
    };
    if !metadata.file_type().is_file() {
        return Reading::Foreign(Foreign::NotRegularFile);
    }
    if metadata.uid() != crate::identity::effective_uid() {
        return Reading::Foreign(Foreign::NotOwned);
    }
    let Ok(bytes) = read_bounded(&file, MAX_BYTES) else {
        return unreadable(Unreadable::ReadFailed);
    };
    if bytes.len() > MAX_BYTES {
        return unreadable(Unreadable::TooLarge);
    }
    understand(&bytes, session_id, pid)
}

/// What the file's bytes say. As of Claude Code 2.1.300 `busy` is a working
/// turn and `waiting` one waiting on the user, for an approval or a dialog;
/// `idle` is no turn and `shell` no turn while a background command runs.
fn understand(bytes: &[u8], session_id: &str, pid: i32) -> Reading {
    let Ok(Value::Object(file)) = serde_json::from_slice::<Value>(bytes) else {
        return Reading::Unreadable {
            reason: Unreadable::NotJsonObject,
            version: None,
        };
    };
    let version = file
        .get("version")
        .and_then(Value::as_str)
        .and_then(|version| safe_text(version, "version").ok());
    let unreadable = |reason| Reading::Unreadable {
        reason,
        version: version.clone(),
    };
    let (Some(session), Some(owner), Some(status), Some(updated_at)) = (
        file.get("sessionId").and_then(Value::as_str),
        file.get("pid").and_then(Value::as_i64),
        file.get("status").and_then(Value::as_str),
        file.get("statusUpdatedAt").and_then(Value::as_u64),
    ) else {
        return unreadable(Unreadable::InvalidField);
    };
    if session != session_id {
        return Reading::Foreign(Foreign::OtherSession);
    }
    if owner != i64::from(pid) {
        return Reading::Foreign(Foreign::OtherProcess);
    }
    match status {
        "busy" | "waiting" => Reading::Working { updated_at },
        "idle" | "shell" => Reading::Ended {
            updated_at,
            version,
        },
        _ => unreadable(Unreadable::UnknownStatus),
    }
}
