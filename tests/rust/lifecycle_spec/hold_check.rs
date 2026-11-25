use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;
use wezterm_attention::hold_check::{self, Request};
use wezterm_attention::lifecycle::apply_provider_event_with_checks;

const HOLD_LINE: &str = "{\"hold\": true, \"answer\": \"waiting_on_own_work\"}\n";

/// The chain budget for tests that hold a check open on purpose. The command's
/// default budget is a few seconds, which a loaded machine can spend before a
/// shell script has started, and then the check never runs.
const CHECK_BUDGET: Duration = Duration::from_secs(30);

fn wait_for_check_start(ready: &Path) {
    let until = Instant::now() + CHECK_BUDGET;
    while !ready.exists() {
        assert!(Instant::now() < until, "check never began");
        thread::sleep(Duration::from_millis(2));
    }
}

fn bind(setup: &Setup, provider: &str) {
    setup.claim();
    setup.apply(
        &event(
            provider,
            if provider == "pi" {
                "session_start"
            } else {
                "SessionStart"
            },
            "hold-session",
            if provider == "pi" {
                json!({"start_source":"startup"})
            } else {
                json!({"source":"startup"})
            },
        ),
        "00000000000000000200",
    );
}

fn executable(setup: &Setup, body: &str) -> PathBuf {
    let path = setup._scratch.0.join(format!("check-{}", Uuid::new_v4()));
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn payload() -> Value {
    json!({"hook_event_name":"Stop", "session_id":"hold-session",
        "last_assistant_message":"SYNTHETIC-REPLY-ONLY",
        "background_tasks":[{"id":"job-1","type":"shell","status":"running","description":"SYNTHETIC-TASK-DESCRIPTION","extra":{"native":true}}]})
}

fn hook(
    setup: &Setup,
    payload: &Value,
    check: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    let check = format!("jev={}", check.display());
    let mut arguments = vec![
        "hooks",
        "event",
        payload
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("claude"),
        payload["hook_event_name"].as_str().unwrap(),
        "--debug",
        "--hold-check",
        &check,
    ];
    arguments.extend_from_slice(extra);
    run_hook(setup, &arguments, payload)
}

fn records(setup: &Setup, provider: &str) -> (Value, Value) {
    let dir = setup.binding_dir(provider, "hold-session");
    (
        serde_json::from_slice(&fs::read(dir.join("activity.json")).unwrap()).unwrap(),
        serde_json::from_slice(&fs::read(dir.join("lifecycle.json")).unwrap()).unwrap(),
    )
}

fn last_end(snapshot: &Value) -> &Value {
    snapshot["pools"]["general"]["observations"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|item| item.get("turn_end").is_some())
        .unwrap()
}

#[test]
fn always_hold_keeps_running_claude_stop_out_of_checkmark() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let captured = setup._scratch.0.join("input.json");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat > '{}'\nprintf '%s' '{}'",
            captured.display(),
            HOLD_LINE
        ),
    );
    let input = payload();
    let output = hook(&setup, &input, &check, &["--strict"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let delivered: Value = serde_json::from_slice(&fs::read(captured).unwrap()).unwrap();
    assert_eq!(delivered["background_tasks"], input["background_tasks"]);
    assert_eq!(delivered["reply"]["text"], input["last_assistant_message"]);
    assert_eq!(delivered["phase"], "before_turn_end");
    assert!(delivered.get("persistence").is_none());
    assert!(delivered.get("transcript_path").is_none());
    let (activity, snapshot) = records(&setup, "claude");
    assert_eq!(activity["type"], "stop");
    let shown = super::mark_clear::plugin_reader_answer(&setup, "held_render");
    assert_eq!(
        shown,
        "raw=stop shown=thinking held=true indicator=\u{f186}  published=\u{f186}  color=#1c1730"
    );
    assert!(!shown.contains('\u{f00c}'));
    assert_eq!(activity["hold_notes"]["jev"]["hold"], true);
    let end = last_end(&snapshot);
    assert_eq!(end["kind"], "response_finished");
    assert_eq!(end["turn_end"]["status"], "recorded");
    assert_eq!(end["turn_end"]["held"], true);
    assert_eq!(end["turn_end"]["hold_checks"][0]["stage"], "completed");
    let root = state_root(&setup.env).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    let ack = wezterm_attention::lifecycle::acknowledge_activity(
        &root,
        &address,
        &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        activity["event_id"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(ack.disposition, "ignored");
    let record_text = snapshot.to_string() + &activity.to_string();
    assert!(!record_text.contains("SYNTHETIC-REPLY-ONLY"));
    assert!(!record_text.contains("SYNTHETIC-TASK-DESCRIPTION"));
    setup.apply(
        &event("claude", "UserPromptSubmit", "hold-session", json!({})),
        "99999999999999999999",
    );
    let (activity, _) = records(&setup, "claude");
    assert_eq!(activity["type"], "thinking");
    assert!(activity.get("hold_notes").is_none());
}

/// A sub-agent waiting on a permission prompt cannot finish without the user,
/// so the lead's turn end is not held for it: no check runs, and the turn end
/// is recorded unheld. That holds for a waiting sub-agent the Stop still lists,
/// as Claude Code lists one blocked on a prompt as running. One the Stop no
/// longer lists has ended, and a sub-agent that is only running keeps the check.
#[test]
fn a_child_waiting_for_permission_keeps_the_hold_check_from_running() {
    for (child_event, listed, held) in [
        ("PermissionRequest", true, false),
        ("PermissionRequest", false, true),
        ("SubagentStart", true, true),
    ] {
        let case = format!("{child_event} listed={listed}");
        let setup = Setup::new();
        bind(&setup, "claude");
        let mut patch = json!({"agent_id":"child-a","agent_type":"Explore"});
        if child_event == "PermissionRequest" {
            patch["tool_name"] = json!("Bash");
        }
        setup.apply(
            &event("claude", child_event, "hold-session", patch),
            "00000000000000000300",
        );
        let mut input = payload();
        if listed {
            input["background_tasks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"child-a","type":"subagent","status":"running"}));
        }
        let ran = setup._scratch.0.join("ran");
        let check = executable(
            &setup,
            &format!(
                "/usr/bin/touch '{}'\n/bin/cat >/dev/null\nprintf '%s' '{}'",
                ran.display(),
                HOLD_LINE
            ),
        );
        let output = hook(&setup, &input, &check, &["--strict"]);
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(ran.exists(), held, "{case}: whether the check ran");
        let (activity, snapshot) = records(&setup, "claude");
        assert_eq!(activity["type"], "stop", "{case}");
        assert_eq!(activity.get("hold_notes").is_some(), held, "{case}");
        let end = last_end(&snapshot);
        assert_eq!(end["turn_end"]["held"], held, "{case}");
        if !held {
            assert_eq!(end["turn_end"]["hold_checks"][0]["stage"], "not_dispatched");
            assert_eq!(end["turn_end"]["hold_checks"][0]["reason"], "child_waiting");
        }
        // The plugin reads the same records: the waiting child's prompt shows
        // over the finished turn, and a hold shows only once nothing waits.
        let shown = super::mark_clear::plugin_reader_answer(&setup, "held_render");
        let expected = if held {
            "raw=stop shown=thinking held=true"
        } else {
            "raw=stop shown=notify held=false"
        };
        assert!(shown.starts_with(expected), "{case}: {shown}");
    }
}

#[test]
fn timed_out_hold_check_records_unheld_stop() {
    let setup = Setup::new();
    bind(&setup, "claude");
    // A budget shorter than the default keeps the test short.
    let budget = Duration::from_millis(2000);
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\nprintf '%s' '{}'\n/bin/sleep {}",
            HOLD_LINE,
            budget.as_secs() + 1
        ),
    );
    let began = Instant::now();
    let output = hook(
        &setup,
        &payload(),
        &check,
        &["--check-budget-ms", &budget.as_millis().to_string()],
    );
    let elapsed = began.elapsed();
    println!("disposable Stop hook elapsed_ms={}", elapsed.as_millis());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (activity, snapshot) = records(&setup, "claude");
    assert_eq!(
        last_end(&snapshot)["turn_end"]["hold_checks"][0]["stage"],
        "timed_out"
    );
    assert!(
        elapsed < budget + Duration::from_millis(300),
        "hook took {elapsed:?}"
    );
    assert!(activity.get("hold_notes").is_none());
    assert_eq!(last_end(&snapshot)["turn_end"]["held"], false);
    assert_eq!(last_end(&snapshot)["turn_end"]["status"], "recorded");
    let shown = super::mark_clear::plugin_reader_answer(&setup, "held_render");
    assert!(shown.contains("indicator=\u{f00c} "), "{shown}");
    assert!(shown.contains("shown=stop"), "{shown}");
}

#[test]
fn next_unheld_stop_replaces_held_stop_and_delivers_exact_receipt() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let hold = executable(
        &setup,
        &format!("/bin/cat >/dev/null\nprintf '%s' '{}'", HOLD_LINE),
    );
    assert!(
        hook(&setup, &payload(), &hold, &["--strict"])
            .status
            .success()
    );
    let no_hold = executable(&setup, "/bin/cat >/dev/null");
    let sink = setup._scratch.0.join("delivery.json");
    let consumer = executable(&setup, &format!("/bin/cat > '{}'", sink.display()));
    let mut finished = payload();
    finished["background_tasks"] = json!([]);
    let output = hook(
        &setup,
        &finished,
        &no_hold,
        &[
            "--strict",
            "--consumer",
            consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "10000",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (activity, snapshot) = records(&setup, "claude");
    assert!(activity.get("hold_notes").is_none());
    let end = last_end(&snapshot);
    let delivery: Value = serde_json::from_slice(&fs::read(sink).unwrap()).unwrap();
    assert_eq!(delivery["observation_id"], end["observation_id"]);
    assert_eq!(delivery["turn_end"], end["turn_end"]);
    assert_eq!(delivery["turn_end"]["held"], false);
    assert_eq!(
        snapshot["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn newer_prompt_supersedes_stop_while_checks_run_without_locks() {
    assert_newer_prompt_supersedes_stop(false);
}

#[test]
fn newer_prompt_supersedes_stop_while_already_thinking() {
    assert_newer_prompt_supersedes_stop(true);
}

fn assert_newer_prompt_supersedes_stop(already_thinking: bool) {
    let setup = Setup::new();
    bind(&setup, "claude");
    if already_thinking {
        let prompt = event("claude", "UserPromptSubmit", "hold-session", json!({}));
        setup.apply(&prompt, "00000000000000000300");
    }
    let ready = setup._scratch.0.join("ready");
    let released = setup._scratch.0.join("released");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
            ready.display(),
            released.display(),
            HOLD_LINE
        ),
    );
    let registrations =
        hold_check::validate_registrations(&[format!("jev={}", check.display())]).unwrap();
    let payload = payload();
    let stop = parse_provider_event("claude", "Stop", &payload, &setup.env);
    let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            wait_for_check_start(&ready);
            let prompt = event("claude", "UserPromptSubmit", "hold-session", json!({}));
            let result = setup.apply(&prompt, "00000000000000000500");
            assert_ne!(result.disposition, "ignored");
            fs::write(&released, "").unwrap();
        });
        let outcome = apply_provider_event_with_checks(
            &stop,
            &setup.env,
            "00000000000000000400",
            &setup.ports(),
            &Request {
                registrations: &registrations,
                reply: &reply,
                background_tasks: payload["background_tasks"].as_array().map(Vec::as_slice),
                prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
                budget: CHECK_BUDGET,
            },
        );
        writer.join().unwrap();
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "ignored");
        assert_eq!(
            outcome.turn_end.as_ref().unwrap().status,
            wezterm_attention::observations::TurnEndStatus::Superseded
        );
        assert_eq!(outcome.turn_end.as_ref().unwrap().held, Some(false));
    });
    assert_eq!(records(&setup, "claude").0["type"], "thinking");
}

#[test]
fn binding_rotation_during_hold_check_cannot_follow_new_occupant() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let ready = setup._scratch.0.join("ready");
    let released = setup._scratch.0.join("released");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
            ready.display(),
            released.display(),
            HOLD_LINE
        ),
    );
    let registrations =
        hold_check::validate_registrations(&[format!("jev={}", check.display())]).unwrap();
    let payload = payload();
    let stop = parse_provider_event("claude", "Stop", &payload, &setup.env);
    let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            wait_for_check_start(&ready);
            let changed = setup.apply(
                &event(
                    "claude",
                    "SessionStart",
                    "replacement",
                    json!({"source":"fork"}),
                ),
                "00000000000000000500",
            );
            assert_eq!(changed.disposition, "replaced");
            fs::write(&released, "").unwrap();
        });
        let outcome = apply_provider_event_with_checks(
            &stop,
            &setup.env,
            "00000000000000000400",
            &setup.ports(),
            &Request {
                registrations: &registrations,
                reply: &reply,
                background_tasks: payload["background_tasks"].as_array().map(Vec::as_slice),
                prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
                budget: CHECK_BUDGET,
            },
        );
        writer.join().unwrap();
        assert!(outcome.admission.is_none());
        assert!(outcome.turn_end.is_none());
        assert!(
            outcome.hold_checks[0].note.is_some(),
            "{:?}",
            outcome.hold_checks
        );
    });
    assert!(
        !setup
            .binding_dir("claude", "replacement")
            .join("activity.json")
            .exists()
    );
    assert!(
        !setup
            .binding_dir("claude", "hold-session")
            .join("lifecycle.json")
            .exists()
    );
}

#[test]
fn inherited_host_changed_during_hold_check_writes_no_turn_end() {
    for state in [
        ProcessRead::Gone,
        ProcessRead::Unknown,
        ProcessRead::Found(process_facts(
            AGENT_PID,
            SHELL_PID,
            ControllingTerminal::Device(PANE_TTY_DEVICE + 1),
        )),
    ] {
        let setup = Setup::new();
        bind(&setup, "claude");
        let mut env = setup.env.clone();
        env.insert("WEZTERM_ATTENTION_HOST_PID".into(), AGENT_PID.to_string());
        let ready = setup._scratch.0.join("ready");
        let released = setup._scratch.0.join("released");
        let check = executable(
            &setup,
            &format!(
                "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
                ready.display(),
                released.display(),
                HOLD_LINE
            ),
        );
        let registrations =
            hold_check::validate_registrations(&[format!("jev={}", check.display())]).unwrap();
        let payload = payload();
        let stop = parse_provider_event("claude", "Stop", &payload, &env);
        let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
        let before = super::self_claim::records(&setup);
        thread::scope(|scope| {
            let change = scope.spawn(|| {
                wait_for_check_start(&ready);
                setup.processes.set(AGENT_PID, state);
                fs::write(&released, "").unwrap();
            });
            let outcome = apply_provider_event_with_checks(
                &stop,
                &env,
                "00000000000000000400",
                &setup.ports(),
                &Request {
                    registrations: &registrations,
                    reply: &reply,
                    background_tasks: payload["background_tasks"].as_array().map(Vec::as_slice),
                    prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
                    budget: CHECK_BUDGET,
                },
            );
            change.join().unwrap();
            assert!(
                outcome.hold_checks[0].note.is_some(),
                "{:?}",
                outcome.hold_checks
            );
            assert_eq!(outcome.result.as_ref().unwrap().disposition, "ignored");
            assert!(outcome.admission.is_none());
            assert!(outcome.turn_end.is_none());
        });
        assert_eq!(super::self_claim::records(&setup), before);
    }
}

#[test]
fn running_task_gate_does_not_require_reconciliation_ids() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let check = executable(
        &setup,
        &format!("/bin/cat >/dev/null\nprintf '%s' '{}'", HOLD_LINE),
    );
    let mut input = payload();
    input["background_tasks"] = json!([{"type":"future_task","status":"running"}]);
    let output = hook(&setup, &input, &check, &["--strict"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        records(&setup, "claude").0["hold_notes"]["jev"]["hold"],
        true
    );
}

#[test]
fn hold_check_bypasses_adjacent_events_and_unavailable_reply() {
    for (tasks, reply, reason) in [
        (json!([]), json!("text"), "not_applicable"),
        (Value::Null, json!("text"), "not_applicable"),
        (json!({"id":"job"}), json!("text"), "not_applicable"),
        (json!([{"id":"job"}]), Value::Null, "reply_unavailable"),
    ] {
        let setup = Setup::new();
        bind(&setup, "claude");
        let ran = setup._scratch.0.join("ran");
        let check = executable(
            &setup,
            &format!(
                "/bin/cat >/dev/null\n/bin/touch '{}'\nprintf '%s' '{}'",
                ran.display(),
                HOLD_LINE
            ),
        );
        let mut input = payload();
        input["background_tasks"] = tasks;
        input["last_assistant_message"] = reply;
        let output = hook(&setup, &input, &check, &[]);
        let report: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            report["result"]["hold_checks"][0]["stage"],
            "not_dispatched"
        );
        assert_eq!(report["result"]["hold_checks"][0]["reason"], reason);
        assert!(!ran.exists());
        assert!(records(&setup, "claude").0.get("hold_notes").is_none());
    }
    let setup = Setup::new();
    bind(&setup, "claude");
    let check = executable(&setup, "exit 99");
    for (provider, native, extra) in [
        ("claude", "PreToolUse", json!({"tool_name":"Bash"})),
        (
            "claude",
            "Stop",
            json!({"agent_id":"child","agent_type":"worker"}),
        ),
        ("codex", "Stop", json!({})),
    ] {
        let mut input = payload();
        input["provider"] = json!(provider);
        input["hook_event_name"] = json!(native);
        for (k, v) in extra.as_object().unwrap() {
            input[k] = v.clone();
        }
        let output = hook(&setup, &input, &check, &[]);
        let report: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            report["result"]["hold_checks"][0]["stage"],
            "not_dispatched"
        );
    }
}

#[test]
fn native_stop_replay_ignores_execution_audit() {
    let setup = Setup::new();
    bind(&setup, "codex");
    let stop = event("codex", "Stop", "hold-session", json!({"turn_id":"turn-1"}));
    let first = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &stop,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    let second = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &stop,
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    assert_eq!(first.observation_id, second.observation_id);
    assert_eq!(first.turn_end, second.turn_end);
    assert_eq!(
        second.persistence.lifecycle,
        wezterm_attention::lifecycle::outcome::Persistence::Confirmed
    );
    assert_eq!(
        records(&setup, "codex").1["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let setup = Setup::new();
    bind(&setup, "claude");
    let check = executable(
        &setup,
        &format!("/bin/cat >/dev/null\nprintf '%s' '{}'", HOLD_LINE),
    );
    assert!(
        hook(&setup, &payload(), &check, &["--strict"])
            .status
            .success()
    );
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_value(records(&setup, "claude").1).unwrap();
    snapshot.pools.general.observations[0].correlation =
        Some(wezterm_attention::observations::NativeCorrelation {
            turn_id: Some("correlated-stop".into()),
            ..Default::default()
        });
    let stored = snapshot.pools.general.observations[0].clone();
    let mut replay = stored.clone();
    replay.observation_id = Uuid::new_v4().to_string();
    replay.observed_mono_ns = "99999999999999999999".into();
    let end = replay.turn_end.as_mut().unwrap();
    end.held = Some(false);
    end.hold_checks[0].check_id = Some(Uuid::new_v4().to_string());
    end.hold_checks[0].elapsed_ms = 999;
    end.hold_checks[0].note = None;
    assert_ne!(stored.turn_end, replay.turn_end);
    assert!(!snapshot.reduce(replay, None).unwrap());
    assert_eq!(snapshot.pools.general.observations.len(), 1);
    assert_eq!(snapshot.pools.general.observations[0], stored);
}

#[test]
fn existing_provider_turn_end_kinds_have_unheld_receipts() {
    for (provider, native, extra) in [
        ("claude", "StopFailure", json!({})),
        ("codex", "Interrupt", json!({})),
        ("pi", "agent_settled", json!({})),
    ] {
        let setup = Setup::new();
        bind(&setup, provider);
        let callback = event(provider, native, "hold-session", extra);
        let result = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
            &callback,
            &setup.env,
            "00000000000000000300",
            &setup.ports(),
        );
        assert_eq!(
            result.turn_end.as_ref().unwrap().held,
            Some(false),
            "{provider} {native}: {result:?}"
        );
        assert_eq!(
            result.turn_end.as_ref().unwrap().status,
            wezterm_attention::observations::TurnEndStatus::Recorded
        );
    }
    let setup = Setup::new();
    bind(&setup, "pi");
    let failed = event(
        "pi",
        "message_end",
        "hold-session",
        json!({"role":"assistant","stop_reason":"error"}),
    );
    assert!(
        wezterm_attention::lifecycle::apply_provider_event_with_outcome(
            &failed,
            &setup.env,
            "00000000000000000300",
            &setup.ports()
        )
        .turn_end
        .is_none()
    );
}

#[test]
fn older_interrupt_is_superseded_by_a_newer_prompt() {
    let setup = Setup::new();
    bind(&setup, "codex");
    setup.apply(
        &event("codex", "UserPromptSubmit", "hold-session", json!({})),
        "00000000000000000500",
    );
    let interrupt = event("codex", "Interrupt", "hold-session", json!({}));
    let outcome = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &interrupt,
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    assert_eq!(
        outcome.turn_end.as_ref().unwrap().status,
        wezterm_attention::observations::TurnEndStatus::Superseded
    );
    assert_eq!(records(&setup, "codex").0["type"], "thinking");
}

#[test]
fn failed_checks_never_attach_notes() {
    for body in [
        format!("/bin/cat >/dev/null\nprintf '%s' '{}'\nexit 7",HOLD_LINE),
        "/bin/cat >/dev/null\nprintf '%s' '{\"hold\":true,\"answer\":\"a\",\"actor\":{\"kind\":\"lead\"}}'".to_owned(),
        "/bin/cat >/dev/null\nprintf '%s' '{\"hold\":true,\"answer\":\"a\",\"hold\":true}'".to_owned(),
        "/bin/cat >/dev/null\nprintf '%s' '{} {}'".to_owned(),
    ] {
        let setup=Setup::new();bind(&setup,"claude");
        let check=executable(&setup,&body);
        let output=hook(&setup,&payload(),&check,&["--strict"]);
        assert_eq!(output.status.code(),Some(1));
        let (activity,snapshot)=records(&setup,"claude");
        assert!(activity.get("hold_notes").is_none());
        assert_eq!(last_end(&snapshot)["turn_end"]["held"],false);
        assert_eq!(last_end(&snapshot)["turn_end"]["status"],"recorded");
    }
}

#[test]
fn hold_audit_reservation_preserves_large_native_observation() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let payload = payload();
    let mut stop = parse_provider_event("claude", "Stop", &payload, &setup.env);
    let observation = stop.observation.as_mut().unwrap();
    observation.source_version = Some("v".repeat(256));
    observation.correlation = Some(wezterm_attention::observations::NativeCorrelation {
        turn_id: Some("t".repeat(256)),
        message_id: Some("m".repeat(256)),
        tool_call_id: Some("c".repeat(256)),
        ..Default::default()
    });
    let original = observation.clone();
    let registrations = hold_check::validate_registrations(
        &(0..3)
            .map(|n| format!("program{n}=/{}", "p".repeat(3800)))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
    let result = apply_provider_event_with_checks(
        &stop,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
        &Request {
            registrations: &registrations,
            reply: &reply,
            background_tasks: payload["background_tasks"].as_array().map(Vec::as_slice),
            prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
            budget: CHECK_BUDGET,
        },
    );
    assert_eq!(
        result.persistence.lifecycle,
        wezterm_attention::lifecycle::outcome::Persistence::Confirmed
    );
    let (_, snapshot) = records(&setup, "claude");
    let end = last_end(&snapshot);
    assert_eq!(end["source_version"], original.source_version.unwrap());
    assert_eq!(
        end["correlation"],
        serde_json::to_value(original.correlation).unwrap()
    );
    assert_eq!(end["turn_end"]["hold_checks"].as_array().unwrap().len(), 3);
    assert!(end.to_string().len() > 2048);
    wezterm_attention::protocol::validate_record(&snapshot, Some("lifecycle_snapshot")).unwrap();
    assert!(
        super::mark_clear::plugin_reader_answer(&setup, "held_render")
            .contains("indicator=\u{f00c} ")
    );
}

const QUIET_LINE: &str = "{\"quiet\":true,\"answer\":\"wezpup_submitted\"}\n";

fn quiet_stop(
    setup: &Setup,
    provider: &str,
    check: &std::path::Path,
    order: &str,
) -> wezterm_attention::lifecycle::HookOutcome {
    let registrations = hold_check::validate_checks(
        &[],
        &[format!("wezpup={}", check.display())],
        hold_check::DEFAULT_BUDGET,
    )
    .unwrap();
    let stop = event(
        provider,
        "Stop",
        "hold-session",
        json!({"turn_id":"driven-turn"}),
    );
    apply_provider_event_with_checks(
        &stop,
        &setup.env,
        order,
        &setup.ports(),
        &Request {
            registrations: &registrations,
            reply: &wezterm_attention::hook_content::HookContent::NotRequested,
            background_tasks: None,
            prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
            budget: CHECK_BUDGET,
        },
    )
}

#[test]
fn quiet_checks_receive_frozen_history_without_reply_or_background_tasks() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        bind(&setup, provider);
        setup.apply(
            &event(
                provider,
                "UserPromptSubmit",
                "hold-session",
                json!({"turn_id":"driven-turn"}),
            ),
            "00000000000000000300",
        );
        let captured = setup._scratch.0.join("quiet-input.json");
        let check = executable(
            &setup,
            &format!(
                "/bin/cat > '{}'\nprintf '%s' '{}'",
                captured.display(),
                QUIET_LINE
            ),
        );
        let outcome = quiet_stop(&setup, provider, &check, "00000000000000000400");
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        assert_eq!(outcome.turn_end.as_ref().unwrap().held, Some(false));
        let (activity, snapshot) = records(&setup, provider);
        assert_eq!(activity["type"], "stop");
        assert_eq!(activity["hold_notes"]["wezpup"]["quiet"], true);
        assert_eq!(
            last_end(&snapshot)["turn_end"]["hold_checks"][0]["note"]["quiet"],
            true
        );
        let input: Value = serde_json::from_slice(&fs::read(captured).unwrap()).unwrap();
        assert_eq!(input["provider"], provider);
        assert_eq!(input["reply"]["availability"], "not_requested");
        assert!(input["lifecycle"]["snapshot_id"].is_string());
        assert_eq!(
            input["lifecycle"]["observations"].as_array().unwrap().len(),
            1
        );
        assert_eq!(input["prospective_observation"]["source_event"], "Stop");
        assert!(
            input["prospective_observation"]
                .get("written_at_unix_ns")
                .is_none()
        );
        // The duplicate native identity must retain the original decision.
        if provider == "codex" {
            let repeated = quiet_stop(&setup, provider, &check, "00000000000000000500");
            assert_eq!(repeated.observation_id, outcome.observation_id);
            assert_eq!(records(&setup, provider).0, activity);
            setup.apply(
                &event(
                    provider,
                    "UserPromptSubmit",
                    "hold-session",
                    json!({"turn_id":"human-turn"}),
                ),
                "00000000000000000600",
            );
            quiet_stop(&setup, provider, &check, "00000000000000000700");
            assert_eq!(records(&setup, provider).0["type"], "thinking");
        }
    }
}

// A quiet check reads the public lifecycle view, so a question answered while
// the turn ran reaches it as answered.
#[test]
fn a_quiet_check_sees_a_question_answered_during_its_turn() {
    let setup = Setup::new();
    bind(&setup, "codex");
    let prompt = |text: &str, at: &str| {
        setup.apply(
            &event(
                "codex",
                "UserPromptSubmit",
                "hold-session",
                json!({"turn_id":"answered-turn","prompt":text}),
            ),
            at,
        );
    };
    prompt("start the work", "00000000000000000300");
    for (name, at, patch) in [
        ("PreToolUse", "00000000000000000310", json!({})),
        (
            "PostToolUse",
            "00000000000000000311",
            json!({"tool_response":"{\"accepted\":true}"}),
        ),
    ] {
        let mut payload = json!({"tool_name":"request_user_input_async",
            "tool_use_id":"call_quiet","turn_id":"answered-turn"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        setup.apply(&event("codex", name, "hold-session", payload), at);
    }
    let id = json!(["request_user_input_async", "call_quiet", 0]).to_string();
    prompt(
        &format!(
            "<send_user_message_question_reply>{}</send_user_message_question_reply>",
            json!([{"answer":"yes","question":"Q","questionItemId":id}])
        ),
        "00000000000000000320",
    );
    let captured = setup._scratch.0.join("quiet-input.json");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat > '{}'\nprintf '%s' '{}'",
            captured.display(),
            QUIET_LINE
        ),
    );
    quiet_stop(&setup, "codex", &check, "00000000000000000400");
    let input: Value = serde_json::from_slice(&fs::read(captured).unwrap()).unwrap();
    let question = input["lifecycle"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["correlation"]["tool_call_id"] == "call_quiet")
        .expect("question group")
        .clone();
    assert_eq!(
        question["answer_observation_ids"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        input["lifecycle"]["latest_prompt"]["observed_mono_ns"],
        "00000000000000000320"
    );
}

#[test]
fn a_quiet_check_on_the_command_line_attaches_its_note_and_prints_nothing() {
    let setup = Setup::new();
    bind(&setup, "claude");
    // A quiet check reads the turn's history, so the turn needs one observation.
    setup.apply(
        &event("claude", "UserPromptSubmit", "hold-session", json!({})),
        "00000000000000000300",
    );
    let check = executable(
        &setup,
        &format!("/bin/cat >/dev/null\nprintf '%s' '{QUIET_LINE}'"),
    );
    let mut input = payload();
    input.as_object_mut().unwrap().remove("background_tasks");
    let quiet = format!("wezpup={}", check.display());
    let output = run_hook(
        &setup,
        &["hooks", "event", "claude", "Stop", "--quiet-check", &quiet],
        &input,
    );
    assert!(output.status.success());
    // Without --debug or --consumer, a hook prints only its diagnostics, and
    // this event has none.
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(
        records(&setup, "claude").0["hold_notes"]["wezpup"]["quiet"],
        true
    );
}

#[test]
fn a_malformed_quiet_check_is_reported_under_its_own_flag() {
    let setup = Setup::new();
    let output = run_hook(
        &setup,
        &[
            "hooks",
            "event",
            "claude",
            "Stop",
            "--quiet-check",
            "wezpup",
        ],
        &payload(),
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "attention: bad_usage: --quiet-check requires NAME=/absolute/executable\n"
    );
}

#[test]
fn a_check_budget_outside_its_range_is_refused() {
    let setup = Setup::new();
    let output = run_hook(
        &setup,
        &["hooks", "event", "claude", "Stop", "--check-budget-ms", "0"],
        &payload(),
    );
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "invalid value '0' for '--check-budget-ms <CHECK_BUDGET_MS>': 0 is not in 1..=86400000"
        ),
        "{stderr}"
    );
}

#[test]
fn a_failed_check_without_debug_still_names_its_stage() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let check = executable(&setup, "/bin/cat >/dev/null\nexit 99");
    let hold = format!("jev={}", check.display());
    let output = run_hook(
        &setup,
        &[
            "hooks",
            "event",
            "claude",
            "Stop",
            "--strict",
            "--hold-check",
            &hold,
        ],
        &payload(),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["result"]["hold_checks"][0]["stage"], "failed");
}

#[test]
fn a_prompt_committed_during_quiet_check_discards_its_note() {
    for prompt_order in ["00000000000000000350", "00000000000000000500"] {
        let setup = Setup::new();
        bind(&setup, "claude");
        setup.apply(
            &event("claude", "UserPromptSubmit", "hold-session", json!({})),
            "00000000000000000300",
        );
        let ready = setup._scratch.0.join("ready");
        let released = setup._scratch.0.join("released");
        let check = executable(
            &setup,
            &format!(
                "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
                ready.display(),
                released.display(),
                QUIET_LINE
            ),
        );
        thread::scope(|scope| {
            let writer = scope.spawn(|| {
                wait_for_check_start(&ready);
                setup.apply(
                    &event("claude", "UserPromptSubmit", "hold-session", json!({})),
                    prompt_order,
                );
                fs::write(&released, "").unwrap();
            });
            let outcome = quiet_stop(&setup, "claude", &check, "00000000000000000400");
            writer.join().unwrap();
            assert_eq!(
                outcome.hold_checks[0].stage,
                hold_check::Stage::EvidenceChanged
            );
            assert!(outcome.hold_checks[0].note.is_none());
            assert!(records(&setup, "claude").0.get("hold_notes").is_none());
            if prompt_order > "00000000000000000400" {
                assert_eq!(outcome.result.as_ref().unwrap().disposition, "ignored");
            } else {
                assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
            }
        });
    }
}

#[test]
fn a_child_permission_committed_during_quiet_check_discards_its_note() {
    permission_during_check(false);
}

#[test]
fn a_child_permission_committed_during_hold_check_discards_its_note() {
    permission_during_check(true);
}

fn permission_during_check(hold: bool) {
    let setup = Setup::new();
    bind(&setup, "claude");
    setup.apply(
        &event("claude", "UserPromptSubmit", "hold-session", json!({})),
        "00000000000000000300",
    );
    setup.apply(
        &event(
            "claude",
            "SubagentStart",
            "hold-session",
            json!({"agent_id":"child-a"}),
        ),
        "00000000000000000310",
    );
    let ready = setup._scratch.0.join("ready");
    let released = setup._scratch.0.join("released");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
            ready.display(),
            released.display(),
            if hold { HOLD_LINE } else { QUIET_LINE }
        ),
    );
    let named = vec![format!("checker={}", check.display())];
    let registrations = if hold {
        hold_check::validate_checks(&named, &[], hold_check::DEFAULT_BUDGET)
    } else {
        hold_check::validate_checks(&[], &named, hold_check::DEFAULT_BUDGET)
    }
    .unwrap();
    let payload = json!({"hook_event_name":"Stop", "session_id":"hold-session",
        "last_assistant_message":"still waiting", "background_tasks":[{"id":"child-a","type":"subagent","status":"running"}]});
    let stop = parse_provider_event("claude", "Stop", &payload, &setup.env);
    let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            wait_for_check_start(&ready);
            setup.apply(
                &event(
                    "claude",
                    "PermissionRequest",
                    "hold-session",
                    json!({"agent_id":"child-a","tool_name":"Bash"}),
                ),
                "00000000000000000350",
            );
            fs::write(&released, "").unwrap();
        });
        let outcome = apply_provider_event_with_checks(
            &stop,
            &setup.env,
            "00000000000000000400",
            &setup.ports(),
            &Request {
                registrations: &registrations,
                reply: &reply,
                background_tasks: payload["background_tasks"].as_array().map(Vec::as_slice),
                prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
                budget: CHECK_BUDGET,
            },
        );
        writer.join().unwrap();
        assert_eq!(
            outcome.hold_checks[0].stage,
            hold_check::Stage::EvidenceChanged
        );
        assert!(outcome.hold_checks[0].note.is_none());
        assert_eq!(outcome.turn_end.unwrap().held, Some(false));
        assert!(records(&setup, "claude").0.get("hold_notes").is_none());
    });
}

// A check compares its deadline with its own reading of the same clock, so the
// clock must be one another process can read. Python reads it through
// `time.clock_gettime_ns`, as jev's check would.
#[test]
fn a_check_in_another_language_reads_its_deadline_on_the_same_clock() {
    let setup = Setup::new();
    bind(&setup, "codex");
    setup.apply(
        &event(
            "codex",
            "UserPromptSubmit",
            "hold-session",
            json!({"turn_id":"driven-turn"}),
        ),
        "00000000000000000300",
    );
    let captured = setup._scratch.0.join("clock.json");
    let check = executable(
        &setup,
        &format!(
            "exec '{}' -c 'import json, sys, time\n\
             deadline = json.load(sys.stdin)[\"deadline_mono_ns\"]\n\
             reading = time.clock_gettime_ns(time.CLOCK_MONOTONIC_RAW)\n\
             open(\"{}\", \"w\").write(json.dumps([deadline, str(reading)]))'",
            super::executables::resolve("python3").display(),
            captured.display()
        ),
    );
    let ns = |text: &str| text.parse::<u128>().unwrap();
    let now = || wezterm_attention::identity::monotonic_raw_ns().unwrap();
    let before = now();
    let outcome = quiet_stop(&setup, "codex", &check, "00000000000000000400");
    let after = now();
    let stages = &outcome.hold_checks;
    let (deadline, reading): (String, String) =
        serde_json::from_slice(&fs::read(&captured).unwrap_or_else(|_| {
            panic!("the check wrote nothing: {stages:?}");
        }))
        .unwrap();
    assert!(
        wezterm_attention::protocol::ns20_text(&deadline),
        "{deadline}"
    );
    assert!(before <= ns(&reading) && ns(&reading) < ns(&deadline));
    // The only eligible check keeps the whole budget: no reserve is taken.
    let budget = CHECK_BUDGET.as_nanos();
    assert!(before + budget <= ns(&deadline) && ns(&deadline) <= after + budget);
}

// The guide's example input is what consumers copy, so it names exactly the
// fields a hold check receives.
#[test]
fn the_guide_example_input_has_the_fields_a_hold_check_receives() {
    let guide = include_str!("../../../docs/consumer-guide.md");
    let example: Value = guide
        .split_once("Stdin is one JSON object, newline and EOF:")
        .and_then(|(_, rest)| rest.split_once("```json\n"))
        .and_then(|(_, rest)| rest.split_once("\n```"))
        .map(|(block, _)| serde_json::from_str(block).unwrap())
        .expect("consumer guide shows a hold-check input");
    let setup = Setup::new();
    bind(&setup, "claude");
    let captured = setup._scratch.0.join("input.json");
    let check = executable(&setup, &format!("/bin/cat > '{}'", captured.display()));
    let mut input = payload();
    input["transcript_path"] = json!("/tmp/hold-session.jsonl");
    let output = hook(&setup, &input, &check, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let delivered: Value = serde_json::from_slice(&fs::read(captured).unwrap()).unwrap();
    assert_eq!(delivered["transcript_path"], "/tmp/hold-session.jsonl");
    let keys = |value: &Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&example), keys(&delivered));
}

// A prompt typed while the turn's Stop hooks run carries that turn's id, so
// only the prompt record keeps it. Committed during a quiet check, it still
// supersedes the older Stop, and the check's note is not applied.
#[test]
fn a_same_turn_prompt_committed_during_quiet_check_supersedes_the_stop() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let prompt = json!({"prompt_id":"turn-1"});
    setup.apply(
        &event("claude", "UserPromptSubmit", "hold-session", prompt.clone()),
        "00000000000000000300",
    );
    let ready = setup._scratch.0.join("ready");
    let released = setup._scratch.0.join("released");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
            ready.display(),
            released.display(),
            QUIET_LINE
        ),
    );
    let registrations = hold_check::validate_checks(
        &[],
        &[format!("wezpup={}", check.display())],
        hold_check::DEFAULT_BUDGET,
    )
    .unwrap();
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            wait_for_check_start(&ready);
            setup.apply(
                &event("claude", "UserPromptSubmit", "hold-session", prompt.clone()),
                "00000000000000000500",
            );
            fs::write(&released, "").unwrap();
        });
        let outcome = apply_provider_event_with_checks(
            &event(
                "claude",
                "Stop",
                "hold-session",
                json!({"prompt_id":"turn-1","stop_hook_active":false}),
            ),
            &setup.env,
            "00000000000000000400",
            &setup.ports(),
            &Request {
                registrations: &registrations,
                reply: &wezterm_attention::hook_content::HookContent::NotRequested,
                background_tasks: None,
                prompt: &wezterm_attention::hook_content::HookContent::NotRequested,
                budget: CHECK_BUDGET,
            },
        );
        writer.join().unwrap();
        assert_eq!(outcome.hold_checks[0].stage, hold_check::Stage::Completed);
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "ignored");
        assert_eq!(
            outcome.turn_end.as_ref().unwrap().status,
            wezterm_attention::observations::TurnEndStatus::Superseded
        );
        assert!(records(&setup, "claude").0.get("hold_notes").is_none());
    });
}

/// Applies `name` with the quiet check `check` registered, as a hook line
/// with `--quiet-check wezpup=…` does, handing the check the prompt as the
/// command does.
fn quiet_thinking(
    setup: &Setup,
    provider: &str,
    name: &str,
    patch: Value,
    check: &std::path::Path,
    order: &str,
) -> wezterm_attention::lifecycle::HookOutcome {
    use wezterm_attention::hook_content::HookContent;
    let registrations = hold_check::validate_checks(
        &[],
        &[format!("wezpup={}", check.display())],
        hold_check::DEFAULT_BUDGET,
    )
    .unwrap();
    let payload = super::payload(provider, name, "hold-session", patch);
    let event = parse_provider_event(provider, name, &payload, &BTreeMap::new());
    let prompt = wezterm_attention::providers::prompt_content(&event, &payload, true);
    apply_provider_event_with_checks(
        &event,
        &setup.env,
        order,
        &setup.ports(),
        &Request {
            registrations: &registrations,
            reply: &HookContent::NotRequested,
            background_tasks: None,
            prompt: &prompt,
            budget: CHECK_BUDGET,
        },
    )
}

/// A quiet check that appends each input it is given to `log`, one line
/// each, and prints what `answer` holds.
fn logging_check(setup: &Setup) -> (PathBuf, PathBuf, PathBuf) {
    let log = setup._scratch.0.join("check-inputs.jsonl");
    let answer = setup._scratch.0.join("check-answer");
    fs::write(&answer, QUIET_LINE).unwrap();
    let check = executable(
        setup,
        &format!(
            "/bin/cat >> '{}'\n/bin/cat '{}'",
            log.display(),
            answer.display()
        ),
    );
    (check, log, answer)
}

fn logged_inputs(log: &Path) -> Vec<Value> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_quiet_check_marks_a_turn_as_it_starts_and_its_later_tool_calls_keep_the_mark() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        bind(&setup, provider);
        let (check, log, answer) = logging_check(&setup);
        let tool = |id: &str| json!({"tool_name":"Bash","tool_use_id":id,"turn_id":"agent-turn"});

        // The prompt is the binding's first observation: there is no history
        // yet, and the check still runs.
        let outcome = quiet_thinking(
            &setup,
            provider,
            "UserPromptSubmit",
            json!({"turn_id":"agent-turn","prompt":"SYNTHETIC-AGENT-PROMPT"}),
            &check,
            "00000000000000000300",
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        assert_eq!(outcome.hold_checks[0].stage, hold_check::Stage::Completed);
        let (started, _) = records(&setup, provider);
        assert_eq!(started["type"], "thinking");
        assert_eq!(started["hold_notes"]["wezpup"]["quiet"], true);
        assert_eq!(
            started["hold_notes"]["wezpup"]["answer"],
            "wezpup_submitted"
        );
        wezterm_attention::protocol::validate_record(&started, Some("activity")).unwrap();
        let inputs = logged_inputs(&log);
        assert_eq!(inputs.len(), 1);
        let input = &inputs[0];
        assert_eq!(input["phase"], "before_thinking");
        assert_eq!(input["source_event"], "UserPromptSubmit");
        assert_eq!(input["prompt"]["availability"], "available");
        assert_eq!(input["prompt"]["text"], "SYNTHETIC-AGENT-PROMPT");
        assert_eq!(input["transcript_path"], "/tmp/session.jsonl");
        assert_eq!(
            input["prospective_observation"]["source_event"],
            "UserPromptSubmit"
        );
        for absent in ["reply", "background_tasks", "lifecycle"] {
            assert!(input.get(absent).is_none(), "{provider} {absent}");
        }
        assert_eq!(
            super::mark_clear::plugin_reader_answer(&setup, "quiet_render"),
            "raw=thinking shown=thinking thinking_quiet=true turn_end_quiet=false color=#0f2a3f"
        );

        // The turn's first tool call is asked again, with no prompt in its
        // input, and the same answer leaves the activity as it was.
        let outcome = quiet_thinking(
            &setup,
            provider,
            "PreToolUse",
            tool("call_1"),
            &check,
            "00000000000000000310",
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "skipped");
        assert_eq!(outcome.hold_checks[0].stage, hold_check::Stage::Completed);
        assert_eq!(records(&setup, provider).0, started);
        let inputs = logged_inputs(&log);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1]["phase"], "before_thinking");
        assert_eq!(inputs[1]["source_event"], "PreToolUse");
        assert!(inputs[1].get("prompt").is_none());

        // Later tool calls of the turn run no check, and the note stays,
        // with or without a check on their hook line.
        fs::write(&answer, "").unwrap();
        let outcome = quiet_thinking(
            &setup,
            provider,
            "PreToolUse",
            tool("call_2"),
            &check,
            "00000000000000000315",
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "skipped");
        assert_eq!(
            outcome.hold_checks[0].stage,
            hold_check::Stage::NotDispatched
        );
        assert_eq!(
            outcome.hold_checks[0].reason,
            Some(hold_check::Bypass::NotApplicable)
        );
        setup.apply(
            &event(provider, "PreToolUse", "hold-session", tool("call_2b")),
            "00000000000000000320",
        );
        assert_eq!(records(&setup, provider).0, started);
        assert_eq!(logged_inputs(&log).len(), 2);

        // A later prompt is asked about again, and an empty answer takes the
        // note away.
        let outcome = quiet_thinking(
            &setup,
            provider,
            "UserPromptSubmit",
            json!({"turn_id":"agent-turn","prompt":"SYNTHETIC-TYPED-PROMPT"}),
            &check,
            "00000000000000000330",
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        let (typed, _) = records(&setup, provider);
        assert_eq!(typed["type"], "thinking");
        assert!(typed.get("hold_notes").is_none());
        assert_ne!(typed["event_id"], started["event_id"]);
        let inputs = logged_inputs(&log);
        assert_eq!(inputs.len(), 3, "{provider} {:?}", outcome.hold_checks);
        assert!(inputs[2]["lifecycle"]["snapshot_id"].is_string());
        assert_eq!(
            super::mark_clear::plugin_reader_answer(&setup, "quiet_render"),
            "raw=thinking shown=thinking thinking_quiet=false turn_end_quiet=false color=#1c1730"
        );

        // After the turn ends, the tool call that starts the next turn's work
        // without a prompt is asked about.
        setup.apply(
            &event(
                provider,
                "Stop",
                "hold-session",
                json!({"turn_id":"agent-turn"}),
            ),
            "00000000000000000340",
        );
        fs::write(&answer, QUIET_LINE).unwrap();
        let outcome = quiet_thinking(
            &setup,
            provider,
            "PreToolUse",
            tool("call_3"),
            &check,
            "00000000000000000350",
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        let (continued, _) = records(&setup, provider);
        assert_eq!(continued["type"], "thinking");
        assert_eq!(continued["hold_notes"]["wezpup"]["quiet"], true);
        let inputs = logged_inputs(&log);
        assert_eq!(inputs.len(), 4);
        assert_eq!(inputs[3]["phase"], "before_thinking");
        assert_eq!(inputs[3]["source_event"], "PreToolUse");
        assert!(inputs[3].get("prompt").is_none());

        // A Codex tool call that asks the user writes notify, not thinking,
        // and runs no check.
        if provider == "codex" {
            setup.apply(
                &event(
                    provider,
                    "Stop",
                    "hold-session",
                    json!({"turn_id":"agent-turn"}),
                ),
                "00000000000000000360",
            );
            let mut ask = tool("call_4");
            ask["tool_name"] = json!("request_user_input");
            quiet_thinking(
                &setup,
                provider,
                "PreToolUse",
                ask,
                &check,
                "00000000000000000370",
            );
            assert_eq!(records(&setup, provider).0["type"], "notify");
            assert_eq!(logged_inputs(&log).len(), 4);
        }
    }
}

#[test]
fn a_turns_first_tool_call_answer_replaces_the_prompts_note() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        bind(&setup, provider);
        let (check, log, answer) = logging_check(&setup);
        let tool = |id: &str| json!({"tool_name":"Bash","tool_use_id":id,"turn_id":"turn"});
        let prompt = |order: &str| {
            quiet_thinking(
                &setup,
                provider,
                "UserPromptSubmit",
                json!({"turn_id":"turn","prompt":"SYNTHETIC-PROMPT"}),
                &check,
                order,
            )
        };
        let call = |id: &str, order: &str| {
            quiet_thinking(&setup, provider, "PreToolUse", tool(id), &check, order)
        };

        // A note the prompt was given goes when the first tool call's
        // answer is empty, and stays gone for the rest of the turn.
        prompt("00000000000000000300");
        assert_eq!(
            records(&setup, provider).0["hold_notes"]["wezpup"]["quiet"],
            true
        );
        fs::write(&answer, "").unwrap();
        let outcome = call("call_1", "00000000000000000310");
        assert_eq!(outcome.hold_checks[0].stage, hold_check::Stage::Completed);
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        let (unmarked, _) = records(&setup, provider);
        assert_eq!(unmarked["type"], "thinking");
        assert!(unmarked.get("hold_notes").is_none());
        // Its lifecycle names the turn's prompt and holds that prompt's
        // observation.
        let lifecycle = logged_inputs(&log)[1]["lifecycle"].clone();
        let prompt_id = &lifecycle["latest_prompt"]["observation_id"];
        assert!(
            lifecycle["observations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["observation_id"] == *prompt_id
                    && item["source_event"] == "UserPromptSubmit"),
            "{provider} {lifecycle}"
        );
        fs::write(&answer, QUIET_LINE).unwrap();
        let outcome = call("call_2", "00000000000000000320");
        assert_eq!(
            outcome.hold_checks[0].stage,
            hold_check::Stage::NotDispatched
        );
        assert_eq!(records(&setup, provider).0, unmarked);
        assert_eq!(logged_inputs(&log).len(), 2);

        // A prompt given no note gets one from its first tool call.
        fs::write(&answer, "").unwrap();
        prompt("00000000000000000330");
        assert!(records(&setup, provider).0.get("hold_notes").is_none());
        fs::write(&answer, QUIET_LINE).unwrap();
        let outcome = call("call_3", "00000000000000000340");
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "applied");
        let (marked, _) = records(&setup, provider);
        assert_eq!(marked["hold_notes"]["wezpup"]["quiet"], true);
        wezterm_attention::protocol::validate_record(&marked, Some("activity")).unwrap();

        // A first tool call whose check gives no answer keeps the prompt's
        // note.
        prompt("00000000000000000350");
        let (noted, _) = records(&setup, provider);
        assert_eq!(noted["hold_notes"]["wezpup"]["quiet"], true);
        fs::write(&answer, "not an answer").unwrap();
        let outcome = call("call_4", "00000000000000000360");
        assert_eq!(
            outcome.hold_checks[0].stage,
            hold_check::Stage::InvalidOutput
        );
        assert_eq!(outcome.result.as_ref().unwrap().disposition, "skipped");
        assert_eq!(records(&setup, provider).0, noted);
        assert_eq!(logged_inputs(&log).len(), 6);
    }
}

#[test]
fn a_tool_call_recorded_during_a_first_prompts_check_discards_its_note() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let ready = setup._scratch.0.join("ready");
    let released = setup._scratch.0.join("released");
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do :; done\nprintf '%s' '{}'",
            ready.display(),
            released.display(),
            QUIET_LINE
        ),
    );
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            wait_for_check_start(&ready);
            setup.apply(
                &event(
                    "claude",
                    "PreToolUse",
                    "hold-session",
                    json!({"tool_name":"Bash","tool_use_id":"call_1"}),
                ),
                "00000000000000000290",
            );
            fs::write(&released, "").unwrap();
        });
        let outcome = quiet_thinking(
            &setup,
            "claude",
            "UserPromptSubmit",
            json!({"prompt":"SYNTHETIC-AGENT-PROMPT"}),
            &check,
            "00000000000000000300",
        );
        writer.join().unwrap();
        assert_eq!(
            outcome.hold_checks[0].stage,
            hold_check::Stage::EvidenceChanged
        );
        assert!(outcome.hold_checks[0].note.is_none());
        let (activity, _) = records(&setup, "claude");
        assert_eq!(activity["type"], "thinking");
        assert!(activity.get("hold_notes").is_none());
    });
}

#[test]
fn a_quiet_check_on_a_prompt_hook_line_reads_the_prompt_without_include_prompt() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let (check, log, _) = logging_check(&setup);
    let quiet = format!("wezpup={}", check.display());
    let output = run_hook(
        &setup,
        &[
            "hooks",
            "event",
            "claude",
            "UserPromptSubmit",
            "--strict",
            "--quiet-check",
            &quiet,
        ],
        &json!({"hook_event_name":"UserPromptSubmit", "session_id":"hold-session",
            "prompt":"SYNTHETIC-AGENT-PROMPT"}),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let inputs = logged_inputs(&log);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["prompt"]["text"], "SYNTHETIC-AGENT-PROMPT");
    assert_eq!(
        records(&setup, "claude").0["hold_notes"]["wezpup"]["quiet"],
        true
    );
}

#[test]
fn a_lead_tool_call_held_by_a_waiting_child_runs_no_quiet_check() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let (check, log, _) = logging_check(&setup);
    setup.apply(
        &event("claude", "UserPromptSubmit", "hold-session", json!({})),
        "00000000000000000300",
    );
    setup.apply(
        &event(
            "claude",
            "SubagentStart",
            "hold-session",
            json!({"agent_id":"child-a"}),
        ),
        "00000000000000000310",
    );
    setup.apply(
        &event(
            "claude",
            "PermissionRequest",
            "hold-session",
            json!({"agent_id":"child-a","tool_name":"Bash","tool_use_id":"child_call"}),
        ),
        "00000000000000000320",
    );
    assert_eq!(records(&setup, "claude").0["type"], "notify");
    let outcome = quiet_thinking(
        &setup,
        "claude",
        "PreToolUse",
        json!({"tool_name":"Bash","tool_use_id":"lead_call"}),
        &check,
        "00000000000000000330",
    );
    assert_eq!(outcome.result.as_ref().unwrap().disposition, "ignored");
    assert_eq!(
        outcome.hold_checks[0].reason,
        Some(hold_check::Bypass::NotApplicable)
    );
    assert!(logged_inputs(&log).is_empty());
    assert_eq!(records(&setup, "claude").0["type"], "notify");
}

fn acknowledge(setup: &Setup, event_id: &Value) -> String {
    let root = state_root(&setup.env).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    wezterm_attention::lifecycle::acknowledge_activity(
        &root,
        &address,
        &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        event_id.as_str().unwrap(),
    )
    .unwrap()
    .disposition
    .as_str()
    .to_owned()
}

/// A person's finished turn stays on the pane until they have looked, through
/// the turn an application starts there next: the tab shows the person's
/// stop, not the application's thinking or its quiet stop, until the plugin
/// acknowledges it. Then the application's activity shows, and its next turn
/// keeps nothing.
#[test]
fn a_stop_the_user_has_not_seen_stays_under_an_applications_turn() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        bind(&setup, provider);
        let (check, _log, _answer) = logging_check(&setup);
        setup.apply(
            &event(
                provider,
                "UserPromptSubmit",
                "hold-session",
                json!({"turn_id":"typed-turn"}),
            ),
            "00000000000000000300",
        );
        setup.apply(
            &event(
                provider,
                "Stop",
                "hold-session",
                json!({"turn_id":"typed-turn"}),
            ),
            "00000000000000000310",
        );
        let (typed, _) = records(&setup, provider);
        assert_eq!(typed["type"], "stop");
        assert!(typed.get("unseen_stop").is_none());
        let typed_render = super::mark_clear::plugin_reader_answer(&setup, "quiet_render");

        quiet_thinking(
            &setup,
            provider,
            "UserPromptSubmit",
            json!({"turn_id":"agent-turn","prompt":"SYNTHETIC-AGENT-PROMPT"}),
            &check,
            "00000000000000000320",
        );
        let (thinking, _) = records(&setup, provider);
        assert_eq!(thinking["type"], "thinking");
        assert_eq!(thinking["hold_notes"]["wezpup"]["quiet"], true);
        assert_eq!(thinking["unseen_stop"]["event_id"], typed["event_id"]);
        assert_eq!(thinking["unseen_stop"]["source"], provider);
        wezterm_attention::protocol::validate_record(&thinking, Some("activity")).unwrap();
        assert_eq!(
            super::mark_clear::plugin_reader_answer(&setup, "quiet_render"),
            typed_render,
            "{provider}: the person's stop shows over the application's thinking"
        );

        // The turn's tool calls leave the record as it was.
        quiet_thinking(
            &setup,
            provider,
            "PreToolUse",
            json!({"tool_name":"Bash","tool_use_id":"call_1","turn_id":"agent-turn"}),
            &check,
            "00000000000000000330",
        );
        assert_eq!(records(&setup, provider).0, thinking);

        quiet_thinking(
            &setup,
            provider,
            "Stop",
            json!({"turn_id":"agent-turn"}),
            &check,
            "00000000000000000340",
        );
        let (ended, _) = records(&setup, provider);
        assert_eq!(ended["type"], "stop");
        assert_eq!(ended["hold_notes"]["wezpup"]["quiet"], true);
        assert_eq!(ended["unseen_stop"], thinking["unseen_stop"]);
        assert_eq!(
            super::mark_clear::plugin_reader_answer(&setup, "quiet_render"),
            typed_render,
            "{provider}: the person's stop shows over the application's quiet stop"
        );

        assert_eq!(acknowledge(&setup, &typed["event_id"]), "applied");
        let quiet_render = super::mark_clear::plugin_reader_answer(&setup, "quiet_render");
        assert!(
            quiet_render
                .starts_with("raw=stop shown=stop thinking_quiet=false turn_end_quiet=true"),
            "{provider}: {quiet_render}"
        );
        assert_eq!(acknowledge(&setup, &ended["event_id"]), "applied");
        assert_eq!(
            super::mark_clear::plugin_reader_answer(&setup, "activity"),
            "activity=none source=none"
        );

        quiet_thinking(
            &setup,
            provider,
            "UserPromptSubmit",
            json!({"turn_id":"next-agent-turn","prompt":"SYNTHETIC-AGENT-PROMPT"}),
            &check,
            "00000000000000000350",
        );
        let (next, _) = records(&setup, provider);
        assert_eq!(next["hold_notes"]["wezpup"]["quiet"], true);
        assert!(next.get("unseen_stop").is_none(), "{provider}: {next}");
    }
}

/// A prompt the quiet checks answered without claiming keeps the unseen stop
/// until its turn's first tool call is answered, since a check can prove a
/// Claude turn an application's only at that call; left unclaimed there too,
/// the turn is the person's, and the stop goes. A prompt no quiet check was
/// asked about keeps nothing, and neither does a held stop or another
/// publisher's stop.
#[test]
fn only_an_applications_turn_keeps_the_unseen_stop() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let (check, _log, answer) = logging_check(&setup);
    let tool = |id: &str, turn: &str| json!({"tool_name":"Bash","tool_use_id":id,"turn_id":turn});
    setup.apply(
        &event("claude", "Stop", "hold-session", json!({"turn_id":"t1"})),
        "00000000000000000300",
    );
    let (first, _) = records(&setup, "claude");
    fs::write(&answer, "").unwrap();
    quiet_thinking(
        &setup,
        "claude",
        "UserPromptSubmit",
        json!({"turn_id":"t2","prompt":"SYNTHETIC-PEER-PROMPT"}),
        &check,
        "00000000000000000310",
    );
    let (unclaimed, _) = records(&setup, "claude");
    assert_eq!(unclaimed["type"], "thinking");
    assert!(unclaimed.get("hold_notes").is_none());
    assert_eq!(unclaimed["unseen_stop"]["event_id"], first["event_id"]);
    wezterm_attention::protocol::validate_record(&unclaimed, Some("activity")).unwrap();
    assert!(
        super::mark_clear::plugin_reader_answer(&setup, "quiet_render")
            .starts_with("raw=stop shown=stop ")
    );
    quiet_thinking(
        &setup,
        "claude",
        "PreToolUse",
        tool("call_1", "t2"),
        &check,
        "00000000000000000315",
    );
    let (person, _) = records(&setup, "claude");
    assert!(person.get("unseen_stop").is_none(), "{person}");
    assert_eq!(person["event_id"], unclaimed["event_id"]);
    assert!(
        super::mark_clear::plugin_reader_answer(&setup, "quiet_render")
            .starts_with("raw=thinking shown=thinking ")
    );
    assert_eq!(acknowledge(&setup, &first["event_id"]), "ignored");

    // A prompt that no quiet check was asked about keeps nothing.
    setup.apply(
        &event("claude", "Stop", "hold-session", json!({"turn_id":"t2"})),
        "00000000000000000320",
    );
    setup.apply(
        &event(
            "claude",
            "UserPromptSubmit",
            "hold-session",
            json!({"turn_id":"t3"}),
        ),
        "00000000000000000330",
    );
    let (unasked, _) = records(&setup, "claude");
    assert_eq!(unasked["type"], "thinking");
    assert!(unasked.get("unseen_stop").is_none(), "{unasked}");

    // A held turn end of the person's turn is not kept under the next turn.
    let hold = executable(
        &setup,
        &format!("/bin/cat > /dev/null\nprintf '%s' '{HOLD_LINE}'"),
    );
    let output = hook(&setup, &payload(), &hold, &[]);
    assert!(output.status.success(), "{output:?}");
    let (held, _) = records(&setup, "claude");
    assert_eq!(held["hold_notes"]["jev"]["hold"], true, "{held}");
    fs::write(&answer, QUIET_LINE).unwrap();
    quiet_thinking(
        &setup,
        "claude",
        "UserPromptSubmit",
        json!({"turn_id":"t4","prompt":"SYNTHETIC-AGENT-PROMPT"}),
        &check,
        "99999999999999999990",
    );
    let (after_hold, _) = records(&setup, "claude");
    assert_eq!(after_hold["hold_notes"]["wezpup"]["quiet"], true);
    assert!(after_hold.get("unseen_stop").is_none(), "{after_hold}");

    // Another publisher's stop is withdrawn by its own clear, so it is not
    // copied under an application's turn.
    apply_mark_activity(
        &setup.env,
        "stop",
        "build",
        None,
        None,
        None,
        "99999999999999999991",
        "00000000012345678900",
    )
    .expect("mark applies");
    quiet_thinking(
        &setup,
        "claude",
        "UserPromptSubmit",
        json!({"turn_id":"t5","prompt":"SYNTHETIC-AGENT-PROMPT"}),
        &check,
        "99999999999999999992",
    );
    let (after_mark, _) = records(&setup, "claude");
    assert_eq!(after_mark["hold_notes"]["wezpup"]["quiet"], true);
    assert!(after_mark.get("unseen_stop").is_none(), "{after_mark}");
}

/// A prompt whose quiet check failed has not been shown to be the person's,
/// so its thinking keeps the unseen stop, and a first tool call that proves
/// the turn an application's keeps it on.
#[test]
fn a_prompt_whose_check_failed_keeps_the_unseen_stop_for_its_first_tool_call() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let (check, _log, _answer) = logging_check(&setup);
    let failing = executable(&setup, "/bin/cat > /dev/null\nexit 3");
    setup.apply(
        &event("claude", "Stop", "hold-session", json!({"turn_id":"t1"})),
        "00000000000000000300",
    );
    let (first, _) = records(&setup, "claude");
    let outcome = quiet_thinking(
        &setup,
        "claude",
        "UserPromptSubmit",
        json!({"turn_id":"t2","prompt":"SYNTHETIC-PEER-PROMPT"}),
        &failing,
        "00000000000000000310",
    );
    assert_ne!(outcome.hold_checks[0].stage, hold_check::Stage::Completed);
    let (prompted, _) = records(&setup, "claude");
    assert!(prompted.get("hold_notes").is_none());
    assert_eq!(prompted["unseen_stop"]["event_id"], first["event_id"]);
    quiet_thinking(
        &setup,
        "claude",
        "PreToolUse",
        json!({"tool_name":"Bash","tool_use_id":"call_1","turn_id":"t2"}),
        &check,
        "00000000000000000315",
    );
    let (claimed, _) = records(&setup, "claude");
    assert_eq!(claimed["hold_notes"]["wezpup"]["quiet"], true);
    assert_eq!(claimed["unseen_stop"]["event_id"], first["event_id"]);
}
