use super::*;
use std::time::Duration;
use wezterm_attention::observations::{
    Actor, AttemptOutcome, LifecycleObservation, NativeCorrelation, ObservationBody,
};
use wezterm_attention::observe::{ObserveCursor, ObserveFilters, ObserveOutcome, observe};
use wezterm_attention::query::PaneScope;

const SESSION: &str = "waiter";

fn filters() -> ObserveFilters {
    ObserveFilters {
        prompt: true,
        answer: true,
        turn_end: true,
        call: None,
    }
}

fn setup() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            SESSION,
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}

fn scope(setup: &Setup, binding: bool) -> PaneScope {
    PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        binding.then(|| binding_id("codex", SESSION, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"])),
    )
    .unwrap()
}

fn baseline(setup: &Setup) -> (PaneScope, ObserveCursor, String) {
    let scope = scope(setup, false);
    let root = state_root(&setup.env).unwrap();
    let answer = observe(&root, &scope, &filters(), None, Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Baseline);
    assert!(answer.events.is_empty());
    let encoded = answer.cursor.clone().expect("baseline cursor");
    let cursor = ObserveCursor::parse(&encoded, &answer.scope, &filters()).unwrap();
    (answer.scope, cursor, encoded)
}

fn run_cli(setup: &Setup, args: &[&str], scope: &Value) -> std::process::Output {
    let mut child = rust_command(setup)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(scope.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn baseline_then_prompt_and_turn_end_arrive_once_in_a_chain() {
    let setup = setup();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    setup.apply(
        &event("codex", "Stop", SESSION, json!({"stop_hook_active":false})),
        "00000000000000000400",
    );
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Events);
    let kinds: Vec<(&str, &str)> = answer
        .events
        .iter()
        .map(|event| (event.event.as_str(), event.kind.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("prompt", "prompt_submitted"),
            ("turn_end", "response_finished")
        ]
    );
    assert_eq!(answer.events[1].source_event.as_deref(), Some("Stop"));
    // The chain advances: the same events are not reported twice.
    let next = ObserveCursor::parse(answer.cursor.as_deref().unwrap(), &scope, &filters()).unwrap();
    let quiet = observe(&root, &scope, &filters(), Some(&next), Duration::ZERO).unwrap();
    assert_eq!(quiet.outcome, ObserveOutcome::Timeout);
    assert!(quiet.events.is_empty());
    // A cursor belongs to its filter set.
    let other = ObserveFilters {
        turn_end: true,
        ..ObserveFilters::default()
    };
    assert!(
        ObserveCursor::parse(answer.cursor.as_deref().unwrap(), &scope, &other).is_err(),
        "a cursor reused under other filters must be refused"
    );
    assert!(
        ObserveCursor::parse("!!!", &scope, &filters()).is_err(),
        "a malformed cursor must be refused"
    );
}

#[test]
fn a_follower_produced_end_is_a_turn_end_like_any_other() {
    let setup = setup();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    // What the turn follower writes for an errored Codex end: an
    // attempt_outcome whose source is the rollout, stamped just after the
    // turn's last record rather than when it was published.
    let dir = setup.binding_dir("codex", SESSION);
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap();
    snapshot
        .reduce(LifecycleObservation {
            observation_id: Uuid::new_v4().to_string(),
            source_event: "task_complete".into(),
            source_version: None,
            observed_mono_ns: "00000000000000000301".into(),
            written_at_unix_ns: "00000000000000000900".into(),
            actor: Actor::Lead,
            correlation: Some(NativeCorrelation {
                turn_id: Some("turn-a".into()),
                ..Default::default()
            }),
            turn_end: None,
            body: ObservationBody::AttemptOutcome {
                outcome: AttemptOutcome::Failed,
                error_category: None,
                error_code: Some("server_overloaded".into()),
            },
        })
        .unwrap();
    atomic_replace(&dir.join("lifecycle.json"), &json!(snapshot)).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Events);
    let end = answer
        .events
        .iter()
        .find(|event| event.event == "turn_end")
        .expect("follower end reported");
    assert_eq!(end.kind, "attempt_outcome");
    assert_eq!(end.source_event.as_deref(), Some("task_complete"));
    assert_eq!(end.error_code.as_deref(), Some("server_overloaded"));
}

#[test]
fn an_answer_names_its_call_and_answers_the_call_filter() {
    let setup = setup();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    for (name, at, patch) in [
        (
            "PreToolUse",
            "00000000000000000300",
            json!({"tool_name":"request_user_input_async","tool_use_id":"call_q","turn_id":"turn-a"}),
        ),
        (
            "PostToolUse",
            "00000000000000000301",
            json!({"tool_name":"request_user_input_async","tool_use_id":"call_q","turn_id":"turn-a","tool_response":"{\"accepted\":true}"}),
        ),
    ] {
        setup.apply(&event("codex", name, SESSION, patch), at);
    }
    let id = json!(["request_user_input_async", "call_q", 0]).to_string();
    let items = json!([{"answer":"yes","question":"Which one?","questionItemId":id}]);
    setup.apply(
        &event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":format!("<send_user_message_question_reply>\n{items}\n</send_user_message_question_reply>"),"turn_id":"turn-a"}),
        ),
        "00000000000000000400",
    );
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    let answered = answer
        .events
        .iter()
        .find(|event| event.event == "answer")
        .expect("answer reported");
    assert_eq!(answered.tool_call_id.as_deref(), Some("call_q"));
    assert_eq!(answered.question_index, Some(0));
}

#[test]
fn an_answer_to_another_call_does_not_match_the_call_filter() {
    let setup = setup();
    let scope = scope(&setup, false);
    let root = state_root(&setup.env).unwrap();
    let other = ObserveFilters {
        call: Some("call_other".into()),
        ..filters()
    };
    let baseline = observe(&root, &scope, &other, None, Duration::ZERO).unwrap();
    let cursor = ObserveCursor::parse(baseline.cursor.as_deref().unwrap(), &scope, &other).unwrap();
    let id = json!(["request_user_input_async", "call_q", 0]).to_string();
    let items = json!([{"answer":"yes","question":"Which one?","questionItemId":id}]);
    setup.apply(
        &event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":format!("<send_user_message_question_reply>\n{items}\n</send_user_message_question_reply>"),"turn_id":"turn-a"}),
        ),
        "00000000000000000300",
    );
    let quiet = observe(&root, &scope, &other, Some(&cursor), Duration::ZERO).unwrap();
    assert!(
        quiet.events.iter().all(|event| event.event != "answer"),
        "an answer to another call must not match"
    );
}

#[test]
fn retention_past_the_cursor_is_a_gap_not_the_survivors() {
    let setup = setup();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap();
    snapshot.pools.general.observations.clear();
    snapshot.pools.general.retention_floor_mono_ns = Some("00000000000000000999".into());
    atomic_replace(&dir.join("lifecycle.json"), &json!(snapshot)).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Gap);
    assert!(answer.cursor.is_none(), "a gap ends the cursor chain");
    assert!(!answer.outcome.complete());
}

#[test]
fn a_floor_that_moved_below_the_highest_stamp_is_still_a_gap() {
    // A late observation stamped below the cursor's newest can arrive
    // and be evicted while a newer one stays: the floor moved, so the
    // chain ends instead of returning the survivors as if complete.
    let setup = setup();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap();
    snapshot.pools.general.retention_floor_mono_ns = Some("00000000000000000200".into());
    atomic_replace(&dir.join("lifecycle.json"), &json!(snapshot)).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Gap);
    assert!(answer.cursor.is_none());
}

#[test]
fn a_prompt_seen_only_in_the_prompt_record_is_not_reported_twice() {
    // lifecycle.json and prompts.json are not a transaction: one read can
    // see the prompt record name a prompt no pool holds yet, and a later
    // read finds it pooled. It is one prompt, reported once.
    let setup = setup();
    setup.apply(
        &event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":"first"}),
        ),
        "00000000000000000250",
    );
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    let observation_id = Uuid::new_v4().to_string();
    let mut prompts: wezterm_attention::observations::LeadPrompts =
        serde_json::from_slice(&fs::read(dir.join("prompts.json")).unwrap()).unwrap();
    prompts.latest = Some(wezterm_attention::observations::PromptReference {
        observation_id: observation_id.clone(),
        observed_mono_ns: "00000000000000000300".into(),
        written_at_unix_ns: "00000000000000000300".into(),
    });
    atomic_replace(&dir.join("prompts.json"), &json!(prompts)).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(
        answer
            .events
            .iter()
            .filter(|event| event.observation_id == observation_id)
            .count(),
        1
    );
    // Now the pool holds it too.
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap();
    snapshot
        .reduce(LifecycleObservation {
            observation_id: observation_id.clone(),
            source_event: "UserPromptSubmit".into(),
            source_version: None,
            observed_mono_ns: "00000000000000000300".into(),
            written_at_unix_ns: "00000000000000000300".into(),
            actor: Actor::Lead,
            correlation: Some(NativeCorrelation {
                turn_id: Some("turn-a".into()),
                ..Default::default()
            }),
            turn_end: None,
            body: ObservationBody::PromptSubmitted { input_source: None },
        })
        .unwrap();
    atomic_replace(&dir.join("lifecycle.json"), &json!(snapshot)).unwrap();
    let next = ObserveCursor::parse(answer.cursor.as_deref().unwrap(), &scope, &filters()).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&next), Duration::ZERO).unwrap();
    assert!(
        answer
            .events
            .iter()
            .all(|event| event.observation_id != observation_id),
        "the pooled prompt must not be reported a second time"
    );
}

#[test]
fn a_replaced_binding_and_an_unreadable_lifecycle_are_their_own_outcomes() {
    let setup = setup();
    let root = state_root(&setup.env).unwrap();
    // A scope naming a binding that is not the current one.
    let wrong = PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        Some("d".repeat(64)),
    )
    .unwrap();
    let answer = observe(&root, &wrong, &filters(), None, Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::BindingReplaced);
    assert_eq!(answer.scope_relation.as_deref(), Some("binding_changed"));
    assert_eq!(
        answer.current_binding_id.as_deref(),
        Some(binding_id("codex", SESSION, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]).as_str())
    );
    // A lifecycle file that cannot be used: unavailable, and the caller's
    // cursor is handed back unchanged to retry from.
    let (scope, cursor, encoded) = baseline(&setup);
    let dir = setup.binding_dir("codex", SESSION);
    fs::write(dir.join("lifecycle.json"), "invalid").unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Unavailable);
    assert_eq!(answer.cursor.as_deref(), Some(encoded.as_str()));
}

#[test]
fn observe_cli_baseline_events_and_usage_errors() {
    let setup = setup();
    let scope = serde_json::to_value(scope(&setup, false)).unwrap();
    let run = |args: &[&str], scope: &Value| run_cli(&setup, args, scope);
    let output = run(&["observe", "--scope", "-", "--for", "turn_end"], &scope);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let baseline: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(baseline["command"], "observe");
    assert_eq!(baseline["result"]["outcome"], "baseline");
    let cursor = baseline["result"]["cursor"].as_str().unwrap().to_owned();
    setup.apply(
        &event("codex", "Stop", SESSION, json!({"stop_hook_active":false})),
        "00000000000000000300",
    );
    let output = run(
        &[
            "observe", "--scope", "-", "--for", "turn_end", "--since", &cursor,
        ],
        &scope,
    );
    assert!(output.status.success());
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["result"]["outcome"], "events");
    assert_eq!(answer["result"]["events"][0]["kind"], "response_finished");
    // The baseline cursor was taken under turn_end alone; all filters is a
    // different chain and a usage error.
    let output = run(&["observe", "--scope", "-", "--since", &cursor], &scope);
    assert_eq!(output.status.code(), Some(2));
    let output = run(
        &[
            "observe", "--scope", "-", "--for", "turn_end", "--since", "!!!",
        ],
        &scope,
    );
    assert_eq!(output.status.code(), Some(2));
    // --call without answer among --for is a usage error.
    let output = run(
        &[
            "observe", "--scope", "-", "--for", "prompt", "--call", "call_q",
        ],
        &scope,
    );
    assert_eq!(output.status.code(), Some(2));
    // A timeout above the documented bound is a usage error.
    let output = run(
        &["observe", "--scope", "-", "--timeout-ms", "86400001"],
        &scope,
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn an_unavailable_resume_keeps_its_cursor_pinned_scope() {
    let setup = setup();
    let scope = serde_json::to_value(scope(&setup, false)).unwrap();
    let output = run_cli(&setup, &["observe", "--scope", "-"], &scope);
    assert!(output.status.success());
    let baseline: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cursor = baseline["result"]["cursor"].as_str().unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    fs::write(dir.join("lifecycle.json"), "invalid").unwrap();

    let output = run_cli(
        &setup,
        &["observe", "--scope", "-", "--since", cursor],
        &scope,
    );
    assert_eq!(output.status.code(), Some(1));
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["result"]["outcome"], "unavailable");
    assert_eq!(answer["result"]["cursor"], cursor);
    assert_eq!(answer["result"]["provider"], "codex");
    assert_eq!(answer["result"]["scope"], baseline["result"]["scope"]);
}

#[test]
fn an_unavailable_blocking_call_keeps_its_baseline_scope() {
    let setup = setup();
    let scope = serde_json::to_value(scope(&setup, false)).unwrap();
    let output = run_cli(&setup, &["observe", "--scope", "-"], &scope);
    assert!(output.status.success());
    let baseline: Value = serde_json::from_slice(&output.stdout).unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        fs::write(dir.join("lifecycle.json"), "invalid").unwrap();
    });

    let output = run_cli(
        &setup,
        &["observe", "--scope", "-", "--timeout-ms", "5000"],
        &scope,
    );
    writer.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["result"]["outcome"], "unavailable");
    assert_eq!(answer["result"]["cursor"], baseline["result"]["cursor"]);
    assert_eq!(answer["result"]["provider"], "codex");
    assert_eq!(answer["result"]["scope"], baseline["result"]["scope"]);
}

#[test]
fn a_floor_moving_between_polls_is_a_gap_not_a_timeout() {
    let setup = setup();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    let dir = setup.binding_dir("codex", SESSION);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let mut snapshot: LifecycleSnapshot =
            serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap();
        snapshot.pools.general.retention_floor_mono_ns = Some("00000000000000000200".into());
        atomic_replace(&dir.join("lifecycle.json"), &json!(snapshot)).unwrap();
    });
    let answer = observe(
        &root,
        &scope,
        &filters(),
        Some(&cursor),
        Duration::from_secs(3),
    )
    .unwrap();
    writer.join().unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Gap);
}

#[test]
fn an_absent_prompt_record_is_not_a_gap_when_it_never_existed() {
    let setup = setup();
    setup.apply(
        &event("codex", "UserPromptSubmit", SESSION, json!({"prompt":"go"})),
        "00000000000000000300",
    );
    let dir = setup.binding_dir("codex", SESSION);
    fs::remove_file(dir.join("prompts.json")).unwrap();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Timeout);
}

#[test]
fn answers_missing_from_a_small_replacement_record_are_a_gap() {
    let setup = setup();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    for (name, at, patch) in [
        (
            "PreToolUse",
            "00000000000000000300",
            json!({"tool_name":"request_user_input_async","tool_use_id":"call_q","turn_id":"turn-a"}),
        ),
        (
            "PostToolUse",
            "00000000000000000301",
            json!({"tool_name":"request_user_input_async","tool_use_id":"call_q","turn_id":"turn-a","tool_response":"{\"accepted\":true}"}),
        ),
    ] {
        setup.apply(&event("codex", name, SESSION, patch), at);
    }
    let id = json!(["request_user_input_async", "call_q", 0]).to_string();
    let items = json!([{"answer":"yes","question":"Which one?","questionItemId":id}]);
    setup.apply(
        &event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":format!("<send_user_message_question_reply>\n{items}\n</send_user_message_question_reply>"),"turn_id":"turn-a"}),
        ),
        "00000000000000000400",
    );
    let answered = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answered.outcome, ObserveOutcome::Events);
    let next =
        ObserveCursor::parse(answered.cursor.as_deref().unwrap(), &scope, &filters()).unwrap();
    // The prompt record is replaced by one that no longer holds the
    // answer the chain already reported, at well under 64 answers.
    let dir = setup.binding_dir("codex", SESSION);
    let mut prompts: wezterm_attention::observations::LeadPrompts =
        serde_json::from_slice(&fs::read(dir.join("prompts.json")).unwrap()).unwrap();
    prompts.answers.clear();
    atomic_replace(&dir.join("prompts.json"), &json!(prompts)).unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&next), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Gap);
}

#[test]
fn a_replacement_is_named_even_when_its_records_are_unreadable() {
    let setup = setup();
    let (_, cursor, _) = baseline(&setup);
    // The caller resumes with the scope it originally asked with —
    // unpinned — plus the cursor; the binding comes from the cursor.
    let scope = scope(&setup, false);
    let root = state_root(&setup.env).unwrap();
    // The launch's pointer now selects another binding, whose records
    // cannot be read: that must not hide the replacement.
    let dir = setup.binding_dir("codex", SESSION);
    let pointer_path = dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("current-binding.json");
    let mut pointer: Value = serde_json::from_slice(&fs::read(&pointer_path).unwrap()).unwrap();
    pointer["binding_id"] = json!("d".repeat(64));
    atomic_replace(&pointer_path, &pointer).unwrap();
    let other = dir.parent().unwrap().join("d".repeat(64));
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("lifecycle.json"), "invalid").unwrap();
    let answer = observe(&root, &scope, &filters(), Some(&cursor), Duration::ZERO).unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::BindingReplaced);
    assert_eq!(
        answer.current_binding_id.as_deref(),
        Some("d".repeat(64).as_str())
    );
}

#[test]
fn a_blocking_observe_returns_the_event_it_waited_for() {
    let setup = setup();
    let (scope, cursor, _) = baseline(&setup);
    let root = state_root(&setup.env).unwrap();
    setup.apply(
        &event("codex", "Stop", SESSION, json!({"stop_hook_active":false})),
        "00000000000000000300",
    );
    let answer = observe(
        &root,
        &scope,
        &filters(),
        Some(&cursor),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Events);
    assert_eq!(answer.events.len(), 1);
    // And an empty wait times out as a complete answer.
    let next = ObserveCursor::parse(answer.cursor.as_deref().unwrap(), &scope, &filters()).unwrap();
    let started = std::time::Instant::now();
    let answer = observe(
        &root,
        &scope,
        &filters(),
        Some(&next),
        Duration::from_millis(250),
    )
    .unwrap();
    assert_eq!(answer.outcome, ObserveOutcome::Timeout);
    assert!(answer.outcome.complete());
    assert!(started.elapsed() >= Duration::from_millis(250));
}
