//! A Codex session rollout, read for one thing: how a lead turn that a hook
//! saw start came to its end. As of Codex 0.162.0 a turn that ends on a model
//! error runs no hook, so its end is only in the rollout, as a `task_complete`
//! record that carries an `error` object.
//!
//! The rollout is Codex's own file, and every line of it is untrusted. Only
//! complete lines of three shapes, as Codex 0.162.0 writes them, are acted on;
//! any other shape yields nothing, never a record.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::PaneAddress;
use crate::launch::{ClaimOwner, OwnerState, owner_state};
use crate::lifecycle::{LifecycleResult, binding_id};
use crate::protocol::{AttentionError, DiagnosticCode, Result};
use crate::providers::{ProviderEvent, safe_text};
use crate::query::PaneScope;
use crate::records::{open_state_lock, state_root, turn_follower_lock};
use crate::wezterm::{ProcessStart, RuntimePorts};

/// The `source_event` a turn end read from the rollout carries. The
/// manifest's `native_hooks` do not declare it, and a hook event they do not
/// declare is ignored, so a hook cannot claim it.
pub const TASK_COMPLETE: &str = "task_complete";

/// The longest line read. A longer one is skipped unread. An errored
/// `task_complete` is far smaller: its `last_agent_message` is null.
const MAX_LINE_BYTES: usize = 4 << 20;

/// How many polls a follower waits for another turn's follower to leave the
/// lock; that follower leaves within one poll of seeing its turn end.
const TAKE_OVER_POLLS: u32 = 10;

/// What a follower is handed: the binding's scope, the turn, the host process
/// whose hook saw the turn start, and where in which rollout to read from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowRequest {
    pub address: PaneAddress,
    pub launch_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub host_pid: i32,
    pub host_started_sec: u64,
    pub host_started_usec: u32,
    pub host_boot_session_id: String,
    pub rollout: String,
    pub offset: u64,
}

impl FollowRequest {
    fn host(&self) -> ClaimOwner {
        ClaimOwner {
            pid: self.host_pid,
            start: ProcessStart {
                seconds: self.host_started_sec,
                microseconds: self.host_started_usec,
            },
            boot_session_id: self.host_boot_session_id.clone(),
        }
    }
}

/// How the followed turn came to its end, as the rollout says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnSignal {
    /// Its `task_complete` carries an error.
    Failed { error_code: Option<String> },
    /// Its `task_complete` carries none; `Stop` records that end.
    Finished,
    /// It was interrupted; `Interrupt` records that end.
    Aborted,
    /// Another turn started, so this one is over.
    OtherTurnStarted,
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

    /// Read what Codex appended since the last read, and return how
    /// `turn_id` ended if a complete line says so.
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
    // Most lines are none of the three records; they are not parsed.
    if !["\"task_complete\"", "\"turn_aborted\"", "\"task_started\""]
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
        "task_complete" if record_turn == turn_id => Some(match payload.get("error") {
            Some(Value::Object(error)) => TurnSignal::Failed {
                error_code: error.get("codex_error_info").and_then(error_code),
            },
            _ => TurnSignal::Finished,
        }),
        "turn_aborted" if record_turn == turn_id => Some(TurnSignal::Aborted),
        "task_started" if record_turn != turn_id => Some(TurnSignal::OtherTurnStarted),
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

/// The binding's follower lock, held by the follower of one of its turns and
/// naming that turn. A follower kept out of it reads without it.
pub struct FollowerLock(File);

impl Drop for FollowerLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// What [`take_turn`] found.
pub enum TakenTurn {
    /// The lock, now naming this turn.
    Taken(FollowerLock),
    /// Another follower has this turn.
    AlreadyFollowed,
    /// Another turn's follower kept the lock for all of the wait.
    KeptOut,
}

/// Take the follower lock at `path` for `turn_id`, waiting up to `wait` for
/// another turn's follower to leave.
pub fn take_turn(path: &Path, turn_id: &str, wait: Duration) -> Result<TakenTurn> {
    let mut file = open_state_lock(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                let mut followed = String::new();
                let _ = file.seek(SeekFrom::Start(0));
                let _ = file.read_to_string(&mut followed);
                if followed == turn_id {
                    return Ok(TakenTurn::AlreadyFollowed);
                }
                if Instant::now() >= deadline {
                    return Ok(TakenTurn::KeptOut);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(AttentionError::new(
                    DiagnosticCode::ProbeUnavailable,
                    "the turn follower's lock is unavailable",
                ));
            }
        }
    }
    let named = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(turn_id.as_bytes()));
    if named.is_err() {
        return Err(AttentionError::new(
            DiagnosticCode::StatePermissions,
            "the turn follower's lock could not be written",
        ));
    }
    Ok(TakenTurn::Taken(FollowerLock(file)))
}

/// How a follower finished.
#[derive(Clone, Debug)]
pub enum Followed {
    /// The turn ended on an error, and this is what recording it did.
    Recorded(LifecycleResult),
    /// The turn ended some other way; its own hooks record that.
    Ended(TurnSignal),
    /// The host process is gone, or could not be read.
    HostGone,
    /// The binding is not the launch's current one or has ended, or the claim
    /// moved on.
    BindingChanged,
    /// Another follower has this turn, or another turn's follower kept the
    /// lock and this turn's end was not yet in the rollout.
    AlreadyFollowed,
    Refused(Refusal),
}

/// Follow `request`'s turn until it ends, its host is gone or its binding
/// stops being current, reading the rollout every `poll`. A follower that
/// another turn's follower keeps out of the lock for its wait reads the
/// rollout once instead. Only an errored end is recorded.
pub fn follow_turn(
    request: &FollowRequest,
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    poll: Duration,
) -> Result<Followed> {
    let invalid = || AttentionError::usage("follow-turn request is invalid");
    PaneScope::new(request.address.clone(), request.launch_id.clone(), None)
        .map_err(|_| invalid())?;
    safe_text(&request.session_id, "session_id").map_err(|_| invalid())?;
    safe_text(&request.turn_id, "turn_id").map_err(|_| invalid())?;
    let root = state_root(env)?;
    let binding = binding_id("codex", &request.session_id, &request.launch_id);
    let current = || {
        crate::lifecycle::followed_binding(&root, &request.address, &request.launch_id, &binding)
            .is_some()
    };
    if !current() {
        return Ok(Followed::BindingChanged);
    }
    let lock = turn_follower_lock(&root, &request.address, &request.launch_id, &binding);
    let held = match take_turn(&lock, &request.turn_id, poll * TAKE_OVER_POLLS)? {
        TakenTurn::Taken(lock) => Some(lock),
        TakenTurn::AlreadyFollowed => return Ok(Followed::AlreadyFollowed),
        // A later turn's follower can hold the lock while this turn's end is
        // already written, so the rollout is read once rather than left.
        TakenTurn::KeptOut => None,
    };
    let mut rollout = match Rollout::open(
        Path::new(&request.rollout),
        &request.session_id,
        request.offset,
    ) {
        Ok(rollout) => rollout,
        Err(refusal) => return Ok(Followed::Refused(refusal)),
    };
    let host = request.host();
    loop {
        if owner_state(&host, ports.processes) != OwnerState::Alive {
            return Ok(Followed::HostGone);
        }
        if !current() {
            return Ok(Followed::BindingChanged);
        }
        match rollout.next_signal(&request.turn_id) {
            Err(refusal) => return Ok(Followed::Refused(refusal)),
            Ok(Some(TurnSignal::Failed { error_code })) => {
                let event = ProviderEvent::codex_turn_failure(
                    &request.session_id,
                    &request.turn_id,
                    error_code,
                );
                return crate::lifecycle::record_codex_turn_failure(
                    &root,
                    &request.address,
                    &request.launch_id,
                    &host,
                    &event,
                    ports,
                )
                .map(Followed::Recorded);
            }
            Ok(Some(signal)) => return Ok(Followed::Ended(signal)),
            Ok(None) if held.is_none() => return Ok(Followed::AlreadyFollowed),
            Ok(None) => thread::sleep(poll),
        }
    }
}
