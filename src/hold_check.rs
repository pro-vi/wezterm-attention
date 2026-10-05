//! Application notes obtained before a native turn end is applied. Input and
//! raw output remain transient; only validated notes and execution facts escape.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::hook_content::HookContent;
use crate::lifecycle::outcome::HookScope;
use crate::observations::Actor;
use crate::protocol::{AttentionError, Result, free_of_control, manifest};

pub const TOTAL_BUDGET: Duration = Duration::from_millis(2000);

#[derive(Clone, Debug)]
pub struct Registration {
    pub name: String,
    pub executable: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HoldNote {
    pub hold: bool,
    pub answer: String,
}

fn token(text: &str, name: bool) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'_'
                || (name && byte == b'-')
        })
}

pub(crate) fn valid_name(text: &str) -> bool {
    token(text, true)
}

impl HoldNote {
    pub fn valid(&self) -> bool {
        self.hold
            && token(&self.answer, false)
            && manifest().is_ok_and(|m| self.answer.len() <= m.limits.safe_label_max_bytes)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    NotDispatched,
    NotStarted,
    Completed,
    Failed,
    StdinFailed,
    StdoutFailed,
    TimedOut,
    InvalidOutput,
    OutputTooLarge,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Bypass {
    NotApplicable,
    NoCurrentScope,
    ReplyUnavailable,
    InputTooLarge,
    TotalDeadline,
    /// A sub-agent waits on a permission prompt, so the lead is not waiting
    /// on work that will finish without the user.
    ChildWaiting,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HoldCheckOutcome {
    pub name: String,
    pub executable: String,
    pub stage: Stage,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Bypass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<HoldNote>,
}

impl HoldCheckOutcome {
    pub fn valid(&self) -> bool {
        let Ok(m) = manifest() else { return false };
        token(&self.name, true)
            && self.name.len() <= m.limits.safe_label_max_bytes
            && Path::new(&self.executable).is_absolute()
            && self.executable.len() <= m.limits.path_max_bytes
            && free_of_control(&self.executable)
            && (self.stage == Stage::NotDispatched) == self.reason.is_some()
            && (self.stage == Stage::NotDispatched) == self.check_id.is_none()
            && self
                .check_id
                .as_ref()
                .is_none_or(|id| Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == *id))
            && self.note.as_ref().is_none_or(|note| {
                self.stage == Stage::Completed && self.exit_code == Some(0) && note.valid()
            })
            && (self.stage != Stage::Completed || self.exit_code == Some(0))
            && (!matches!(self.stage, Stage::NotDispatched | Stage::NotStarted)
                || self.exit_code.is_none())
            && (self.stage != Stage::NotDispatched || self.elapsed_ms == 0)
    }
}

fn initial(registration: &Registration) -> HoldCheckOutcome {
    HoldCheckOutcome {
        name: registration.name.clone(),
        executable: registration.executable.clone(),
        stage: Stage::NotDispatched,
        elapsed_ms: 0,
        check_id: None,
        exit_code: None,
        reason: None,
        note: None,
    }
}

pub fn bypass(registrations: &[Registration], reason: Bypass) -> Vec<HoldCheckOutcome> {
    registrations
        .iter()
        .map(|r| {
            let mut result = initial(r);
            result.reason = Some(reason);
            result
        })
        .collect()
}

pub fn notes(outcomes: &[HoldCheckOutcome]) -> BTreeMap<String, HoldNote> {
    outcomes
        .iter()
        .filter_map(|result| {
            result
                .note
                .as_ref()
                .map(|note| (result.name.clone(), note.clone()))
        })
        .collect()
}

pub fn failed(outcomes: &[HoldCheckOutcome]) -> bool {
    outcomes.iter().any(|o| match o.stage {
        Stage::Completed => false,
        Stage::NotDispatched => !matches!(
            o.reason,
            Some(Bypass::NotApplicable | Bypass::NoCurrentScope | Bypass::ChildWaiting)
        ),
        Stage::NotStarted
        | Stage::Failed
        | Stage::StdinFailed
        | Stage::StdoutFailed
        | Stage::TimedOut
        | Stage::InvalidOutput
        | Stage::OutputTooLarge => true,
    })
}

pub fn validate_registrations(values: &[String]) -> Result<Vec<Registration>> {
    let limits = &manifest()?.limits;
    let mut names = BTreeSet::new();
    let mut registrations = Vec::new();
    for value in values {
        let Some((name, executable)) = value.split_once('=') else {
            return Err(AttentionError::usage(
                "--hold-check requires NAME=/absolute/executable",
            ));
        };
        if !token(name, true)
            || name.len() > limits.safe_label_max_bytes
            || !names.insert(name.to_owned())
            || !Path::new(executable).is_absolute()
            || executable.len() > limits.path_max_bytes
            || !free_of_control(executable)
        {
            return Err(AttentionError::usage(
                "--hold-check requires a unique lowercase name and absolute executable path",
            ));
        }
        registrations.push(Registration {
            name: name.into(),
            executable: executable.into(),
        });
    }
    // Reserve the longest typed outcome, including worst JSON escaping for
    // supplied paths, before native state can be touched. Other stages omit
    // fields or replace these short discriminants with shorter spellings.
    let reservation: Vec<_> = registrations
        .iter()
        .map(|r| HoldCheckOutcome {
            stage: Stage::OutputTooLarge,
            elapsed_ms: u64::MAX,
            check_id: Some("ffffffff-ffff-4fff-bfff-ffffffffffff".into()),
            exit_code: Some(i32::MIN),
            reason: Some(Bypass::ReplyUnavailable),
            note: Some(HoldNote {
                hold: true,
                answer: "a".repeat(limits.safe_label_max_bytes),
            }),
            ..initial(r)
        })
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "status":"superseded", "held":false, "hold_checks":reservation
    }))
    .map_err(AttentionError::record_json)?;
    if bytes.len() + b",\"turn_end\":".len() > limits.lifecycle_envelope_max_bytes {
        return Err(AttentionError::usage(
            "--hold-check registrations exceed the turn-end audit budget",
        ));
    }
    Ok(registrations)
}

#[derive(Serialize)]
pub struct HoldCheckInput<'a> {
    pub schema: u8,
    pub phase: &'static str,
    pub check_id: &'a str,
    pub check_name: &'a str,
    pub scope: &'a HookScope,
    pub provider: &'a str,
    pub provider_session_id: &'a str,
    pub source_event: &'a str,
    pub actor: Actor,
    pub observed_mono_ns: &'a str,
    pub reply: &'a HookContent,
    pub background_tasks: &'a [Value],
}

pub struct Request<'a> {
    pub registrations: &'a [Registration],
    pub reply: &'a HookContent,
    pub background_tasks: Option<&'a [Value]>,
}

/// Runs a chain against one checked prospective event. No input or raw output
/// is included in the returned execution facts.
pub fn run(
    registrations: &[Registration],
    mut input: impl FnMut(&Registration, &str) -> Option<Vec<u8>>,
) -> Vec<HoldCheckOutcome> {
    let deadline = Instant::now() + TOTAL_BUDGET;
    registrations
        .iter()
        .map(|registration| {
            if Instant::now() >= deadline {
                return bypass(std::slice::from_ref(registration), Bypass::TotalDeadline).remove(0);
            }
            let id = Uuid::new_v4().to_string();
            let Some(bytes) = input(registration, &id) else {
                return bypass(std::slice::from_ref(registration), Bypass::InputTooLarge).remove(0);
            };
            if Instant::now() >= deadline {
                return bypass(std::slice::from_ref(registration), Bypass::TotalDeadline).remove(0);
            }
            dispatch(registration, &id, &bytes, deadline)
        })
        .collect()
}

fn nonblocking(fd: std::os::fd::RawFd) -> bool {
    // SAFETY: fcntl's flag commands take no pointers; the caller keeps fd open.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: as above.
    flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0
}

fn terminate(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: this invocation created the process group; killpg takes no pointers.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn dispatch(
    registration: &Registration,
    id: &str,
    bytes: &[u8],
    deadline: Instant,
) -> HoldCheckOutcome {
    let began = Instant::now();
    let mut result = HoldCheckOutcome {
        stage: Stage::NotStarted,
        check_id: Some(id.into()),
        ..initial(registration)
    };
    (|| {
        let Ok(program) = CString::new(registration.executable.as_str()) else {
            return;
        };
        let mut command = Command::new(&registration.executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: after fork this calls only execv and errno access. program is
        // owned by the closure, argv is on the stack and ends in a null pointer.
        unsafe {
            command.pre_exec(move || {
                let argv = [program.as_ptr(), std::ptr::null()];
                libc::execv(program.as_ptr(), argv.as_ptr());
                Err(std::io::Error::last_os_error())
            });
        }
        let Ok(mut child) = command.spawn() else {
            return;
        };
        let Some(mut output) = child.stdout.take() else {
            result.stage = Stage::StdoutFailed;
            terminate(&mut child);
            return;
        };
        let Some(stdin) = child.stdin.take() else {
            result.stage = Stage::StdinFailed;
            terminate(&mut child);
            return;
        };
        if !nonblocking(stdin.as_raw_fd()) || !nonblocking(output.as_raw_fd()) {
            result.stage = Stage::Failed;
            terminate(&mut child);
            return;
        }
        let Ok(m) = manifest() else {
            result.stage = Stage::Failed;
            terminate(&mut child);
            return;
        };
        // Spaced JSON and a terminal newline fit. This limits transport
        // bytes, not the whitespace accepted between JSON tokens.
        let maximum = m.limits.safe_label_max_bytes + b"{\"hold\": true, \"answer\": \"\"}\n".len();
        let mut input = Some(stdin);
        let mut offset = 0;
        let mut captured = Vec::new();
        let mut eof = false;
        let mut exit = None;
        loop {
            if Instant::now() >= deadline {
                result.stage = Stage::TimedOut;
                terminate(&mut child);
                return;
            }
            if let Some(writer) = &mut input {
                match writer.write(&bytes[offset..]) {
                    Ok(0) => {
                        result.stage = Stage::StdinFailed;
                        terminate(&mut child);
                        return;
                    }
                    Ok(count) => {
                        offset += count;
                        if offset == bytes.len() {
                            input = None;
                        }
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    Err(_) => {
                        result.stage = Stage::StdinFailed;
                        terminate(&mut child);
                        return;
                    }
                }
            }
            if !eof {
                let mut buffer = [0_u8; 512];
                match output.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        captured.extend_from_slice(&buffer[..count]);
                        if captured.len() > maximum {
                            result.stage = Stage::OutputTooLarge;
                            terminate(&mut child);
                            return;
                        }
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                    Err(_) => {
                        result.stage = Stage::StdoutFailed;
                        terminate(&mut child);
                        return;
                    }
                }
            }
            if exit.is_none() {
                match child.try_wait() {
                    Ok(status) => exit = status,
                    Err(_) => {
                        result.stage = Stage::Failed;
                        terminate(&mut child);
                        return;
                    }
                }
            }
            if let Some(status) = exit {
                result.exit_code = status.code();
                if !status.success() {
                    result.stage = Stage::Failed;
                    terminate(&mut child);
                    return;
                }
                if input.is_some() {
                    result.stage = Stage::StdinFailed;
                    terminate(&mut child);
                    return;
                }
                if eof {
                    if captured.iter().all(u8::is_ascii_whitespace) {
                        result.stage = Stage::Completed;
                    } else {
                        match serde_json::from_slice::<HoldNote>(&captured) {
                            Ok(note) if note.valid() => {
                                result.stage = Stage::Completed;
                                result.note = Some(note);
                            }
                            _ => result.stage = Stage::InvalidOutput,
                        }
                    }
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    result.elapsed_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn check(body: &str) -> (std::path::PathBuf, Registration) {
        let root = std::env::temp_dir().join(format!("attention-hold-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("check");
        fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let r = Registration {
            name: "jev".into(),
            executable: executable.to_str().unwrap().into(),
        };
        (root, r)
    }

    #[test]
    fn spaced_json_note_requires_completed_input_and_zero_exit() {
        for (body, stage, held) in [
            (
                "/bin/cat >/dev/null\nprintf '%s\\n' '{\"hold\": true, \"answer\": \"waiting_on_own_work\"}'",
                Stage::Completed,
                true,
            ),
            ("/bin/cat >/dev/null", Stage::Completed, false),
            (
                "/bin/cat >/dev/null\nprintf '%s\\n' '{\"hold\": true, \"answer\": \"waiting_on_own_work\"}'\nexit 7",
                Stage::Failed,
                false,
            ),
            (
                "/bin/cat >/dev/null\nprintf '%s' '{\"hold\":true,\"answer\":\"a\",\"hold\":true}'",
                Stage::InvalidOutput,
                false,
            ),
            (
                "/bin/cat >/dev/null\nprintf '%s' '{\"hold\":true,\"answer\":\"a\",\"actor\":\"lead\"}'",
                Stage::InvalidOutput,
                false,
            ),
            (
                "/bin/cat >/dev/null\nprintf '%s' '{} {}'",
                Stage::InvalidOutput,
                false,
            ),
        ] {
            let (root, r) = check(body);
            let result = dispatch(
                &r,
                &Uuid::new_v4().to_string(),
                b"{}\n",
                Instant::now() + Duration::from_secs(20),
            );
            assert_eq!(result.stage, stage);
            assert_eq!(result.note.is_some(), held);
            assert!(result.valid());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn timeout_covers_stdin_and_inherited_stdout() {
        for body in [
            "/bin/sleep 4",
            "/bin/cat >/dev/null\n/bin/sleep 4 &\nexit 0",
        ] {
            let (root, r) = check(body);
            let began = Instant::now();
            let result = run(&[r], |_, _| Some(vec![b'a'; 500_000])).remove(0);
            assert_eq!(result.stage, Stage::TimedOut);
            assert!(result.note.is_none());
            assert!(began.elapsed() < Duration::from_millis(3000));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn timeout_terminates_the_stdout_descendant() {
        use crate::wezterm::{ProcessInspector, ProcessRead, SystemProcessInspector};
        let (root, mut registration) = check("exit 0");
        let pid_file = root.join("descendant.pid");
        fs::write(&registration.executable,format!(
            "#!/bin/sh\n/bin/cat >/dev/null\n/bin/sleep 20 &\nprintf '%s' \"$!\" > '{}'\nexit 0\n",pid_file.display())).unwrap();
        registration.name = "descendant".into();
        let outcome = dispatch(
            &registration,
            &Uuid::new_v4().to_string(),
            b"{}\n",
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome.stage, Stage::TimedOut);
        let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let stopped = loop {
            match SystemProcessInspector.process(pid) {
                ProcessRead::Gone => break true,
                ProcessRead::Found(facts) if facts.zombie => break true,
                _ if Instant::now() >= deadline => break false,
                ProcessRead::Found(_) | ProcessRead::Unknown => {
                    std::thread::sleep(Duration::from_millis(10))
                }
            }
        };
        if !stopped {
            // SAFETY: this pid was written by the disposable program we started.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        fs::remove_dir_all(root).unwrap();
        assert!(
            stopped,
            "timeout left its stdout-holding descendant running"
        );
    }

    #[test]
    fn ordered_checks_preserve_each_outcome() {
        let (first_root, mut first) = check(
            "/bin/cat >/dev/null\nprintf '%s\\n' '{\"hold\": true, \"answer\": \"waiting_on_own_work\"}'",
        );
        let (second_root, mut second) = check("/bin/cat >/dev/null\nexit 7");
        let (third_root, mut third) = check("/bin/cat >/dev/null\n/bin/sleep 3");
        let (fourth_root, mut fourth) = check("exit 99");
        first.name = "first".into();
        second.name = "second".into();
        third.name = "third".into();
        fourth.name = "fourth".into();
        let outcomes = run(&[first, second, third, fourth], |_, _| {
            Some(b"{}\n".to_vec())
        });
        assert_eq!(
            outcomes.iter().map(|o| o.stage).collect::<Vec<_>>(),
            [
                Stage::Completed,
                Stage::Failed,
                Stage::TimedOut,
                Stage::NotDispatched
            ]
        );
        assert_eq!(outcomes[3].reason, Some(Bypass::TotalDeadline));
        assert_eq!(notes(&outcomes).len(), 1);
        assert!(outcomes.iter().all(HoldCheckOutcome::valid));
        for root in [first_root, second_root, third_root, fourth_root] {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn startup_output_and_stdin_failures_produce_no_note() {
        for (body, input_size, expected) in [
            (
                "/bin/cat >/dev/null\nprintf '%s' '{\"hold\": false, \"answer\": \"needs_user\"}'",
                2,
                Stage::InvalidOutput,
            ),
            ("exec 0<&-\n/bin/sleep 0.1", 500_000, Stage::StdinFailed),
            (
                "/bin/cat >/dev/null\n/bin/dd if=/dev/zero bs=512 count=1 2>/dev/null",
                2,
                Stage::OutputTooLarge,
            ),
        ] {
            let (root, r) = check(body);
            // This exercises the I/O failure itself; the chain's 2 s budget
            // has separate deadline tests. Leave startup scheduling out of
            // this branch's assertion by supplying a generous private deadline.
            let outcome = dispatch(
                &r,
                &Uuid::new_v4().to_string(),
                &vec![b'x'; input_size],
                Instant::now() + Duration::from_secs(20),
            );
            assert_eq!(outcome.stage, expected);
            assert!(outcome.note.is_none());
            fs::remove_dir_all(root).unwrap();
        }
        let (root, r) = check("exit 0");
        fs::write(&r.executable, "not an executable format").unwrap();
        let outcome = run(&[r], |_, _| Some(b"{}\n".to_vec())).remove(0);
        assert_eq!(outcome.stage, Stage::NotStarted);
        assert!(outcome.note.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registration_validation_reserves_escaped_audit_bytes() {
        assert!(validate_registrations(&["jev=/opt/example/check".into()]).is_ok());
        for values in [
            vec!["jev=relative".into()],
            vec!["Jev=/opt/example/check".into()],
            vec!["jev=/opt/a".into(), "jev=/opt/b".into()],
            (0..4)
                .map(|n| format!("check{n}=/{}", "\\".repeat(4000)))
                .collect(),
        ] {
            assert!(validate_registrations(&values).is_err());
        }
    }
}
