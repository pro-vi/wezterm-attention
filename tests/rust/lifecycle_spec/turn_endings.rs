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
    apply_mark_review(&setup.env, "pi-bus", false).expect("review from another writer");
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
