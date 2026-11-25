//! Private on-disk readers for Antigravity CLI (`agy`).
//!
//! As of `agy` 1.5.16, four runtime facts live in `agy`'s own on-disk files
//! rather than in hook stdin JSON:
//! 1. The final assistant reply text at `Stop` time (`transcript_full.jsonl`,
//!    flushed synchronously before `Stop` runs; `Stop.finalModelOutput` is
//!    unset).
//! 2. Whether a lead `PreInvocation(invocationNum: 0)` started from a new user
//!    prompt or from a background subagent/task wakeup (`transcript_full.jsonl`,
//!    where `USER_INPUT` is flushed before `PreInvocation` runs).
//! 3. Subagent parent/root lineage and role/type (`brain/<id>/.system_generated/
//!    messages/*.json`, written before child `SessionStart` fires; hook stdin
//!    omits `parentConversationId`, `rootConversationId`, and `agentName`).
//! 4. Whether an open lead turn was stopped with Esc, which runs no `Stop` hook
//!    (`conversation_summaries.db` and `conversations/<id>.db`, polled by the
//!    turn follower).
//!
//! All of these files are `agy`'s own, private and undocumented, and treated as
//! untrusted. This module is the only code that knows their paths, schemas, and
//! status values; every reader fails closed on unexpected or oversized data.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::providers::safe_text;
use crate::records::read_bounded;

/// The `source_event` of a turn end read from `agy`'s status databases; see
/// [`crate::turn_follower::FOLLOWER_SOURCES`].
pub const AGY_STATUS: &str = "agy_status";

/// Default tail window read from `transcript_full.jsonl` when inspecting the
/// latest prompt step.
const TRANSCRIPT_PROMPT_WINDOW_BYTES: usize = 512 << 10;

/// Upper bound on tail bytes scanned from `transcript_full.jsonl` when reading
/// a final assistant reply (large enough to exceed `max_json_bytes` = 1 MiB
/// plus sibling JSON fields without scanning arbitrary multi-GB logs).
const TRANSCRIPT_MAX_TAIL_BYTES: usize = 4 << 20;

/// Maximum size of one `.system_generated/messages/*.json` file inspected while
/// resolving subagent lineage.
const MAX_MESSAGE_BYTES: usize = 256 << 10;

/// Maximum number of message files inspected in one conversation directory.
const MAX_MESSAGE_FILES: usize = 256;

/// Maximum directory entries enumerated from `.system_generated/messages/` per
/// lookup, keeping `lstat` work bounded on busy sessions.
const MAX_MESSAGE_DIR_SCAN: usize = 512;

/// Maximum subagent ancestry depth walked toward the root lead session.
const MAX_SUBAGENT_DEPTH: usize = 8;

/// Result of extracting the final assistant reply from an `agy` transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyRead {
    Available(String),
    Absent,
    TooLarge,
}

/// Root linkage recovered for an `agy` subagent conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentLineage {
    pub root_session_id: String,
    pub role: Option<String>,
    pub type_name: Option<String>,
}

/// What one poll of `agy`'s local databases says about a lead turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reading {
    /// The turn is running, generating, waiting on user input (`ask_question`),
    /// or waiting on background work (`not_fully_idle != 0`).
    Working,
    /// The conversation is idle with no pending background work.
    Idle,
    /// A step created during this turn carries `CORTEX_STEP_STATUS_CANCELED`
    /// (`6`) or `CORTEX_STEP_STATUS_INTERRUPTED` (`12`) with no active step
    /// running.
    Canceled,
    /// Neither database held a row for this conversation yet.
    Absent,
    /// A transient lock, missing `-shm` during checkpoint, platform limit, or
    /// unexpected status prevented reading; a later poll decides.
    Unreadable(Unreadable),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unreadable {
    ReadFailed,
    Foreign,
    UnknownStatus,
    PlatformUnsupported,
}

/// Derive the `agy` application data directory from a `transcriptPath` such as
/// `<appDataDir>/brain/<conversationId>/.system_generated/logs/transcript_full.jsonl`,
/// falling back to `<home>/.gemini/antigravity-cli`.
pub fn app_data_dir(transcript_path: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = transcript_path.map(Path::new)
        && path.is_absolute()
        && let Some(dir) = app_data_dir_from_brain_descendant(path)
    {
        return Some(dir);
    }
    let home = Path::new(home?);
    home.is_absolute()
        .then(|| home.join(".gemini").join("antigravity-cli"))
}

fn app_data_dir_from_brain_descendant(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    while let Some(parent) = current.parent() {
        if current.file_name().and_then(|name| name.to_str()) == Some("brain")
            && parent.is_absolute()
        {
            return Some(parent.to_path_buf());
        }
        current = parent;
    }
    None
}

/// Path to `conversation_summaries.db` for an `agy` session.
pub fn summaries_db_path(transcript_path: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    Some(app_data_dir(transcript_path, home)?.join("conversation_summaries.db"))
}

/// Validate that `id` is a safe single path segment (rejecting empty strings,
/// control characters, `.`, `..`, and path separators).
fn valid_conversation_segment(id: &str) -> bool {
    safe_text(id, "conversationId").is_ok()
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

/// Path to `conversations/<conversationId>.db` for an `agy` session.
pub fn conversation_db_path(
    transcript_path: Option<&str>,
    home: Option<&str>,
    conversation_id: &str,
) -> Option<PathBuf> {
    if !valid_conversation_segment(conversation_id) {
        return None;
    }
    Some(
        app_data_dir(transcript_path, home)?
            .join("conversations")
            .join(format!("{conversation_id}.db")),
    )
}

/// Open a regular file owned by the effective user without following symlinks
/// or blocking on FIFOs.
fn open_owned_regular(path: &Path) -> Option<File> {
    if !path.is_absolute() {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.uid() != crate::identity::effective_uid() {
        return None;
    }
    Some(file)
}

struct TailBytes {
    bytes: Vec<u8>,
    /// True when `bytes` starts mid-line (the byte immediately before the
    /// window was not `\n`).
    partial_first_line: bool,
    /// True whenever the file was larger than `max_window` so earlier lines
    /// were omitted before `bytes`.
    omitted_prefix: bool,
}

/// Read up to `max_window` trailing bytes from an owned regular transcript file.
fn read_tail_bytes(path: &Path, max_window: usize) -> Option<TailBytes> {
    let mut file = open_owned_regular(path)?;
    let size = file.metadata().ok()?.len();
    let window = u64::try_from(max_window).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    if size > window {
        let offset = size - window;
        file.seek(SeekFrom::Start(offset - 1)).ok()?;
        let take = window.saturating_add(1);
        file.take(take).read_to_end(&mut bytes).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let partial_first_line = bytes[0] != b'\n';
        bytes.drain(..1);
        Some(TailBytes {
            bytes,
            partial_first_line,
            omitted_prefix: true,
        })
    } else {
        file.take(window.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        Some(TailBytes {
            bytes,
            partial_first_line: false,
            omitted_prefix: false,
        })
    }
}

fn parse_jsonl_object(trimmed: &[u8]) -> Option<Value> {
    let line = std::str::from_utf8(trimmed).ok()?;
    let value = serde_json::from_str::<Value>(line).ok()?;
    value.is_object().then_some(value)
}

/// Extract the latest assistant reply written for the current turn in
/// `transcript_full.jsonl`. Scans backward line-by-line, stopping fail-closed
/// at `USER_INPUT`, `ERROR_MESSAGE`, or any malformed non-blank row so a
/// terminated or corrupt turn boundary never leaks an earlier turn's reply.
pub fn last_reply_from_transcript(path: &Path, max_text_bytes: usize) -> ReplyRead {
    let window = max_text_bytes
        .saturating_mul(2)
        .clamp(TRANSCRIPT_PROMPT_WINDOW_BYTES, TRANSCRIPT_MAX_TAIL_BYTES);
    let Some(tail) = read_tail_bytes(path, window) else {
        return ReplyRead::Absent;
    };
    let raw_lines: Vec<&[u8]> = tail.bytes.split(|&byte| byte == b'\n').collect();
    let mut saw_complete_line = false;
    for (idx, raw) in raw_lines.iter().enumerate().rev() {
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        if idx == 0 && tail.partial_first_line {
            return if !saw_complete_line {
                ReplyRead::TooLarge
            } else {
                ReplyRead::Absent
            };
        }
        saw_complete_line = true;
        let Some(step) = parse_jsonl_object(trimmed) else {
            return ReplyRead::Absent;
        };
        let Some(step_type) = step.get("type").and_then(Value::as_str) else {
            return ReplyRead::Absent;
        };
        match step_type {
            "USER_INPUT" | "ERROR_MESSAGE" => return ReplyRead::Absent,
            "PLANNER_RESPONSE" => {
                if let Some(content) = step.get("content").and_then(Value::as_str)
                    && !content.is_empty()
                {
                    if content.len() > max_text_bytes {
                        return ReplyRead::TooLarge;
                    }
                    return ReplyRead::Available(content.to_owned());
                }
            }
            _ => {}
        }
    }
    ReplyRead::Absent
}

/// Determine whether `transcript_full.jsonl` at `PreInvocation(invocationNum: 0)`
/// ends on a freshly submitted user prompt rather than a background subagent or
/// background-task wakeup.
///
/// Returns `None` when the file is absent, rejected, unreadable, malformed, or
/// truncated across the deciding boundary so callers fail closed.
pub fn is_user_prompt_invocation(path: &Path) -> Option<bool> {
    let tail = read_tail_bytes(path, TRANSCRIPT_PROMPT_WINDOW_BYTES)?;
    let raw_lines: Vec<&[u8]> = tail.bytes.split(|&byte| byte == b'\n').collect();
    let mut saw_system_tail = false;
    let mut saw_user_before_system = false;
    for (idx, raw) in raw_lines.iter().enumerate().rev() {
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        if idx == 0 && tail.partial_first_line {
            return None;
        }
        let step = parse_jsonl_object(trimmed)?;
        let step_type = step.get("type").and_then(Value::as_str)?;
        if !saw_system_tail && !saw_user_before_system {
            if step_type == "USER_INPUT" {
                return Some(true);
            }
            if step_type == "SYSTEM_MESSAGE" {
                saw_system_tail = true;
                continue;
            }
            return Some(false);
        }
        if !saw_user_before_system {
            if step_type == "SYSTEM_MESSAGE" {
                continue;
            }
            if step_type == "USER_INPUT" {
                saw_user_before_system = true;
                continue;
            }
            return Some(false);
        }
        // After finding USER_INPUT before trailing SYSTEM_MESSAGE(s), confirm
        // no earlier PLANNER_RESPONSE exists (SessionStart systemMessage runs
        // before any planner response; background wakeups follow one).
        if step_type == "PLANNER_RESPONSE" {
            return Some(false);
        }
    }
    (saw_user_before_system && !tail.omitted_prefix).then_some(true)
}

struct DirectParent {
    parent_id: String,
    role: Option<String>,
    type_name: Option<String>,
}

fn direct_parent_from_messages(
    conversation_id: &str,
    conv_brain_dir: &Path,
) -> Option<DirectParent> {
    let messages_dir = conv_brain_dir.join(".system_generated").join("messages");
    let entries = fs::read_dir(&messages_dir).ok()?;
    let mut candidates: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .take(MAX_MESSAGE_DIR_SCAN)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json")
                || path.file_name().and_then(|name| name.to_str()) == Some("read.json")
            {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, path))
        })
        .collect();
    // `agy` 1.5.16 names message files with random UUIDv4s and writes the
    // `invoke_subagent` spawn message before child `SessionStart`, so sorting
    // by earliest modification time keeps the spawn message first even in busy
    // subagent directories.
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, path) in candidates.into_iter().take(MAX_MESSAGE_FILES) {
        let Some(file) = open_owned_regular(&path) else {
            continue;
        };
        let Ok(bytes) = read_bounded(&file, MAX_MESSAGE_BYTES) else {
            continue;
        };
        if bytes.len() > MAX_MESSAGE_BYTES
            || !bytes
                .windows(b"invoke_subagent".len())
                .any(|window| window == b"invoke_subagent")
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(tool) = value
            .get("sourceMetadata")
            .and_then(|meta| meta.get("tool"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(tool_call) = tool.get("toolCall").and_then(Value::as_object) else {
            continue;
        };
        if tool_call.get("name").and_then(Value::as_str) != Some("invoke_subagent") {
            continue;
        }
        let raw_parent = value
            .get("sender")
            .and_then(Value::as_str)
            .filter(|sender| !sender.is_empty())
            .or_else(|| {
                tool.get("conversationId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
            });
        let Some(parent_id) = raw_parent
            .filter(|id| valid_conversation_segment(id) && *id != conversation_id)
            .map(str::to_owned)
        else {
            continue;
        };
        let (role, type_name) = tool_call
            .get("argumentsJson")
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .map(|args| {
                let role = args
                    .get("Role")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .and_then(|text| safe_text(text, "Role").ok());
                let type_name = args
                    .get("TypeName")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .and_then(|text| safe_text(text, "TypeName").ok());
                (role, type_name)
            })
            .unwrap_or((None, None));
        return Some(DirectParent {
            parent_id,
            role,
            type_name,
        });
    }
    None
}

/// Resolve a subagent conversation's direct parent, root lead conversation id,
/// and `Role`/`TypeName` from `<artifactDir>/.system_generated/messages/*.json`.
/// Walks upward through `<brain>/<parentId>/.system_generated/messages/*.json`
/// so nested grandchildren map to the pane's root lead session. Fails closed
/// (`None`) on invalid segments, cycles, or depth beyond `MAX_SUBAGENT_DEPTH`.
pub fn resolve_subagent_lineage(
    conversation_id: &str,
    artifact_dir: &Path,
) -> Option<SubagentLineage> {
    if !artifact_dir.is_absolute() || !valid_conversation_segment(conversation_id) {
        return None;
    }
    let brain_dir = artifact_dir.parent()?;
    if brain_dir.file_name().and_then(|name| name.to_str()) != Some("brain") {
        return None;
    }
    let first = direct_parent_from_messages(conversation_id, artifact_dir)?;
    let mut visited = BTreeSet::new();
    visited.insert(conversation_id.to_owned());
    visited.insert(first.parent_id.clone());
    let mut root_id = first.parent_id;
    for _ in 1..MAX_SUBAGENT_DEPTH {
        let next_dir = brain_dir.join(&root_id);
        let Some(next) = direct_parent_from_messages(&root_id, &next_dir) else {
            return Some(SubagentLineage {
                root_session_id: root_id,
                role: first.role,
                type_name: first.type_name,
            });
        };
        if !visited.insert(next.parent_id.clone()) {
            return None;
        }
        root_id = next.parent_id;
    }
    // Confirm the ancestor at MAX_SUBAGENT_DEPTH is actually a root with no parent.
    let final_dir = brain_dir.join(&root_id);
    if direct_parent_from_messages(&root_id, &final_dir).is_some() {
        return None;
    }
    Some(SubagentLineage {
        root_session_id: root_id,
        role: first.role,
        type_name: first.type_name,
    })
}

/// Check that a SQLite database path is a regular file owned by the effective
/// user and not a symlink.
#[cfg(target_os = "macos")]
fn check_owned_regular_path(path: &Path) -> Result<(), Reading> {
    if !path.is_absolute() {
        return Err(Reading::Unreadable(Unreadable::Foreign));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Reading::Absent),
        Err(_) => return Err(Reading::Unreadable(Unreadable::ReadFailed)),
    };
    if !metadata.file_type().is_file() || metadata.uid() != crate::identity::effective_uid() {
        return Err(Reading::Unreadable(Unreadable::Foreign));
    }
    Ok(())
}

/// Read the current status of `agy` conversation `conversation_id` from
/// `summaries_db` (`conversation_summaries.db`) and optionally `conv_db`
/// (`conversations/<id>.db`), taking `initial_step_count` as the first step
/// index that can belong to the followed turn.
pub fn read_status(
    summaries_db: &Path,
    conv_db: Option<&Path>,
    conversation_id: &str,
    initial_step_count: i64,
) -> Reading {
    platform::read_status(summaries_db, conv_db, conversation_id, initial_step_count)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString, OsString, c_char, c_int, c_void};
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::ptr;

    use super::{Reading, Unreadable, check_owned_regular_path};

    #[repr(C)]
    struct Sqlite3 {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct Sqlite3Stmt {
        _private: [u8; 0],
    }

    const SQLITE_OK: c_int = 0;
    const SQLITE_INTEGER: c_int = 1;
    const SQLITE_CANTOPEN: c_int = 14;
    const SQLITE_ROW: c_int = 100;
    const SQLITE_DONE: c_int = 101;
    const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
    const SQLITE_OPEN_URI: c_int = 0x0000_0040;
    const SQLITE_OPEN_NOFOLLOW: c_int = 0x0100_0000;
    // SQLITE_TRANSIENT tells SQLite to copy the bound text immediately.
    const SQLITE_TRANSIENT: isize = -1;

    // `CORTEX_STEP_STATUS_*` enum values verified from `agy` 1.5.16's protobuf
    // descriptors (`conversations/<id>.db` `steps.status`):
    //   1 = PENDING, 2 = RUNNING, 8 = GENERATING, 9 = WAITING, 11 = QUEUED
    //   6 = CANCELED, 12 = INTERRUPTED
    //   0 = UNSPECIFIED, 3 = DONE, 4 = INVALID, 5 = CLEARED, 7 = ERROR
    const STEP_STATUS_UNSPECIFIED: i64 = 0;
    const STEP_STATUS_PENDING: i64 = 1;
    const STEP_STATUS_RUNNING: i64 = 2;
    const STEP_STATUS_DONE: i64 = 3;
    const STEP_STATUS_INVALID: i64 = 4;
    const STEP_STATUS_CLEARED: i64 = 5;
    const STEP_STATUS_CANCELED: i64 = 6;
    const STEP_STATUS_ERROR: i64 = 7;
    const STEP_STATUS_GENERATING: i64 = 8;
    const STEP_STATUS_WAITING: i64 = 9;
    const STEP_STATUS_QUEUED: i64 = 11;
    const STEP_STATUS_INTERRUPTED: i64 = 12;

    #[link(name = "sqlite3")]
    unsafe extern "C" {
        fn sqlite3_open_v2(
            filename: *const c_char,
            pp_db: *mut *mut Sqlite3,
            flags: c_int,
            z_vfs: *const c_char,
        ) -> c_int;
        fn sqlite3_close_v2(db: *mut Sqlite3) -> c_int;
        fn sqlite3_busy_timeout(db: *mut Sqlite3, ms: c_int) -> c_int;
        fn sqlite3_prepare_v2(
            db: *mut Sqlite3,
            z_sql: *const c_char,
            n_byte: c_int,
            pp_stmt: *mut *mut Sqlite3Stmt,
            pz_tail: *mut *const c_char,
        ) -> c_int;
        fn sqlite3_bind_text(
            stmt: *mut Sqlite3Stmt,
            idx: c_int,
            text: *const c_char,
            n_bytes: c_int,
            destructor: *const c_void,
        ) -> c_int;
        fn sqlite3_bind_int64(stmt: *mut Sqlite3Stmt, idx: c_int, val: i64) -> c_int;
        fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
        fn sqlite3_column_type(stmt: *mut Sqlite3Stmt, col: c_int) -> c_int;
        fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, col: c_int) -> *const u8;
        fn sqlite3_column_int64(stmt: *mut Sqlite3Stmt, col: c_int) -> i64;
        fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    }

    struct DbGuard(*mut Sqlite3);
    impl Drop for DbGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was returned by `sqlite3_open_v2` and is closed once here.
                unsafe {
                    let _ = sqlite3_close_v2(self.0);
                }
            }
        }
    }

    struct StmtGuard(*mut Sqlite3Stmt);
    impl Drop for StmtGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was prepared by `sqlite3_prepare_v2` and finalized once here.
                unsafe {
                    let _ = sqlite3_finalize(self.0);
                }
            }
        }
    }

    fn file_stamp(path: &Path) -> Option<(u64, u64, u64, i64, i64)> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_file() || meta.uid() != crate::identity::effective_uid() {
            return None;
        }
        Some((
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        ))
    }

    fn wal_absent_or_empty(db_path: &Path) -> bool {
        let mut raw = OsString::from(db_path.as_os_str());
        raw.push("-wal");
        let wal_path = PathBuf::from(raw);
        match fs::symlink_metadata(&wal_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(meta) => {
                meta.file_type().is_file()
                    && meta.uid() == crate::identity::effective_uid()
                    && meta.len() == 0
            }
            Err(_) => false,
        }
    }

    fn immutable_file_uri(path: &Path) -> Option<CString> {
        let text = path.to_str()?;
        let mut uri = String::with_capacity(text.len() + 24);
        uri.push_str("file:");
        for byte in text.bytes() {
            match byte {
                b'%' | b'?' | b'#' | b' ' | 0x00..=0x1F | 0x7F..=0xFF => {
                    uri.push('%');
                    let hi = byte >> 4;
                    let lo = byte & 0x0F;
                    uri.push(char::from(if hi < 10 { b'0' + hi } else { b'A' + hi - 10 }));
                    uri.push(char::from(if lo < 10 { b'0' + lo } else { b'A' + lo - 10 }));
                }
                _ => uri.push(char::from(byte)),
            }
        }
        uri.push_str("?immutable=1");
        CString::new(uri).ok()
    }

    fn open_with_flags(target: &CStr, flags: c_int) -> Result<DbGuard, c_int> {
        let mut db = ptr::null_mut();
        // SAFETY: `target` is NUL-terminated and `db` is a valid out-pointer.
        let rc = unsafe { sqlite3_open_v2(target.as_ptr(), &mut db, flags, ptr::null()) };
        let guard = DbGuard(db);
        if rc != SQLITE_OK || guard.0.is_null() {
            return Err(if rc == SQLITE_OK { SQLITE_CANTOPEN } else { rc });
        }
        // SAFETY: `guard.0` is an open sqlite3 handle.
        unsafe {
            let _ = sqlite3_busy_timeout(guard.0, 200);
        }
        Ok(guard)
    }

    /// Resolve parent directory symlinks (e.g. `/var` -> `/private/var` on
    /// macOS) without following the leaf filename itself: as of macOS
    /// libsqlite3 3.51.0, `SQLITE_OPEN_NOFOLLOW` fails with `SQLITE_CANTOPEN`
    /// if any ancestor component is a symlink, whereas canonicalizing only the
    /// parent lets `SQLITE_OPEN_NOFOLLOW` enforce atomic `O_NOFOLLOW` on the
    /// database file itself.
    fn resolve_parent_keeping_leaf(path: &Path) -> Result<PathBuf, Unreadable> {
        let parent = path.parent().ok_or(Unreadable::ReadFailed)?;
        let leaf = path.file_name().ok_or(Unreadable::ReadFailed)?;
        let real_parent = fs::canonicalize(parent).map_err(|_| Unreadable::ReadFailed)?;
        Ok(real_parent.join(leaf))
    }

    /// Run `query` against a read-only connection to `path`.
    ///
    /// As of `agy` 1.5.16, SQLite databases use `journal_mode=wal`: when the
    /// last writer closes cleanly, SQLite deletes `-wal` and `-shm` while the
    /// database header stays in WAL format, so a read-only open returns
    /// `SQLITE_CANTOPEN` on `prepare`/`step` because it cannot create `-shm`.
    /// When `SQLITE_CANTOPEN` occurs and `-wal` is absent or empty, re-read
    /// via `file:<path>?immutable=1` and verify before returning that `-wal`
    /// stayed absent/empty and the main database file's metadata did not move.
    fn with_readonly_db<T>(
        path: &Path,
        query: impl Fn(*mut Sqlite3) -> Result<T, c_int>,
    ) -> Result<T, Unreadable> {
        let resolved = resolve_parent_keeping_leaf(path)?;
        if let Err(reading) = check_owned_regular_path(&resolved) {
            return Err(match reading {
                Reading::Unreadable(reason) => reason,
                Reading::Working | Reading::Idle | Reading::Canceled | Reading::Absent => {
                    Unreadable::ReadFailed
                }
            });
        }
        let c_path = CString::new(resolved.to_str().ok_or(Unreadable::ReadFailed)?)
            .map_err(|_| Unreadable::ReadFailed)?;
        let primary = open_with_flags(&c_path, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOFOLLOW)
            .and_then(|db| query(db.0));
        match primary {
            Ok(value) => Ok(value),
            Err(SQLITE_CANTOPEN) if wal_absent_or_empty(&resolved) => {
                let before = file_stamp(&resolved).ok_or(Unreadable::ReadFailed)?;
                let uri = immutable_file_uri(&resolved).ok_or(Unreadable::ReadFailed)?;
                let db = open_with_flags(
                    &uri,
                    SQLITE_OPEN_READONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(|_| Unreadable::ReadFailed)?;
                let value = query(db.0).map_err(|_| Unreadable::ReadFailed)?;
                drop(db);
                if !wal_absent_or_empty(&resolved) || file_stamp(&resolved) != Some(before) {
                    return Err(Unreadable::ReadFailed);
                }
                Ok(value)
            }
            Err(_) => Err(Unreadable::ReadFailed),
        }
    }

    enum SummaryRow {
        Working,
        Idle,
        Absent,
        Unreadable(Unreadable),
    }

    fn read_summary(summaries_db: &Path, conversation_id: &str) -> SummaryRow {
        if let Err(reading) = check_owned_regular_path(summaries_db) {
            return match reading {
                Reading::Absent => SummaryRow::Absent,
                Reading::Unreadable(reason) => SummaryRow::Unreadable(reason),
                Reading::Working | Reading::Idle | Reading::Canceled => {
                    SummaryRow::Unreadable(Unreadable::ReadFailed)
                }
            };
        }
        let Ok(cid_len) = c_int::try_from(conversation_id.len()) else {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        };
        let query = |db: *mut Sqlite3| -> Result<SummaryRow, c_int> {
            let sql = c"SELECT status, not_fully_idle FROM conversation_summaries WHERE conversation_id = ?1 LIMIT 1";
            let mut stmt = ptr::null_mut();
            // SAFETY: `db` is open, `sql` is a static NUL-terminated string, `stmt` is a valid out-pointer.
            let prep_rc =
                unsafe { sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
            if prep_rc != SQLITE_OK {
                return Err(prep_rc);
            }
            let stmt = StmtGuard(stmt);
            // SAFETY: `stmt.0` is valid, `conversation_id` bytes stay valid across bind with SQLITE_TRANSIENT.
            let bind_rc = unsafe {
                sqlite3_bind_text(
                    stmt.0,
                    1,
                    conversation_id.as_ptr().cast::<c_char>(),
                    cid_len,
                    SQLITE_TRANSIENT as *const c_void,
                )
            };
            if bind_rc != SQLITE_OK {
                return Err(bind_rc);
            }
            // SAFETY: `stmt.0` is a bound prepared statement.
            let step_rc = unsafe { sqlite3_step(stmt.0) };
            if step_rc == SQLITE_DONE {
                return Ok(SummaryRow::Absent);
            }
            if step_rc != SQLITE_ROW {
                return Err(step_rc);
            }
            // SAFETY: `stmt.0` returned SQLITE_ROW; column 0 is text (or null), column 1 is integer.
            let status_ptr = unsafe { sqlite3_column_text(stmt.0, 0) };
            if status_ptr.is_null() {
                return Ok(SummaryRow::Unreadable(Unreadable::UnknownStatus));
            }
            // SAFETY: SQLite guarantees `sqlite3_column_text` returns a NUL-terminated UTF-8 string
            // valid until the next step or finalize.
            let Ok(status) = unsafe { CStr::from_ptr(status_ptr.cast::<c_char>()) }.to_str() else {
                return Ok(SummaryRow::Unreadable(Unreadable::UnknownStatus));
            };
            // SAFETY: `stmt.0` returned SQLITE_ROW; column 1 is `not_fully_idle`.
            let not_fully_idle = unsafe { sqlite3_column_int64(stmt.0, 1) };
            Ok(match status {
                "CASCADE_RUN_STATUS_RUNNING"
                | "CASCADE_RUN_STATUS_BUSY"
                | "CASCADE_RUN_STATUS_CANCELING" => SummaryRow::Working,
                "CASCADE_RUN_STATUS_IDLE" => {
                    if not_fully_idle != 0 {
                        SummaryRow::Working
                    } else {
                        SummaryRow::Idle
                    }
                }
                _ => SummaryRow::Unreadable(Unreadable::UnknownStatus),
            })
        };
        match with_readonly_db(summaries_db, query) {
            Ok(row) => row,
            Err(reason) => SummaryRow::Unreadable(reason),
        }
    }

    enum StepsState {
        Active,
        Canceled,
        Quiescent,
        Absent,
        Unreadable(Unreadable),
    }

    fn read_conv_steps(conv_db: &Path, initial_step_count: i64) -> StepsState {
        if let Err(reading) = check_owned_regular_path(conv_db) {
            return match reading {
                Reading::Absent => StepsState::Absent,
                Reading::Unreadable(reason) => StepsState::Unreadable(reason),
                Reading::Working | Reading::Idle | Reading::Canceled => {
                    StepsState::Unreadable(Unreadable::ReadFailed)
                }
            };
        }
        let query = |db: *mut Sqlite3| -> Result<StepsState, c_int> {
            let sql = c"SELECT status FROM steps WHERE idx >= ?1 ORDER BY idx ASC";
            let mut stmt = ptr::null_mut();
            // SAFETY: `db` is open, `sql` is a static NUL-terminated string, `stmt` is a valid out-pointer.
            let prep_rc =
                unsafe { sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
            if prep_rc != SQLITE_OK {
                return Err(prep_rc);
            }
            let stmt = StmtGuard(stmt);
            // SAFETY: `stmt.0` is valid.
            let bind_rc = unsafe { sqlite3_bind_int64(stmt.0, 1, initial_step_count) };
            if bind_rc != SQLITE_OK {
                return Err(bind_rc);
            }
            let mut saw_canceled = false;
            loop {
                // SAFETY: `stmt.0` is a bound prepared statement.
                match unsafe { sqlite3_step(stmt.0) } {
                    SQLITE_ROW => {
                        // SAFETY: `stmt.0` returned SQLITE_ROW; column 0 is `status`.
                        if unsafe { sqlite3_column_type(stmt.0, 0) } != SQLITE_INTEGER {
                            return Ok(StepsState::Unreadable(Unreadable::UnknownStatus));
                        }
                        // SAFETY: column 0 was just confirmed to hold SQLITE_INTEGER.
                        let st = unsafe { sqlite3_column_int64(stmt.0, 0) };
                        match st {
                            STEP_STATUS_PENDING
                            | STEP_STATUS_RUNNING
                            | STEP_STATUS_GENERATING
                            | STEP_STATUS_WAITING
                            | STEP_STATUS_QUEUED => return Ok(StepsState::Active),
                            STEP_STATUS_CANCELED | STEP_STATUS_INTERRUPTED => {
                                saw_canceled = true;
                            }
                            STEP_STATUS_UNSPECIFIED
                            | STEP_STATUS_DONE
                            | STEP_STATUS_INVALID
                            | STEP_STATUS_CLEARED
                            | STEP_STATUS_ERROR => {}
                            _ => return Ok(StepsState::Unreadable(Unreadable::UnknownStatus)),
                        }
                    }
                    SQLITE_DONE => break,
                    rc => return Err(rc),
                }
            }
            Ok(if saw_canceled {
                StepsState::Canceled
            } else {
                StepsState::Quiescent
            })
        };
        match with_readonly_db(conv_db, query) {
            Ok(state) => state,
            Err(reason) => StepsState::Unreadable(reason),
        }
    }

    pub(super) fn read_status(
        summaries_db: &Path,
        conv_db: Option<&Path>,
        conversation_id: &str,
        initial_step_count: i64,
    ) -> Reading {
        let steps_state = conv_db
            .map(|path| read_conv_steps(path, initial_step_count))
            .unwrap_or(StepsState::Absent);
        match steps_state {
            StepsState::Active => return Reading::Working,
            StepsState::Canceled => return Reading::Canceled,
            StepsState::Unreadable(reason) => return Reading::Unreadable(reason),
            StepsState::Quiescent | StepsState::Absent => {}
        }
        match read_summary(summaries_db, conversation_id) {
            SummaryRow::Working => Reading::Working,
            SummaryRow::Idle => Reading::Idle,
            SummaryRow::Absent => Reading::Absent,
            SummaryRow::Unreadable(reason) => Reading::Unreadable(reason),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::Path;

    use super::{Reading, Unreadable};

    pub(super) fn read_status(
        _summaries_db: &Path,
        _conv_db: Option<&Path>,
        _conversation_id: &str,
        _initial_step_count: i64,
    ) -> Reading {
        Reading::Unreadable(Unreadable::PlatformUnsupported)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        Reading, ReplyRead, SubagentLineage, Unreadable, app_data_dir, conversation_db_path,
        is_user_prompt_invocation, last_reply_from_transcript, read_status,
        resolve_subagent_lineage, summaries_db_path,
    };

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("attention-agy-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tail_reply_reads_normal_continuation_and_stops_at_error_message() {
        let dir = scratch("reply");
        let tx = dir.join("transcript_full.jsonl");
        let lines = [
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": ""}),
            json!({"step_index": 2, "type": "GENERIC", "status": "DONE", "content": "tool"}),
            json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "FIRST_REPLY"}),
        ];
        fs::write(
            &tx,
            lines
                .iter()
                .map(|v| serde_json::to_string(v).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        assert_eq!(
            last_reply_from_transcript(&tx, 1024),
            ReplyRead::Available("FIRST_REPLY".into())
        );
        assert_eq!(last_reply_from_transcript(&tx, 4), ReplyRead::TooLarge);

        // Appending a terminated turn with ERROR_MESSAGE must not leak FIRST_REPLY.
        let with_err = format!(
            "{}{}\n{}\n",
            fs::read_to_string(&tx).unwrap(),
            json!({"step_index": 4, "type": "USER_INPUT", "status": "DONE", "content": "next"}),
            json!({"step_index": 5, "type": "ERROR_MESSAGE", "status": "DONE", "content": ""})
        );
        fs::write(&tx, with_err).unwrap();
        assert_eq!(last_reply_from_transcript(&tx, 1024), ReplyRead::Absent);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn tail_reply_handles_large_replies_invalid_utf8_and_corrupt_turn_boundary() {
        let dir = scratch("reply-edge");
        let tx = dir.join("transcript_full.jsonl");

        // >512 KiB valid reply succeeds when within max_bytes and reports TooLarge when over.
        let big = "a".repeat(600 * 1024);
        fs::write(
            &tx,
            format!(
                "{}\n{}\n",
                json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"}),
                json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": big})
            ),
        )
        .unwrap();
        assert_eq!(
            last_reply_from_transcript(&tx, 1_048_576),
            ReplyRead::Available(big.clone())
        );
        assert_eq!(
            last_reply_from_transcript(&tx, 600 * 1024 - 1),
            ReplyRead::TooLarge
        );

        // Corrupt USER_INPUT line between an earlier reply and an empty current
        // PLANNER_RESPONSE must stop scanning and never leak the earlier reply.
        fs::write(
            &tx,
            format!(
                "{}\n{}\n{{\"step_index\":2,\"type\":\"USER_INPUT\",BROKEN\n{}\n",
                json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "turn1"}),
                json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "PRIOR_TURN_REPLY"}),
                json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": ""})
            ),
        )
        .unwrap();
        assert_eq!(last_reply_from_transcript(&tx, 1024), ReplyRead::Absent);

        // Invalid UTF-8 in the tail fails closed for both reply and prompt checks.
        let mut raw = format!(
            "{}\n{}\n",
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "turn1"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "PRIOR_TURN_REPLY"})
        )
        .into_bytes();
        raw.extend_from_slice(b"{\"type\":\"USER_INPUT\",\"content\":\"\xff\xfe\"}\n");
        fs::write(&tx, raw).unwrap();
        assert_eq!(last_reply_from_transcript(&tx, 1024), ReplyRead::Absent);
        assert_eq!(is_user_prompt_invocation(&tx), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn user_prompt_invocation_distinguishes_prompts_from_wakeups() {
        let dir = scratch("preinv");
        let tx = dir.join("transcript_full.jsonl");
        assert_eq!(is_user_prompt_invocation(&tx), None);

        // Turn 1: ends on USER_INPUT
        fs::write(
            &tx,
            format!(
                "{}\n",
                json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE"})
            ),
        )
        .unwrap();
        assert_eq!(is_user_prompt_invocation(&tx), Some(true));

        // SessionStart injected SYSTEM_MESSAGE before any PLANNER_RESPONSE -> still user prompt
        fs::write(
            &tx,
            format!(
                "{}\n{}\n",
                json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE"}),
                json!({"step_index": 1, "type": "SYSTEM_MESSAGE", "status": "DONE"})
            ),
        )
        .unwrap();
        assert_eq!(is_user_prompt_invocation(&tx), Some(true));

        // Background subagent wakeup: PLANNER_RESPONSE followed by SYSTEM_MESSAGE -> NOT a user prompt
        fs::write(
            &tx,
            format!(
                "{}\n{}\n{}\n",
                json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE"}),
                json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "LAUNCHED"}),
                json!({"step_index": 2, "type": "SYSTEM_MESSAGE", "status": "DONE"})
            ),
        )
        .unwrap();
        assert_eq!(is_user_prompt_invocation(&tx), Some(false));

        // Turn 2: new USER_INPUT appended -> user prompt again
        let next = format!(
            "{}{}\n",
            fs::read_to_string(&tx).unwrap(),
            json!({"step_index": 3, "type": "USER_INPUT", "status": "DONE"})
        );
        fs::write(&tx, next).unwrap();
        assert_eq!(is_user_prompt_invocation(&tx), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn subagent_lineage_walks_grandchild_to_root_lead() {
        let dir = scratch("lineage");
        let brain = dir.join("brain");
        let lead = "lead-1111";
        let child = "child-2222";
        let grand = "grand-3333";

        let write_spawn_msg = |recipient: &str, sender: &str, role: &str, type_name: &str| {
            let mdir = brain
                .join(recipient)
                .join(".system_generated")
                .join("messages");
            fs::create_dir_all(&mdir).unwrap();
            let args = json!({"Role": role, "TypeName": type_name}).to_string();
            fs::write(
                mdir.join("0001.json"),
                serde_json::to_string(&json!({
                    "sender": sender,
                    "recipient": recipient,
                    "sourceMetadata": {
                        "tool": {
                            "conversationId": sender,
                            "toolCall": {
                                "id": "call_1",
                                "name": "invoke_subagent",
                                "argumentsJson": args
                            }
                        }
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        };

        fs::create_dir_all(brain.join(lead)).unwrap();
        write_spawn_msg(child, lead, "Coordinator", "self");
        write_spawn_msg(grand, child, "Worker", "research");

        assert_eq!(resolve_subagent_lineage(lead, &brain.join(lead)), None);
        assert_eq!(
            resolve_subagent_lineage(child, &brain.join(child)),
            Some(SubagentLineage {
                root_session_id: lead.into(),
                role: Some("Coordinator".into()),
                type_name: Some("self".into()),
            })
        );
        assert_eq!(
            resolve_subagent_lineage(grand, &brain.join(grand)),
            Some(SubagentLineage {
                root_session_id: lead.into(),
                role: Some("Worker".into()),
                type_name: Some("research".into()),
            })
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn subagent_lineage_rejects_traversal_cycles_and_excess_depth() {
        let dir = scratch("lineage-guard");
        let brain = dir.join("brain");
        let write_spawn = |recipient: &str, sender: &str| {
            let mdir = brain
                .join(recipient)
                .join(".system_generated")
                .join("messages");
            fs::create_dir_all(&mdir).unwrap();
            fs::write(
                mdir.join("spawn.json"),
                serde_json::to_string(&json!({
                    "sender": sender,
                    "sourceMetadata": {
                        "tool": {
                            "conversationId": sender,
                            "toolCall": {
                                "name": "invoke_subagent",
                                "argumentsJson": "{\"Role\":\"Worker\",\"TypeName\":\"self\"}"
                            }
                        }
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        };

        // Path traversal or absolute sender ids fail closed.
        write_spawn("child-rel", "../escape");
        write_spawn("child-abs", "/tmp/escape");
        assert_eq!(
            resolve_subagent_lineage("child-rel", &brain.join("child-rel")),
            None
        );
        assert_eq!(
            resolve_subagent_lineage("child-abs", &brain.join("child-abs")),
            None
        );

        // Cyclic ancestry A -> B -> A fails closed.
        write_spawn("cyc-a", "cyc-b");
        write_spawn("cyc-b", "cyc-a");
        assert_eq!(
            resolve_subagent_lineage("cyc-a", &brain.join("cyc-a")),
            None
        );

        // Depth > MAX_SUBAGENT_DEPTH (8) fails closed instead of returning an intermediate node.
        fs::create_dir_all(brain.join("deep-0")).unwrap();
        for idx in 1..=10 {
            write_spawn(&format!("deep-{idx}"), &format!("deep-{}", idx - 1));
        }
        assert_eq!(
            resolve_subagent_lineage("deep-10", &brain.join("deep-10")),
            None
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn subagent_lineage_finds_earliest_spawn_message_past_32_uuid_files() {
        let dir = scratch("lineage-many");
        let brain = dir.join("brain");
        let lead = "lead-many";
        let child = "child-many";
        fs::create_dir_all(brain.join(lead)).unwrap();
        let mdir = brain.join(child).join(".system_generated").join("messages");
        fs::create_dir_all(&mdir).unwrap();

        let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Lexicographically late filename, but earliest mtime holds invoke_subagent.
        let spawn_path = mdir.join("ffffffff-ffff-4fff-8fff-ffffffffffff.json");
        fs::write(
            &spawn_path,
            serde_json::to_string(&json!({
                "sender": lead,
                "sourceMetadata": {
                    "tool": {
                        "conversationId": lead,
                        "toolCall": {
                            "name": "invoke_subagent",
                            "argumentsJson": "{\"Role\":\"Scout\",\"TypeName\":\"research\"}"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&spawn_path)
            .unwrap()
            .set_modified(base_time)
            .unwrap();

        // Write 40 newer messages with lexicographically earlier filenames.
        for idx in 0..40u64 {
            let p = mdir.join(format!("00000000-0000-4000-8000-{idx:012}.json"));
            fs::write(
                &p,
                serde_json::to_string(&json!({
                    "sender": lead,
                    "sourceMetadata": { "tool": { "toolCall": { "name": "send_message" } } }
                }))
                .unwrap(),
            )
            .unwrap();
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(base_time + Duration::from_secs(idx + 1))
                .unwrap();
        }

        assert_eq!(
            resolve_subagent_lineage(child, &brain.join(child)),
            Some(SubagentLineage {
                root_session_id: lead.into(),
                role: Some("Scout".into()),
                type_name: Some("research".into()),
            })
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn app_data_paths_derive_from_transcript_or_home() {
        let tx = "/tmp/agy-app/brain/conv-1/.system_generated/logs/transcript_full.jsonl";
        assert_eq!(
            app_data_dir(Some(tx), Some("/Users/x")),
            Some(PathBuf::from("/tmp/agy-app"))
        );
        assert_eq!(
            summaries_db_path(Some(tx), Some("/Users/x")),
            Some(PathBuf::from("/tmp/agy-app/conversation_summaries.db"))
        );
        assert_eq!(
            conversation_db_path(Some(tx), Some("/Users/x"), "conv-1"),
            Some(PathBuf::from("/tmp/agy-app/conversations/conv-1.db"))
        );
        assert_eq!(
            conversation_db_path(Some(tx), Some("/Users/x"), "../escape"),
            None
        );
        assert_eq!(
            summaries_db_path(None, Some("/Users/x")),
            Some(PathBuf::from(
                "/Users/x/.gemini/antigravity-cli/conversation_summaries.db"
            ))
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_read_status_reports_platform_unsupported() {
        let dir = scratch("non-macos");
        assert_eq!(
            read_status(&dir.join("summaries.db"), None, "conv-1", 0),
            Reading::Unreadable(Unreadable::PlatformUnsupported)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sqlite_status_tracks_running_nfi_idle_and_canceled_steps() {
        use std::process::Command;

        let dir = scratch("sqlite");
        let summaries = dir.join("conversation_summaries.db");
        let conv = dir.join("conv-1.db");

        let run_sql = |db: &PathBuf, sql: &str| {
            let status = Command::new("/usr/bin/sqlite3")
                .arg(db)
                .arg(sql)
                .status()
                .expect("sqlite3 binary available on macOS");
            assert!(status.success(), "sqlite3 failed on {sql}");
        };

        run_sql(
            &summaries,
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT NOT NULL, not_fully_idle INTEGER NOT NULL);
             INSERT INTO conversation_summaries VALUES ('conv-1', 'CASCADE_RUN_STATUS_RUNNING', 1);",
        );
        run_sql(
            &conv,
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER NOT NULL);
             INSERT INTO steps VALUES (0, 3), (1, 8);",
        );
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-1", 1),
            Reading::Working
        );
        assert_eq!(
            read_status(&summaries, None, "missing-conv", 0),
            Reading::Absent
        );

        // Background subagent wait: IDLE with not_fully_idle=1 stays Working.
        run_sql(
            &summaries,
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=1 WHERE conversation_id='conv-1';",
        );
        run_sql(&conv, "UPDATE steps SET status=3 WHERE idx=1;");
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-1", 1),
            Reading::Working
        );

        // Esc during generation: step flips to status=6 (CANCELED) even if summaries lags.
        run_sql(&conv, "INSERT INTO steps VALUES (2, 6);");
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-1", 1),
            Reading::Canceled
        );

        // ERROR (7) is quiescent, not active: IDLE with not_fully_idle=0 -> Idle.
        run_sql(
            &summaries,
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='conv-1';",
        );
        run_sql(&conv, "INSERT INTO steps VALUES (3, 7);");
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-1", 3),
            Reading::Idle
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sqlite_wal_cleanly_closed_db_reads_working_idle_and_canceled() {
        use std::process::Command;

        let dir = scratch("sqlite-wal");
        let summaries = dir.join("conversation_summaries.db");
        let conv = dir.join("conv-wal.db");
        let run_sql_and_close_wal = |db: &PathBuf, sql: &str| {
            let out = Command::new("/usr/bin/sqlite3")
                .arg(db)
                .arg(format!("{sql}\nPRAGMA wal_checkpoint(TRUNCATE);"))
                .output()
                .expect("sqlite3 binary available on macOS");
            assert!(
                out.status.success(),
                "sqlite3 failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            // Match non-persisted WAL clean-close state (as `agy` 1.5.16 leaves
            // on disk when its writer closes: WAL header, no -wal/-shm sidecars).
            for suffix in ["-wal", "-shm"] {
                let sidecar = PathBuf::from(format!("{}{suffix}", db.display()));
                let _ = fs::remove_file(sidecar);
            }
        };

        run_sql_and_close_wal(
            &summaries,
            "PRAGMA journal_mode=WAL;
             CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT NOT NULL, not_fully_idle INTEGER NOT NULL);
             INSERT INTO conversation_summaries VALUES ('conv-wal', 'CASCADE_RUN_STATUS_RUNNING', 1);",
        );
        run_sql_and_close_wal(
            &conv,
            "PRAGMA journal_mode=WAL;
             CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER NOT NULL);
             INSERT INTO steps VALUES (0, 3), (1, 9);",
        );
        assert!(!PathBuf::from(format!("{}-wal", summaries.display())).exists());
        assert!(!PathBuf::from(format!("{}-wal", conv.display())).exists());
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-wal", 1),
            Reading::Working
        );

        run_sql_and_close_wal(
            &summaries,
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='conv-wal';",
        );
        run_sql_and_close_wal(&conv, "UPDATE steps SET status=6 WHERE idx=1;");
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-wal", 1),
            Reading::Canceled
        );

        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-wal", 2),
            Reading::Idle
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sqlite_unreadable_or_symlinked_dbs_fail_closed() {
        use std::os::unix::fs::symlink;
        use std::process::Command;

        let dir = scratch("sqlite-guard");
        let summaries = dir.join("conversation_summaries.db");
        let conv_corrupt = dir.join("conv-corrupt.db");
        let conv_real = dir.join("conv-real.db");
        let conv_link = dir.join("conv-link.db");
        let summaries_link = dir.join("summaries-link.db");

        let status = Command::new("/usr/bin/sqlite3")
            .arg(&summaries)
            .arg("CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT NOT NULL, not_fully_idle INTEGER NOT NULL);
                  INSERT INTO conversation_summaries VALUES ('conv-1', 'CASCADE_RUN_STATUS_IDLE', 0);")
            .status()
            .unwrap();
        assert!(status.success());

        // Corrupt conv_db must fail closed instead of falling through to Idle.
        fs::write(&conv_corrupt, b"not a sqlite database").unwrap();
        assert!(matches!(
            read_status(&summaries, Some(&conv_corrupt), "conv-1", 0),
            Reading::Unreadable(_)
        ));

        // Symlinked conv_db and summaries_db return Foreign.
        fs::write(&conv_real, b"").unwrap();
        symlink(&conv_real, &conv_link).unwrap();
        symlink(&summaries, &summaries_link).unwrap();
        assert_eq!(
            read_status(&summaries, Some(&conv_link), "conv-1", 0),
            Reading::Unreadable(Unreadable::Foreign)
        );
        assert_eq!(
            read_status(&summaries_link, None, "conv-1", 0),
            Reading::Unreadable(Unreadable::Foreign)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn transcript_readers_fail_closed_on_missing_type_and_newline_aligned_truncation() {
        let dir = scratch("tx-fail-closed");
        let tx = dir.join("transcript_full.jsonl");

        // JSON object missing/non-string "type" stops reverse reply scan instead
        // of falling through to PRIOR_REPLY.
        for bad_row in [
            json!({"step_index": 2, "status": "DONE", "content": "missing type"}),
            json!({"step_index": 2, "type": null, "status": "DONE"}),
            json!({"step_index": 2, "type": 42, "status": "DONE"}),
        ] {
            fs::write(
                &tx,
                format!(
                    "{}\n{}\n{}\n{}\n",
                    json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "t1"}),
                    json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "PRIOR_REPLY"}),
                    bad_row,
                    json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": ""})
                ),
            )
            .unwrap();
            assert_eq!(last_reply_from_transcript(&tx, 1024), ReplyRead::Absent);
        }

        // >512 KiB transcript where truncation lands right on a newline boundary
        // after an earlier PLANNER_RESPONSE must return None (never Some(true)).
        let earlier = format!(
            "{}\n{}\n",
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "t1"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "done"})
        );
        let tail = format!(
            "{}\n{}\n",
            json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "content": "x".repeat(600 * 1024)}),
            json!({"step_index": 3, "type": "SYSTEM_MESSAGE", "status": "DONE", "content": "msg"})
        );
        // Pad earlier so size - 512 KiB lands on an exact '\n' byte.
        let mut content = earlier;
        let target_prefix = 64 * 1024;
        let pad_len = target_prefix - content.len() - 1;
        content.push_str(&format!(
            "{}\n",
            json!({"step_index": 99, "type": "GENERIC", "content": "p".repeat(pad_len.saturating_sub(50))})
        ));
        content.push_str(&tail);
        fs::write(&tx, &content).unwrap();
        assert_eq!(is_user_prompt_invocation(&tx), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sqlite_non_ascii_wal_paths_unknown_statuses_and_nonempty_wal_guard() {
        use std::process::Command;

        let dir = scratch("sqlite-utf8-status");
        let nested = dir.join("café 用户 %20#?");
        fs::create_dir_all(&nested).unwrap();
        let summaries = nested.join("conversation_summaries.db");
        let conv = nested.join("conv-utf8.db");

        let run_sql_and_close_wal = |db: &PathBuf, sql: &str| {
            let out = Command::new("/usr/bin/sqlite3")
                .arg(db)
                .arg(format!("{sql}\nPRAGMA wal_checkpoint(TRUNCATE);"))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "sqlite3 failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            for suffix in ["-wal", "-shm"] {
                let _ = fs::remove_file(PathBuf::from(format!("{}{suffix}", db.display())));
            }
        };

        run_sql_and_close_wal(
            &summaries,
            "PRAGMA journal_mode=WAL;
             CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
             INSERT INTO conversation_summaries VALUES ('conv-utf8', 'CASCADE_RUN_STATUS_IDLE', 0);",
        );
        run_sql_and_close_wal(
            &conv,
            "PRAGMA journal_mode=WAL;
             CREATE TABLE steps (idx INTEGER PRIMARY KEY, status);
             INSERT INTO steps VALUES (0, 3), (1, 9);",
        );
        // Cleanly-closed WAL under non-ASCII UTF-8 + URI-special characters reads cleanly.
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-utf8", 1),
            Reading::Working
        );

        // Non-empty -wal in a read-only directory forces SQLITE_CANTOPEN on the
        // primary handle (cannot create -shm) and refuses the immutable=1 fallback.
        use std::os::unix::fs::PermissionsExt;
        let conv_wal = PathBuf::from(format!("{}-wal", conv.display()));
        fs::write(&conv_wal, vec![0x37u8; 4096]).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o555)).unwrap();
        let wal_guard_reading = read_status(&summaries, Some(&conv), "conv-utf8", 1);
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_file(&conv_wal);
        assert_eq!(
            wal_guard_reading,
            Reading::Unreadable(Unreadable::ReadFailed)
        );

        // Unknown integer (99), TEXT, and NULL in steps.status fail closed as UnknownStatus
        // (both alone and alongside a CANCELED=6 step).
        for bad_sql in [
            "DELETE FROM steps; INSERT INTO steps VALUES (1, 99);",
            "DELETE FROM steps; INSERT INTO steps VALUES (1, 6), (2, 99);",
            "DELETE FROM steps; INSERT INTO steps VALUES (1, 'CORTEX_STEP_STATUS_WAITING');",
            "DELETE FROM steps; INSERT INTO steps VALUES (1, NULL);",
        ] {
            run_sql_and_close_wal(&conv, bad_sql);
            assert_eq!(
                read_status(&summaries, Some(&conv), "conv-utf8", 1),
                Reading::Unreadable(Unreadable::UnknownStatus),
                "expected UnknownStatus for {bad_sql}"
            );
        }

        // NULL and unknown text in conversation_summaries.status fail closed as UnknownStatus.
        run_sql_and_close_wal(&conv, "DELETE FROM steps; INSERT INTO steps VALUES (1, 3);");
        for bad_summary in [
            "UPDATE conversation_summaries SET status=NULL WHERE conversation_id='conv-utf8';",
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_FUTURE' WHERE conversation_id='conv-utf8';",
        ] {
            run_sql_and_close_wal(&summaries, bad_summary);
            assert_eq!(
                read_status(&summaries, Some(&conv), "conv-utf8", 1),
                Reading::Unreadable(Unreadable::UnknownStatus),
                "expected UnknownStatus for {bad_summary}"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }
}
