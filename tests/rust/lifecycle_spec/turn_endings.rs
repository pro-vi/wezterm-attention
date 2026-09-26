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
    let review = pane_path(&state_root(&setup.env).unwrap(), &address)
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
        let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
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

// A child blocked on an approval prompt emits nothing until the user answers,
// so its presence is never refreshed. The wait is still real after the
// presence lifetime has passed: only the child's next event, a prompt or the
// end of the lead's turn ends it.
#[test]
fn a_waiting_childs_notify_outlasts_its_presence_lifetime() {
    for (provider, agent_type, tool) in PROVIDERS {
        let mut setup = bound(provider, "parent");
        setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            "00000000000000000400",
        );
        for (unix, observation) in [
            ("00000000611345678900", "00000000000000000500"),
            ("00000000613345678900", "00000000000000000550"),
            ("00000086412345678900", "00000000000000000600"),
        ] {
            setup.clock = FixedClock {
                monotonic: "00000000000000000100",
                unix,
            };
            let held = setup.apply(&lead(provider, "PreToolUse", tool), observation);
            assert_eq!(held.disposition, "ignored", "{provider} at {unix}");
            assert_eq!(shown(&setup, provider), "notify", "{provider} at {unix}");
        }

        setup.apply(
            &child(provider, "PreToolUse", "child-a", agent_type),
            "00000000000000000700",
        );
        setup.apply(&lead(provider, "PreToolUse", tool), "00000000000000000750");
        assert_eq!(shown(&setup, provider), "thinking", "{provider}");
    }
}

// The child's presence only extends the notify. A presence or fence record
// that cannot be read must not take the notify down with it: the request still
// shows, and the unreadable record is reported.
#[test]
fn a_child_permission_request_shows_past_an_unreadable_presence() {
    let key = wezterm_attention::protocol::sha256_hex(b"child-a");
    let presence = format!("agents/{key}.json");
    for (provider, agent_type) in [("claude", "Explore"), ("codex", "worker")] {
        for record in [presence.as_str(), "agents-clear.json", "agents-floor.json"] {
            let setup = bound(provider, "parent");
            let directory = setup.binding_dir(provider, "parent");
            fs::create_dir_all(directory.join("agents")).unwrap();
            fs::write(directory.join(record), b"{not json").unwrap();
            let result = setup.apply(
                &child(provider, "PermissionRequest", "child-a", agent_type),
                "00000000000000000400",
            );
            assert_eq!(result.disposition, "partial", "{provider} {record}");
            assert_eq!(
                result.diagnostic.as_ref().map(|d| d.code.as_str()),
                Some("record_invalid"),
                "{provider} {record}"
            );
            assert_eq!(shown(&setup, provider), "notify", "{provider} {record}");
            let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
            assert!(raw.contains("approval_requested"), "{provider}: {raw}");
        }
    }
}
