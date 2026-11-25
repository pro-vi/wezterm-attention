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

use serde_json::Value;

use crate::providers::safe_text;
use crate::records::read_bounded;

/// The `source_event` of a turn end read from `agy`'s status databases; see
/// [`crate::turn_follower::FOLLOWER_SOURCES`].
pub const AGY_STATUS: &str = "agy_status";

/// Tail window read from `transcript_full.jsonl` when inspecting the latest
/// reply or prompt step. Large enough for big replies without scanning multi-MB
/// transcripts from byte 0.
const TAIL_WINDOW_BYTES: usize = 512 << 10;

/// Maximum size of one `.system_generated/messages/*.json` file inspected while
/// resolving subagent lineage.
const MAX_MESSAGE_BYTES: usize = 256 << 10;

/// Maximum number of message files inspected in one conversation directory.
const MAX_MESSAGE_FILES: usize = 32;

/// Maximum subagent ancestry depth walked toward the root lead session.
const MAX_SUBAGENT_DEPTH: usize = 8;

/// Result of extracting the final assistant reply from an `agy` transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyRead {
    Available(String),
    Absent,
    TooLarge,
}

/// Parent/root linkage recovered for an `agy` subagent conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentLineage {
    pub root_session_id: String,
    pub parent_session_id: String,
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
    /// (`6`) with no active step running.
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

/// Path to `conversations/<conversationId>.db` for an `agy` session.
pub fn conversation_db_path(
    transcript_path: Option<&str>,
    home: Option<&str>,
    conversation_id: &str,
) -> Option<PathBuf> {
    if conversation_id.is_empty()
        || conversation_id.contains('/')
        || conversation_id.contains('\\')
        || conversation_id.contains('\0')
        || conversation_id == "."
        || conversation_id == ".."
    {
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

/// Parse complete JSON objects from the trailing `TAIL_WINDOW_BYTES` of a JSONL
/// transcript file. Returns `None` when the file is absent, foreign, or holds
/// no decodable objects.
fn read_transcript_tail_steps(path: &Path) -> Option<Vec<Value>> {
    let mut file = open_owned_regular(path)?;
    let size = file.metadata().ok()?.len();
    let window = u64::try_from(TAIL_WINDOW_BYTES).unwrap_or(u64::MAX);
    let truncated_head = size > window;
    let offset = size.saturating_sub(window);
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).ok()?;
    }
    let mut buf = Vec::new();
    let take = u64::try_from(TAIL_WINDOW_BYTES).unwrap_or(u64::MAX);
    file.take(take).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if truncated_head {
        let _ = lines.next();
    }
    let mut steps = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed)
            && value.is_object()
        {
            steps.push(value);
        }
    }
    (!steps.is_empty()).then_some(steps)
}

/// Extract the latest assistant reply written for the current turn in
/// `transcript_full.jsonl`. Stops at `USER_INPUT` or `ERROR_MESSAGE` so a
/// terminated turn never leaks an earlier turn's reply.
pub fn last_reply_from_transcript(path: &Path, max_text_bytes: usize) -> ReplyRead {
    let Some(steps) = read_transcript_tail_steps(path) else {
        return ReplyRead::Absent;
    };
    for step in steps.iter().rev() {
        match step.get("type").and_then(Value::as_str) {
            Some("USER_INPUT" | "ERROR_MESSAGE") => return ReplyRead::Absent,
            Some("PLANNER_RESPONSE") => {
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
/// Returns `None` when the file is absent or unreadable (allowing synthetic
/// callers without a transcript file on disk to fall back to payload fields).
pub fn is_user_prompt_invocation(path: &Path) -> Option<bool> {
    let steps = read_transcript_tail_steps(path)?;
    let last_type = steps.last()?.get("type").and_then(Value::as_str)?;
    if last_type == "USER_INPUT" {
        return Some(true);
    }
    if last_type == "SYSTEM_MESSAGE" {
        // A `SessionStart` hook that injects a `systemMessage` places it after
        // the initial `USER_INPUT` before any `PLANNER_RESPONSE` exists. By
        // contrast, a background subagent or command wakeup appends
        // `SYSTEM_MESSAGE` after an earlier `PLANNER_RESPONSE`.
        let has_prior_planner = steps
            .iter()
            .any(|step| step.get("type").and_then(Value::as_str) == Some("PLANNER_RESPONSE"));
        if !has_prior_planner {
            let preceded_by_user = steps
                .iter()
                .rev()
                .find(|step| step.get("type").and_then(Value::as_str) != Some("SYSTEM_MESSAGE"))
                .and_then(|step| step.get("type").and_then(Value::as_str))
                == Some("USER_INPUT");
            return Some(preceded_by_user);
        }
    }
    Some(false)
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
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.file_name().and_then(|name| name.to_str()) != Some("read.json")
        })
        .collect();
    paths.sort();
    for path in paths.into_iter().take(MAX_MESSAGE_FILES) {
        let Some(file) = open_owned_regular(&path) else {
            continue;
        };
        let Ok(bytes) = read_bounded(&file, MAX_MESSAGE_BYTES) else {
            continue;
        };
        if bytes.len() > MAX_MESSAGE_BYTES {
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
            .and_then(|id| safe_text(id, "parentConversationId").ok())
            .filter(|id| id != conversation_id)
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
/// so nested grandchildren map to the pane's root lead session.
pub fn resolve_subagent_lineage(
    conversation_id: &str,
    artifact_dir: &Path,
) -> Option<SubagentLineage> {
    if !artifact_dir.is_absolute() {
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
    let mut root_id = first.parent_id.clone();
    for _ in 1..MAX_SUBAGENT_DEPTH {
        let next_dir = brain_dir.join(&root_id);
        let Some(next) = direct_parent_from_messages(&root_id, &next_dir) else {
            break;
        };
        if !visited.insert(next.parent_id.clone()) {
            break;
        }
        root_id = next.parent_id;
    }
    Some(SubagentLineage {
        root_session_id: root_id,
        parent_session_id: first.parent_id,
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
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::path::Path;
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
    const SQLITE_ROW: c_int = 100;
    const SQLITE_DONE: c_int = 101;
    const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
    // SQLITE_TRANSIENT tells SQLite to copy the bound text immediately.
    const SQLITE_TRANSIENT: isize = -1;

    // Step statuses observed in agy 1.5.16 `conversations/<id>.db` `steps.status`:
    //   2 = CORTEX_STEP_STATUS_RUNNING
    //   3 = CORTEX_STEP_STATUS_DONE
    //   6 = CORTEX_STEP_STATUS_CANCELED
    //   8 = CORTEX_STEP_STATUS_GENERATING
    //   9 = CORTEX_STEP_STATUS_WAITING (e.g. ask_question menu)
    const STEP_STATUS_PENDING: i64 = 1;
    const STEP_STATUS_RUNNING: i64 = 2;
    const STEP_STATUS_CANCELED: i64 = 6;
    const STEP_STATUS_WAITING_ALT: i64 = 7;
    const STEP_STATUS_GENERATING: i64 = 8;
    const STEP_STATUS_WAITING: i64 = 9;

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

    fn open_readonly(path: &Path) -> Result<DbGuard, Unreadable> {
        let c_path = CString::new(path.to_str().ok_or(Unreadable::ReadFailed)?)
            .map_err(|_| Unreadable::ReadFailed)?;
        let mut db = ptr::null_mut();
        // SAFETY: `c_path` is a valid NUL-terminated string and `db` is a valid out-pointer.
        let rc =
            unsafe { sqlite3_open_v2(c_path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, ptr::null()) };
        let guard = DbGuard(db);
        if rc != SQLITE_OK || guard.0.is_null() {
            return Err(Unreadable::ReadFailed);
        }
        // SAFETY: `guard.0` is an open sqlite3 handle.
        unsafe {
            let _ = sqlite3_busy_timeout(guard.0, 200);
        }
        Ok(guard)
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
        let db = match open_readonly(summaries_db) {
            Ok(db) => db,
            Err(reason) => return SummaryRow::Unreadable(reason),
        };
        let Ok(sql) = CString::new(
            "SELECT status, not_fully_idle FROM conversation_summaries WHERE conversation_id = ?1 LIMIT 1",
        ) else {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        };
        let mut stmt = ptr::null_mut();
        // SAFETY: `db.0` is open, `sql` is NUL-terminated, `stmt` is a valid out-pointer.
        if unsafe { sqlite3_prepare_v2(db.0, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) }
            != SQLITE_OK
        {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        }
        let stmt = StmtGuard(stmt);
        let Ok(cid_len) = c_int::try_from(conversation_id.len()) else {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        };
        // SAFETY: `stmt.0` is valid, `conversation_id` bytes stay valid across bind with SQLITE_TRANSIENT.
        if unsafe {
            sqlite3_bind_text(
                stmt.0,
                1,
                conversation_id.as_ptr().cast::<c_char>(),
                cid_len,
                SQLITE_TRANSIENT as *const c_void,
            )
        } != SQLITE_OK
        {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        }
        // SAFETY: `stmt.0` is a bound prepared statement.
        let step_rc = unsafe { sqlite3_step(stmt.0) };
        if step_rc == SQLITE_DONE {
            return SummaryRow::Absent;
        }
        if step_rc != SQLITE_ROW {
            return SummaryRow::Unreadable(Unreadable::ReadFailed);
        }
        // SAFETY: `stmt.0` returned SQLITE_ROW; column 0 is text (or null), column 1 is integer.
        let status_ptr = unsafe { sqlite3_column_text(stmt.0, 0) };
        if status_ptr.is_null() {
            return SummaryRow::Unreadable(Unreadable::UnknownStatus);
        }
        // SAFETY: SQLite guarantees `sqlite3_column_text` returns a NUL-terminated UTF-8 string
        // valid until the next step or finalize.
        let Ok(status) = unsafe { CStr::from_ptr(status_ptr.cast::<c_char>()) }.to_str() else {
            return SummaryRow::Unreadable(Unreadable::UnknownStatus);
        };
        // SAFETY: `stmt.0` returned SQLITE_ROW; column 1 is `not_fully_idle`.
        let not_fully_idle = unsafe { sqlite3_column_int64(stmt.0, 1) };
        match status {
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
        }
    }

    enum StepsState {
        Active,
        Canceled,
        Quiescent,
        Unavailable,
    }

    fn read_conv_steps(conv_db: &Path, initial_step_count: i64) -> StepsState {
        if check_owned_regular_path(conv_db).is_err() {
            return StepsState::Unavailable;
        }
        let Ok(db) = open_readonly(conv_db) else {
            return StepsState::Unavailable;
        };
        let Ok(sql) = CString::new("SELECT status FROM steps WHERE idx >= ?1 ORDER BY idx ASC")
        else {
            return StepsState::Unavailable;
        };
        let mut stmt = ptr::null_mut();
        // SAFETY: `db.0` is open, `sql` is NUL-terminated, `stmt` is a valid out-pointer.
        if unsafe { sqlite3_prepare_v2(db.0, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) }
            != SQLITE_OK
        {
            return StepsState::Unavailable;
        }
        let stmt = StmtGuard(stmt);
        // SAFETY: `stmt.0` is valid.
        if unsafe { sqlite3_bind_int64(stmt.0, 1, initial_step_count) } != SQLITE_OK {
            return StepsState::Unavailable;
        }
        let mut saw_canceled = false;
        loop {
            // SAFETY: `stmt.0` is a bound prepared statement.
            match unsafe { sqlite3_step(stmt.0) } {
                SQLITE_ROW => {
                    // SAFETY: column 0 is `status` integer.
                    let st = unsafe { sqlite3_column_int64(stmt.0, 0) };
                    if matches!(
                        st,
                        STEP_STATUS_PENDING
                            | STEP_STATUS_RUNNING
                            | STEP_STATUS_WAITING_ALT
                            | STEP_STATUS_GENERATING
                            | STEP_STATUS_WAITING
                    ) {
                        return StepsState::Active;
                    }
                    if st == STEP_STATUS_CANCELED {
                        saw_canceled = true;
                    }
                }
                SQLITE_DONE => break,
                _ => return StepsState::Unavailable,
            }
        }
        if saw_canceled {
            StepsState::Canceled
        } else {
            StepsState::Quiescent
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
            .unwrap_or(StepsState::Unavailable);
        match steps_state {
            StepsState::Active => return Reading::Working,
            StepsState::Canceled => return Reading::Canceled,
            StepsState::Quiescent | StepsState::Unavailable => {}
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

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        Reading, ReplyRead, SubagentLineage, app_data_dir, conversation_db_path,
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
                parent_session_id: lead.into(),
                role: Some("Coordinator".into()),
                type_name: Some("self".into()),
            })
        );
        assert_eq!(
            resolve_subagent_lineage(grand, &brain.join(grand)),
            Some(SubagentLineage {
                root_session_id: lead.into(),
                parent_session_id: child.into(),
                role: Some("Worker".into()),
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

        // Normal idle (no canceled step in this turn): IDLE with not_fully_idle=0 -> Idle.
        run_sql(
            &summaries,
            "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='conv-1';",
        );
        assert_eq!(
            read_status(&summaries, Some(&conv), "conv-1", 3),
            Reading::Idle
        );
        let _ = fs::remove_dir_all(dir);
    }
}
