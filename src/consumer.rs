//! Transient executable delivery. No content storage and no consumer output logging.
use std::ffi::CString;
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::identity::PaneAddress;
use crate::observations::{Actor, NativeCorrelation};
use crate::protocol::{AttentionError, Result, manifest};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Persistence {
    NotRequested,
    Confirmed,
    Rejected,
    Unconfirmed,
}

#[derive(Clone, Debug, Serialize)]
pub struct HookPersistence {
    pub native_state: Persistence,
    pub activity: Persistence,
    pub compatibility: Persistence,
    pub lifecycle: Persistence,
}

#[derive(Clone, Debug, Serialize)]
pub struct HookScope {
    pub address: PaneAddress,
    pub launch_id: String,
    pub target: BindingTarget,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingTarget {
    Binding { binding_id: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct AdmittedHook {
    pub action: crate::providers::ProviderAction,
    pub scope: HookScope,
    pub provider: String,
    pub provider_session_id: String,
    pub source_event: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<NativeCorrelation>,
}

// Deliberately no Debug: reply bodies must not enter diagnostics accidentally.
#[derive(Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum ReplyContent {
    NotRequested,
    Available { text: String },
    Absent,
    Unsupported,
    Invalid,
    TooLarge,
}

#[derive(Serialize)]
pub struct HookDelivery {
    pub schema: u8,
    pub delivery_id: String,
    #[serde(flatten)]
    pub source: AdmittedHook,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    pub persistence: HookPersistence,
    pub reply: ReplyContent,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStage {
    NotDispatched,
    NotStarted,
    Completed,
    Failed,
    TimedOut,
    StdinFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEffect {
    None,
    Possible,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotDispatchedReason {
    NoAdmittedScope,
    NativeStateRejected,
    NativeStateUnconfirmed,
    LifecycleRejected,
    LifecycleUnconfirmed,
    CompatibilityRejected,
    CompatibilityUnconfirmed,
    EnvelopeTooLarge,
}

#[derive(Debug, Serialize)]
pub struct DeliveryOutcome {
    pub executable: String,
    pub stage: DeliveryStage,
    pub effect: DeliveryEffect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotDispatchedReason>,
}

pub fn validate_consumers(
    executables: &[String],
    timeout_ms: Option<u64>,
) -> Result<Option<Duration>> {
    let maximum = manifest()?.limits.path_max_bytes;
    for executable in executables {
        if !Path::new(executable).is_absolute()
            || executable.len() > maximum
            || executable.chars().any(|c| c < ' ' || c == '\u{7f}')
        {
            return Err(AttentionError::usage(
                "--consumer requires an absolute executable path",
            ));
        }
    }
    let timeout = match timeout_ms {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        None if executables.is_empty() => return Ok(None),
        _ => {
            return Err(AttentionError::usage(
                "--consumer requires a positive --consumer-timeout-ms",
            ));
        }
    };
    if Instant::now().checked_add(timeout).is_none() {
        return Err(AttentionError::usage(
            "--consumer-timeout-ms is not representable",
        ));
    }
    Ok(Some(timeout))
}

pub fn delivery_bytes(
    outcome: &crate::lifecycle::HookOutcome,
    reply: ReplyContent,
) -> std::result::Result<Vec<u8>, NotDispatchedReason> {
    let source = outcome
        .admission
        .clone()
        .ok_or(NotDispatchedReason::NoAdmittedScope)?;
    let confirmed = |value| matches!(value, Persistence::Confirmed | Persistence::NotRequested);
    if outcome.persistence.native_state == Persistence::Rejected
        || outcome.persistence.activity == Persistence::Rejected
    {
        return Err(NotDispatchedReason::NativeStateRejected);
    }
    if !confirmed(outcome.persistence.native_state) || !confirmed(outcome.persistence.activity) {
        return Err(NotDispatchedReason::NativeStateUnconfirmed);
    }
    if outcome.persistence.lifecycle == Persistence::Rejected {
        return Err(NotDispatchedReason::LifecycleRejected);
    }
    if !confirmed(outcome.persistence.lifecycle) {
        return Err(NotDispatchedReason::LifecycleUnconfirmed);
    }
    if outcome.persistence.compatibility == Persistence::Rejected {
        return Err(NotDispatchedReason::CompatibilityRejected);
    }
    if !confirmed(outcome.persistence.compatibility) {
        return Err(NotDispatchedReason::CompatibilityUnconfirmed);
    }
    let mut delivery = HookDelivery {
        schema: 1,
        delivery_id: Uuid::new_v4().to_string(),
        source,
        observation_id: outcome.observation_id.clone(),
        persistence: outcome.persistence.clone(),
        reply,
    };
    let maximum = manifest()
        .map_err(|_| NotDispatchedReason::EnvelopeTooLarge)?
        .limits
        .max_json_bytes;
    let mut bytes =
        serde_json::to_vec(&delivery).map_err(|_| NotDispatchedReason::EnvelopeTooLarge)?;
    if bytes.len() > maximum {
        delivery.reply = ReplyContent::TooLarge;
        bytes = serde_json::to_vec(&delivery).map_err(|_| NotDispatchedReason::EnvelopeTooLarge)?;
    }
    if bytes.len() > maximum {
        return Err(NotDispatchedReason::EnvelopeTooLarge);
    }
    Ok(bytes)
}

pub fn not_dispatched(executable: &str, reason: NotDispatchedReason) -> DeliveryOutcome {
    DeliveryOutcome {
        executable: executable.into(),
        stage: DeliveryStage::NotDispatched,
        effect: DeliveryEffect::None,
        exit_code: None,
        reason: Some(reason),
    }
}

/// The deadline covers nonblocking stdin writes and child completion. Descendants
/// are not supervised; only this invocation's direct child is killed and reaped.
pub fn dispatch(executable: &str, bytes: &[u8], timeout: Duration) -> DeliveryOutcome {
    let mut outcome = DeliveryOutcome {
        executable: executable.into(),
        stage: DeliveryStage::NotStarted,
        effect: DeliveryEffect::None,
        exit_code: None,
        reason: None,
    };
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return outcome;
    };
    let Ok(program) = CString::new(executable) else {
        return outcome;
    };
    let mut command = Command::new(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // execvp on macOS can fall back to a shell for ENOEXEC. The contract is a
    // direct executable, so call execv after std has prepared child stdio.
    // This closure performs only an async-signal-safe syscall after fork.
    unsafe {
        command.pre_exec(move || {
            let argv = [program.as_ptr(), std::ptr::null()];
            libc::execv(program.as_ptr(), argv.as_ptr());
            Err(std::io::Error::last_os_error())
        });
    }
    let Ok(mut child) = command.spawn() else {
        return outcome;
    };
    outcome.effect = DeliveryEffect::Possible;
    outcome.stage = DeliveryStage::StdinFailed;
    let Some(stdin) = child.stdin.take() else {
        outcome.exit_code = terminate_child(&mut child);
        return outcome;
    };
    let fd = stdin.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        outcome.exit_code = terminate_child(&mut child);
        return outcome;
    }
    let mut offset = 0;
    let mut input = Some(stdin);
    loop {
        if Instant::now() >= deadline {
            outcome.stage = DeliveryStage::TimedOut;
            outcome.exit_code = terminate_child(&mut child);
            return outcome;
        }
        if let Some(writer) = &mut input {
            match writer.write(&bytes[offset..]) {
                Ok(0) => {
                    outcome.exit_code = terminate_child(&mut child);
                    return outcome;
                }
                Ok(count) => {
                    offset += count;
                    if offset == bytes.len() {
                        input = None;
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(_) => {
                    outcome.exit_code = terminate_child(&mut child);
                    return outcome;
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                outcome.exit_code = status.code();
                outcome.stage = if input.is_some() {
                    DeliveryStage::StdinFailed
                } else if status.success() {
                    DeliveryStage::Completed
                } else {
                    DeliveryStage::Failed
                };
                return outcome;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => {
                outcome.stage = DeliveryStage::Failed;
                outcome.exit_code = terminate_child(&mut child);
                return outcome;
            }
        }
    }
}

fn terminate_child(child: &mut std::process::Child) -> Option<i32> {
    let _ = child.kill();
    child.wait().ok().and_then(|status| status.code())
}
