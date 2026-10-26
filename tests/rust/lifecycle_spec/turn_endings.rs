use super::*;

fn bound(provider: &str, session: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            provider,
            "SessionStart",
            session,
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(provider, "UserPromptSubmit", session, json!({})),
        "00000000000000000300",
    );
    setup
}

fn read(path: PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).expect("record exists")).expect("record JSON")
}

// Claude sends StopFailure instead of Stop when an API error (rate limit,
// overload, billing) ends the turn. No Stop follows, so the prompt's thinking
// would otherwise stay on the tab; the user has to act, which is notify.
#[test]
fn a_failed_claude_turn_asks_for_the_user_and_keeps_its_observation() {
    let setup = bound("claude", "failed");
    let failure = event(
        "claude",
        "StopFailure",
        "failed",
        json!({"error":"rate_limit"}),
    );
    assert_eq!(failure.action, ProviderAction::Activity);
    assert_eq!(
        setup.apply(&failure, "00000000000000000400").disposition,
        "applied"
    );
    let directory = setup.binding_dir("claude", "failed");
    assert_eq!(read(directory.join("activity.json"))["type"], "notify");
    let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
    assert!(raw.contains("attempt_outcome"), "{raw}");

    let child = event(
        "claude",
        "StopFailure",
        "failed",
        json!({"error":"overloaded","agent_id":"child-a","agent_type":"Explore"}),
    );
    assert_eq!(child.action, ProviderAction::Observation);
}

// Codex runs no Stop after an interrupted turn. The user interrupted it, so
// there is nothing left to report: the lead activity is cleared.
#[test]
fn an_interrupted_codex_turn_clears_its_activity_and_keeps_its_observation() {
    let setup = bound("codex", "interrupted");
    apply_mark_review(&setup.env, "pi-bus").expect("review from another writer");
    let interrupt = event(
        "codex",
        "Interrupt",
        "interrupted",
        json!({"turn_id":"turn-1"}),
    );
    assert_eq!(interrupt.action, ProviderAction::Clear);
    assert_eq!(
        setup.apply(&interrupt, "00000000000000000400").disposition,
        "applied"
    );
    let directory = setup.binding_dir("codex", "interrupted");
    let activity = read(directory.join("activity.json"));
    let clear = read(directory.join("activity-clear.json"));
    assert!(
        activity["observed_mono_ns"].as_str() <= clear["observed_mono_ns"].as_str(),
        "the prompt's thinking is hidden"
    );
    let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
    assert!(raw.contains("user_interrupt"), "{raw}");
    let (address, _) = pane_address(&setup.env).unwrap();
    let review = pane_dir(&state_root(&setup.env).unwrap(), &address)
        .join("reviews")
        .join(format!(
            "{}.json",
            wezterm_attention::protocol::sha256_hex(b"pi-bus")
        ));
    assert!(
        review.exists(),
        "a Codex interrupt leaves Pi's review alone"
    );

    let next = event("codex", "UserPromptSubmit", "interrupted", json!({}));
    setup.apply(&next, "00000000000000000500");
    assert_eq!(read(directory.join("activity.json"))["type"], "thinking");
}

// A sub-agent blocked on a permission prompt is waiting for the user just as
// the lead would be. Codex has no Notification hook, so this is the only
// signal that the tab needs attention.
#[test]
fn a_child_waiting_for_permission_asks_for_the_user() {
    for (provider, agent_type) in [("claude", "Explore"), ("codex", "worker")] {
        let setup = bound(provider, "parent");
        let request = event(
            provider,
            "PermissionRequest",
            "parent",
            json!({"tool_name":"Bash","agent_id":"child-a","agent_type":agent_type}),
        );
        assert_eq!(request.action, ProviderAction::Activity, "{provider}");
        assert_eq!(
            setup.apply(&request, "00000000000000000400").disposition,
            "applied",
            "{provider}"
        );
        let directory = setup.binding_dir(provider, "parent");
        assert_eq!(
            read(directory.join("activity.json"))["type"],
            "notify",
            "{provider}"
        );
        let raw = fs::read_to_string(directory.join("children-lifecycle.json")).unwrap();
        assert!(raw.contains("approval_requested"), "{provider}: {raw}");
        assert!(
            raw.contains("child-a"),
            "{provider}: the actor stays the child"
        );
    }
}

const PROVIDERS: [(&str, &str, &str); 2] = [
    ("claude", "Explore", "Task"),
    ("codex", "worker", "wait_agent"),
];

fn child(provider: &str, name: &str, agent: &str, agent_type: &str) -> ProviderEvent {
    let mut patch = json!({"agent_id":agent,"agent_type":agent_type});
    match name {
        "SubagentStop" => patch["stop_hook_active"] = json!(false),
        _ => patch["tool_name"] = json!("Bash"),
    }
    event(provider, name, "parent", patch)
}

fn lead(provider: &str, name: &str, tool: &str) -> ProviderEvent {
    let patch = match name {
        "PreToolUse" => json!({"tool_name":tool}),
        "Stop" => json!({"stop_hook_active":false}),
        _ => json!({}),
    };
    event(provider, name, "parent", patch)
}

fn shown(setup: &Setup, provider: &str) -> String {
    let activity = read(setup.binding_dir(provider, "parent").join("activity.json"));
    activity["type"].as_str().unwrap().to_owned()
}

// While a lead waits on a sub-agent it keeps calling tools (Codex polls
// wait_agent), and each call would repaint the tab as thinking. The child is
// still waiting for the user, so its notify stays until that child moves on.
#[test]
fn a_waiting_childs_notify_outlasts_the_leads_tool_calls() {
    for (provider, agent_type, tool) in PROVIDERS {
        let setup = bound(provider, "parent");
        setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            "00000000000000000400",
        );
        setup.apply(
            &child(provider, "PreToolUse", "child-b", agent_type),
            "00000000000000000450",
        );
        let held = setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000500");
        assert_eq!(held.disposition, "ignored", "{provider}");
        assert_eq!(shown(&setup, provider), "notify", "{provider}");
        let raw = fs::read_to_string(setup.binding_dir(provider, "parent").join("lifecycle.json"))
            .unwrap();
        assert!(
            raw.contains(tool),
            "{provider}: the held call keeps its observation"
        );

        setup.apply(
            &child(provider, "PreToolUse", "child-a", agent_type),
            "00000000000000000600",
        );
        setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000700");
        assert_eq!(
            shown(&setup, provider),
            "thinking",
            "{provider}: the child's next event says its request was answered"
        );
    }
}

#[test]
fn a_waiting_childs_notify_ends_when_the_child_stops() {
    for (provider, agent_type, tool) in PROVIDERS {
        let setup = bound(provider, "parent");
        setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            "00000000000000000400",
        );
        setup.apply(
            &child(provider, "SubagentStop", "child-a", agent_type),
            "00000000000000000500",
        );
        setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000600");
        assert_eq!(shown(&setup, provider), "thinking", "{provider}");
    }
}

// A user prompt or the end of the lead's turn replaces the notify as it would
// replace any other: the user has acted, or the turn is over.
#[test]
fn a_prompt_or_a_turn_end_replaces_a_waiting_childs_notify() {
    for (provider, agent_type, _) in PROVIDERS {
        for (name, expected) in [("UserPromptSubmit", "thinking"), ("Stop", "stop")] {
            let setup = bound(provider, "parent");
            setup.apply(
                &child(provider, "PermissionRequest", "child-a", agent_type),
                "00000000000000000400",
            );
            setup.apply(&lead(provider, name, ""), "00000000000000000500");
            assert_eq!(shown(&setup, provider), expected, "{provider} {name}");
        }
    }
}

// Two children wait at once. The notify stays until neither is waiting,
// whichever of them is answered first.
#[test]
fn a_notify_stays_while_any_child_still_waits() {
    for (provider, agent_type, tool) in PROVIDERS {
        let setup = bound(provider, "parent");
        for (agent, observation) in [
            ("child-a", "00000000000000000400"),
            ("child-b", "00000000000000000450"),
        ] {
            setup.apply(
                &child(provider, "PermissionRequest", agent, agent_type),
                observation,
            );
        }
        setup.apply(
            &child(provider, "PreToolUse", "child-b", agent_type),
            "00000000000000000500",
        );
        setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000550");
        assert_eq!(shown(&setup, provider), "notify", "{provider}");
        setup.apply(
            &child(provider, "PreToolUse", "child-a", agent_type),
            "00000000000000000600",
        );
        setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000650");
        assert_eq!(shown(&setup, provider), "thinking", "{provider}");
    }
}

// The child set only holds the notify. A child set that cannot be
// read must not take the notify down with it: the request still shows, and
// the read failure is reported. A set that is present but invalid is moved
// aside and started again, so the request applies as usual.
#[test]
fn a_child_permission_request_shows_past_an_unreadable_child_set() {
    for (provider, agent_type) in [("claude", "Explore"), ("codex", "worker")] {
        let setup = bound(provider, "parent");
        let directory = setup.binding_dir(provider, "parent");
        fs::create_dir_all(directory.join("children.json")).unwrap();
        let result = setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            "00000000000000000400",
        );
        assert_eq!(result.disposition, "partial", "{provider}");
        assert_eq!(
            result.diagnostic.as_ref().map(|d| d.code.as_str()),
            Some("probe_unavailable"),
            "{provider}"
        );
        assert_eq!(shown(&setup, provider), "notify", "{provider}");
        let raw = fs::read_to_string(directory.join("children-lifecycle.json")).unwrap();
        assert!(raw.contains("approval_requested"), "{provider}: {raw}");

        let setup = bound(provider, "parent");
        let directory = setup.binding_dir(provider, "parent");
        fs::write(directory.join("children.json"), b"{not json").unwrap();
        let result = setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            "00000000000000000400",
        );
        assert_eq!(result.disposition, "applied", "{provider}");
        assert_eq!(shown(&setup, provider), "notify", "{provider}");
    }
}

// A Stop hook that blocks lets the agent go on with the same turn, and the
// next Stop says so with `stop_hook_active`. Each of those Stops ends the turn
// again: the tab shows it finished, and the newest end follows the
// continuation's tool calls. The turn's id is the same on all of them.
#[test]
fn each_stop_after_a_blocked_stop_ends_the_turn_again() {
    for (provider, id_field, tool) in [
        ("codex", "turn_id", "shell"),
        ("claude", "prompt_id", "Bash"),
    ] {
        let setup = Setup::new();
        setup.claim();
        let turn = |name: &str, mut patch: Value, at: &str| {
            patch[id_field] = json!("turn-1");
            setup.apply(&event(provider, name, "blocked", patch), at)
        };
        setup.apply(
            &event(
                provider,
                "SessionStart",
                "blocked",
                json!({"source":"startup"}),
            ),
            "00000000000000000200",
        );
        turn("UserPromptSubmit", json!({}), "00000000000000000300");
        let mut at = 400;
        for active in [false, true, true] {
            turn(
                "PreToolUse",
                json!({"tool_name":tool,"tool_use_id":format!("call-{at}")}),
                &format!("{at:020}"),
            );
            let stop = turn(
                "Stop",
                json!({"stop_hook_active":active}),
                &format!("{:020}", at + 50),
            );
            assert_eq!(stop.disposition, "applied", "{provider} at {at}");
            let directory = setup.binding_dir(provider, "blocked");
            assert_eq!(
                read(directory.join("activity.json"))["type"],
                "stop",
                "{provider}: the Stop at {} shows the turn finished",
                at + 50
            );
            let lifecycle = read(directory.join("lifecycle.json"));
            let newest = lifecycle["pools"]["general"]["observations"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone();
            assert_eq!(newest["kind"], "response_finished", "{provider} at {at}");
            assert_eq!(
                newest["observed_mono_ns"],
                format!("{:020}", at + 50),
                "{provider}: the newest end is this Stop"
            );
            at += 100;
        }
    }
}

// Claude Code's `prompt_id` is the turn id of the lead's prompts and turn
// ends. A tool call, a compaction and a sub-agent's events keep none, so
// request groups and stored compactions are keyed as before.
#[test]
fn a_claude_prompt_id_is_the_turn_id_of_lead_prompts_and_turn_ends() {
    let turn_id = |name: &str, mut patch: Value| {
        patch["prompt_id"] = json!("4d1f6c3e-0b5a-4f8e-9c2d-7a6b5e4d3c2b");
        event("claude", name, "turns", patch)
            .observation
            .and_then(|observation| observation.correlation)
            .and_then(|correlation| correlation.turn_id)
    };
    let id = Some("4d1f6c3e-0b5a-4f8e-9c2d-7a6b5e4d3c2b".to_owned());
    for (name, patch) in [
        ("UserPromptSubmit", json!({"prompt":"go"})),
        ("Stop", json!({"stop_hook_active":false})),
        ("StopFailure", json!({"error":"rate_limit"})),
    ] {
        assert_eq!(turn_id(name, patch), id, "{name}");
    }
    for (name, patch) in [
        (
            "PreToolUse",
            json!({"tool_name":"Bash","tool_use_id":"call-1"}),
        ),
        ("PreCompact", json!({"trigger":"auto"})),
        (
            "UserPromptSubmit",
            json!({"agent_id":"child-a","agent_type":"Explore"}),
        ),
        (
            "StopFailure",
            json!({"agent_id":"child-a","agent_type":"Explore","error":"overloaded"}),
        ),
    ] {
        assert_eq!(turn_id(name, patch), None, "{name}");
    }
}

// A Pi end has no turn id; its identity is the transport id of its delivery.
// The same delivery arriving again after newer records is still a repeat, and
// leaves the newer activity in place.
#[test]
fn a_pi_end_delivered_again_after_newer_records_is_still_a_repeat() {
    let setup = Setup::new();
    setup.claim();
    let apply =
        |name: &str, patch: Value, at: &str| setup.apply(&event("pi", name, "settled", patch), at);
    apply(
        "session_start",
        json!({"start_source":"startup"}),
        "00000000000000000200",
    );
    let settled = json!({"transport_id":"7b6e2c1a-4f3d-4e5a-9b8c-0d1e2f3a4b5c"});
    apply("agent_settled", settled.clone(), "00000000000000000300");
    apply(
        "tool_execution_start",
        json!({"tool_name":"bash","tool_use_id":"call-1"}),
        "00000000000000000400",
    );
    let directory = setup.binding_dir("pi", "settled");
    assert_eq!(read(directory.join("activity.json"))["type"], "thinking");
    apply("agent_settled", settled, "00000000000000000500");
    assert_eq!(
        read(directory.join("activity.json"))["type"],
        "thinking",
        "the repeated end does not overwrite the newer activity"
    );
}
