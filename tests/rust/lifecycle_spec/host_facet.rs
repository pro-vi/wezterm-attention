//! The agent process a binding's lead hooks proved, which the writer keeps in
//! the binding's `host.json` and `inspect` reports in its `host` facet after
//! checking it again.

use super::self_claim::{apply_as, launch_of, setup_launch_dir, start, start_agent, stored_claim};
use super::*;
use wezterm_attention::host::{HostCheckPorts, HostFacet};
use wezterm_attention::query::{
    PaneFacts, PaneScope, read_pane_facts_checking_host, read_pane_facts_with_ports,
};
use wezterm_attention::records::FileRecords;

const SESSION: &str = "s";
/// A second agent in the pane, and the hook it runs.
const SECOND_AGENT: i32 = 4100;
const SECOND_HOOK: i32 = 5100;

/// A pane its shell claimed, where `provider` started session "s" from a hook
/// that names its agent.
fn shell_session(provider: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &start(provider, SESSION),
        "00000000000000000200",
    );
    assert_eq!(result.disposition, "applied", "{provider}: {result:?}");
    setup
}

pub(super) fn launch(setup: &Setup) -> String {
    launch_of(&stored_claim(setup).expect("the pane's claim"))
}

fn host_path(setup: &Setup, provider: &str) -> PathBuf {
    let launch = launch(setup);
    setup_launch_dir(setup, &launch)
        .join("bindings")
        .join(binding_id(provider, SESSION, &launch))
        .join("host.json")
}

fn stored_host(setup: &Setup, provider: &str) -> Option<String> {
    fs::read_to_string(host_path(setup, provider)).ok()
}

fn stored_host_json(setup: &Setup, provider: &str) -> Value {
    serde_json::from_str(&stored_host(setup, provider).expect("host.json")).expect("host JSON")
}

fn scope(setup: &Setup, binding: &str) -> PaneScope {
    PaneScope::new(
        pane_address(&setup.env).expect("address").0,
        launch(setup),
        Some(binding.to_owned()),
    )
    .expect("scope")
}

/// The `host` facet `inspect` gives for the binding `binding`, checking the
/// terminal fingerprint with `tty`.
pub(super) fn facts_with(setup: &Setup, binding: &str, tty: &dyn TtyWriter) -> PaneFacts {
    read_pane_facts_checking_host(
        &state_root(&setup.env).expect("state root"),
        &scope(setup, binding),
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
        Some(&HostCheckPorts {
            processes: &setup.processes,
            tty,
        }),
    )
    .expect("inspect answers")
}

fn checked_with(setup: &Setup, binding: &str, tty: &dyn TtyWriter) -> HostFacet {
    facts_with(setup, binding, tty).host
}

fn session_binding(setup: &Setup, provider: &str) -> String {
    binding_id(provider, SESSION, &launch(setup))
}

/// The `host` facet `inspect` gives for session "s" of `provider`.
fn facet(setup: &Setup, provider: &str) -> Value {
    let binding = session_binding(setup, provider);
    serde_json::to_value(checked_with(setup, &binding, &setup.tty)).expect("facet JSON")
}

fn refused(facet: &Value, availability: &str, reason: &str) -> bool {
    facet["availability"] == availability
        && facet["reason"] == reason
        && facet.get("record").is_none()
}

#[test]
fn a_shell_claimed_agent_that_names_itself_is_recorded_and_checked_as_the_host() {
    let setup = shell_session("codex");
    let agent = setup.processes.facts(AGENT_PID);
    assert_eq!(
        facet(&setup, "codex"),
        json!({
            "availability": "available",
            "checked_at_unix_ns": "00000000012345678900",
            "record": {
                "pid": AGENT_PID,
                "start": {
                    "seconds": agent.start.seconds.to_string(),
                    "microseconds": agent.start.microseconds,
                },
                "boot_session_id": BOOT_SESSION,
                "terminal": {
                    "path": setup.tty.path,
                    "device": PANE_TTY_DEVICE.to_string(),
                    "fingerprint": setup.tty.fingerprint,
                },
                "admitted": {
                    "source_event": "SessionStart",
                    "written_at_unix_ns": "00000000012345678900",
                },
                "foreground": true,
            },
        })
    );
    let stored = stored_host_json(&setup, "codex");
    assert_eq!(stored["kind"], "host");
    assert_eq!(stored["owner_pid"], AGENT_PID.to_string());
    assert!(stored.get("contested_at_unix_ns").is_none());

    // The host's process group is a fact read at the check, not a stored one.
    setup.processes.change(AGENT_PID, |agent| {
        agent.terminal_foreground_group = SHELL_PID
    });
    assert_eq!(facet(&setup, "codex")["record"]["foreground"], false);
}

#[test]
fn an_agent_s_own_claim_records_its_owner_as_the_host() {
    for provider in ["claude", "codex", "pi"] {
        let setup = Setup::new();
        let result = apply_as(
            &setup,
            &setup.agent_env(),
            &start(provider, SESSION),
            "00000000000000000200",
        );
        assert_eq!(result.disposition, "applied", "{provider}: {result:?}");
        let claim = stored_claim(&setup).expect("the agent claimed the pane");
        let facet = facet(&setup, provider);
        assert_eq!(facet["availability"], "available", "{provider}: {facet}");
        let record = &facet["record"];
        assert_eq!(
            record["pid"].to_string(),
            claim["owner_pid"].as_str().unwrap()
        );
        assert_eq!(record["start"]["seconds"], claim["owner_started_sec"]);
        assert_eq!(
            record["start"]["microseconds"].to_string(),
            claim["owner_started_usec"].as_str().unwrap()
        );
        assert_eq!(record["boot_session_id"], claim["owner_boot_session_id"]);
        assert_eq!(record["terminal"]["path"], claim["tty_path"]);
        assert_eq!(record["terminal"]["fingerprint"], claim["tty_fingerprint"]);
    }
}

#[test]
fn a_hook_that_proves_the_same_host_or_names_none_rewrites_nothing() {
    let mut setup = shell_session("codex");
    let before = stored_host(&setup, "codex").expect("host.json");
    setup.clock = FixedClock {
        monotonic: "00000000000000000100",
        unix: "00000000099999999900",
    };
    let prompt = event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"}));
    let tool = event(
        "codex",
        "PreToolUse",
        SESSION,
        json!({"tool_name":"shell","tool_use_id":"call-1"}),
    );
    let named = setup.naming(AGENT_PID);
    for (env, event, observation) in [
        (&named, &prompt, "00000000000000000300"),
        (&named, &tool, "00000000000000000400"),
        (&setup.env, &prompt, "00000000000000000500"),
        (&setup.env, &tool, "00000000000000000600"),
    ] {
        // A second prompt in a row repeats the badge, which is skipped.
        let result = apply_as(&setup, env, event, observation);
        assert!(
            ["applied", "skipped"].contains(&result.disposition.as_str()),
            "{result:?}"
        );
        assert!(result.diagnostic.is_none(), "{result:?}");
    }
    assert_eq!(
        stored_host(&setup, "codex").as_deref(),
        Some(before.as_str())
    );
    assert_eq!(
        facet(&setup, "codex")["record"]["admitted"],
        json!({"source_event":"SessionStart","written_at_unix_ns":"00000000012345678900"})
    );
}

#[test]
fn a_hook_that_names_no_agent_records_no_host() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(&start("codex", SESSION), "00000000000000000200");
    assert!(stored_host(&setup, "codex").is_none());
    assert!(refused(&facet(&setup, "codex"), "absent", "none_recorded"));
}

#[test]
fn a_sub_agent_s_hook_records_no_host() {
    let setup = shell_session("claude");
    fs::remove_file(host_path(&setup, "claude")).expect("remove host.json");
    let child = event(
        "claude",
        "PreToolUse",
        SESSION,
        json!({"agent_id":"child-a","agent_type":"Explore","tool_name":"Bash"}),
    );
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &child,
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
    assert!(stored_host(&setup, "claude").is_none());
}

/// Put a second agent in the pane, on its terminal, running the hook that
/// reads the process table now, and give the environment its hooks run in.
pub(super) fn second_agent(setup: &Setup) -> BTreeMap<String, String> {
    let mut env = start_agent(setup, SECOND_AGENT, SECOND_HOOK);
    env.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
    );
    env
}

#[test]
fn a_second_live_agent_contests_the_host_until_the_first_is_gone() {
    let setup = shell_session("codex");
    let env = second_agent(&setup);
    let tool = event(
        "codex",
        "PreToolUse",
        SESSION,
        json!({"tool_name":"shell","tool_use_id":"call-1"}),
    );
    let result = apply_as(&setup, &env, &tool, "00000000000000000300");
    assert_eq!(result.disposition, "applied", "{result:?}");
    let contested = stored_host_json(&setup, "codex");
    assert_eq!(contested["owner_pid"], AGENT_PID.to_string(), "kept");
    assert_eq!(contested["contested_at_unix_ns"], "00000000012345678900");
    assert!(refused(
        &facet(&setup, "codex"),
        "unavailable",
        "host_contested"
    ));

    // A further proof by either process leaves the record contested.
    let before = stored_host(&setup, "codex");
    apply_as(&setup, &env, &tool, "00000000000000000400");
    *setup.processes.own.lock().unwrap() = HOOK_PID;
    apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &tool,
        "00000000000000000500",
    );
    assert_eq!(stored_host(&setup, "codex"), before);

    // Once the first has exited, the record no longer names a live host, and
    // the next proof replaces it.
    setup.processes.set(AGENT_PID, ProcessRead::Gone);
    assert!(refused(
        &facet(&setup, "codex"),
        "unavailable",
        "host_exited"
    ));
    *setup.processes.own.lock().unwrap() = SECOND_HOOK;
    apply_as(&setup, &env, &tool, "00000000000000000600");
    let replaced = stored_host_json(&setup, "codex");
    assert_eq!(replaced["owner_pid"], SECOND_AGENT.to_string());
    assert!(replaced.get("contested_at_unix_ns").is_none());
    let facet = facet(&setup, "codex");
    assert_eq!(facet["availability"], "available", "{facet}");
    assert_eq!(facet["record"]["pid"], SECOND_AGENT);
    assert_eq!(facet["record"]["admitted"]["source_event"], "PreToolUse");
}

#[test]
fn a_hook_from_a_process_off_the_pane_terminal_leaves_the_host_as_it_is() {
    let setup = shell_session("codex");
    let before = stored_host(&setup, "codex");
    let env = second_agent(&setup);
    // A helper the agent started on a terminal of its own, as an
    // `app-server --stdio` child or a nested multiplexer has.
    setup.processes.change(SECOND_AGENT, |helper| {
        helper.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 7)
    });
    let tool = event("codex", "PreToolUse", SESSION, json!({"tool_name":"shell"}));
    let result = apply_as(&setup, &env, &tool, "00000000000000000300");
    assert_eq!(result.disposition, "ignored", "{result:?}");
    assert_eq!(
        result.diagnostic.expect("why").code,
        DiagnosticCode::UnsafeTty
    );
    assert_eq!(stored_host(&setup, "codex"), before);
    *setup.processes.own.lock().unwrap() = HOOK_PID;
    assert_eq!(facet(&setup, "codex")["record"]["pid"], AGENT_PID);
}

#[test]
fn inspect_refuses_a_recorded_host_that_no_longer_holds() {
    type Arrange = Box<dyn Fn(&Setup)>;
    let cases: Vec<(&str, &str, &str, Arrange)> = vec![
        (
            "the host exited",
            "unavailable",
            "host_exited",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Gone)),
        ),
        (
            "the host exited and is not yet reaped",
            "unavailable",
            "host_exited",
            Box::new(|setup| setup.processes.change(AGENT_PID, |host| host.zombie = true)),
        ),
        (
            "another process holds its pid",
            "unavailable",
            "host_replaced",
            Box::new(|setup| {
                setup
                    .processes
                    .change(AGENT_PID, |host| host.start.microseconds += 1)
            }),
        ),
        (
            "its pid runs as another user",
            "unavailable",
            "host_replaced",
            Box::new(|setup| setup.processes.change(AGENT_PID, |host| host.uid = 0)),
        ),
        (
            "the machine booted again",
            "unavailable",
            "boot_changed",
            Box::new(|setup| {
                *setup.processes.boot.lock().unwrap() =
                    Some("1f9a7c3e-51b2-4d6e-8a1b-2c3d4e5f6a7b".into())
            }),
        ),
        (
            "the boot cannot be read",
            "unavailable",
            "probe_unavailable",
            Box::new(|setup| *setup.processes.boot.lock().unwrap() = None),
        ),
        (
            "the host cannot be read",
            "unavailable",
            "probe_unavailable",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Unknown)),
        ),
        (
            "the reader itself cannot be read",
            "unavailable",
            "probe_unavailable",
            Box::new(|setup| setup.processes.set(HOOK_PID, ProcessRead::Unknown)),
        ),
        (
            "the host runs on another terminal",
            "unavailable",
            "terminal_changed",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |host| {
                    host.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 7)
                })
            }),
        ),
        (
            "the host left its terminal",
            "unavailable",
            "terminal_changed",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |host| {
                    host.terminal = ControllingTerminal::Absent
                })
            }),
        ),
        (
            "the kernel names no device for the host's terminal",
            "unavailable",
            "probe_unavailable",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |host| {
                    host.terminal = ControllingTerminal::Unknown
                })
            }),
        ),
        (
            "the recorded path names another terminal",
            "unavailable",
            "terminal_changed",
            Box::new(|setup| {
                setup
                    .processes
                    .devices
                    .lock()
                    .unwrap()
                    .insert(setup.tty.path.clone(), PANE_TTY_DEVICE + 7);
            }),
        ),
        (
            "the recorded path is no longer a terminal",
            "unavailable",
            "terminal_changed",
            Box::new(|setup| setup.processes.devices.lock().unwrap().clear()),
        ),
        (
            "the process table cannot be read on this platform",
            "unsupported",
            "platform_unsupported",
            Box::new(|setup| *setup.processes.supported.lock().unwrap() = false),
        ),
    ];
    for (label, availability, reason, arrange) in cases {
        let setup = shell_session("codex");
        let before = stored_host(&setup, "codex");
        arrange(&setup);
        let facet = facet(&setup, "codex");
        assert!(refused(&facet, availability, reason), "{label}: {facet}");
        assert_eq!(
            facet["checked_at_unix_ns"], "00000000012345678900",
            "{label}"
        );
        assert_eq!(
            stored_host(&setup, "codex"),
            before,
            "{label}: a read wrote"
        );
    }

    // The terminal at the recorded path is another device node with the
    // same number.
    let setup = shell_session("codex");
    let mut other = FakeTty::new();
    other.fingerprint = "e".repeat(64);
    let facet = serde_json::to_value(checked_with(
        &setup,
        &session_binding(&setup, "codex"),
        &other,
    ))
    .unwrap();
    assert!(
        refused(&facet, "unavailable", "terminal_changed"),
        "{facet}"
    );
}

#[test]
fn inspect_vouches_for_no_host_outside_a_live_matched_binding() {
    // Without a way to read the process table, nothing can be checked.
    let setup = shell_session("codex");
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope(&setup, &session_binding(&setup, "codex")),
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap();
    let host = serde_json::to_value(facts.host).unwrap();
    assert!(refused(&host, "unavailable", "probe_unavailable"), "{host}");

    // A scope naming another binding than the launch's current one.
    let other = binding_id("codex", "other", &launch(&setup));
    let host = serde_json::to_value(checked_with(&setup, &other, &setup.tty)).unwrap();
    assert!(refused(&host, "unavailable", "scope_not_matched"), "{host}");
    assert!(host.get("checked_at_unix_ns").is_none());

    // An ended binding.
    let setup = shell_session("claude");
    apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &event("claude", "SessionEnd", SESSION, json!({"reason":"other"})),
        "00000000000000000300",
    );
    assert!(refused(
        &facet(&setup, "claude"),
        "unavailable",
        "binding_ended"
    ));
}

#[test]
fn an_invalid_host_record_is_refused_and_recorded_again_by_the_next_proof() {
    type Spoil = fn(&mut Value);
    let cases: [(&str, Spoil); 2] = [
        ("not a host record", |record| {
            *record = json!({"kind":"host"})
        }),
        ("a pid no process has", |record| {
            record["owner_pid"] = json!("0")
        }),
    ];
    for (label, spoil) in cases {
        let setup = shell_session("codex");
        let path = host_path(&setup, "codex");
        let mut record = stored_host_json(&setup, "codex");
        spoil(&mut record);
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(
            refused(&facet(&setup, "codex"), "invalid", "record_invalid"),
            "{label}"
        );
        let result = apply_as(
            &setup,
            &setup.naming(AGENT_PID),
            &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
            "00000000000000000300",
        );
        assert_eq!(result.disposition, "applied", "{label}: {result:?}");
        let diagnostic = result.diagnostic.expect("the restart is reported");
        assert_eq!(diagnostic.code, DiagnosticCode::RecordInvalid, "{label}");
        assert_eq!(diagnostic.context["facet"], "host", "{label}");
        let facet = facet(&setup, "codex");
        assert_eq!(facet["availability"], "available", "{label}: {facet}");
        assert_eq!(
            facet["record"]["admitted"]["source_event"],
            "UserPromptSubmit"
        );
        let directory = path.parent().unwrap();
        assert!(
            fs::read_dir(directory).unwrap().flatten().any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".host.json.invalid.")),
            "{label}: the invalid record is kept aside"
        );
    }
}

/// A real agent in a real pane: a shell made the session leader of a fresh
/// pseudo-terminal, a mux socket and a `wezterm` that lists that terminal as
/// pane 42, so the writer and `inspect` read the kernel's own process table.
#[cfg(target_os = "macos")]
pub(super) mod real_processes {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::time::Instant;

    pub(in super::super) struct Pane {
        scratch: Scratch,
        _socket: UnixListener,
        master: i32,
        slave: i32,
        tty_path: String,
        shell: Option<std::process::Child>,
    }

    impl Drop for Pane {
        fn drop(&mut self) {
            // The shell leads the process group of everything it started.
            if let Some(shell) = &mut self.shell {
                let group = i32::try_from(shell.id()).expect("pid");
                unsafe { libc::killpg(group, libc::SIGKILL) };
                let _ = shell.wait();
            }
            unsafe {
                libc::close(self.slave);
                libc::close(self.master);
            }
        }
    }

    /// Read whatever reaches the terminal, as the mux does: a session leader
    /// that exits waits until its terminal's output has been read, and the
    /// claim's publication is written there.
    fn drain(master: i32) {
        let reader = unsafe { libc::dup(master) };
        thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while unsafe { libc::read(reader, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
            unsafe { libc::close(reader) };
        });
    }

    impl Pane {
        pub(in super::super) fn new() -> Self {
            let scratch = Scratch::new();
            let socket = UnixListener::bind(scratch.0.join("mux.sock")).expect("bind socket");
            let (mut master, mut slave) = (0, 0);
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            drain(master);
            let tty_path = wezterm_attention::wezterm::tty_path_from_fd(slave).expect("tty path");
            let wezterm = scratch.0.join("wezterm");
            fs::write(
                &wezterm,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                    json!([{"pane_id": 42, "tty_name": tty_path}])
                ),
            )
            .unwrap();
            fs::set_permissions(&wezterm, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                scratch.0.join("payload.json"),
                serde_json::to_vec(&payload(
                    "codex",
                    "SessionStart",
                    "real",
                    json!({"source":"startup"}),
                ))
                .unwrap(),
            )
            .unwrap();
            // The agent runs its hook as Codex does: a shell it starts runs
            // the registered command, which hands the agent's pid on.
            fs::write(
                scratch.0.join("agent.sh"),
                concat!(
                    "sh -c 'WEZTERM_ATTENTION_HOST_PID=$PPID exec \"$ATTENTION\" ",
                    "hooks event codex SessionStart --strict --debug' ",
                    "< \"$OUT/payload.json\" > \"$OUT/start.out\" 2> \"$OUT/start.err\"\n",
                    "echo $? > \"$OUT/start.status\"\n",
                    "echo $$ > \"$OUT/agent.tmp\" && mv \"$OUT/agent.tmp\" \"$OUT/agent.pid\"\n",
                    "sleep 60\n",
                ),
            )
            .unwrap();
            Self {
                scratch,
                _socket: socket,
                master,
                slave,
                tty_path,
                shell: None,
            }
        }

        /// The scratch directory the pane's scripts and records live in.
        pub(in super::super) fn dir(&self) -> &std::path::Path {
            &self.scratch.0
        }

        pub(in super::super) fn env(&self) -> Vec<(String, String)> {
            let dir = self.scratch.0.display().to_string();
            vec![
                ("HOME".into(), dir.clone()),
                ("OUT".into(), dir.clone()),
                ("PATH".into(), format!("{dir}:/usr/bin:/bin")),
                ("WEZTERM_ATTENTION_DIR".into(), format!("{dir}/state")),
                ("WEZTERM_UNIX_SOCKET".into(), format!("{dir}/mux.sock")),
                ("WEZTERM_PANE".into(), "42".into()),
                ("ATTENTION".into(), env!("CARGO_BIN_EXE_attention").into()),
            ]
        }

        /// Start the pane's shell running `script` on the terminal, as its
        /// controlling terminal.
        pub(in super::super) fn start_shell(&mut self, script: &str, launch: Option<&str>) {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]).env_clear().envs(self.env());
            if let Some(launch) = launch {
                command.env("WEZTERM_ATTENTION_LAUNCH_ID", launch);
            }
            let stdio = || unsafe { Stdio::from(fs::File::from_raw_fd(libc::dup(self.slave))) };
            command.stdin(stdio()).stdout(stdio()).stderr(stdio());
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            self.shell = Some(command.spawn().expect("start the pane's shell"));
        }

        /// The agent's pid, once its hook has run.
        pub(in super::super) fn agent(&self) -> i32 {
            let path = self.scratch.0.join("agent.pid");
            let read = |name: &str| {
                fs::read_to_string(self.scratch.0.join(name))
                    .unwrap_or_else(|_| format!("({name} absent)"))
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            while !path.exists() {
                assert!(
                    Instant::now() < deadline,
                    "the agent never ran its hook: claim {} {} start {}",
                    read("claim.out"),
                    read("claim.err"),
                    read("start.err")
                );
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                read("start.status").trim(),
                "0",
                "the hook failed: {}",
                read("start.err")
            );
            read("agent.pid").trim().parse().expect("agent pid")
        }

        fn inspect(&self) -> Value {
            let claim = self.claim();
            let launch = claim["launch_id"].as_str().unwrap();
            let scope = json!({
                "address": claim["address"],
                "launch_id": launch,
                "binding_id": binding_id("codex", "real", launch),
            });
            let mut child = Command::new(env!("CARGO_BIN_EXE_attention"))
                .args(["inspect", "--scope", "-", "--json"])
                .env_clear()
                .envs(self.env())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("run inspect");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&serde_json::to_vec(&scope).unwrap())
                .unwrap();
            let output = child.wait_with_output().expect("inspect exits");
            let envelope: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|_| panic!("inspect envelope: {output:?}"));
            envelope["result"]["host"].clone()
        }

        /// The pane's claim, as the hook that made it stored it.
        pub(in super::super) fn claim(&self) -> Value {
            let env: BTreeMap<String, String> = self.env().into_iter().collect();
            let path = pane_dir(&state_root(&env).unwrap(), &pane_address(&env).unwrap().0)
                .join("claim.json");
            serde_json::from_slice(&fs::read(path).expect("a claim was made")).unwrap()
        }

        pub(in super::super) fn kill_shell(&mut self) {
            let mut shell = self.shell.take().expect("a shell runs");
            let group = i32::try_from(shell.id()).expect("pid");
            unsafe { libc::killpg(group, libc::SIGKILL) };
            shell.wait().expect("reap the shell");
        }
    }

    /// What `ps` says of `pid`, independently of the reads under test.
    fn ps(pid: i32, field: &str) -> String {
        let output = Command::new("/bin/ps")
            .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn assert_available_for(pane: &Pane, agent: i32) {
        let host = pane.inspect();
        assert_eq!(host["availability"], "available", "{host}");
        assert_eq!(host["record"]["pid"], agent, "{host}");
        assert_eq!(host["record"]["terminal"]["path"], pane.tty_path.as_str());
        assert_eq!(host["record"]["foreground"], true, "{host}");
        // The process the record names is the agent this test started, on
        // this terminal.
        assert_eq!(
            format!("/dev/{}", ps(agent, "tty")),
            pane.tty_path,
            "the recorded host is not on the pane's terminal"
        );
        let device = {
            use std::os::unix::fs::MetadataExt;
            u64::from(fs::metadata(&pane.tty_path).unwrap().rdev() as u32)
        };
        assert_eq!(host["record"]["terminal"]["device"], device.to_string());
    }

    fn assert_refused_once_the_agent_is_killed(mut pane: Pane) {
        pane.kill_shell();
        let host = pane.inspect();
        assert_eq!(
            (host["availability"].as_str(), host["reason"].as_str()),
            (Some("unavailable"), Some("host_exited")),
            "{host}"
        );
    }

    #[test]
    fn a_real_agent_started_by_the_pane_s_claiming_shell_is_its_binding_s_host() {
        let mut pane = Pane::new();
        pane.start_shell(
            concat!(
                "\"$ATTENTION\" hooks claim --json > \"$OUT/claim.out\" 2> \"$OUT/claim.err\" ",
                "&& sh \"$OUT/agent.sh\"",
            ),
            Some("00000000-0000-4000-8000-000000000a01"),
        );
        let agent = pane.agent();
        let shell = i32::try_from(pane.shell.as_ref().unwrap().id()).unwrap();
        assert_eq!(
            ps(agent, "ppid"),
            shell.to_string(),
            "the shell started the agent"
        );
        let claim = pane.claim();
        assert!(claim.get("owner_pid").is_none(), "a shell claim: {claim}");
        assert_available_for(&pane, agent);
        assert_refused_once_the_agent_is_killed(pane);
    }

    #[test]
    fn a_real_agent_that_claimed_the_pane_itself_is_its_binding_s_host() {
        let mut pane = Pane::new();
        pane.start_shell("sh \"$OUT/agent.sh\"", None);
        let agent = pane.agent();
        let claim = pane.claim();
        assert_eq!(claim["owner_pid"], agent.to_string(), "{claim}");
        assert_available_for(&pane, agent);
        assert_refused_once_the_agent_is_killed(pane);
    }
}
