//! A turn follower: a process a lead prompt hook leaves behind to learn how
//! the turn it began came to an end, for the ends a provider runs no hook for.
//! It lives no longer than the agent process whose hook started it, the
//! binding that recorded the prompt, or its turn, and records an end only
//! through the writer, under the locks every writer takes.
//!
//! What it reads is the provider's own file, which a reader for that provider
//! turns into what the follower acts on; nothing else here knows that file.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::claude_status::{Foreign, Reading, SESSION_STATUS};
use crate::codex_rollout::{Refusal, Rollout, TASK_COMPLETE, TurnSignal};
use crate::identity::PaneAddress;
use crate::launch::{ClaimOwner, OwnerState, owner_state};
use crate::lifecycle::{LifecycleResult, binding_id};
use crate::protocol::{AttentionError, DiagnosticCode, Provider, Result};
use crate::providers::{ProviderEvent, safe_text};
use crate::query::PaneScope;
use crate::records::{open_state_lock, state_root, turn_follower_lock};
use crate::wezterm::{ProcessStart, RuntimePorts};

/// Each provider's `source_event` for the turn ends its follower records.
/// The manifest's `native_hooks` do not declare them, and a hook event they do
/// not declare is ignored, so a hook cannot claim one.
pub const FOLLOWER_SOURCES: [(Provider, &str); 2] = [
    (Provider::Codex, TASK_COMPLETE),
    (Provider::Claude, SESSION_STATUS),
];

/// How many polls a follower waits for another turn's follower to leave the
/// lock; that follower leaves within one poll of seeing its turn end.
const TAKE_OVER_POLLS: u32 = 10;

/// What a follower is handed: the binding's scope, the host process whose
/// hook saw the turn start, and the turn, with what to read it from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowRequest {
    pub address: PaneAddress,
    pub launch_id: String,
    pub session_id: String,
    pub host_pid: i32,
    pub host_started_sec: u64,
    pub host_started_usec: u32,
    pub host_boot_session_id: String,
    pub turn: FollowedTurn,
}

/// The followed turn, by its provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum FollowedTurn {
    Codex(CodexTurn),
    Claude(ClaudeTurn),
}

/// A Codex turn, and where in which rollout to read from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexTurn {
    pub turn_id: String,
    pub rollout: String,
    pub offset: u64,
}

/// A Claude Code turn, named by the prompt that began it, since a Claude
/// prompt carries no turn id: that prompt as the writer stored it, and the
/// status file of the host process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeTurn {
    pub prompt_observation_id: String,
    pub prompt_observed_mono_ns: String,
    pub status_file: String,
}

impl FollowedTurn {
    pub fn provider(&self) -> Provider {
        match self {
            Self::Codex(_) => Provider::Codex,
            Self::Claude(_) => Provider::Claude,
        }
    }

    /// What names this turn among the binding's turns: the follower lock
    /// holds it.
    pub fn key(&self) -> &str {
        match self {
            Self::Codex(turn) => &turn.turn_id,
            Self::Claude(turn) => &turn.prompt_observation_id,
        }
    }
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

    /// The binding that recorded the followed turn's prompt.
    pub fn binding_id(&self) -> String {
        binding_id(
            self.turn.provider().as_str(),
            &self.session_id,
            &self.launch_id,
        )
    }
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

/// Take the follower lock at `path` for the turn `key` names, waiting up to
/// `wait` for another turn's follower to leave.
pub fn take_turn(path: &Path, key: &str, wait: Duration) -> Result<TakenTurn> {
    let mut file = open_state_lock(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                let mut followed = String::new();
                let _ = file.seek(SeekFrom::Start(0));
                let _ = file.read_to_string(&mut followed);
                if followed == key {
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
        .and_then(|_| file.write_all(key.as_bytes()));
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
    /// The turn ended in a way its follower records, and this is what
    /// recording it did.
    Recorded(LifecycleResult),
    /// The Codex turn ended some other way; its own hook records that, or
    /// nothing does.
    Ended(TurnSignal),
    /// The host process is gone, or could not be read.
    HostGone,
    /// The binding is not the launch's current one or has ended, or the claim
    /// moved on.
    BindingChanged,
    /// Another follower has this turn, or another turn's follower kept the
    /// lock and this turn's end was not there to read.
    AlreadyFollowed,
    /// The Codex rollout could not be read for this turn.
    Refused(Refusal),
    /// A newer prompt replaced the followed Claude prompt, or the prompt
    /// record no longer names it.
    PromptReplaced,
    /// The Claude Code status file is gone: its process has exited.
    StatusGone,
    /// The file at the status file's path is not the one to read.
    StatusForeign(Foreign),
}

/// What one provider's follower reads its turn's end from.
enum Reader<'a> {
    Codex {
        turn: &'a CodexTurn,
        rollout: Rollout,
    },
    /// Read whole at every poll. `working` is when the latest reading that
    /// showed the turn working says it changed.
    Claude {
        turn: &'a ClaudeTurn,
        working: Option<u64>,
    },
}

impl<'a> Reader<'a> {
    fn open(request: &'a FollowRequest) -> std::result::Result<Self, Refusal> {
        match &request.turn {
            FollowedTurn::Codex(turn) => {
                Rollout::open(Path::new(&turn.rollout), &request.session_id, turn.offset)
                    .map(|rollout| Self::Codex { turn, rollout })
            }
            FollowedTurn::Claude(turn) => Ok(Self::Claude {
                turn,
                working: None,
            }),
        }
    }

    /// Read once. `Some` when the follower is done, with how; `None` while
    /// the turn has not yet ended in a way this reader decides.
    fn read(
        &mut self,
        request: &FollowRequest,
        root: &Path,
        host: &ClaimOwner,
        ports: &RuntimePorts<'_>,
    ) -> Result<Option<Followed>> {
        match self {
            Self::Codex { turn, rollout } => match rollout.next_signal(&turn.turn_id) {
                Err(refusal) => Ok(Some(Followed::Refused(refusal))),
                Ok(Some(TurnSignal::Failed { error_code })) => {
                    let event = ProviderEvent::codex_turn_failure(
                        &request.session_id,
                        &turn.turn_id,
                        error_code,
                    );
                    crate::lifecycle::record_codex_turn_failure(
                        root,
                        &request.address,
                        &request.launch_id,
                        host,
                        &event,
                        ports,
                    )
                    .map(|result| Some(Followed::Recorded(result)))
                }
                Ok(Some(signal)) => Ok(Some(Followed::Ended(signal))),
                Ok(None) => Ok(None),
            },
            Self::Claude { turn, working } => {
                if !crate::lifecycle::latest_prompt_is(
                    root,
                    &request.address,
                    &request.launch_id,
                    &request.binding_id(),
                    turn,
                ) {
                    return Ok(Some(Followed::PromptReplaced));
                }
                match crate::claude_status::read(
                    Path::new(&turn.status_file),
                    &request.session_id,
                    request.host_pid,
                ) {
                    Reading::Gone => Ok(Some(Followed::StatusGone)),
                    Reading::Foreign(reason) => Ok(Some(Followed::StatusForeign(reason))),
                    // Neither working nor ended: a later reading decides.
                    Reading::Unreadable { .. } => Ok(None),
                    Reading::Working { updated_at } => {
                        *working = Some(updated_at);
                        Ok(None)
                    }
                    // Only a change from working that this follower saw, by
                    // the file's own clock; an ended reading alone may be from
                    // before the turn began.
                    Reading::Ended {
                        updated_at,
                        version,
                    } if working.is_some_and(|working| updated_at > working) => {
                        let event =
                            ProviderEvent::claude_status_interrupt(&request.session_id, version);
                        crate::lifecycle::record_claude_interrupt(
                            root,
                            &request.address,
                            &request.launch_id,
                            host,
                            turn,
                            &event,
                            ports,
                        )
                        .map(|result| Some(Followed::Recorded(result)))
                    }
                    Reading::Ended { .. } => Ok(None),
                }
            }
        }
    }
}

/// Follow `request`'s turn until it ends, its host is gone or its binding
/// stops being current, reading every `poll`. A follower that another turn's
/// follower keeps out of the lock for its wait reads once instead.
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
    safe_text(request.turn.key(), "turn").map_err(|_| invalid())?;
    let root = state_root(env)?;
    let binding = request.binding_id();
    let current = || {
        crate::lifecycle::followed_binding(&root, &request.address, &request.launch_id, &binding)
            .is_some()
    };
    if !current() {
        return Ok(Followed::BindingChanged);
    }
    let lock = turn_follower_lock(&root, &request.address, &request.launch_id, &binding);
    let held = match take_turn(&lock, request.turn.key(), poll * TAKE_OVER_POLLS)? {
        TakenTurn::Taken(lock) => Some(lock),
        TakenTurn::AlreadyFollowed => return Ok(Followed::AlreadyFollowed),
        // A later turn's follower can hold the lock while this turn's end is
        // already written, so the end is read once rather than left.
        TakenTurn::KeptOut => None,
    };
    let mut reader = match Reader::open(request) {
        Ok(reader) => reader,
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
        match reader.read(request, &root, &host, ports)? {
            Some(followed) => return Ok(followed),
            None if held.is_none() => return Ok(Followed::AlreadyFollowed),
            None => thread::sleep(poll),
        }
    }
}
