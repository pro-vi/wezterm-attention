//! A turn follower: a process a lead hook leaves behind to learn how a turn
//! came to an end, for the ends a provider runs no hook for. It lives no
//! longer than the agent process whose hook started it or the binding that
//! recorded the turn, and records an end only through the writer, under the
//! locks every writer takes. A Claude Code or Agy follower follows the turn of
//! one prompt. A Codex follower follows its turn, then each lead turn the
//! rollout starts after it, recording each that ends on a model error.
//!
//! What it reads is the provider's own file, which a reader for that provider
//! turns into what the follower acts on; nothing else here knows that file.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agy_status::{self, AGY_STATUS};
use crate::claude_status::{self, Foreign, SESSION_STATUS};
use crate::codex_rollout::{Refusal, Rollout, TASK_COMPLETE, TurnSignal};
use crate::identity::PaneAddress;
use crate::launch::{ClaimOwner, OwnerState, owner_state};
use crate::lifecycle::{LifecycleResult, binding_id};
use crate::protocol::{AttentionError, DiagnosticCode, Disposition, Provider, Result};
use crate::providers::{ProviderEvent, safe_text};
use crate::query::PaneScope;
use crate::records::{open_state_lock, state_root, turn_follower_lock};
use crate::wezterm::{ProcessStart, RuntimePorts};

/// Each provider's `source_event` for the turn ends its follower records.
/// The manifest's `native_hooks` do not declare them, and a hook event they do
/// not declare is ignored, so a hook cannot claim one.
pub const FOLLOWER_SOURCES: [(Provider, &str); 3] = [
    (Provider::Codex, TASK_COMPLETE),
    (Provider::Claude, SESSION_STATUS),
    (Provider::Agy, AGY_STATUS),
];

/// How many polls a follower waits for another turn's follower to leave the
/// lock or to name this turn: a Claude or Agy follower leaves within one poll
/// of seeing its turn end or be replaced, and a Codex follower names the next
/// turn within one poll of reading its start.
const TAKE_OVER_POLLS: u32 = 10;

/// What a follower is handed: the binding's scope, the host process whose
/// hook saw the turn, and the turn, with what to read it from.
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
    Agy(AgyTurn),
}

/// A Codex turn, and where in which rollout to read from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexTurn {
    pub turn_id: String,
    pub rollout: String,
    pub offset: u64,
}

/// A Claude Code turn, named by the prompt it follows, whether or not that
/// prompt carries a turn id: the prompt as the writer stored it, and the
/// status file of the host process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeTurn {
    pub prompt_observation_id: String,
    pub prompt_observed_mono_ns: String,
    pub status_file: String,
}

/// An Antigravity CLI (`agy`) lead turn, named by the prompt and invocation it
/// follows: the prompt as the writer stored it, the monotonic timestamp of the
/// `PreInvocation` `thinking` activity that started or resumed this invocation,
/// `conversation_summaries.db`, optionally `conversations/<id>.db`, and the
/// invocation's starting step index.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgyTurn {
    pub prompt_observation_id: String,
    pub prompt_observed_mono_ns: String,
    pub invocation_observed_mono_ns: String,
    pub summaries_db: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv_db: Option<String>,
    pub initial_step_count: i64,
}

impl FollowedTurn {
    pub fn provider(&self) -> Provider {
        match self {
            Self::Codex(_) => Provider::Codex,
            Self::Claude(_) => Provider::Claude,
            Self::Agy(_) => Provider::Agy,
        }
    }

    /// What names this turn among the binding's turns: the follower lock
    /// holds it.
    pub fn key(&self) -> Cow<'_, str> {
        match self {
            Self::Codex(turn) => Cow::Borrowed(&turn.turn_id),
            Self::Claude(turn) => Cow::Borrowed(&turn.prompt_observation_id),
            Self::Agy(turn) => Cow::Owned(format!(
                "{}:{}",
                turn.prompt_observation_id, turn.invocation_observed_mono_ns
            )),
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

    /// The binding the followed turn belongs to.
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

impl FollowerLock {
    /// Name the turn `key` as the one followed.
    fn name(&mut self, key: &str) -> Result<()> {
        self.0
            .set_len(0)
            .and_then(|()| self.0.seek(SeekFrom::Start(0)))
            .and_then(|_| self.0.write_all(key.as_bytes()))
            .map_err(|_| {
                AttentionError::new(
                    DiagnosticCode::StatePermissions,
                    "the turn follower's lock could not be written",
                )
            })
    }
}

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
/// `wait` for another turn's follower to leave or to name this turn.
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
    let mut lock = FollowerLock(file);
    lock.name(key)?;
    Ok(TakenTurn::Taken(lock))
}

/// Whether a follower holds the follower lock at `path`, or one could not take
/// it. Asking takes the lock shared for an instant; a follower that tries to
/// take it in that instant, while the lock names its turn, leaves, and the
/// hook that asked then starts one itself.
pub fn is_followed(path: &Path) -> bool {
    File::open(path).is_ok_and(|file| file.try_lock_shared().is_err())
}

/// If a follower holds the follower lock at `path`, returns the key written in
/// the lock file; returns `None` when the lock is absent or unheld.
pub fn followed_key(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    if file.try_lock_shared().is_ok() {
        return None;
    }
    let mut followed = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut followed);
    Some(followed)
}

/// How a follower finished.
#[derive(Clone, Debug)]
pub enum Followed {
    /// The turn ended in a way its follower records, and this is what
    /// recording it did. A Codex follower that holds the lock finishes on it
    /// only when the caller says so.
    Recorded(LifecycleResult),
    /// The host process is gone, or could not be read.
    HostGone,
    /// The binding is not the launch's current one or has ended, or the claim
    /// moved on.
    BindingChanged,
    /// Another follower has this turn, or another turn's follower kept the
    /// lock and this turn's end was not there to read.
    AlreadyFollowed,
    /// The Codex rollout could not be read for this turn.
    RolloutRefused(Refusal),
    /// A newer prompt replaced the followed Claude or Agy prompt, or the prompt
    /// record no longer names it.
    PromptReplaced,
    /// The Claude Code status file is not there: its process is exiting, or
    /// never wrote one at that path.
    StatusGone,
    /// The file at the status file's path is not the one to read.
    StatusForeign(Foreign),
    /// The Antigravity CLI status database could not be trusted or is not
    /// supported on this platform.
    AgyStatusRefused(agy_status::Unreadable),
}

/// What one provider's follower reads its turn's end from.
enum Reader<'a> {
    /// `turn_id` is the turn followed now, and `before` the turns this
    /// follower followed before it, oldest first.
    Codex {
        turn_id: String,
        before: Vec<String>,
        rollout: Rollout,
    },
    /// Read whole at every poll. `last_working_at` is when the latest
    /// reading that showed the turn working says it changed.
    Claude {
        turn: &'a ClaudeTurn,
        last_working_at: Option<u64>,
    },
    /// Polled against `agy`'s local SQLite databases. `seen_working` tracks
    /// whether this follower observed the turn running before seeing idle.
    Agy {
        turn: &'a AgyTurn,
        seen_working: bool,
    },
}

impl<'a> Reader<'a> {
    fn open(request: &'a FollowRequest) -> std::result::Result<Self, Refusal> {
        match &request.turn {
            FollowedTurn::Codex(turn) => {
                Rollout::open(Path::new(&turn.rollout), &request.session_id, turn.offset).map(
                    |rollout| Self::Codex {
                        turn_id: turn.turn_id.clone(),
                        before: Vec::new(),
                        rollout,
                    },
                )
            }
            FollowedTurn::Claude(turn) => Ok(Self::Claude {
                turn,
                last_working_at: None,
            }),
            FollowedTurn::Agy(turn) => Ok(Self::Agy {
                turn,
                seen_working: false,
            }),
        }
    }

    /// Read once. `Some` when the follower is done, with how; `None` while
    /// the turn has not yet ended in a way this reader decides. A Codex reader
    /// that `held` the lock goes on to the next lead turn, naming it in the
    /// lock, and past each end it records unless `recorded` says to finish;
    /// one kept out leaves that turn to the holder.
    fn read(
        &mut self,
        request: &FollowRequest,
        root: &Path,
        host: &ClaimOwner,
        ports: &RuntimePorts<'_>,
        mut held: Option<&mut FollowerLock>,
        recorded: &mut dyn FnMut() -> ControlFlow<()>,
    ) -> Result<Option<Followed>> {
        match self {
            Self::Codex {
                turn_id,
                before,
                rollout,
            } => loop {
                match rollout.next_signal(turn_id) {
                    Err(refusal) => return Ok(Some(Followed::RolloutRefused(refusal))),
                    Ok(Some(TurnSignal::Failed { error_code })) => {
                        let event = ProviderEvent::codex_turn_failure(
                            &request.session_id,
                            turn_id,
                            error_code,
                        );
                        let result = crate::lifecycle::record_codex_turn_failure(
                            root,
                            &request.address,
                            &request.launch_id,
                            host,
                            before,
                            &event,
                            ports,
                        )?;
                        if held.is_none() || recorded().is_break() {
                            return Ok(Some(Followed::Recorded(result)));
                        }
                    }
                    Ok(Some(TurnSignal::OtherTurnStarted { turn_id: next })) => {
                        let Some(lock) = held.as_deref_mut() else {
                            return Ok(Some(Followed::AlreadyFollowed));
                        };
                        lock.name(&next)?;
                        before.push(std::mem::replace(turn_id, next));
                    }
                    Ok(None) => return Ok(None),
                }
            },
            Self::Claude {
                turn,
                last_working_at,
            } => {
                let prompt_turn = crate::lifecycle::PromptTurn {
                    observation_id: &turn.prompt_observation_id,
                    observed_mono_ns: &turn.prompt_observed_mono_ns,
                    invocation_observed_mono_ns: None,
                    confirmed_canceled: false,
                };
                if !crate::lifecycle::latest_prompt_is(
                    root,
                    &request.address,
                    &request.launch_id,
                    &request.binding_id(),
                    prompt_turn,
                ) {
                    return Ok(Some(Followed::PromptReplaced));
                }
                match claude_status::read(
                    Path::new(&turn.status_file),
                    &request.session_id,
                    request.host_pid,
                ) {
                    claude_status::Reading::Gone => Ok(Some(Followed::StatusGone)),
                    claude_status::Reading::Foreign(reason) => {
                        Ok(Some(Followed::StatusForeign(reason)))
                    }
                    // Neither working nor ended: a later reading decides.
                    claude_status::Reading::Unreadable { .. } => Ok(None),
                    claude_status::Reading::Working { updated_at } => {
                        *last_working_at = Some(updated_at);
                        Ok(None)
                    }
                    // Only a change from working that this follower saw, by
                    // the file's own clock; an ended reading alone may be from
                    // before the turn began.
                    claude_status::Reading::Ended {
                        updated_at,
                        version,
                    } if last_working_at.is_some_and(|working| updated_at > working) => {
                        let event =
                            ProviderEvent::claude_turn_interrupt(&request.session_id, version);
                        crate::lifecycle::record_prompt_turn_interrupt(
                            root,
                            &request.address,
                            &request.launch_id,
                            host,
                            prompt_turn,
                            &event,
                            ports,
                        )
                        .map(|result| Some(Followed::Recorded(result)))
                    }
                    claude_status::Reading::Ended { .. } => Ok(None),
                }
            }
            Self::Agy { turn, seen_working } => {
                let mut prompt_turn = crate::lifecycle::PromptTurn {
                    observation_id: &turn.prompt_observation_id,
                    observed_mono_ns: &turn.prompt_observed_mono_ns,
                    invocation_observed_mono_ns: Some(&turn.invocation_observed_mono_ns),
                    confirmed_canceled: false,
                };
                if !crate::lifecycle::latest_prompt_is(
                    root,
                    &request.address,
                    &request.launch_id,
                    &request.binding_id(),
                    prompt_turn,
                ) {
                    return Ok(Some(Followed::PromptReplaced));
                }
                if crate::lifecycle::agy_invocation_ended(
                    root,
                    &request.address,
                    &request.launch_id,
                    &request.binding_id(),
                    &turn.invocation_observed_mono_ns,
                ) {
                    return Ok(Some(Followed::Recorded(LifecycleResult::new(
                        Disposition::Skipped,
                    ))));
                }
                let reading = agy_status::read_status(
                    Path::new(&turn.summaries_db),
                    turn.conv_db.as_deref().map(Path::new),
                    &request.session_id,
                    turn.initial_step_count,
                );
                let confirmed_canceled = matches!(reading, agy_status::Reading::Canceled);
                match (reading, *seen_working) {
                    (agy_status::Reading::Working, _) => {
                        *seen_working = true;
                        Ok(None)
                    }
                    (agy_status::Reading::Idle, true)
                        if crate::lifecycle::agy_execution_history_lost(
                            root,
                            &request.address,
                            &request.launch_id,
                            &request.binding_id(),
                        ) =>
                    {
                        Ok(None)
                    }
                    (agy_status::Reading::Canceled, _) | (agy_status::Reading::Idle, true) => {
                        prompt_turn.confirmed_canceled = confirmed_canceled;
                        let event = ProviderEvent::agy_turn_interrupt(&request.session_id);
                        crate::lifecycle::record_prompt_turn_interrupt(
                            root,
                            &request.address,
                            &request.launch_id,
                            host,
                            prompt_turn,
                            &event,
                            ports,
                        )
                        .map(|result| {
                            if !confirmed_canceled
                                && result.disposition == Disposition::Skipped
                                && crate::lifecycle::agy_execution_history_lost(
                                    root,
                                    &request.address,
                                    &request.launch_id,
                                    &request.binding_id(),
                                )
                            {
                                None
                            } else {
                                Some(Followed::Recorded(result))
                            }
                        })
                    }
                    (
                        agy_status::Reading::Unreadable(
                            reason @ (agy_status::Unreadable::Foreign
                            | agy_status::Unreadable::PlatformUnsupported),
                        ),
                        _,
                    ) => Ok(Some(Followed::AgyStatusRefused(reason))),
                    (
                        agy_status::Reading::Idle
                        | agy_status::Reading::Absent
                        | agy_status::Reading::Unreadable(_),
                        _,
                    ) => Ok(None),
                }
            }
        }
    }
}

/// Follow `request`'s turn until it ends, its host is gone or its binding
/// stops being current, reading every `poll`. A Codex follower that holds the
/// lock goes on to each later lead turn, past each end it records, since as of
/// Codex 0.163.0 `/goal resume` runs no hook and starts a goal turn that can
/// fail before it runs one; after each such end it asks `recorded` whether to
/// finish instead. A follower that another turn's follower keeps out of the
/// lock for its wait reads once instead.
pub fn follow_turn(
    request: &FollowRequest,
    env: &BTreeMap<String, String>,
    ports: &RuntimePorts<'_>,
    poll: Duration,
    recorded: &mut dyn FnMut() -> ControlFlow<()>,
) -> Result<Followed> {
    let invalid = || AttentionError::usage("follow-turn request is invalid");
    PaneScope::new(request.address.clone(), request.launch_id.clone(), None)
        .map_err(|_| invalid())?;
    safe_text(&request.session_id, "session_id").map_err(|_| invalid())?;
    let turn_key = request.turn.key();
    safe_text(&turn_key, "turn").map_err(|_| invalid())?;
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
    let mut held = match take_turn(&lock, &turn_key, poll * TAKE_OVER_POLLS)? {
        TakenTurn::Taken(lock) => Some(lock),
        TakenTurn::AlreadyFollowed => return Ok(Followed::AlreadyFollowed),
        // A later turn's follower can hold the lock while this turn's end is
        // already written, so the end is read once rather than left. A Claude
        // end needs two readings, so a Claude follower kept out writes nothing.
        TakenTurn::KeptOut => None,
    };
    let mut reader = match Reader::open(request) {
        Ok(reader) => reader,
        Err(refusal) => return Ok(Followed::RolloutRefused(refusal)),
    };
    let host = request.host();
    loop {
        if owner_state(&host, ports.processes) != OwnerState::Alive {
            return Ok(Followed::HostGone);
        }
        if !current() {
            return Ok(Followed::BindingChanged);
        }
        match reader.read(request, &root, &host, ports, held.as_mut(), recorded)? {
            Some(followed) => return Ok(followed),
            None if held.is_none() => return Ok(Followed::AlreadyFollowed),
            None => thread::sleep(poll),
        }
    }
}
