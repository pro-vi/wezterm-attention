use super::*;

use wezterm_attention::lifecycle::apply_provider_event_with_outcome;
use wezterm_attention::lifecycle::outcome::Persistence;

pub(super) const AGENT_LAUNCH: &str = "00000000-0000-4000-8000-0000000005a1";

/// What one case changes about the fake machine before the event runs.
type Arrangement = Box<dyn Fn(&Setup)>;
type EnvArrangement = Box<dyn Fn(&Setup, &mut BTreeMap<String, String>)>;

pub(super) fn claim_path(setup: &Setup) -> PathBuf {
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    pane_path(&root, &address).join("claim.json")
}

pub(super) fn stored_claim(setup: &Setup) -> Option<Value> {
    fs::read(claim_path(setup))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("claim JSON"))
}

/// A claim the agent process `owner` holds at the pane's terminal.
pub(super) fn self_owned_claim(setup: &Setup, launch_id: &str, owner: i32) -> Value {
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

pub(super) fn install(setup: &Setup, claim: &Value) {
    atomic_replace(&claim_path(setup), claim).expect("write claim");
}

pub(super) fn launch_dir(setup: &Setup, launch_id: &str) -> PathBuf {
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    launch_path(&root, &address, launch_id)
}

/// Every record file under the state root and its bytes, lock files aside,
/// so a refusal can be shown to have written nothing.
pub(super) fn records(setup: &Setup) -> BTreeMap<PathBuf, String> {
    fn walk(directory: &std::path::Path, found: &mut BTreeMap<PathBuf, String>) {
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
                found.insert(
                    path.clone(),
                    fs::read_to_string(&path).expect("record text"),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(&state_root(&setup.env).expect("state root"), &mut found);
    found
}

/// One event for every action a provider event can take, in one session per
/// provider.
pub(super) fn every_action() -> Vec<ProviderEvent> {
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
    let cases: Vec<(&str, EnvArrangement)> = vec![
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
                let reads = std::sync::atomic::AtomicUsize::new(0);
                *setup.processes.on_read.lock().unwrap() = Some(std::sync::Arc::new(move |pid| {
                    if pid != AGENT_PID {
                        return None;
                    }
                    let read = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    read.is_multiple_of(2)
                        .then(|| ProcessRead::Found(changed.clone()))
                }));
            }),
        ),
        (
            "a hook reparented while it is checked",
            Box::new(|setup, _| {
                let mut orphaned = setup.processes.facts(HOOK_PID);
                orphaned.parent_pid = 1;
                let reads = std::sync::atomic::AtomicUsize::new(0);
                *setup.processes.on_read.lock().unwrap() = Some(std::sync::Arc::new(move |pid| {
                    if pid != HOOK_PID {
                        return None;
                    }
                    let read = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    read.is_multiple_of(2)
                        .then(|| ProcessRead::Found(orphaned.clone()))
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
    let cases: Vec<(&str, &str, Arrangement)> = vec![
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
    *setup.panes.on_list.lock().unwrap() = Some(std::sync::Arc::new(move || {
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

pub(super) fn start(provider: &str, session: &str) -> ProviderEvent {
    match provider {
        "pi" => event(
            "pi",
            "session_start",
            session,
            json!({"start_source":"startup"}),
        ),
        _ => event(
            provider,
            "SessionStart",
            session,
            json!({"source":"startup"}),
        ),
    }
}

pub(super) fn apply_as(
    setup: &Setup,
    env: &BTreeMap<String, String>,
    event: &ProviderEvent,
    observation: &str,
) -> wezterm_attention::lifecycle::LifecycleResult {
    apply_provider_event(event, env, observation, &setup.ports()).expect("event applies")
}

pub(super) fn launch_of(claim: &Value) -> String {
    claim["launch_id"]
        .as_str()
        .expect("claim launch")
        .to_owned()
}

/// Put another agent in the pane: `agent` started by the shell, running the
/// hook `hook`, and leading the terminal's foreground job.
fn start_agent(setup: &Setup, agent: i32, hook: i32) -> BTreeMap<String, String> {
    let mut facts = process_facts(
        agent,
        SHELL_PID,
        ControllingTerminal::Device(PANE_TTY_DEVICE),
    );
    facts.terminal_foreground_group = agent;
    setup.processes.set(agent, ProcessRead::Found(facts));
    setup.processes.set(
        hook,
        ProcessRead::Found(process_facts(hook, agent, ControllingTerminal::Absent)),
    );
    *setup.processes.own.lock().unwrap() = hook;
    let mut env = setup.agent_env();
    env.insert("WEZTERM_ATTENTION_HOST_PID".into(), agent.to_string());
    env
}

#[test]
fn an_agent_s_first_session_start_claims_the_pane_for_its_own_process() {
    for provider in ["claude", "codex", "pi"] {
        let setup = Setup::new();
        let env = setup.agent_env();
        assert_eq!(
            apply_as(&setup, &env, &start(provider, "s"), "00000000000000000200").disposition,
            "applied",
            "{provider}"
        );
        let claim = stored_claim(&setup).expect("the start claimed the pane");
        let agent = setup.processes.facts(AGENT_PID);
        assert_eq!(claim["owner_pid"], json!(AGENT_PID.to_string()));
        assert_eq!(
            claim["owner_started_sec"],
            json!(agent.start.seconds.to_string())
        );
        assert_eq!(
            claim["owner_started_usec"],
            json!(agent.start.microseconds.to_string())
        );
        assert_eq!(claim["owner_boot_session_id"], json!(BOOT_SESSION));
        assert_eq!(claim["tty_path"], json!(setup.tty.path));
        assert_eq!(claim["tty_fingerprint"], json!(setup.tty.fingerprint));
        let launch = launch_of(&claim);
        Uuid::parse_str(&launch).expect("a fresh launch id");
        assert!(!env.contains_key("WEZTERM_ATTENTION_LAUNCH_ID"));
        assert!(
            launch_dir(&setup, &launch)
                .join("bindings")
                .join(binding_id(provider, "s", &launch))
                .join("binding.json")
                .exists()
        );
        let writes = setup.tty.writes.lock().unwrap();
        let published = writes.last().expect("the claim was published");
        let (address, _) = pane_address(&setup.env).expect("address");
        assert_eq!(
            published,
            &wezterm_attention::wezterm::publication_bytes(&address, Some(&launch))
                .expect("publication")
        );
    }
}

#[test]
fn only_a_session_start_claims_a_pane() {
    let setup = Setup::new();
    for event in every_action() {
        if event.action == ProviderAction::Binding {
            continue;
        }
        let diagnostic = refused(&setup, &setup.agent_env(), &event);
        assert_eq!(diagnostic.code, "claim_stale", "{}", event.source_event);
    }
    assert!(stored_claim(&setup).is_none());
}

#[test]
fn an_agent_that_is_not_the_foreground_job_does_not_claim_but_keeps_its_claim_when_backgrounded() {
    let setup = Setup::new();
    let env = setup.agent_env();
    setup.processes.change(AGENT_PID, |agent| {
        agent.terminal_foreground_group = SHELL_PID
    });
    let diagnostic = refused(&setup, &env, &start("claude", "s"));
    assert_eq!(diagnostic.code, "claim_stale");
    assert!(diagnostic.message.contains("foreground"), "{diagnostic:?}");
    assert!(stored_claim(&setup).is_none());

    setup.processes.change(AGENT_PID, |agent| {
        agent.terminal_foreground_group = AGENT_PID
    });
    apply_as(&setup, &env, &start("claude", "s"), "00000000000000000200");
    let claim = stored_claim(&setup).expect("claimed in the foreground");
    // Suspended with Ctrl-Z, or put behind another job: the claim stays its.
    setup.processes.change(AGENT_PID, |agent| {
        agent.terminal_foreground_group = SHELL_PID
    });
    let prompt = event("claude", "UserPromptSubmit", "s", json!({"prompt":"go"}));
    assert_eq!(
        apply_as(&setup, &env, &prompt, "00000000000000000300").disposition,
        "applied"
    );
    let resumed = event("claude", "SessionStart", "s", json!({"source":"resume"}));
    assert_ne!(
        apply_as(&setup, &env, &resumed, "00000000000000000400").disposition,
        "ignored",
        "its own claim is reused without a foreground check"
    );
    assert_eq!(stored_claim(&setup).as_ref(), Some(&claim));
}

#[test]
fn the_same_agent_starting_again_keeps_its_claim_exactly() {
    let setup = Setup::new();
    let env = setup.agent_env();
    apply_as(&setup, &env, &start("claude", "s"), "00000000000000000200");
    let first = fs::read(claim_path(&setup)).expect("claim bytes");
    let clear = event("claude", "SessionStart", "t", json!({"source":"clear"}));
    assert_eq!(
        apply_as(&setup, &env, &clear, "00000000000000000300").disposition,
        "replaced"
    );
    assert_eq!(
        fs::read(claim_path(&setup)).expect("claim bytes"),
        first,
        "a reused claim is not rewritten, so callbacks resolved against it stay valid"
    );
}

#[test]
fn sequential_agents_in_one_pane_get_distinct_launch_ids() {
    let setup = Setup::new();
    let first_env = setup.agent_env();
    apply_as(
        &setup,
        &first_env,
        &start("claude", "a"),
        "00000000000000000200",
    );
    let first = launch_of(&stored_claim(&setup).expect("first claim"));
    apply_as(
        &setup,
        &first_env,
        &event("claude", "SessionEnd", "a", json!({"reason":"other"})),
        "00000000000000000300",
    );
    setup.processes.set(AGENT_PID, ProcessRead::Gone);
    setup.processes.set(HOOK_PID, ProcessRead::Gone);
    let second_env = start_agent(&setup, 4100, 5100);
    assert_eq!(
        apply_as(
            &setup,
            &second_env,
            &start("codex", "b"),
            "00000000000000000400"
        )
        .disposition,
        "applied"
    );
    let claim = stored_claim(&setup).expect("second claim");
    let second = launch_of(&claim);
    assert_ne!(first, second);
    assert_eq!(claim["owner_pid"], json!("4100"));
    assert!(
        launch_dir(&setup, &first)
            .join("bindings")
            .join(binding_id("claude", "a", &first))
            .join("end.json")
            .exists(),
        "the first agent's records stay where they were"
    );
}

#[test]
fn a_successor_binds_after_its_predecessor_is_proven_gone() {
    let cases: Vec<(&str, Arrangement)> = vec![
        (
            "killed without a session end",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Gone)),
        ),
        (
            "its pid now another process's",
            Box::new(|setup| {
                setup
                    .processes
                    .change(AGENT_PID, |agent| agent.start.microseconds += 1)
            }),
        ),
        (
            "started before the machine rebooted",
            Box::new(|setup| {
                *setup.processes.boot.lock().unwrap() =
                    Some("1f9a7c3e-51b2-4d6e-8a1b-2c3d4e5f6a7b".into());
            }),
        ),
    ];
    for (label, retire) in cases {
        let setup = Setup::new();
        apply_as(
            &setup,
            &setup.agent_env(),
            &start("claude", "a"),
            "00000000000000000200",
        );
        let first = launch_of(&stored_claim(&setup).expect("first claim"));
        let second_env = start_agent(&setup, 4100, 5100);
        retire(&setup);
        assert_eq!(
            apply_as(
                &setup,
                &second_env,
                &start("claude", "b"),
                "00000000000000000300"
            )
            .disposition,
            "applied",
            "{label}"
        );
        let second = launch_of(&stored_claim(&setup).expect("second claim"));
        assert_ne!(first, second, "{label}");
        let tool = event("claude", "PreToolUse", "b", json!({"tool_name":"Bash"}));
        assert_eq!(
            apply_as(&setup, &second_env, &tool, "00000000000000000400").disposition,
            "applied",
            "{label}"
        );
    }
}

#[test]
fn a_running_or_unreadable_owner_keeps_the_pane() {
    let cases: Vec<(&str, &str, Arrangement)> = vec![
        ("still running", "claim_stale", Box::new(|_| {})),
        (
            "exited and not yet reaped",
            "probe_unavailable",
            Box::new(|setup| {
                setup
                    .processes
                    .change(AGENT_PID, |agent| agent.zombie = true)
            }),
        ),
        (
            "unreadable",
            "probe_unavailable",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Unknown)),
        ),
    ];
    for (label, code, arrange) in cases {
        let setup = Setup::new();
        apply_as(
            &setup,
            &setup.agent_env(),
            &start("claude", "a"),
            "00000000000000000200",
        );
        let claim = stored_claim(&setup).expect("first claim");
        let second_env = start_agent(&setup, 4100, 5100);
        arrange(&setup);
        let before = records(&setup);
        let diagnostic = refused(&setup, &second_env, &start("claude", "b"));
        assert_eq!(diagnostic.code, code, "{label}: {diagnostic:?}");
        assert_eq!(stored_claim(&setup).as_ref(), Some(&claim), "{label}");
        assert_eq!(records(&setup), before, "{label}");
    }
    // A boot id this machine cannot read proves nothing about any owner, and
    // leaves the new agent unproven too.
    let setup = Setup::new();
    apply_as(
        &setup,
        &setup.agent_env(),
        &start("claude", "a"),
        "00000000000000000200",
    );
    let claim = stored_claim(&setup).expect("first claim");
    let second_env = start_agent(&setup, 4100, 5100);
    setup.processes.set(AGENT_PID, ProcessRead::Gone);
    *setup.processes.boot.lock().unwrap() = None;
    assert_eq!(
        refused(&setup, &second_env, &start("claude", "b")).code,
        "probe_unavailable"
    );
    assert_eq!(stored_claim(&setup).as_ref(), Some(&claim));
}

#[test]
fn two_racing_first_hooks_of_one_agent_end_with_one_claim_both_resolve_to() {
    let setup = Setup::new();
    let env = setup.agent_env();
    // Both hooks have proven their agent before either takes the claim lock.
    let proven = std::sync::Arc::new(std::sync::Barrier::new(2));
    let gate = proven.clone();
    *setup.panes.on_list.lock().unwrap() = Some(std::sync::Arc::new(move || {
        gate.wait();
    }));
    let results: Vec<_> = thread::scope(|scope| {
        // One callback delivered twice, as a retrying runner would.
        let hooks: Vec<_> = (0..2)
            .map(|_| {
                let (setup, env) = (&setup, &env);
                scope.spawn(move || {
                    apply_as(setup, env, &start("claude", "s"), "00000000000000000200")
                })
            })
            .collect();
        hooks.into_iter().map(|hook| hook.join().unwrap()).collect()
    });
    *setup.panes.on_list.lock().unwrap() = None;
    let dispositions: BTreeSet<_> = results
        .iter()
        .map(|result| result.disposition.as_str())
        .collect();
    assert_eq!(
        dispositions,
        BTreeSet::from(["applied", "confirmed"]),
        "{results:?}"
    );
    let launch = launch_of(&stored_claim(&setup).expect("one claim"));
    let launches: Vec<_> = fs::read_dir(launch_dir(&setup, &launch).parent().unwrap())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(launches, [launch], "both resolved to the one claim");
}
