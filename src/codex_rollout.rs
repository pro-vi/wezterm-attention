//! A Codex session rollout, read for one thing: whether the lead turn a hook
//! saw, or a lead turn after it, ended on a model error. As of Codex 0.162.0 a
//! turn that ends on a model error runs no hook, so its end is only in the
//! rollout, as a `task_complete` record that carries an `error` object.
//!
//! The rollout is Codex's own file, and every line of it is untrusted. Only
//! complete lines of two shapes, as Codex 0.162.0 and 0.163.0 write them, are
//! acted on; any other shape yields nothing, never a record.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::providers::safe_text;

/// The `source_event` a turn end read from the rollout carries; see
/// [`crate::turn_follower::FOLLOWER_SOURCES`].
pub const TASK_COMPLETE: &str = "task_complete";

/// The longest line read. A longer one is skipped unread.
const MAX_LINE_BYTES: usize = 4 << 20;

/// What the rollout says while a turn is followed. Any other end of that turn,
/// a `task_complete` without an error or a `turn_aborted`, yields nothing: its
/// own hook records it, or nothing does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnSignal {
    /// Its `task_complete` carries an error.
    Failed { error_code: Option<String> },
    /// Another lead turn started, so this one is over; this is its id.
    OtherTurnStarted { turn_id: String },
}

/// Why a rollout was not read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The path is a link, or names nothing that can be opened.
    NotOpened,
    NotRegularFile,
    NotOwned,
    /// The file is not named for the session.
    OtherName,
    /// Its first record names another session, or is not a session header.
    OtherSession,
    /// It is shorter than the offset read to.
    Shrunk,
    /// The path now names another file.
    Replaced,
    Unreadable,
}

/// One rollout, opened once and read through that descriptor.
pub struct Rollout {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
    /// Where the next unread line starts; a partial line is left unread.
    offset: u64,
    /// Whether the read stopped inside an overlong line, skipped to its end.
    skipping: bool,
}

impl Rollout {
    /// Open the rollout at `path` for `session_id`, to read from `offset`.
    pub fn open(path: &Path, session_id: &str, offset: u64) -> std::result::Result<Self, Refusal> {
        let file = OpenOptions::new()
            .read(true)
            // Without O_NONBLOCK a FIFO would hold the open until a writer came.
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Refusal::NotOpened)?;
        let metadata = file.metadata().map_err(|_| Refusal::Unreadable)?;
        if !metadata.file_type().is_file() {
            return Err(Refusal::NotRegularFile);
        }
        if metadata.uid() != crate::identity::effective_uid() {
            return Err(Refusal::NotOwned);
        }
        let suffix = format!("-{session_id}.jsonl");
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&suffix))
        {
            return Err(Refusal::OtherName);
        }
        let mut header = Vec::new();
        BufReader::new((&file).take(MAX_LINE_BYTES as u64 + 1))
            .read_until(b'\n', &mut header)
            .map_err(|_| Refusal::Unreadable)?;
        if header.pop() != Some(b'\n') {
            return Err(Refusal::OtherSession);
        }
        let header: Value = serde_json::from_slice(&header).map_err(|_| Refusal::OtherSession)?;
        if header.get("type").and_then(Value::as_str) != Some("session_meta")
            || header.pointer("/payload/id").and_then(Value::as_str) != Some(session_id)
        {
            return Err(Refusal::OtherSession);
        }
        if offset > metadata.len() {
            return Err(Refusal::Shrunk);
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
            offset,
            skipping: false,
        })
    }

    /// Read what Codex appended since the last read, and return the first
    /// thing a complete line says about `turn_id`: that it ended on an error,
    /// or that another lead turn started.
    pub fn next_signal(
        &mut self,
        turn_id: &str,
    ) -> std::result::Result<Option<TurnSignal>, Refusal> {
        let named = std::fs::symlink_metadata(&self.path).map_err(|_| Refusal::Replaced)?;
        if named.dev() != self.device || named.ino() != self.inode {
            return Err(Refusal::Replaced);
        }
        let length = self.file.metadata().map_err(|_| Refusal::Unreadable)?.len();
        if length < self.offset {
            return Err(Refusal::Shrunk);
        }
        if length == self.offset {
            return Ok(None);
        }
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(|_| Refusal::Unreadable)?;
        let mut reader = BufReader::new((&self.file).take(length - self.offset));
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = (&mut reader)
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|_| Refusal::Unreadable)?;
            if read == 0 {
                return Ok(None);
            }
            let complete = line.last() == Some(&b'\n');
            let consumed = u64::try_from(read).unwrap_or(u64::MAX);
            if self.skipping || (!complete && read > MAX_LINE_BYTES) {
                self.offset += consumed;
                self.skipping = !complete;
                continue;
            }
            if !complete {
                return Ok(None);
            }
            self.offset += consumed;
            if let Some(signal) = classify(&line, turn_id) {
                return Ok(Some(signal));
            }
        }
    }
}

/// What one complete rollout line says about `turn_id`, if anything.
fn classify(line: &[u8], turn_id: &str) -> Option<TurnSignal> {
    let line = std::str::from_utf8(line).ok()?;
    // Most lines are neither record; they are not parsed.
    if !["\"task_complete\"", "\"task_started\""]
        .iter()
        .any(|name| line.contains(name))
    {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    let kind = payload.get("type")?.as_str()?;
    let record_turn = payload.get("turn_id")?.as_str()?;
    // As of Codex 0.162.0 a child turn's record names its root turn in
    // `root_turn_id`; only the lead's are read.
    if payload
        .get("root_turn_id")
        .is_some_and(|root| !root.is_null() && root.as_str() != Some(record_turn))
    {
        return None;
    }
    match kind {
        "task_complete" if record_turn == turn_id => match payload.get("error") {
            Some(Value::Object(error)) => Some(TurnSignal::Failed {
                error_code: error.get("codex_error_info").and_then(error_code),
            }),
            _ => None,
        },
        "task_started" if record_turn != turn_id => safe_text(record_turn, "turn_id")
            .ok()
            .map(|turn_id| TurnSignal::OtherTurnStarted { turn_id }),
        _ => None,
    }
}

/// Codex's name for an error: `codex_error_info` as a string, or the one key
/// of an object, kept only when it is a safe label.
fn error_code(info: &Value) -> Option<String> {
    let code = match info {
        Value::String(code) => code,
        Value::Object(object) if object.len() == 1 => object.keys().next()?,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            return None;
        }
    };
    safe_text(code, "codex_error_info").ok()
}
