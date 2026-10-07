use super::self_claim::start;
use super::turn_endings::read;
use super::*;

// What these tests pin was read from cursor-agent 2026.10.01-e373342 running
// interactively under a pseudo-terminal, with a hook that logged each payload.

fn started(session: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    let begin = start("cursor", session);
    assert_eq!(begin.action, ProviderAction::Binding);
    assert_eq!(begin.start_source.as_deref(), Some("startup"));
    assert_eq!(
        setup.apply(&begin, "00000000000000000200").disposition,
        "applied"
    );
    setup
}

#[test]
fn a_cursor_turn_thinks_from_the_prompt_and_stops_when_it_completes() {
    let setup = started("lead");
    let directory = setup.binding_dir("cursor", "lead");
    let prompt = event(
        "cursor",
        "beforeSubmitPrompt",
        "lead",
        json!({"prompt":"hi"}),
    );
    assert_eq!(
        setup.apply(&prompt, "00000000000000000300").disposition,
        "applied"
    );
    assert_eq!(read(directory.join("activity.json"))["type"], "thinking");

    let call =
        "call-5865ee6a-0874-4009-bf19-8b674a1bf321-1\nfc_f722cfa0-76ba-918d-a8e1-88edec6568d7_0";
    for (name, observed) in [("preToolUse", "400"), ("postToolUse", "500")] {
        let parsed = event(
            "cursor",
            name,
            "lead",
            json!({"tool_name":"Shell","tool_use_id":call}),
        );
        assert!(parsed.observation_diagnostic.is_none(), "{name}");
        let correlation =
            serde_json::to_value(parsed.observation.as_ref().unwrap()).unwrap()["correlation"]
                .clone();
        assert_eq!(
            correlation["tool_call_id"], "call-5865ee6a-0874-4009-bf19-8b674a1bf321-1",
            "{name}: the first line names the call, so a pre and a post hook pair"
        );
        setup.apply(&parsed, &format!("{observed:0>20}"));
    }

    let stop = event("cursor", "stop", "lead", json!({"status":"completed"}));
    assert_eq!(
        setup.apply(&stop, "00000000000000000600").disposition,
        "applied"
    );
    assert_eq!(read(directory.join("activity.json"))["type"], "stop");
    let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
    for kind in [
        "prompt_submitted",
        "tool_preflight",
        "tool_result",
        "response_finished",
    ] {
        assert!(raw.contains(kind), "{kind}: {raw}");
    }
}

// `postToolUseFailure` and `preCompact` write no pane state, so nothing but
// their lifecycle records shows that the mapping took: a failed tool result
// that reads as a success, or a compaction recorded as another kind.
#[test]
fn the_observation_only_hooks_record_their_kinds_and_leave_the_pane_alone() {
    let setup = started("lead");
    let directory = setup.binding_dir("cursor", "lead");
    let prompt = event("cursor", "beforeSubmitPrompt", "lead", json!({}));
    setup.apply(&prompt, "00000000000000000300");
    let activity = fs::read(directory.join("activity.json")).unwrap();
    for (name, patch, observed) in [
        (
            "postToolUseFailure",
            json!({"tool_name":"Shell","tool_use_id":"call-x-0\nfc_x_0","failure_type":"error","is_interrupt":false}),
            "00000000000000000400",
        ),
        (
            "preCompact",
            json!({"trigger":"auto"}),
            "00000000000000000500",
        ),
    ] {
        let hook = event("cursor", name, "lead", patch);
        assert_eq!(hook.action, ProviderAction::Observation, "{name}");
        assert_eq!(
            setup.apply(&hook, observed).disposition,
            "applied",
            "{name}"
        );
    }
    assert_eq!(fs::read(directory.join("activity.json")).unwrap(), activity);

    let snapshot = read(directory.join("lifecycle.json"));
    let general = snapshot["pools"]["general"]["observations"]
        .as_array()
        .unwrap();
    let of = |source: &str| {
        general
            .iter()
            .find(|item| item["source_event"] == source)
            .unwrap_or_else(|| panic!("no {source} observation in {general:?}"))
    };
    let failure = of("postToolUseFailure");
    assert_eq!(failure["kind"], "tool_result");
    assert_eq!(failure["is_error"], true);
    assert_eq!(failure["correlation"]["tool_call_id"], "call-x-0");
    let compaction = of("preCompact");
    assert_eq!(compaction["kind"], "compaction_attempted");
    assert_eq!(compaction["trigger"], "auto");
}

// Cursor's `/new` starts another conversation in the same process and sends no
// `sessionEnd` for the old one and no `sessionStart` for the new one, so the
// new conversation's first prompt is where its session is first seen.
#[test]
fn a_prompt_in_a_conversation_cursor_never_started_takes_over_the_binding() {
    let setup = started("first");
    let new_prompt = event("cursor", "beforeSubmitPrompt", "second", json!({}));
    assert_eq!(new_prompt.action, ProviderAction::Activity);
    assert_eq!(
        setup.apply(&new_prompt, "00000000000000000300").disposition,
        "applied"
    );
    assert_eq!(setup.current_session(), "second");
    assert!(
        setup
            .binding_dir("cursor", "first")
            .join("end.json")
            .exists(),
        "no hook ends the conversation /new left, so the prompt that replaces it does"
    );
    let directory = setup.binding_dir("cursor", "second");
    assert_eq!(read(directory.join("activity.json"))["type"], "thinking");

    let stop = event("cursor", "stop", "second", json!({"status":"completed"}));
    assert_eq!(
        setup.apply(&stop, "00000000000000000400").disposition,
        "applied"
    );
    assert_eq!(read(directory.join("activity.json"))["type"], "stop");

    // A conversation that comes back takes the binding again, live, and a
    // prompt in the current one does not rewrite its binding.
    let back = event("cursor", "beforeSubmitPrompt", "first", json!({}));
    setup.apply(&back, "00000000000000000500");
    assert_eq!(setup.current_session(), "first");
    assert!(
        setup
            .binding_dir("cursor", "second")
            .join("end.json")
            .exists(),
        "the conversation it replaced ends"
    );
    let (rows, _) = read_bindings(&state_root(&setup.env).unwrap()).unwrap();
    let first = rows
        .iter()
        .find(|row| row.provider_session_id == "first")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&first.binding_phase).unwrap(),
        "active"
    );
    let binding = setup.binding_dir("cursor", "first").join("binding.json");
    let before = fs::read(&binding).unwrap();
    let again = event("cursor", "beforeSubmitPrompt", "first", json!({}));
    setup.apply(&again, "00000000000000000600");
    assert_eq!(fs::read(&binding).unwrap(), before);
}

// A sub-agent's tool hooks arrive under a session id of their own, and Cursor
// sends no sub-agent hooks to say whose they are. Only a prompt binds a
// session, so a tool call under a stranger's id changes nothing.
#[test]
fn tool_hooks_under_another_session_id_neither_bind_nor_change_the_pane() {
    let setup = started("lead");
    let prompt = event("cursor", "beforeSubmitPrompt", "lead", json!({}));
    setup.apply(&prompt, "00000000000000000300");
    let stop = event("cursor", "stop", "lead", json!({"status":"completed"}));
    setup.apply(&stop, "00000000000000000400");

    for (name, observed) in [("preToolUse", "500"), ("postToolUse", "600")] {
        let child = event(
            "cursor",
            name,
            "child",
            json!({"tool_name":"Read","tool_use_id":"call-child-0\nfc_child_0"}),
        );
        let result = setup.apply(&child, &format!("{observed:0>20}"));
        assert_ne!(result.disposition, "applied", "{name}");
        assert_eq!(
            result.diagnostic.as_ref().map(|item| item.code.as_str()),
            Some("claim_stale"),
            "{name}"
        );
    }
    assert_eq!(setup.current_session(), "lead");
    let activity = read(setup.binding_dir("cursor", "lead").join("activity.json"));
    assert_eq!(activity["type"], "stop");
    assert!(!setup.binding_dir("cursor", "child").exists());
}

// Esc sends two `stop` hooks for the turn, `error` and `aborted`, and Cursor
// runs them at the same time, in no fixed order. Both clear, so the pane is
// clear whichever the writer sees last.
#[test]
fn either_order_of_the_two_stops_an_esc_sends_leaves_the_pane_clear() {
    for statuses in [["error", "aborted"], ["aborted", "error"]] {
        let setup = started("lead");
        let directory = setup.binding_dir("cursor", "lead");
        let prompt = event("cursor", "beforeSubmitPrompt", "lead", json!({}));
        setup.apply(&prompt, "00000000000000000300");
        for (index, status) in statuses.into_iter().enumerate() {
            let stop = event("cursor", "stop", "lead", json!({"status":status}));
            assert_eq!(stop.action, ProviderAction::Clear, "{status}");
            setup.apply(&stop, &format!("{:020}", 400 + index));
        }
        let activity = read(directory.join("activity.json"));
        let clear = read(directory.join("activity-clear.json"));
        assert!(
            activity["observed_mono_ns"].as_str() <= clear["observed_mono_ns"].as_str(),
            "{statuses:?}: the turn's activity is hidden"
        );
        let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
        assert!(raw.contains("user_interrupt"), "{statuses:?}: {raw}");
        assert!(raw.contains("attempt_outcome"), "{statuses:?}: {raw}");

        let next = event("cursor", "beforeSubmitPrompt", "lead", json!({}));
        setup.apply(&next, "00000000000000000500");
        assert_eq!(read(directory.join("activity.json"))["type"], "thinking");
    }
}

// With a prompt on the command line, cursor-agent 2026.10.01 sent the first
// `beforeSubmitPrompt` 0.07 s before `sessionStart` and did not wait for it. In
// a pane a shell claimed, the prompt binds the session, and a start that comes
// after it, stamped older or newer, leaves that session bound and its activity
// standing.
#[test]
fn a_session_start_that_trails_the_first_prompt_leaves_the_session_bound() {
    for start_order in ["00000000000000000200", "00000000000000000400"] {
        let setup = Setup::new();
        setup.claim();
        let prompt = event("cursor", "beforeSubmitPrompt", "first", json!({}));
        assert_eq!(
            setup.apply(&prompt, "00000000000000000300").disposition,
            "applied"
        );
        let late_start = start("cursor", "first");
        assert_ne!(
            setup.apply(&late_start, start_order).disposition,
            "conflict"
        );
        assert_eq!(setup.current_session(), "first", "{start_order}");
        let directory = setup.binding_dir("cursor", "first");
        assert_eq!(read(directory.join("activity.json"))["type"], "thinking");
        assert!(!directory.join("end.json").exists(), "{start_order}");
    }
}

// Every conversation a launch moved through ends, not only the first and the
// last: a left-over active binding in a pane that has moved on reads as a
// conflict once its session is resumed elsewhere.
#[test]
fn a_session_end_after_two_new_conversations_ends_all_three_bindings() {
    let setup = started("first");
    for (session, observed) in [("second", "300"), ("third", "400")] {
        let prompt = event("cursor", "beforeSubmitPrompt", session, json!({}));
        setup.apply(&prompt, &format!("{observed:0>20}"));
    }
    let end = event(
        "cursor",
        "sessionEnd",
        "first",
        json!({"reason":"completed"}),
    );
    setup.apply(&end, "00000000000000000500");
    for session in ["first", "second", "third"] {
        assert!(
            setup
                .binding_dir("cursor", session)
                .join("end.json")
                .exists(),
            "{session}"
        );
    }
}

// A `cursor-agent` the launch's agent starts inherits the launch. Its own
// session was refused as a second agent, so its `sessionEnd` is not the
// agent's end and must not end the agent's conversation.
#[test]
fn the_end_of_an_agent_the_launch_never_bound_ends_nothing() {
    let setup = started("lead");
    let nested_start = start("cursor", "nested");
    assert_eq!(
        setup
            .apply(&nested_start, "00000000000000000300")
            .disposition,
        "conflict"
    );
    let nested_end = event(
        "cursor",
        "sessionEnd",
        "nested",
        json!({"reason":"completed"}),
    );
    setup.apply(&nested_end, "00000000000000000400");
    assert!(
        !setup
            .binding_dir("cursor", "lead")
            .join("end.json")
            .exists()
    );
    assert_eq!(setup.current_session(), "lead");
}

#[test]
fn a_cursor_session_end_ends_the_binding() {
    let setup = started("lead");
    let end = event(
        "cursor",
        "sessionEnd",
        "lead",
        json!({"reason":"completed"}),
    );
    assert_eq!(end.action, ProviderAction::End);
    assert_eq!(
        setup.apply(&end, "00000000000000000300").disposition,
        "applied"
    );
    assert!(
        setup
            .binding_dir("cursor", "lead")
            .join("end.json")
            .exists()
    );
}
