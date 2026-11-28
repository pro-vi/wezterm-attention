//! Hold and quiet checks registered in `checks.json` in the state root.
//! `hooks event` runs them after the checks of the same kind its hook line
//! names, so an installer can add, change or remove a check without changing
//! the line, which a provider may tie its approval to. See ADR 0005's
//! amendment on `checks.json`. `attention checks add` and `attention checks
//! remove` change one entry under the same rules, so an installer need not
//! copy them.
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::hold_check::{self, Purpose, Registration};
use crate::protocol::{
    AttentionError, Diagnostic, DiagnosticCode, HookRegistration, Result, manifest,
};
use crate::records::{
    LOCK_TIMEOUT, atomic_replace, naming_record, read_bounded, with_lock, within_read_bound,
};

/// The file's name in the state root.
pub const FILE_NAME: &str = "checks.json";
/// The one schema this build reads.
const SCHEMA: u64 = 1;
/// Held while `add` or `remove` reads and replaces the file, so two
/// installers that run at once do not each drop the other's entry. Hooks read
/// the file without it: a rename never shows them half a file.
const LOCK_NAME: &str = ".checks.lock";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckFile {
    schema: u64,
    checks: Vec<Entry>,
}

/// One check the file registers on one provider's hook event, as one
/// `--hold-check` or `--quiet-check` on that event's line would.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub provider: String,
    pub event: String,
    pub kind: Purpose,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    pub executable: String,
}

impl Entry {
    /// The entry as a hook line spells it after `--hold-check` or
    /// `--quiet-check`. [`validate`] has refused a name that holds the `:` or
    /// `=` this spelling splits at, so it parses back to the same check.
    fn flag_value(&self) -> String {
        match self.time_ms {
            Some(time) => format!("{}:{time}ms={}", self.name, self.executable),
            None => format!("{}={}", self.name, self.executable),
        }
    }
}

/// What the file holds.
#[derive(Debug)]
pub enum Read {
    /// There is no file, so it registers nothing.
    Absent,
    Valid(Vec<Entry>),
    /// A file that is not used: it could not be read, it is not this user's
    /// or is writable by group or others, or it is not a valid registration.
    /// The diagnostic names the file's path in the state root.
    Invalid(Diagnostic),
}

pub fn read(root: &Path) -> Read {
    let path = root.join(FILE_NAME);
    match read_entries(&path) {
        Ok(Some(entries)) => Read::Valid(entries),
        Ok(None) => Read::Absent,
        Err(error) => Read::Invalid(naming_record(root, &path, error).diagnostic),
    }
}

/// Registers `entry` in the file under `root`, creating both when missing,
/// and says whether it wrote the file. An equal entry already there leaves
/// the file as it is.
pub fn add(root: &Path, entry: Entry) -> Result<bool> {
    registrable(&entry)?;
    edit(root, |entries| {
        let absent = position(entries, &entry)?.is_none();
        if absent {
            entries.push(entry);
        }
        Ok(absent)
    })
}

/// Removes the entry equal to `entry`, keeping every other entry in its
/// order, and says whether it wrote the file. With no such entry, or no
/// file, nothing changes, and nothing is created.
pub fn remove(root: &Path, entry: &Entry) -> Result<bool> {
    registrable(entry)?;
    if fs::symlink_metadata(root.join(FILE_NAME))
        .is_err_and(|error| error.kind() == ErrorKind::NotFound)
    {
        return Ok(false);
    }
    edit(root, |entries| {
        let found = position(entries, entry)?;
        if let Some(index) = found {
            entries.remove(index);
        }
        Ok(found.is_some())
    })
}

/// Refuses, as a usage error, an entry no file could hold, before the file is
/// read: for `remove`, a mistyped event would otherwise read as a removal
/// already done.
fn registrable(entry: &Entry) -> Result<()> {
    validate(std::slice::from_ref(entry)).map_err(|error| {
        AttentionError::usage(format!(
            "this check cannot be registered: {}",
            error.diagnostic.message
        ))
    })
}

/// Where `entries` holds `entry`. The file carries no owner, so naming the
/// entry whole is the nearest a caller comes to showing it is theirs: a
/// different check under the same name for the same provider and event,
/// which a valid file holds at most once, is refused, since it may be another
/// installer's. Callers that added an equal entry share it.
fn position(entries: &[Entry], entry: &Entry) -> Result<Option<usize>> {
    match entries.iter().position(|found| {
        found.provider == entry.provider && found.event == entry.event && found.name == entry.name
    }) {
        Some(index) if entries[index] == *entry => Ok(Some(index)),
        Some(index) => Err(registered_otherwise(&entries[index])),
        None => Ok(None),
    }
}

/// Reads the file under its lock, applies `change`, and when it reports a
/// change, writes the result in place of the file if this build would run
/// its checks. A file this build would not use is left as it is, whatever
/// the change: rewriting it would drop what it holds.
fn edit(root: &Path, change: impl FnOnce(&mut Vec<Entry>) -> Result<bool>) -> Result<bool> {
    let path = root.join(FILE_NAME);
    let refused = |prefix: &str, error: AttentionError| {
        AttentionError::new(
            error.diagnostic.code,
            format!("{prefix}: {}", error.diagnostic.message),
        )
    };
    with_lock(&root.join(LOCK_NAME), LOCK_TIMEOUT, || {
        // `read_entries` follows a link; a rename over it would replace the link.
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(AttentionError::new(
                DiagnosticCode::RecordInvalid,
                "checks.json is a link, which this command does not replace; edit the file it names",
            ));
        }
        let mut entries = read_entries(&path)
            .map_err(|error| refused("left as it is, since hooks do not use the file now", error))?
            .unwrap_or_default();
        if !change(&mut entries)? {
            return Ok(false);
        }
        let value = json!({"schema": SCHEMA, "checks": entries});
        validate(&entries)
            .and_then(|()| within_read_bound(&value))
            .map_err(|error| {
                refused(
                    "checks.json was left as it is: with this change hooks would refuse the file and run none of its checks",
                    error,
                )
            })?;
        atomic_replace(&path, &value)?;
        Ok(true)
    })
    .map_err(|error| naming_record(root, &path, error))
}

fn registered_otherwise(found: &Entry) -> AttentionError {
    AttentionError::new(
        DiagnosticCode::RecordInvalid,
        format!(
            "checks.json registers a different {} check named {} for {} {}, which this command does not replace or remove: {}",
            match found.kind {
                Purpose::Hold => "hold",
                Purpose::Quiet => "quiet",
            },
            found.name,
            found.provider,
            found.event,
            found.flag_value()
        ),
    )
}

fn read_entries(path: &Path) -> Result<Option<Vec<Entry>>> {
    let unreadable = || {
        AttentionError::new(
            DiagnosticCode::StatePermissions,
            "checks.json could not be read",
        )
    };
    let invalid = |error: serde_json::Error| {
        AttentionError::new(
            DiagnosticCode::RecordInvalid,
            format!("checks.json is invalid: {error}"),
        )
    };
    // A link is followed, so an installer may keep the file elsewhere; the
    // checks below apply to the file it reaches.
    let file = match OpenOptions::new()
        .read(true)
        // Without O_NONBLOCK a FIFO would hold the open until a writer came.
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable()),
    };
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.file_type().is_file() {
        return Err(AttentionError::new(
            DiagnosticCode::RecordInvalid,
            "checks.json is not a regular file",
        ));
    }
    // The file names programs that hooks run.
    if metadata.uid() != crate::identity::effective_uid() || metadata.mode() & 0o022 != 0 {
        return Err(AttentionError::new(
            DiagnosticCode::StatePermissions,
            "checks.json must be owned by this user and not writable by group or others",
        ));
    }
    let maximum = manifest()?.limits.max_json_bytes;
    let too_large = || {
        AttentionError::new(
            DiagnosticCode::RecordInvalid,
            format!("checks.json is larger than {maximum} bytes"),
        )
    };
    // Its size is known before the read, which would otherwise fill a buffer
    // to the limit on every hook call; the read is bounded too, for a file
    // that grows meanwhile.
    if usize::try_from(metadata.len()).map_or(true, |size| size > maximum) {
        return Err(too_large());
    }
    let bytes = read_bounded(&file, maximum).map_err(|_| unreadable())?;
    if bytes.len() > maximum {
        return Err(too_large());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(invalid)?;
    // A later schema is refused as one whatever else it holds, before the
    // shape this build knows refuses the fields it added.
    let schema = value.get("schema").and_then(Value::as_u64);
    if let Some(schema) = schema.filter(|schema| *schema > SCHEMA) {
        return Err(AttentionError::new(
            DiagnosticCode::FutureSchema,
            format!("checks.json has schema {schema}; this build reads schema {SCHEMA}"),
        ));
    }
    let parsed: CheckFile = serde_json::from_value(value).map_err(invalid)?;
    if parsed.schema != SCHEMA {
        return Err(AttentionError::new(
            DiagnosticCode::RecordInvalid,
            format!(
                "checks.json has schema {}; this build reads schema {SCHEMA}",
                parsed.schema
            ),
        ));
    }
    validate(&parsed.checks)?;
    Ok(Some(parsed.checks))
}

/// Each entry names a hook Attention asks to be registered, and the entries
/// for one event pass what a hook line with the same flags would, under the
/// largest budget a line can set.
fn validate(entries: &[Entry]) -> Result<()> {
    if let Some(entry) = entries
        .iter()
        .find(|entry| !hold_check::valid_name(&entry.name))
    {
        return Err(AttentionError::new(
            DiagnosticCode::RecordInvalid,
            format!(
                "checks.json names a check {:?}; a name is lowercase letters, digits, `_` and `-`, starting with a letter",
                entry.name
            ),
        ));
    }
    let manifest = manifest()?;
    let events: BTreeSet<_> = entries
        .iter()
        .map(|entry| (entry.provider.as_str(), entry.event.as_str()))
        .collect();
    for (provider, event) in events {
        let registered = manifest
            .native_hooks
            .get(provider)
            .and_then(|hooks| hooks.get(event))
            .is_some_and(|hook| hook.registration == HookRegistration::Register);
        if !registered {
            return Err(AttentionError::new(
                DiagnosticCode::RecordInvalid,
                format!(
                    "checks.json names {provider} {event}, which is not a hook Attention registers"
                ),
            ));
        }
        let (holds, quiets) = flag_values(entries, provider, event);
        hold_check::validate_checks(
            &holds,
            &quiets,
            Duration::from_millis(hold_check::MAX_BUDGET_MS),
        ).map_err(|error| {
            AttentionError::new(
                DiagnosticCode::RecordInvalid,
                format!(
                    "checks.json registers checks for {provider} {event} that a hook line could not: {}",
                    error.diagnostic.message
                ),
            )
        })?;
    }
    Ok(())
}

fn flag_values(entries: &[Entry], provider: &str, event: &str) -> (Vec<String>, Vec<String>) {
    let of = |kind| {
        entries
            .iter()
            .filter(|entry| {
                entry.kind == kind && entry.provider == provider && entry.event == event
            })
            .map(Entry::flag_value)
            .collect()
    };
    (of(Purpose::Hold), of(Purpose::Quiet))
}

/// `line`, the hook line's registrations as `validate_checks` returned them
/// from `line_holds` and `line_quiets`, with the file's checks for
/// `provider`'s `event` after the line's of the same kind: hold checks run
/// first, then quiet checks, the line's before the file's.
///
/// The file's checks for the event are all left out, and the line's run as
/// they would alone, when the file is not used or when the line would be
/// refused with them on it: a name the line already gives, times the line's
/// budget cannot give, or more checks than the turn-end audit holds. The error
/// says which.
pub fn with_line_checks(
    line: Vec<Registration>,
    line_holds: &[String],
    line_quiets: &[String],
    budget: Duration,
    file: Read,
    provider: &str,
    event: &str,
) -> (Vec<Registration>, Option<Diagnostic>) {
    let entries = match file {
        Read::Absent => return (line, None),
        Read::Invalid(diagnostic) => return (line, Some(diagnostic)),
        Read::Valid(entries) => entries,
    };
    let (holds, quiets) = flag_values(&entries, provider, event);
    if holds.is_empty() && quiets.is_empty() {
        return (line, None);
    }
    let joined = |line: &[String], file: Vec<String>| [line, &file].concat();
    match hold_check::validate_checks(
        &joined(line_holds, holds),
        &joined(line_quiets, quiets),
        budget,
    ) {
        Ok(registrations) => (registrations, None),
        Err(error) => (
            line,
            Some(
                Diagnostic::new(
                    DiagnosticCode::RecordInvalid,
                    format!(
                        "checks.json's checks for {provider} {event} were not run beside the hook line's: {}",
                        error.diagnostic.message
                    ),
                )
                .with("path", FILE_NAME),
            ),
        ),
    }
}
