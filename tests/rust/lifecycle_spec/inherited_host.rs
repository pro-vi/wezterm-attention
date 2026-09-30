//! An event that inherited a shell claim's launch id, from a hook that names
//! its agent process: that process must hold the terminal the claim was made
//! at.

use super::self_claim::{every_action, records, start};
use super::*;

/// The environment of a hook that inherited the pane's launch id and names
/// its agent, as a hook registered as
/// `WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event ...` does.
fn asserting_env(setup: &Setup) -> BTreeMap<String, String> {
    let mut env = setup.env.clone();
    env.insert(
        "WEZTERM_ATTENTION_HOST_PID".to_owned(),
        AGENT_PID.to_string(),
    );
    env
}

fn outcome(
    setup: &Setup,
    env: &BTreeMap<String, String>,
    event: &ProviderEvent,
    observation: &str,
) -> wezterm_attention::lifecycle::LifecycleResult {
    apply_provider_event(event, env, observation, &setup.ports())
        .expect("a refusal is a result, not an error")
}

/// The agent runs its sessions in a server it started with no terminal, the
/// way Codex's shared app-server runs: the hook and the process that runs it
/// both have none, and the environment is the one of the pane that started
/// the server.
fn detach_agent(setup: &Setup) {
    setup.processes.change(AGENT_PID, |agent| {
        agent.terminal = ControllingTerminal::Absent;
    });
}

#[test]
fn a_session_run_by_a_server_with_no_terminal_writes_nothing_to_the_pane_it_inherited() {
    let setup = Setup::new();
    setup.claim();
    detach_agent(&setup);
    let env = asserting_env(&setup);
    let before = records(&setup);
    for event in every_action() {
        let result = outcome(&setup, &env, &event, "00000000000000000900");
        assert_eq!(result.disposition, "ignored", "{}", event.source_event);
        let diagnostic = result.diagnostic.expect("a refusal says why");
        assert_eq!(
            diagnostic.code,
            DiagnosticCode::SessionDetached,
            "{}: {diagnostic:?}",
            event.source_event
        );
        for part in ["background", "--no-daemon"] {
            assert!(diagnostic.message.contains(part), "{part}: {diagnostic:?}");
        }
    }
    assert_eq!(records(&setup), before);
}

#[test]
fn a_session_start_from_a_server_with_no_terminal_binds_nothing() {
    let setup = Setup::new();
    setup.claim();
    detach_agent(&setup);
    let env = asserting_env(&setup);
    let result = outcome(&setup, &env, &start("codex", "s"), "00000000000000000200");
    assert_eq!(result.disposition, "ignored");
    assert_eq!(
        result.diagnostic.expect("why").code,
        DiagnosticCode::SessionDetached
    );
    assert!(!setup.binding_dir("codex", "s").exists());
}

#[test]
fn an_agent_on_another_terminal_or_one_that_cannot_be_read_writes_nothing() {
    type Arrange = Box<dyn Fn(&Setup)>;
    let cases: Vec<(&str, &str, Arrange)> = vec![
        (
            "an agent on a terminal of its own inside the pane, as in tmux",
            "unsafe_tty",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |agent| {
                    agent.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 7)
                })
            }),
        ),
        (
            "a hook on another terminal",
            "unsafe_tty",
            Box::new(|setup| {
                setup.processes.change(HOOK_PID, |hook| {
                    hook.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 7)
                })
            }),
        ),
        (
            "an agent whose terminal the kernel does not name",
            "probe_unavailable",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |agent| {
                    agent.terminal = ControllingTerminal::Unknown
                })
            }),
        ),
        (
            "an agent that cannot be read",
            "self_claim_parent_unverified",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Unknown)),
        ),
        (
            "a hook whose parent is not the agent it names",
            "self_claim_parent_unverified",
            Box::new(|setup| {
                setup.processes.set(
                    4500,
                    ProcessRead::Found(process_facts(4500, AGENT_PID, ControllingTerminal::Absent)),
                );
                setup
                    .processes
                    .change(HOOK_PID, |hook| hook.parent_pid = 4500);
            }),
        ),
        (
            "a claim terminal that is no longer a terminal device",
            "unsafe_tty",
            Box::new(|setup| setup.processes.devices.lock().unwrap().clear()),
        ),
    ];
    for (label, code, arrange) in cases {
        let setup = Setup::new();
        setup.claim();
        arrange(&setup);
        let env = asserting_env(&setup);
        let before = records(&setup);
        for event in [start("codex", "s"), start("claude", "c"), start("pi", "p")] {
            let result = outcome(&setup, &env, &event, "00000000000000000900");
            assert_eq!(result.disposition, "ignored", "{label}");
            let diagnostic = result.diagnostic.expect("a refusal says why");
            assert_eq!(diagnostic.code.as_str(), code, "{label}: {diagnostic:?}");
        }
        assert_eq!(records(&setup), before, "{label}");
    }
}

/// Start a session and send one tool event, both as `env`, and require both
/// to be written with their lifecycle facts.
fn assert_session_accepted(setup: &Setup, env: &BTreeMap<String, String>, provider: &str) {
    let result = outcome(setup, env, &start(provider, "s"), "00000000000000000200");
    assert_eq!(result.disposition, "applied", "{provider}: {result:?}");
    let next = match provider {
        "pi" => event("pi", "bus", "s", json!({"state":"review"})),
        _ => event(
            provider,
            "PreToolUse",
            "s",
            json!({"tool_name":"shell","tool_use_id":"call-1"}),
        ),
    };
    let outcome = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &next,
        env,
        "00000000000000000300",
        &setup.ports(),
    );
    let result = outcome.result.expect("event applies");
    assert_eq!(result.disposition, "applied", "{provider}: {result:?}");
    assert!(result.diagnostic.is_none(), "{provider}: {result:?}");
    assert!(
        setup
            .binding_dir(provider, "s")
            .join("binding.json")
            .exists()
    );
}

#[test]
fn claude_and_codex_on_the_pane_terminal_are_accepted_with_the_hook_detached() {
    // Claude Code, and Codex run with `--no-daemon`, run the session in the
    // process on the pane's terminal and start each hook with no terminal.
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        setup.claim();
        assert_session_accepted(&setup, &asserting_env(&setup), provider);
        assert!(
            setup
                .binding_dir(provider, "s")
                .join("lifecycle.json")
                .exists(),
            "{provider}: an inherited launch keeps its lifecycle facts"
        );
    }
}

#[test]
fn pi_asserting_its_own_pid_is_accepted() {
    // Pi starts the writer itself, as its direct child on the pane's
    // terminal, and names its own pid.
    let setup = Setup::new();
    setup.claim();
    setup.processes.change(HOOK_PID, |hook| {
        hook.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE)
    });
    assert_session_accepted(&setup, &asserting_env(&setup), "pi");
}

#[test]
fn without_a_host_assertion_or_process_reads_an_inherited_launch_decides_alone() {
    // With nothing naming the agent, no process can be taken for it: the
    // server's own parent is the agent of the pane that started it, on that
    // pane's terminal. The event is accepted as the launch id says.
    let setup = Setup::new();
    setup.claim();
    detach_agent(&setup);
    assert_session_accepted(&setup, &setup.env, "codex");

    // Where the process table cannot be read, the check is not made.
    let setup = Setup::new();
    setup.claim();
    detach_agent(&setup);
    *setup.processes.supported.lock().unwrap() = false;
    assert_session_accepted(&setup, &asserting_env(&setup), "codex");
}
