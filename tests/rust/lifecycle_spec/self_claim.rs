use super::*;

use wezterm_attention::lifecycle::apply_provider_event_with_outcome;
use wezterm_attention::lifecycle::outcome::Persistence;

const AGENT_LAUNCH: &str = "00000000-0000-4000-8000-0000000005a1";

fn claim_path(setup: &Setup) -> PathBuf {
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    pane_path(&root, &address).join("claim.json")
}

fn stored_claim(setup: &Setup) -> Option<Value> {
    fs::read(claim_path(setup))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("claim JSON"))
}

/// A claim the agent process `owner` holds at the pane's terminal.
fn self_owned_claim(setup: &Setup, launch_id: &str, owner: i32) -> Value {
    let (address, _) = pane_address(&setup.env).expect("address");
    let start = setup.processes.facts(owner).start;
    json!({
        "kind": "claim",
        "schema": wezterm_attention::protocol::manifest().expect("manifest").record_schema,
        "address": address,
        "launch_id": launch_id,
        "tty_path": setup.tty.path,
        "tty_fingerprint": setup.tty.fingerprint,
        "observed_mono_ns": "00000000000000000050",
        "owner_pid": owner.to_string(),
        "owner_started_sec": start.seconds.to_string(),
        "owner_started_usec": start.microseconds.to_string(),
        "owner_boot_session_id": BOOT_SESSION,
    })
}

fn install(setup: &Setup, claim: &Value) {
    atomic_replace(&claim_path(setup), claim).expect("write claim");
}

fn launch_dir(setup: &Setup, launch_id: &str) -> PathBuf {
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    launch_path(&root, &address, launch_id)
}

/// Every record file under the state root and its bytes, lock files aside,
/// so a refusal can be shown to have written nothing.
fn records(setup: &Setup) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(directory: &std::path::Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                found.insert(path.clone(), fs::read(&path).expect("record bytes"));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(&state_root(&setup.env).expect("state root"), &mut found);
    found
}

/// One event for every action a provider event can take, in one session per
/// provider.
fn every_action() -> Vec<ProviderEvent> {
    let events = vec![
        event("codex", "SessionStart", "s", json!({"source":"startup"})),
        event(
            "codex",
            "PostToolUse",
            "s",
            json!({"tool_name":"shell","tool_use_id":"call-1"}),
        ),
        event("codex", "PreToolUse", "s", json!({"tool_name":"shell"})),
        event("codex", "Stop", "s", json!({"stop_hook_active":false})),
        event(
            "codex",
            "PreToolUse",
            "s",
            json!({"tool_name":"shell","agent_id":"child-a","agent_type":"worker"}),
        ),
        event(
            "codex",
            "SubagentStop",
            "s",
            json!({"agent_id":"child-a","agent_type":"worker","stop_hook_active":false}),
        ),
        event("codex", "SessionEnd", "s", json!({"reason":"other"})),
        event("codex", "Interrupt", "s", json!({})),
        event("pi", "bus", "s", json!({"state":"review"})),
        event("pi", "bus", "s", json!({"state":"clear"})),
    ];
    let actions: BTreeSet<_> = events.iter().map(|event| event.action.as_str()).collect();
    let declared: BTreeSet<_> = ProviderAction::ALL
        .iter()
        .filter(|action| **action != ProviderAction::Ignored)
        .map(|action| action.as_str())
        .collect();
    assert_eq!(actions, declared, "one event per action");
    events
}

fn refused(
    setup: &Setup,
    env: &BTreeMap<String, String>,
    event: &ProviderEvent,
) -> wezterm_attention::protocol::Diagnostic {
    let result = apply_provider_event(event, env, "00000000000000000900", &setup.ports())
        .expect("a refusal is a result, not an error");
    assert_eq!(result.disposition, "ignored", "{}", event.source_event);
    result.diagnostic.expect("a refusal says why")
}

#[test]
fn an_inherited_launch_id_needs_no_host_assertion_or_process_probe() {
    let setup = Setup::new();
    setup.claim();
    // Nothing about processes can be read, and the platform claims nothing
    // itself: the inherited path must not care.
    *setup.processes.supported.lock().unwrap() = false;
    *setup.processes.boot.lock().unwrap() = None;
    setup.processes.table.lock().unwrap().clear();
    let mut env = setup.env.clone();
    env.insert("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM".into(), "0".into());
    let start = event("codex", "SessionStart", "s", json!({"source":"startup"}));
    let result = apply_provider_event(&start, &env, "00000000000000000200", &setup.ports())
        .expect("event applies");
    assert_eq!(result.disposition, "applied");
    let tool = event(
        "codex",
        "PreToolUse",
        "s",
        json!({"tool_name":"shell","tool_use_id":"call-1"}),
    );
    let outcome =
        apply_provider_event_with_outcome(&tool, &env, "00000000000000000300", &setup.ports());
    assert_eq!(outcome.result.expect("tool applies").disposition, "applied");
    assert!(outcome.admission.is_some(), "rich admission stays with it");
    assert!(
        setup
            .binding_dir("codex", "s")
            .join("lifecycle.json")
            .exists()
    );
}

#[test]
fn a_stale_inherited_launch_id_never_falls_through_to_the_agent_s_own_claim() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let mut env = setup.agent_env();
    env.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        "00000000-0000-4000-8000-0000000005ff".into(),
    );
    let before = records(&setup);
    for event in every_action() {
        assert_eq!(refused(&setup, &env, &event).code, "claim_stale");
    }
    env.insert("WEZTERM_ATTENTION_LAUNCH_ID".into(), "not-a-uuid".into());
    for event in every_action() {
        assert_eq!(refused(&setup, &env, &event).code, "record_invalid");
    }
    assert_eq!(records(&setup), before);
}

#[test]
fn a_shell_claim_refuses_every_event_that_lacks_its_launch_id() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("codex", "SessionStart", "s", json!({"source":"startup"})),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "pi",
            "session_start",
            "s",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000210",
    );
    let before = records(&setup);
    for event in every_action() {
        let diagnostic = refused(&setup, &setup.agent_env(), &event);
        assert_eq!(diagnostic.code, "claim_stale", "{}", event.source_event);
        assert!(diagnostic.message.contains("shell claim"), "{diagnostic:?}");
    }
    assert_eq!(records(&setup), before);
}

#[test]
fn an_agent_s_own_claim_keeps_its_indicator_and_leaves_lifecycle_facts_unwritten() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let env = setup.agent_env();
    let start = event("codex", "SessionStart", "s", json!({"source":"startup"}));
    let result = apply_provider_event(&start, &env, "00000000000000000200", &setup.ports())
        .expect("start applies");
    assert_eq!(result.disposition, "applied");
    let tool = event(
        "codex",
        "PreToolUse",
        "s",
        json!({"tool_name":"shell","tool_use_id":"call-1"}),
    );
    assert!(
        tool.observation.is_some(),
        "the event carries lifecycle facts"
    );
    let outcome =
        apply_provider_event_with_outcome(&tool, &env, "00000000000000000300", &setup.ports());
    let result = outcome.result.expect("tool applies");
    assert_eq!(result.disposition, "applied");
    assert!(result.diagnostic.is_none(), "{:?}", result.diagnostic);
    assert!(outcome.admission.is_none());
    assert_eq!(outcome.persistence.lifecycle, Persistence::Rejected);
    assert_eq!(outcome.persistence.activity, Persistence::Confirmed);
    let binding = launch_dir(&setup, AGENT_LAUNCH)
        .join("bindings")
        .join(binding_id("codex", "s", AGENT_LAUNCH));
    assert!(binding.join("activity.json").exists());
    assert!(!binding.join("lifecycle.json").exists());
}

#[test]
fn switched_off_or_unsupported_self_claim_never_takes_a_weaker_path() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let tool = event("codex", "PreToolUse", "s", json!({"tool_name":"shell"}));
    let before = records(&setup);
    for value in ["0", "", "no", "true"] {
        let mut env = setup.agent_env();
        env.insert("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM".into(), value.into());
        for event in every_action() {
            let diagnostic = refused(&setup, &env, &event);
            assert_eq!(diagnostic.code, "claim_stale", "{value:?}");
            assert!(
                diagnostic.message.contains("switched off"),
                "{diagnostic:?}"
            );
        }
    }
    *setup.processes.supported.lock().unwrap() = false;
    for event in every_action() {
        let diagnostic = refused(&setup, &setup.agent_env(), &event);
        assert!(diagnostic.message.contains("platform"), "{diagnostic:?}");
    }
    assert_eq!(records(&setup), before);
    *setup.processes.supported.lock().unwrap() = true;
    let mut env = setup.agent_env();
    env.insert("WEZTERM_ATTENTION_ENABLE_SELF_CLAIM".into(), "1".into());
    apply_provider_event(
        &event("codex", "SessionStart", "s", json!({"source":"startup"})),
        &env,
        "00000000000000000200",
        &setup.ports(),
    )
    .expect("start applies");
    let result =
        apply_provider_event(&tool, &env, "00000000000000000300", &setup.ports()).expect("tool");
    assert_eq!(result.disposition, "applied", "1 leaves it on");
}

#[test]
fn a_hook_whose_origin_is_not_proven_changes_nothing() {
    let cases: Vec<(&str, Box<dyn Fn(&Setup, &mut BTreeMap<String, String>)>)> = vec![
        (
            "missing assertion, as a relay that drops it leaves",
            Box::new(|_, env| {
                env.remove("WEZTERM_ATTENTION_HOST_PID");
            }),
        ),
        (
            "empty assertion",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), String::new());
            }),
        ),
        (
            "zero",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "0".into());
            }),
        ),
        (
            "leading zero",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "04000".into());
            }),
        ),
        (
            "signed",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "+4000".into());
            }),
        ),
        (
            "negative",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "-4000".into());
            }),
        ),
        (
            "spaced",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), " 4000".into());
            }),
        ),
        (
            "not a number",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "$PPID".into());
            }),
        ),
        (
            "overflowing",
            Box::new(|_, env| {
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "2147483648".into());
            }),
        ),
        (
            "far overflowing",
            Box::new(|_, env| {
                env.insert(
                    "WEZTERM_ATTENTION_HOST_PID".into(),
                    "99999999999999999999999".into(),
                );
            }),
        ),
        (
            "a relay forwarding the agent's assertion",
            Box::new(|setup, _| {
                setup.processes.set(
                    6000,
                    ProcessRead::Found(process_facts(
                        6000,
                        SHELL_PID,
                        ControllingTerminal::Device(PANE_TTY_DEVICE),
                    )),
                );
                setup
                    .processes
                    .change(HOOK_PID, |hook| hook.parent_pid = 6000);
            }),
        ),
        (
            "a retained helper shell between agent and hook",
            Box::new(|setup, _| {
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
            "a traced hook",
            Box::new(|setup, _| setup.processes.change(HOOK_PID, |hook| hook.traced = true)),
        ),
        (
            "an orphaned hook",
            Box::new(|setup, env| {
                setup.processes.change(HOOK_PID, |hook| hook.parent_pid = 1);
                env.insert("WEZTERM_ATTENTION_HOST_PID".into(), "1".into());
            }),
        ),
        (
            "a parent of another user",
            Box::new(|setup, _| setup.processes.change(AGENT_PID, |agent| agent.uid = 0)),
        ),
        (
            "a parent that has exited",
            Box::new(|setup, _| {
                setup
                    .processes
                    .change(AGENT_PID, |agent| agent.zombie = true)
            }),
        ),
        (
            "a parent that cannot be read",
            Box::new(|setup, _| setup.processes.set(AGENT_PID, ProcessRead::Unknown)),
        ),
        (
            "a hook that cannot be read",
            Box::new(|setup, _| setup.processes.set(HOOK_PID, ProcessRead::Unknown)),
        ),
        (
            "a parent pid given to another process while it is checked",
            Box::new(|setup, _| {
                let mut changed = setup.processes.facts(AGENT_PID);
                changed.start.seconds += 1;
                // Each event reads the parent twice: once to prove it, and
                // once more after the listing. The second reading differs.
                let mut reads = 0;
                *setup.processes.on_read.lock().unwrap() = Some(Box::new(move |pid| {
                    if pid != AGENT_PID {
                        return None;
                    }
                    reads += 1;
                    (reads % 2 == 0).then(|| ProcessRead::Found(changed.clone()))
                }));
            }),
        ),
        (
            "a hook reparented while it is checked",
            Box::new(|setup, _| {
                let mut orphaned = setup.processes.facts(HOOK_PID);
                orphaned.parent_pid = 1;
                let mut reads = 0;
                *setup.processes.on_read.lock().unwrap() = Some(Box::new(move |pid| {
                    if pid != HOOK_PID {
                        return None;
                    }
                    reads += 1;
                    (reads % 2 == 0).then(|| ProcessRead::Found(orphaned.clone()))
                }));
            }),
        ),
    ];
    for (label, arrange) in cases {
        let setup = Setup::new();
        let claim = self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID);
        install(&setup, &claim);
        let mut env = setup.agent_env();
        apply_provider_event(
            &event("codex", "SessionStart", "s", json!({"source":"startup"})),
            &env,
            "00000000000000000200",
            &setup.ports(),
        )
        .expect("start applies");
        arrange(&setup, &mut env);
        let before = records(&setup);
        for event in every_action() {
            let diagnostic = refused(&setup, &env, &event);
            assert_eq!(
                diagnostic.code, "self_claim_parent_unverified",
                "{label}: {diagnostic:?}"
            );
        }
        assert_eq!(records(&setup), before, "{label}");
        assert_eq!(stored_claim(&setup).as_ref(), Some(&claim), "{label}");
    }
}

#[test]
fn a_detached_hook_and_one_that_holds_the_terminal_both_prove_their_agent() {
    for (label, hook_terminal) in [
        (
            "detached, as Claude Code runs it",
            ControllingTerminal::Absent,
        ),
        (
            "on the pane terminal",
            ControllingTerminal::Device(PANE_TTY_DEVICE),
        ),
    ] {
        let setup = Setup::new();
        setup
            .processes
            .change(HOOK_PID, |hook| hook.terminal = hook_terminal);
        install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
        let env = setup.agent_env();
        for (name, patch, disposition, observation) in [
            (
                "SessionStart",
                json!({"source":"startup"}),
                "applied",
                "00000000000000000300",
            ),
            (
                "UserPromptSubmit",
                json!({"prompt":"go"}),
                "applied",
                "00000000000000000400",
            ),
            (
                "Stop",
                json!({"stop_hook_active":false}),
                "applied",
                "00000000000000000500",
            ),
        ] {
            let result = apply_provider_event(
                &event("claude", name, "s", patch),
                &env,
                observation,
                &setup.ports(),
            )
            .expect("event applies");
            assert_eq!(result.disposition, disposition, "{label}: {name}");
        }
    }
}

#[test]
fn only_a_known_absent_terminal_lets_the_parent_s_terminal_decide() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let tool = event("codex", "PreToolUse", "s", json!({"tool_name":"shell"}));
    let cases: Vec<(&str, &str, Box<dyn Fn(&Setup)>)> = vec![
        (
            "a hook whose terminal cannot be read",
            "probe_unavailable",
            Box::new(|setup| {
                setup.processes.change(HOOK_PID, |hook| {
                    hook.terminal = ControllingTerminal::Unknown
                })
            }),
        ),
        (
            "a hook on another terminal, with its agent on the pane's",
            "unsafe_tty",
            Box::new(|setup| {
                setup.processes.change(HOOK_PID, |hook| {
                    hook.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 1)
                })
            }),
        ),
        (
            "an agent whose terminal cannot be read",
            "probe_unavailable",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |agent| {
                    agent.terminal = ControllingTerminal::Unknown
                })
            }),
        ),
        (
            "an agent with no terminal either",
            "unsafe_tty",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |agent| {
                    agent.terminal = ControllingTerminal::Absent
                })
            }),
        ),
        (
            "an agent in tmux inside the pane",
            "unsafe_tty",
            Box::new(|setup| {
                setup.processes.change(AGENT_PID, |agent| {
                    agent.terminal = ControllingTerminal::Device(PANE_TTY_DEVICE + 7)
                })
            }),
        ),
        (
            "a listing that fails",
            "realm_unavailable",
            Box::new(|setup| setup.panes.set(None)),
        ),
        (
            "a listing without the pane",
            "unsafe_tty",
            Box::new(|setup| {
                setup.panes.set(Some(vec![PaneRow {
                    pane_id: "43".into(),
                    tty_name: Some(setup.tty.path.clone()),
                }]))
            }),
        ),
        (
            "a listing with the pane twice",
            "unsafe_tty",
            Box::new(|setup| {
                let row = PaneRow {
                    pane_id: "42".into(),
                    tty_name: Some(setup.tty.path.clone()),
                };
                setup.panes.set(Some(vec![row.clone(), row]))
            }),
        ),
        (
            "a pane listed with no terminal",
            "unsafe_tty",
            Box::new(|setup| {
                setup.panes.set(Some(vec![PaneRow {
                    pane_id: "42".into(),
                    tty_name: None,
                }]))
            }),
        ),
        (
            "a pane terminal that is not a terminal device",
            "unsafe_tty",
            Box::new(|setup| setup.processes.devices.lock().unwrap().clear()),
        ),
        (
            "a boot session id that cannot be read",
            "probe_unavailable",
            Box::new(|setup| *setup.processes.boot.lock().unwrap() = None),
        ),
    ];
    let before = records(&setup);
    for (label, code, arrange) in cases {
        let setup_state = (
            setup.processes.table.lock().unwrap().clone(),
            setup.processes.devices.lock().unwrap().clone(),
            setup.processes.boot.lock().unwrap().clone(),
            setup.panes.rows.lock().unwrap().clone(),
        );
        arrange(&setup);
        let diagnostic = refused(&setup, &setup.agent_env(), &tool);
        assert_eq!(diagnostic.code, code, "{label}: {diagnostic:?}");
        *setup.processes.table.lock().unwrap() = setup_state.0;
        *setup.processes.devices.lock().unwrap() = setup_state.1;
        *setup.processes.boot.lock().unwrap() = setup_state.2;
        *setup.panes.rows.lock().unwrap() = setup_state.3;
    }
    assert_eq!(records(&setup), before);
}

#[test]
fn a_socket_replaced_while_the_agent_is_checked_proves_nothing() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let socket = PathBuf::from(&setup.env["WEZTERM_UNIX_SOCKET"]);
    let replacement = std::sync::Arc::new(Mutex::new(None::<UnixListener>));
    let slot = replacement.clone();
    *setup.panes.on_list.lock().unwrap() = Some(Box::new(move || {
        let _ = fs::remove_file(&socket);
        std::thread::sleep(Duration::from_millis(5));
        *slot.lock().unwrap() = Some(UnixListener::bind(&socket).expect("rebind socket"));
    }));
    let before = records(&setup);
    let diagnostic = refused(
        &setup,
        &setup.agent_env(),
        &event("codex", "PreToolUse", "s", json!({"tool_name":"shell"})),
    );
    assert_eq!(diagnostic.code, "incarnation_changed", "{diagnostic:?}");
    assert_eq!(records(&setup), before);
}
