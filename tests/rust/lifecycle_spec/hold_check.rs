use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;
use wezterm_attention::hold_check::{self, Request};
use wezterm_attention::lifecycle::apply_provider_event_with_checks;

const HOLD_LINE: &str = "{\"hold\": true, \"answer\": \"waiting_on_own_work\"}\n";

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
    let (activity, snapshot) = records(&setup, "claude");
    assert_eq!(activity["type"], "stop");
    let shown = super::mark_clear::plugin_reader_answer(&setup, "held_render");
    assert_eq!(
        shown,
        "raw=stop shown=thinking held=true indicator=⏾  published=⏾  color=#1c1730"
    );
    assert!(!shown.contains('✓'));
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
    let check = executable(
        &setup,
        &format!(
            "/bin/cat >/dev/null\nprintf '%s' '{}'\n/bin/sleep 3",
            HOLD_LINE
        ),
    );
    let began = Instant::now();
    let output = hook(&setup, &payload(), &check, &[]);
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
        elapsed < Duration::from_millis(2800),
        "hook took {elapsed:?}"
    );
    assert!(activity.get("hold_notes").is_none());
    assert_eq!(last_end(&snapshot)["turn_end"]["held"], false);
    assert_eq!(last_end(&snapshot)["turn_end"]["status"], "recorded");
    let shown = super::mark_clear::plugin_reader_answer(&setup, "held_render");
    assert!(shown.contains("indicator=✓ "), "{shown}");
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
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < until, "check never began");
                thread::sleep(Duration::from_millis(2));
            }
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
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < until, "check never began");
                thread::sleep(Duration::from_millis(2));
            }
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
                let until = Instant::now() + Duration::from_secs(5);
                while !ready.exists() {
                    assert!(Instant::now() < until, "check never began");
                    thread::sleep(Duration::from_millis(2));
                }
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
    assert!(!snapshot.reduce(replay).unwrap());
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
        super::mark_clear::plugin_reader_answer(&setup, "held_render").contains("indicator=✓ ")
    );
}

const QUIET_LINE: &str = "{\"quiet\":true,\"answer\":\"wezpup_submitted\"}\n";

fn quiet_stop(
    setup: &Setup,
    provider: &str,
    check: &std::path::Path,
    order: &str,
) -> wezterm_attention::lifecycle::HookOutcome {
    let registrations =
        hold_check::validate_checks(&[], &[format!("wezpup={}", check.display())]).unwrap();
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
                let until = Instant::now() + Duration::from_secs(5);
                while !ready.exists() {
                    assert!(Instant::now() < until, "check never began");
                    thread::sleep(Duration::from_millis(2));
                }
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
        hold_check::validate_checks(&named, &[])
    } else {
        hold_check::validate_checks(&[], &named)
    }
    .unwrap();
    let payload = json!({"hook_event_name":"Stop", "session_id":"hold-session",
        "last_assistant_message":"still waiting", "background_tasks":[{"id":"child-a","type":"subagent","status":"running"}]});
    let stop = parse_provider_event("claude", "Stop", &payload, &setup.env);
    let reply = wezterm_attention::providers::reply_content(&stop, &payload, true);
    thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < until, "check never began");
                thread::sleep(Duration::from_millis(2));
            }
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
