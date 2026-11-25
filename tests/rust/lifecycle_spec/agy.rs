//! Antigravity CLI (`agy`) lifecycle, subagent lineage, quiet checks,
//! transient prompt/reply delivery, `--resume` lazy binding, clear replay
//! guard, and Esc status-database turn follower integration tests.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::self_claim::apply_as;
#[cfg(target_os = "macos")]
use super::turn_follower::{follow_every, last_request, recorded};
use super::*;
use wezterm_attention::lifecycle::apply_provider_event_with_outcome;
#[cfg(target_os = "macos")]
use wezterm_attention::turn_follower::{AgyTurn, FollowedTurn};

const LEAD: &str = "agy-lead-1";

fn make_executable(setup: &Setup, name: &str, body: &str) -> PathBuf {
    let path = setup._scratch.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn write_jsonl(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, content).unwrap();
}

fn agy_brain_dir(setup: &Setup, conv_id: &str) -> (PathBuf, PathBuf) {
    let artifact = setup
        ._scratch
        .0
        .join("app_data")
        .join("brain")
        .join(conv_id);
    let tx = artifact
        .join(".system_generated")
        .join("logs")
        .join("transcript_full.jsonl");
    (artifact, tx)
}

fn agy_hook_cli(
    _setup: &Setup,
    env: &BTreeMap<String, String>,
    event_name: &str,
    payload: &Value,
    extra: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_attention"));
    command
        .env_clear()
        .envs(env)
        .args(["hooks", "event", "agy", event_name])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(payload).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn run_cli_with_stdin(
    setup: &Setup,
    args: &[&str],
    stdin: &Value,
) -> (std::process::Output, Value) {
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
        .write_all(&serde_json::to_vec(stdin).unwrap())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
    (out, parsed)
}

fn read_json(path: PathBuf) -> Option<Value> {
    fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("record JSON"))
}

fn shown_activity(binding_dir: &Path) -> Option<Value> {
    let activity = read_json(binding_dir.join("activity.json"))?;
    let clear = read_json(binding_dir.join("activity-clear.json"));
    let order = activity["observed_mono_ns"].as_str().unwrap_or("");
    if clear.is_some_and(|clear| order <= clear["observed_mono_ns"].as_str().unwrap_or("")) {
        return None;
    }
    Some(activity)
}

#[test]
fn agy_lead_turn_prompt_reply_observation_kind_and_wakeup_separation() {
    let setup = Setup::new();
    setup.claim();
    let (artifact, tx) = agy_brain_dir(&setup, LEAD);
    let tx_str = tx.to_string_lossy().into_owned();
    let artifact_str = artifact.to_string_lossy().into_owned();

    // SessionStart binds the conversation.
    let start_payload = json!({
        "hookEventName": "SessionStart",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "artifactDirectoryPath": artifact_str,
        "cwd": "/tmp/project",
        "modelName": "gemini-3.8-flash-medium",
        "lastUserInput": "Summarize the tests"
    });
    let start_out = agy_hook_cli(
        &setup,
        &setup.env,
        "SessionStart",
        &start_payload,
        &["--strict"],
    );
    assert!(
        start_out.status.success(),
        "{}",
        String::from_utf8_lossy(&start_out.stderr)
    );
    assert!(start_out.stdout.is_empty());

    // Transcript ends on USER_INPUT -> real user prompt.
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
        ],
    );
    let prompt_sink = setup._scratch.0.join("prompt-delivery.json");
    let prompt_consumer = make_executable(
        &setup,
        "prompt-consumer",
        &format!("/bin/cat > '{}'", prompt_sink.display()),
    );
    let pre_inv = json!({
        "hookEventName": "PreInvocation",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "artifactDirectoryPath": artifact_str,
        "cwd": "/tmp/project",
        "modelName": "gemini-3.8-flash-medium",
        "lastUserInput": "Summarize the tests",
        "invocationNum": 0,
        "initialNumSteps": 1
    });
    let pre_out = agy_hook_cli(
        &setup,
        &setup.env,
        "PreInvocation",
        &pre_inv,
        &[
            "--consumer",
            prompt_consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "5000",
            "--include-prompt",
            "--strict",
        ],
    );
    assert!(
        pre_out.status.success(),
        "{}",
        String::from_utf8_lossy(&pre_out.stderr)
    );
    let prompt_delivery: Value = serde_json::from_slice(&fs::read(&prompt_sink).unwrap()).unwrap();
    assert_eq!(prompt_delivery["provider"], "agy");
    assert_eq!(prompt_delivery["source_event"], "PreInvocation");
    assert_eq!(prompt_delivery["observation_kind"], "prompt_submitted");
    assert!(prompt_delivery["observation_id"].is_string());
    assert_eq!(
        prompt_delivery["prompt"],
        json!({"availability": "available", "text": "Summarize the tests"})
    );

    // Background subagent wakeup: PLANNER_RESPONSE followed by SYSTEM_MESSAGE
    // still sends invocationNum=0 and stale lastUserInput, but must NOT emit
    // PromptSubmitted or deliver --include-prompt.
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "LAUNCHED"}),
            json!({"step_index": 2, "type": "SYSTEM_MESSAGE", "status": "DONE", "content": "subagent done"}),
        ],
    );
    let wakeup_sink = setup._scratch.0.join("wakeup-delivery.json");
    let wakeup_consumer = make_executable(
        &setup,
        "wakeup-consumer",
        &format!("/bin/cat > '{}'", wakeup_sink.display()),
    );
    let wakeup_out = agy_hook_cli(
        &setup,
        &setup.env,
        "PreInvocation",
        &pre_inv,
        &[
            "--consumer",
            wakeup_consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "5000",
            "--include-prompt",
            "--strict",
        ],
    );
    assert!(
        wakeup_out.status.success(),
        "{}",
        String::from_utf8_lossy(&wakeup_out.stderr)
    );
    let wakeup_delivery: Value = serde_json::from_slice(&fs::read(&wakeup_sink).unwrap()).unwrap();
    assert!(wakeup_delivery.get("observation_id").is_none());
    assert!(wakeup_delivery.get("observation_kind").is_none());
    assert_eq!(
        wakeup_delivery["prompt"],
        json!({"availability": "unsupported"})
    );

    // Normal Stop with empty finalModelOutput falls back to transcript tail.
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": ""}),
            json!({"step_index": 2, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "All 67 tests pass."}),
        ],
    );
    let stop_sink = setup._scratch.0.join("stop-delivery.json");
    let stop_consumer = make_executable(
        &setup,
        "stop-consumer",
        &format!("/bin/cat > '{}'", stop_sink.display()),
    );
    let stop_payload = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "artifactDirectoryPath": artifact_str,
        "cwd": "/tmp/project",
        "modelName": "gemini-3.8-flash-medium",
        "lastUserInput": "Summarize the tests",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 1
    });
    let stop_out = agy_hook_cli(
        &setup,
        &setup.env,
        "Stop",
        &stop_payload,
        &[
            "--consumer",
            stop_consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "5000",
            "--include-reply",
            "--strict",
        ],
    );
    assert!(
        stop_out.status.success(),
        "{}",
        String::from_utf8_lossy(&stop_out.stderr)
    );
    let stop_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(stop_delivery["observation_kind"], "response_finished");
    assert_eq!(
        stop_delivery["reply"],
        json!({"availability": "available", "text": "All 67 tests pass."})
    );
    // executionNum: 1 with continuationActive: false sets stop_hook_active: true.
    let snapshot: LifecycleSnapshot = serde_json::from_slice(
        &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
    )
    .unwrap();
    let stop_obs = snapshot
        .pools
        .general
        .observations
        .iter()
        .find(|item| item.body.kind() == "response_finished")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&stop_obs.body).unwrap()["stop_hook_active"],
        json!(true)
    );

    // Forward-compat: populated finalModelOutput takes precedence over transcript.
    let mut fwd_stop = stop_payload.clone();
    fwd_stop["finalModelOutput"] = json!("Direct payload reply");
    let fwd_parsed = parse_provider_event("agy", "Stop", &fwd_stop, &setup.env);
    assert_eq!(
        serde_json::to_value(wezterm_attention::providers::reply_content(
            &fwd_parsed,
            &fwd_stop,
            true
        ))
        .unwrap(),
        json!({"availability": "available", "text": "Direct payload reply"})
    );

    // Error Stop (TERMINAL_CUSTOM_HOOK) produces attempt_outcome, notify with
    // label, and unsupported reply (never leaks the prior turn's reply).
    let err_sink = setup._scratch.0.join("err-delivery.json");
    let err_consumer = make_executable(
        &setup,
        "err-consumer",
        &format!("/bin/cat > '{}'", err_sink.display()),
    );
    let err_stop = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "Next turn",
        "terminationReason": "TERMINAL_CUSTOM_HOOK",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 0
    });
    let err_out = agy_hook_cli(
        &setup,
        &setup.env,
        "Stop",
        &err_stop,
        &[
            "--consumer",
            err_consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "5000",
            "--include-reply",
            "--strict",
        ],
    );
    assert!(
        err_out.status.success(),
        "{}",
        String::from_utf8_lossy(&err_out.stderr)
    );
    let err_delivery: Value = serde_json::from_slice(&fs::read(&err_sink).unwrap()).unwrap();
    assert_eq!(err_delivery["observation_kind"], "attempt_outcome");
    assert_eq!(
        err_delivery["reply"],
        json!({"availability": "unsupported"})
    );
    let activity = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["label"], "TERMINAL_CUSTOM_HOOK");
}

#[test]
fn agy_resume_lazily_binds_self_claimed_pane_and_blocks_foreign_host() {
    let setup = Setup::new();
    let agent_env = setup.agent_env();
    let (_, tx) = agy_brain_dir(&setup, "resumed-conv");
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "old"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "old"}),
            json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "content": "resumed prompt"}),
        ],
    );
    let pre_inv = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "resumed-conv",
            "transcriptPath": tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "resumed prompt",
            "invocationNum": 0,
            "initialNumSteps": 3
        }),
        &agent_env,
    );
    let res = apply_as(&setup, &agent_env, &pre_inv, "00000000000000000300");
    assert_eq!(res.disposition, "applied", "{res:?}");

    // Same host `/new` SessionStart replaces active binding cleanly.
    let (_, tx2) = agy_brain_dir(&setup, "new-conv");
    let new_start = parse_provider_event(
        "agy",
        "SessionStart",
        &json!({
            "hookEventName": "SessionStart",
            "conversationId": "new-conv",
            "transcriptPath": tx2.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "brand new chat"
        }),
        &agent_env,
    );
    let replaced = apply_as(&setup, &agent_env, &new_start, "00000000000000000400");
    assert_eq!(replaced.disposition, "replaced", "{replaced:?}");

    // A second live agent process on the same terminal cannot hijack via
    // SessionStart or PreInvocation.
    let foreign_pid = AGENT_PID + 55;
    setup.processes.set(
        foreign_pid,
        ProcessRead::Found(process_facts(
            foreign_pid,
            SHELL_PID,
            ControllingTerminal::Device(PANE_TTY_DEVICE),
        )),
    );
    setup
        .processes
        .change(HOOK_PID, |facts| facts.parent_pid = foreign_pid);
    let mut foreign_env = agent_env.clone();
    foreign_env.insert(
        "WEZTERM_ATTENTION_HOST_PID".to_owned(),
        foreign_pid.to_string(),
    );
    let foreign_pre = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "hijack-conv",
            "transcriptPath": "/tmp/hijack.jsonl",
            "cwd": "/tmp/project",
            "lastUserInput": "hijack",
            "invocationNum": 0
        }),
        &foreign_env,
    );
    let refused = apply_as(&setup, &foreign_env, &foreign_pre, "00000000000000000500");
    assert_eq!(refused.disposition, "ignored", "{refused:?}");
    assert_eq!(
        refused.diagnostic.as_ref().map(|d| d.code.as_str()),
        Some("claim_stale")
    );
}

#[test]
fn agy_quiet_checks_run_on_prompt_first_tool_and_normal_stop_only() {
    let setup = Setup::new();
    setup.claim();
    let (_, tx) = agy_brain_dir(&setup, LEAD);
    let tx_str = tx.to_string_lossy().into_owned();
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "auto prompt"}),
        ],
    );
    let start_payload = json!({
        "hookEventName": "SessionStart",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt"
    });
    assert!(
        agy_hook_cli(
            &setup,
            &setup.env,
            "SessionStart",
            &start_payload,
            &["--strict"]
        )
        .status
        .success()
    );

    let log_file = setup._scratch.0.join("quiet-inputs.ndjson");
    let checker = make_executable(
        &setup,
        "quiet-check",
        &format!(
            "line=$(/bin/cat)\nprintf '%s\\n' \"$line\" >> '{}'\nprintf '{{\"quiet\":true,\"answer\":\"controller_submitted\"}}\\n'",
            log_file.display()
        ),
    );
    let check_arg = format!("wezpup={}", checker.display());

    // 1. PreInvocation(inv=0) runs before_thinking quiet check with prompt.
    let pre0 = json!({
        "hookEventName": "PreInvocation",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "invocationNum": 0,
        "initialNumSteps": 1
    });
    let out = agy_hook_cli(
        &setup,
        &setup.env,
        "PreInvocation",
        &pre0,
        &["--quiet-check", &check_arg, "--strict"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(act["type"], "thinking");
    assert_eq!(act["hold_notes"]["wezpup"]["quiet"], json!(true));

    // 2. Intra-turn PreInvocation(inv=1) and Stop(fullyIdle: false) preserve
    // the thinking quiet note without re-running the check.
    let pre1 = json!({
        "hookEventName": "PreInvocation",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "invocationNum": 1,
        "initialNumSteps": 3
    });
    assert!(
        agy_hook_cli(
            &setup,
            &setup.env,
            "PreInvocation",
            &pre1,
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(act["hold_notes"]["wezpup"]["quiet"], json!(true));

    let non_idle_stop = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": false
    });
    assert!(
        agy_hook_cli(
            &setup,
            &setup.env,
            "Stop",
            &non_idle_stop,
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(act["type"], "thinking");
    assert_eq!(act["hold_notes"]["wezpup"]["quiet"], json!(true));

    // 3. Error Stop must NOT run quiet check (`not_applicable`).
    let err_stop = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "ERROR",
        "fullyIdle": true
    });
    let err_out = agy_hook_cli(
        &setup,
        &setup.env,
        "Stop",
        &err_stop,
        &["--quiet-check", &check_arg, "--debug", "--strict"],
    );
    assert!(
        err_out.status.success(),
        "{}",
        String::from_utf8_lossy(&err_out.stderr)
    );
    let err_env: Value = serde_json::from_slice(&err_out.stderr).unwrap();
    assert_eq!(
        err_env["result"]["hold_checks"][0]["reason"],
        "not_applicable"
    );

    // 4. Normal ResponseFinished Stop runs before_turn_end quiet check.
    let ok_stop = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 0
    });
    let ok_out = agy_hook_cli(
        &setup,
        &setup.env,
        "Stop",
        &ok_stop,
        &["--quiet-check", &check_arg, "--strict"],
    );
    assert!(
        ok_out.status.success(),
        "{}",
        String::from_utf8_lossy(&ok_out.stderr)
    );
    let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(act["type"], "stop");
    assert_eq!(act["hold_notes"]["wezpup"]["quiet"], json!(true));

    // Exactly 2 invocations: PreInvocation(0) and normal Stop.
    let logged: Vec<Value> = fs::read_to_string(&log_file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[0]["phase"], "before_thinking");
    assert_eq!(logged[0]["source_event"], "PreInvocation");
    assert_eq!(
        logged[0]["prompt"],
        json!({"availability": "available", "text": "auto prompt"})
    );
    assert_eq!(logged[1]["phase"], "before_turn_end");
    assert_eq!(logged[1]["source_event"], "Stop");
}

#[test]
fn agy_ask_question_surfaces_notify_request_and_answer_and_replayed_clear_does_not_wipe_new_turn() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );

    let (address, _) = pane_address(&setup.env).unwrap();
    let scope = json!({
        "address": address,
        "launch_id": setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        "binding_id": binding_id("agy", LEAD, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"])
    });
    let (_, base) = run_cli_with_stdin(
        &setup,
        &[
            "observe", "--scope", "-", "--for", "request", "--for", "answer", "--for", "turn_end",
        ],
        &scope,
    );
    let cursor = base["result"]["cursor"].as_str().unwrap().to_owned();

    // Blocking ask_question sets notify and emits request.
    setup.apply(
        &event(
            "agy",
            "PreToolUse",
            LEAD,
            json!({"toolCall": {"name": "ask_question", "id": "call-ask-1"}}),
        ),
        "00000000000000000300",
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // PostToolUse restores thinking and emits answer.
    setup.apply(
        &event(
            "agy",
            "PostToolUse",
            LEAD,
            json!({"toolCall": {"name": "ask_question", "id": "call-ask-1"}, "error": ""}),
        ),
        "00000000000000000400",
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    let (_, obs) = run_cli_with_stdin(
        &setup,
        &[
            "observe", "--scope", "-", "--since", &cursor, "--for", "request", "--for", "answer",
            "--for", "turn_end",
        ],
        &scope,
    );
    let events = obs["result"]["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["event"] == "request"
        && e["request_kind"] == "question"
        && e["question_mode"] == "blocking"
        && e["tool_call_id"] == "call-ask-1"));
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "answer" && e["tool_call_id"] == "call-ask-1")
    );

    // USER_CANCELED Stop clears activity with executionId=turn-1.
    let cancel_ev = event(
        "agy",
        "Stop",
        LEAD,
        json!({"terminationReason": "USER_CANCELED", "fullyIdle": true, "executionId": "turn-1"}),
    );
    assert_eq!(
        setup.apply(&cancel_ev, "00000000000000000500").disposition,
        "applied"
    );
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());

    // New turn starts (turn-2) -> thinking.
    setup.apply(
        &event(
            "agy",
            "PreInvocation",
            LEAD,
            json!({"invocationNum": 0, "executionId": "turn-2"}),
        ),
        "00000000000000000600",
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    // Late replay of turn-1's USER_CANCELED Stop with a newer local timestamp
    // must be skipped and MUST NOT wipe turn-2's thinking!
    let replay_res = setup.apply(&cancel_ev, "00000000000000000700");
    assert_eq!(replay_res.disposition, "skipped", "{replay_res:?}");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
}

#[test]
fn agy_subagents_walk_grandchild_messages_and_handle_kill_and_reconcile() {
    let setup = Setup::new();
    setup.claim();
    let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
    let (child_dir, child_tx) = agy_brain_dir(&setup, "child-1");
    let (grand_dir, grand_tx) = agy_brain_dir(&setup, "grand-1");

    // Write invoke_subagent message JSON for child -> lead and grandchild -> child.
    let child_msgs = child_dir.join(".system_generated").join("messages");
    let grand_msgs = grand_dir.join(".system_generated").join("messages");
    fs::create_dir_all(&child_msgs).unwrap();
    fs::create_dir_all(&grand_msgs).unwrap();
    fs::write(
        child_msgs.join("0001.json"),
        serde_json::to_vec(&json!({
            "sender": LEAD,
            "sourceMetadata": {
                "tool": {
                    "conversationId": LEAD,
                    "toolCall": {
                        "name": "invoke_subagent",
                        "argumentsJson": "{\"Role\":\"Worker\",\"TypeName\":\"self\"}"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        grand_msgs.join("0001.json"),
        serde_json::to_vec(&json!({
            "sender": "child-1",
            "sourceMetadata": {
                "tool": {
                    "conversationId": "child-1",
                    "toolCall": {
                        "name": "invoke_subagent",
                        "argumentsJson": "{\"Role\":\"Leaf\",\"TypeName\":\"research\"}"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // Lead binds.
    setup.apply(
        &parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "spawn tree"
            }),
            &setup.env,
        ),
        "00000000000000000200",
    );

    // Unlinked event with empty lastUserInput is skipped, never hijacks lead.
    let unlinked = parse_provider_event(
        "agy",
        "SessionStart",
        &json!({
            "hookEventName": "SessionStart",
            "conversationId": "orphan-1",
            "cwd": "/tmp/project",
            "lastUserInput": ""
        }),
        &setup.env,
    );
    let skip_res = setup.apply(&unlinked, "00000000000000000250");
    assert_eq!(skip_res.disposition, "skipped");
    assert!(skip_res.diagnostic.is_none());

    // Child and grandchild SessionStart both link onto root lead's children.json.
    for (cid, dir, tx, ts) in [
        ("child-1", &child_dir, &child_tx, "00000000000000000300"),
        ("grand-1", &grand_dir, &grand_tx, "00000000000000000310"),
    ] {
        let ev = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": cid,
                "transcriptPath": tx.to_string_lossy(),
                "artifactDirectoryPath": dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": ""
            }),
            &setup.env,
        );
        assert_eq!(ev.provider_session_id.as_deref(), Some(LEAD));
        assert_eq!(ev.agent_id.as_deref(), Some(cid));
        assert_eq!(setup.apply(&ev, ts).disposition, "applied");
    }
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 2);

    // Child Stop with fullyIdle=false keeps child live while grandchild runs.
    let child_pause = parse_provider_event(
        "agy",
        "Stop",
        &json!({
            "hookEventName": "Stop",
            "conversationId": "child-1",
            "artifactDirectoryPath": child_dir.to_string_lossy(),
            "lastUserInput": "",
            "terminationReason": "NO_TOOL_CALL",
            "fullyIdle": false
        }),
        &setup.env,
    );
    setup.apply(&child_pause, "00000000000000000320");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 2);

    // Child asks a question -> pane shows notify and child is waiting.
    let child_ask = parse_provider_event(
        "agy",
        "PreToolUse",
        &json!({
            "hookEventName": "PreToolUse",
            "conversationId": "child-1",
            "artifactDirectoryPath": child_dir.to_string_lossy(),
            "lastUserInput": "",
            "toolCall": {"name": "ask_question", "id": "child-q1"}
        }),
        &setup.env,
    );
    setup.apply(&child_ask, "00000000000000000330");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // Lead PostToolUse(manage_subagents, kill child-1) stops child-1 AND
    // releases its held notify so lead resumes thinking!
    let kill_ev = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "call-kill",
                "args": {"Action": "kill", "ConversationIds": ["child-1"]}
            },
            "error": ""
        }),
    );
    setup.apply(&kill_ev, "00000000000000000350");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);
    assert_eq!(children["live"][0]["agent_id"], "grand-1");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    // Lead terminal Stop(fullyIdle=true) reconciles any remaining grandchild.
    let lead_stop = event(
        "agy",
        "Stop",
        LEAD,
        json!({
            "terminationReason": "NO_TOOL_CALL",
            "fullyIdle": true,
            "continuationActive": false,
            "executionNum": 0
        }),
    );
    setup.apply(&lead_stop, "00000000000000000400");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 0);
}

#[cfg(target_os = "macos")]
fn run_sqlite(db: &Path, sql: &str) {
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let out = Command::new("/usr/bin/sqlite3")
        .arg(db)
        .arg(sql)
        .output()
        .expect("run sqlite3");
    assert!(
        out.status.success(),
        "sqlite3 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn agy_turn_follower_records_esc_from_working_to_idle_and_canceled_step() {
    // 1. Working -> Idle (without last_modified_time change) clears thinking.
    {
        let mut setup = Setup::new();
        setup.claim();
        let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
        write_jsonl(
            &lead_tx,
            &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"})],
        );
        let env = setup.naming(AGENT_PID);
        let start = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "hi"
            }),
            &env,
        );
        assert_eq!(
            apply_as(&setup, &env, &start, "00000000000000000200").disposition,
            "applied"
        );
        let pre = parse_provider_event(
            "agy",
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "hi",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
            &env,
        );
        assert_eq!(
            apply_as(&setup, &env, &pre, "00000000000000000300").disposition,
            "applied"
        );
        let req = last_request(&setup);
        let FollowedTurn::Agy(AgyTurn {
            ref summaries_db,
            ref conv_db,
            initial_step_count,
            ..
        }) = req.turn
        else {
            panic!("expected AgyTurn follower request: {req:?}");
        };
        assert_eq!(initial_step_count, 1);
        let summaries = PathBuf::from(summaries_db);
        let conv = PathBuf::from(conv_db.as_ref().unwrap());
        run_sqlite(
            &summaries,
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
             INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_RUNNING', 1);",
        );
        run_sqlite(
            &conv,
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);",
        );

        setup.clock = FixedClock {
            monotonic: "00000000000000000900",
            unix: "00000000099999999900",
        };
        let polls = Arc::new(AtomicUsize::new(0));
        let sum_clone = summaries.clone();
        *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
            if pid != AGENT_PID {
                return None;
            }
            let n = polls.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                run_sqlite(
                    &sum_clone,
                    "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='agy-lead-1';",
                );
            } else if n > 4 {
                return Some(ProcessRead::Gone);
            }
            None
        }));
        let followed = follow_every(&setup, &req, Duration::from_millis(10));
        *setup.processes.on_read.lock().unwrap() = None;
        assert_eq!(recorded(&followed), "applied");
        assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
        let snapshot: Value = serde_json::from_slice(
            &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
        )
        .unwrap();
        let interrupt = snapshot["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "user_interrupt")
            .unwrap();
        assert_eq!(interrupt["source_event"], "agy_status");
    }

    // 2. Canceled step at idx >= initial_step_count fires even when summaries
    // stayed IDLE; canceled step from earlier turn (< initial_step_count) or
    // status=9 (ask_question WAITING) does NOT fire.
    {
        let setup = Setup::new();
        setup.claim();
        let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
        write_jsonl(
            &lead_tx,
            &[
                json!({"step_index": 3, "type": "USER_INPUT", "status": "DONE", "content": "turn 2"}),
            ],
        );
        let env = setup.naming(AGENT_PID);
        let start = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "turn 2"
            }),
            &env,
        );
        apply_as(&setup, &env, &start, "00000000000000000200");
        let pre = parse_provider_event(
            "agy",
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "turn 2",
                "invocationNum": 0,
                "initialNumSteps": 4
            }),
            &env,
        );
        apply_as(&setup, &env, &pre, "00000000000000000300");
        let req = last_request(&setup);
        let FollowedTurn::Agy(AgyTurn {
            ref summaries_db,
            ref conv_db,
            ..
        }) = req.turn
        else {
            panic!("expected AgyTurn");
        };
        let summaries = PathBuf::from(summaries_db);
        let conv = PathBuf::from(conv_db.as_ref().unwrap());
        run_sqlite(
            &summaries,
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
             INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_IDLE', 0);",
        );
        // Step 2 (older turn) is CANCELED (6), step 4 (this turn) is WAITING (9).
        run_sqlite(
            &conv,
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);
             INSERT INTO steps VALUES (2, 6);
             INSERT INTO steps VALUES (4, 9);",
        );
        let polls = Arc::new(AtomicUsize::new(0));
        let conv_clone = conv.clone();
        *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
            if pid != AGENT_PID {
                return None;
            }
            let n = polls.fetch_add(1, Ordering::SeqCst);
            if n == 2 {
                // Now user hits Esc on step 4 -> status flips 9 -> 6.
                run_sqlite(&conv_clone, "UPDATE steps SET status=6 WHERE idx=4;");
            } else if n > 5 {
                return Some(ProcessRead::Gone);
            }
            None
        }));
        let followed = follow_every(&setup, &req, Duration::from_millis(10));
        *setup.processes.on_read.lock().unwrap() = None;
        assert_eq!(recorded(&followed), "applied");
        assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
    }
}

#[test]
fn agy_hook_outcome_reports_observation_kind() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );
    let stop = event(
        "agy",
        "Stop",
        LEAD,
        json!({"terminationReason": "NO_TOOL_CALL", "fullyIdle": true}),
    );
    let outcome = apply_provider_event_with_outcome(
        &stop,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    assert!(outcome.observation_id.is_some());
    assert_eq!(
        outcome.observation_kind.as_deref(),
        Some("response_finished")
    );
}

#[test]
fn agy_stop_ids_conflict_is_atomic_and_self_kill_and_delayed_child_kill_work() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );
    for (cid, ts) in [
        ("c1", "00000000000000000300"),
        ("c2", "00000000000000000350"),
    ] {
        let start = event(
            "agy",
            "SessionStart",
            cid,
            json!({"parentConversationId": LEAD, "agentName": "worker", "lastUserInput": ""}),
        );
        assert_eq!(setup.apply(&start, ts).disposition, "applied");
    }

    // 1. Multi-id StopIds from lead where 2nd id conflicts at equal order
    // leaves children.json completely unchanged (c1 stays live).
    let before_children =
        fs::read_to_string(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    let conflict_kill = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "call-k-conflict",
                "args": {"Action": "kill", "ConversationIds": ["c1", "c2"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&conflict_kill, "00000000000000000350");
    assert_eq!(res.disposition, "partial", "{res:?}");
    let after_children =
        fs::read_to_string(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(before_children, after_children);

    // 2. Child apply_child with StopIds conflict commits neither transition.
    let child_conflict = event(
        "agy",
        "PostToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {
                "name": "manage_subagents",
                "id": "child-k-conflict",
                "args": {"Action": "kill", "ConversationIds": ["c2"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&child_conflict, "00000000000000000350");
    assert_eq!(res.disposition, "conflict", "{res:?}");
    let after_child_conflict =
        fs::read_to_string(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(before_children, after_child_conflict);

    // 3. Delayed child PostToolUse where caller's own Tool is older than its
    // latest timestamp still applies StopIds for an older target child.
    let c1_newer = event(
        "agy",
        "PreToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "run_command", "id": "c1-run"}
        }),
    );
    assert_eq!(
        setup.apply(&c1_newer, "00000000000000000400").disposition,
        "applied"
    );
    let delayed_kill = event(
        "agy",
        "PostToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {
                "name": "manage_subagents",
                "id": "c1-kill-c2",
                "args": {"Action": "kill", "ConversationIds": ["c2"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&delayed_kill, "00000000000000000380");
    assert_eq!(res.disposition, "applied", "{res:?}");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);
    assert_eq!(children["live"][0]["agent_id"], "c1");

    // 4. Child self-kill removes itself cleanly without self-conflicting.
    let self_kill = event(
        "agy",
        "PostToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {
                "name": "manage_subagents",
                "id": "c1-self-kill",
                "args": {"Action": "kill", "ConversationIds": ["c1"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&self_kill, "00000000000000000450");
    assert_eq!(res.disposition, "applied", "{res:?}");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 0);
}

#[test]
fn agy_sequential_waiting_child_kills_and_delayed_kills_preserve_notify_semantics() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );
    for (cid, ts, expected) in [
        ("w1", "00000000000000000310", "applied"),
        ("w2", "00000000000000000320", "skipped"),
    ] {
        let ask = event(
            "agy",
            "PreToolUse",
            cid,
            json!({
                "parentConversationId": LEAD,
                "agentName": "worker",
                "lastUserInput": "",
                "toolCall": {"name": "ask_question", "id": format!("q-{cid}")}
            }),
        );
        assert_eq!(setup.apply(&ask, ts).disposition, expected);
    }
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 2);
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // Kill w1 first: w1 must be removed from children.json while w2 still holds notify.
    let kill_w1 = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "kill-w1",
                "args": {"Action": "kill", "ConversationIds": ["w1"]}
            },
            "error": ""
        }),
    );
    let res1 = setup.apply(&kill_w1, "00000000000000000350");
    assert_eq!(res1.disposition, "applied", "{res1:?}");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);
    assert_eq!(children["live"][0]["agent_id"], "w2");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // Kill w2 second: clears w2 and releases notify back to thinking.
    let kill_w2 = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "kill-w2",
                "args": {"Action": "kill", "ConversationIds": ["w2"]}
            },
            "error": ""
        }),
    );
    let res2 = setup.apply(&kill_w2, "00000000000000000360");
    assert_eq!(res2.disposition, "applied", "{res2:?}");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 0);
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    // Newer waiting child w3 at t=460 must survive both delayed kill and delayed kill_all at t=450.
    let ask_w3 = event(
        "agy",
        "PreToolUse",
        "w3",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "ask_question", "id": "q-w3"}
        }),
    );
    assert_eq!(
        setup.apply(&ask_w3, "00000000000000000460").disposition,
        "applied"
    );
    let delayed_kill_w3 = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "delayed-kill-w3",
                "args": {"Action": "kill", "ConversationIds": ["w3"]}
            },
            "error": ""
        }),
    );
    setup.apply(&delayed_kill_w3, "00000000000000000450");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);

    let delayed_kill_all = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "delayed-kill-all",
                "args": {"Action": "kill_all"}
            },
            "error": ""
        }),
    );
    setup.apply(&delayed_kill_all, "00000000000000000450");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);
}

#[test]
fn agy_malformed_post_tool_error_generic_child_post_tool_and_grandchild_parent_id() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );

    // 1. Malformed non-string PostToolUse.error fails closed without killing subagents.
    let child_ask = event(
        "agy",
        "PreToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "ask_question", "id": "q-1"}
        }),
    );
    assert_eq!(
        setup.apply(&child_ask, "00000000000000000300").disposition,
        "applied"
    );
    for args in [
        json!({"Action": "kill", "ConversationIds": ["c1"]}),
        json!({"Action": "kill_all"}),
    ] {
        let bad_kill = event(
            "agy",
            "PostToolUse",
            LEAD,
            json!({
                "toolCall": {"name": "manage_subagents", "id": "bad-err", "args": args},
                "error": 123
            }),
        );
        assert!(bad_kill.stopped_child_ids.is_empty());
        assert!(bad_kill.in_flight_task_ids.is_none());
        assert_eq!(bad_kill.action, ProviderAction::Observation);
        setup.apply(&bad_kill, "00000000000000000320");
    }
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 1);

    // 2. Generic child PostToolUse (e.g. run_command) stays Observation and
    // preserves child Waiting state so lead PreToolUse cannot clear notify.
    let generic_child_post = event(
        "agy",
        "PostToolUse",
        "c1",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "run_command", "id": "run-1"},
            "error": ""
        }),
    );
    assert_eq!(generic_child_post.action, ProviderAction::Observation);
    setup.apply(&generic_child_post, "00000000000000000340");
    let lead_tool = event(
        "agy",
        "PreToolUse",
        LEAD,
        json!({"toolCall": {"name": "run_command", "id": "lead-run"}}),
    );
    setup.apply(&lead_tool, "00000000000000000350");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // 3. Grandchild supplying parentConversationId without rootConversationId
    // still walks messages/*.json up to the root lead session id.
    let (child_dir, _) = agy_brain_dir(&setup, "mid-child");
    let (grand_dir, _) = agy_brain_dir(&setup, "leaf-grand");
    let (lead_dir, _) = agy_brain_dir(&setup, LEAD);
    fs::create_dir_all(&lead_dir).unwrap();
    for (dir, sender) in [(&child_dir, LEAD), (&grand_dir, "mid-child")] {
        let mdir = dir.join(".system_generated").join("messages");
        fs::create_dir_all(&mdir).unwrap();
        fs::write(
            mdir.join("0001.json"),
            serde_json::to_vec(&json!({
                "sender": sender,
                "sourceMetadata": {
                    "tool": {
                        "conversationId": sender,
                        "toolCall": {
                            "name": "invoke_subagent",
                            "argumentsJson": "{\"Role\":\"Worker\",\"TypeName\":\"self\"}"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let grand_ev = parse_provider_event(
        "agy",
        "SessionStart",
        &json!({
            "hookEventName": "SessionStart",
            "conversationId": "leaf-grand",
            "parentConversationId": "mid-child",
            "artifactDirectoryPath": grand_dir.to_string_lossy(),
            "lastUserInput": ""
        }),
        &setup.env,
    );
    assert_eq!(grand_ev.provider_session_id.as_deref(), Some(LEAD));
    assert_eq!(grand_ev.agent_id.as_deref(), Some("leaf-grand"));
}

#[test]
fn agy_rejected_transcript_and_corrupt_user_input_fail_closed() {
    use std::os::unix::fs::symlink;

    let setup = Setup::new();
    setup.claim();
    let (_, tx) = agy_brain_dir(&setup, LEAD);
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "turn1"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "PRIOR_REPLY"}),
            json!({"step_index": 2, "type": "SYSTEM_MESSAGE", "status": "DONE", "content": "wakeup"}),
        ],
    );
    let tx_link = setup._scratch.0.join("tx-symlink.jsonl");
    symlink(&tx, &tx_link).unwrap();

    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );

    // Symlinked and rejected relative transcriptPath on PreInvocation(inv=0)
    // must never emit PromptSubmitted or deliver stale lastUserInput.
    for (idx, bad_path) in [
        tx_link.to_string_lossy().into_owned(),
        "relative/transcript.jsonl".to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        let sink = setup._scratch.0.join(format!("bad-tx-delivery-{idx}.json"));
        let consumer = make_executable(
            &setup,
            &format!("bad-tx-consumer-{idx}"),
            &format!("/bin/cat > '{}'", sink.display()),
        );
        let pre = json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": bad_path,
            "cwd": "/tmp/project",
            "lastUserInput": "stale prompt",
            "invocationNum": 0,
            "initialNumSteps": 3
        });
        let out = agy_hook_cli(
            &setup,
            &setup.env,
            "PreInvocation",
            &pre,
            &[
                "--consumer",
                consumer.to_str().unwrap(),
                "--consumer-timeout-ms",
                "5000",
                "--include-prompt",
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let delivery: Value = serde_json::from_slice(&fs::read(&sink).unwrap()).unwrap();
        assert!(delivery.get("observation_id").is_none());
        assert!(delivery.get("observation_kind").is_none());
        assert_eq!(delivery["prompt"], json!({"availability": "unsupported"}));
    }

    // Corrupt USER_INPUT boundary in transcript prevents leaking PRIOR_REPLY on Stop.
    fs::write(
        &tx,
        format!(
            "{}\n{}\n{{\"step_index\":2,\"type\":\"USER_INPUT\",CORRUPT\n{}\n",
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "turn1"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "PRIOR_REPLY"}),
            json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": ""})
        ),
    )
    .unwrap();
    let stop_sink = setup._scratch.0.join("corrupt-stop-delivery.json");
    let stop_consumer = make_executable(
        &setup,
        "corrupt-stop-consumer",
        &format!("/bin/cat > '{}'", stop_sink.display()),
    );
    let stop_payload = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "transcriptPath": tx.to_string_lossy(),
        "cwd": "/tmp/project",
        "lastUserInput": "turn2",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 0
    });
    let stop_out = agy_hook_cli(
        &setup,
        &setup.env,
        "Stop",
        &stop_payload,
        &[
            "--consumer",
            stop_consumer.to_str().unwrap(),
            "--consumer-timeout-ms",
            "5000",
            "--include-reply",
            "--strict",
        ],
    );
    assert!(
        stop_out.status.success(),
        "{}",
        String::from_utf8_lossy(&stop_out.stderr)
    );
    let stop_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(stop_delivery["reply"], json!({"availability": "absent"}));
}

#[cfg(target_os = "macos")]
#[test]
fn agy_follower_guards_missing_initial_steps_corrupt_conv_db_and_foreign_db() {
    use std::os::unix::fs::symlink;

    // 1. Missing initialNumSteps omits conv_db so an older canceled step
    // cannot falsely fire user_interrupt while summaries says RUNNING.
    {
        let setup = Setup::new();
        setup.claim();
        let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
        write_jsonl(
            &lead_tx,
            &[json!({"step_index": 3, "type": "USER_INPUT", "status": "DONE", "content": "turn2"})],
        );
        let env = setup.naming(AGENT_PID);
        let start = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "turn2"
            }),
            &env,
        );
        apply_as(&setup, &env, &start, "00000000000000000200");
        let pre = parse_provider_event(
            "agy",
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "turn2",
                "invocationNum": 0
            }),
            &env,
        );
        apply_as(&setup, &env, &pre, "00000000000000000300");
        let req = last_request(&setup);
        let FollowedTurn::Agy(AgyTurn {
            ref summaries_db,
            ref conv_db,
            ..
        }) = req.turn
        else {
            panic!("expected AgyTurn");
        };
        assert!(
            conv_db.is_none(),
            "missing initialNumSteps must omit conv_db"
        );
        let summaries = PathBuf::from(summaries_db);
        let conv = setup
            ._scratch
            .0
            .join("app_data")
            .join("conversations")
            .join(format!("{LEAD}.db"));
        run_sqlite(
            &summaries,
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
             INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_RUNNING', 1);",
        );
        run_sqlite(
            &conv,
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);
             INSERT INTO steps VALUES (1, 6);",
        );
        let polls = Arc::new(AtomicUsize::new(0));
        *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
            if pid == AGENT_PID && polls.fetch_add(1, Ordering::SeqCst) >= 3 {
                return Some(ProcessRead::Gone);
            }
            None
        }));
        let followed = follow_every(&setup, &req, Duration::from_millis(10));
        *setup.processes.on_read.lock().unwrap() = None;
        assert!(matches!(
            followed,
            wezterm_attention::turn_follower::Followed::HostGone
        ));
        assert_eq!(
            shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
            "thinking"
        );
    }

    // 2. Corrupt conv_db mid-ask_question after seen_working=true must NOT
    // fall through to summaries IDLE/nfi=0 and falsely interrupt the wait.
    {
        let setup = Setup::new();
        setup.claim();
        let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
        write_jsonl(
            &lead_tx,
            &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "ask"})],
        );
        let env = setup.naming(AGENT_PID);
        let start = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "ask"
            }),
            &env,
        );
        apply_as(&setup, &env, &start, "00000000000000000200");
        let pre = parse_provider_event(
            "agy",
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "ask",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
            &env,
        );
        apply_as(&setup, &env, &pre, "00000000000000000300");
        let req = last_request(&setup);
        let FollowedTurn::Agy(AgyTurn {
            ref summaries_db,
            ref conv_db,
            ..
        }) = req.turn
        else {
            panic!("expected AgyTurn");
        };
        let summaries = PathBuf::from(summaries_db);
        let conv = PathBuf::from(conv_db.as_ref().unwrap());
        run_sqlite(
            &summaries,
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
             INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_IDLE', 0);",
        );
        run_sqlite(
            &conv,
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);
             INSERT INTO steps VALUES (1, 9);",
        );
        let polls = Arc::new(AtomicUsize::new(0));
        let conv_clone = conv.clone();
        *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
            if pid != AGENT_PID {
                return None;
            }
            let n = polls.fetch_add(1, Ordering::SeqCst);
            if n == 1 {
                fs::write(&conv_clone, b"corrupt-sqlite").unwrap();
            } else if n >= 4 {
                return Some(ProcessRead::Gone);
            }
            None
        }));
        let followed = follow_every(&setup, &req, Duration::from_millis(10));
        *setup.processes.on_read.lock().unwrap() = None;
        assert!(matches!(
            followed,
            wezterm_attention::turn_follower::Followed::HostGone
        ));
        assert_eq!(
            shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
            "thinking"
        );
    }

    // 3. Symlinked summaries_db exits immediately with AgyStatusRefused(Foreign).
    {
        let setup = Setup::new();
        setup.claim();
        let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
        write_jsonl(
            &lead_tx,
            &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"})],
        );
        let env = setup.naming(AGENT_PID);
        let start = parse_provider_event(
            "agy",
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "hi"
            }),
            &env,
        );
        apply_as(&setup, &env, &start, "00000000000000000200");
        let pre = parse_provider_event(
            "agy",
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": lead_tx.to_string_lossy(),
                "artifactDirectoryPath": lead_dir.to_string_lossy(),
                "cwd": "/tmp/project",
                "lastUserInput": "hi",
                "invocationNum": 0
            }),
            &env,
        );
        apply_as(&setup, &env, &pre, "00000000000000000300");
        let req = last_request(&setup);
        let FollowedTurn::Agy(AgyTurn {
            ref summaries_db, ..
        }) = req.turn
        else {
            panic!("expected AgyTurn");
        };
        let summaries = PathBuf::from(summaries_db);
        fs::create_dir_all(summaries.parent().unwrap()).unwrap();
        let real = setup._scratch.0.join("real-summaries.db");
        fs::write(&real, b"").unwrap();
        symlink(&real, &summaries).unwrap();
        let followed = follow_every(&setup, &req, Duration::from_millis(10));
        assert!(matches!(
            followed,
            wezterm_attention::turn_follower::Followed::AgyStatusRefused(
                wezterm_attention::agy_status::Unreadable::Foreign
            )
        ));
    }
}

#[test]
fn agy_delayed_self_kill_ignores_empty_transcript_fails_closed_and_delayed_lead_kill_runs_past_unheld_notify()
 {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );

    // 1. Delayed child PostToolUse self-kill returns Ignored (not Skipped)
    // matching ChildTransition::Stop parity.
    let start_c = event(
        "agy",
        "SessionStart",
        "self-c",
        json!({"parentConversationId": LEAD, "agentName": "worker", "lastUserInput": ""}),
    );
    assert_eq!(
        setup.apply(&start_c, "00000000000000000300").disposition,
        "applied"
    );
    let delayed_self_kill = event(
        "agy",
        "PostToolUse",
        "self-c",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {
                "name": "manage_subagents",
                "id": "self-kill-old",
                "args": {"Action": "kill", "ConversationIds": ["self-c"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&delayed_self_kill, "00000000000000000250");
    assert_eq!(res.disposition, "ignored", "{res:?}");

    // 2. Empty-string transcriptPath on lead PreInvocation(inv=0) fails closed
    // (no PromptSubmitted observation, dropped_fields diagnostic reported).
    let empty_tx_pre = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": "",
            "cwd": "/tmp/project",
            "lastUserInput": "stale prompt",
            "invocationNum": 0
        }),
        &setup.env,
    );
    assert!(empty_tx_pre.observation.is_none());
    assert!(empty_tx_pre.diagnostic.is_some());

    // 3. Delayed lead manage_subagents kill for child A (t=400) still removes A
    // even when child B's newer notify (t=500) is no longer held (B answered at t=550).
    let start_a = event(
        "agy",
        "SessionStart",
        "child-a",
        json!({"parentConversationId": LEAD, "agentName": "worker", "lastUserInput": ""}),
    );
    assert_eq!(
        setup.apply(&start_a, "00000000000000000350").disposition,
        "applied"
    );
    let ask_b = event(
        "agy",
        "PreToolUse",
        "child-b",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "ask_question", "id": "q-b"}
        }),
    );
    assert_eq!(
        setup.apply(&ask_b, "00000000000000000500").disposition,
        "applied"
    );
    let answer_b = event(
        "agy",
        "PostToolUse",
        "child-b",
        json!({
            "parentConversationId": LEAD,
            "agentName": "worker",
            "lastUserInput": "",
            "toolCall": {"name": "ask_question", "id": "q-b"},
            "error": ""
        }),
    );
    assert_eq!(
        setup.apply(&answer_b, "00000000000000000550").disposition,
        "applied"
    );
    let delayed_kill_a = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "kill-a-delayed",
                "args": {"Action": "kill", "ConversationIds": ["child-a"]}
            },
            "error": ""
        }),
    );
    let res = setup.apply(&delayed_kill_a, "00000000000000000400");
    assert_eq!(res.disposition, "applied", "{res:?}");
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    let live_ids: Vec<&str> = children["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["agent_id"].as_str().unwrap())
        .collect();
    assert!(!live_ids.contains(&"child-a"), "{live_ids:?}");
    assert!(live_ids.contains(&"child-b"), "{live_ids:?}");
    // Newer notify at t=500 stays preserved.
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );

    // 4. USER_CANCELED Stop reconciles live children to empty while clearing activity.
    let cancel = event(
        "agy",
        "Stop",
        LEAD,
        json!({"terminationReason": "USER_CANCELED", "fullyIdle": true, "executionId": "turn-c"}),
    );
    assert_eq!(
        setup.apply(&cancel, "00000000000000000600").disposition,
        "applied"
    );
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 0);

    // 5. Lazy binding refusal under a shell claim without host proof when
    // another binding is already active aborts without replacing the binding.
    let (_, other_tx) = agy_brain_dir(&setup, "other-session");
    write_jsonl(
        &other_tx,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "other"})],
    );
    let other_pre = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "other-session",
            "transcriptPath": other_tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "other",
            "invocationNum": 0
        }),
        &setup.env,
    );
    let refused = setup.apply(&other_pre, "00000000000000000700");
    assert_eq!(refused.disposition, "conflict", "{refused:?}");
    assert_eq!(
        refused.diagnostic.as_ref().map(|d| d.code.as_str()),
        Some("binding_conflict")
    );
}

#[test]
fn claude_stop_hold_check_still_runs_when_optional_observation_field_is_malformed() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "c-hold",
            json!({"source": "startup"}),
        ),
        "00000000000000000200",
    );
    let checker = make_executable(
        &setup,
        "claude-hold-check",
        "/bin/cat >/dev/null\nprintf '{\"hold\":true,\"answer\":\"still_waiting\"}\\n'",
    );
    let check_arg = format!("jev={}", checker.display());
    // Malformed stop_hook_active (non-bool) causes observation parsing to fail
    // (event.observation = None), while activity_type is still "stop".
    let stop_payload = json!({
        "hook_event_name": "Stop",
        "session_id": "c-hold",
        "transcript_path": "/tmp/c-hold.jsonl",
        "cwd": "/tmp/project",
        "last_assistant_message": "Waiting for background job",
        "background_tasks": [{"id": "bg-1", "type": "shell", "status": "running", "description": "bg task"}],
        "stop_hook_active": "not-a-bool"
    });
    let mut child = rust_command(&setup)
        .args([
            "hooks",
            "event",
            "claude",
            "Stop",
            "--hold-check",
            &check_arg,
            "--debug",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&stop_payload).unwrap())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let act = shown_activity(&setup.binding_dir("claude", "c-hold")).unwrap();
    assert_eq!(act["type"], "stop");
    assert_eq!(act["hold_notes"]["jev"]["hold"], json!(true));
}

#[cfg(target_os = "macos")]
#[test]
fn agy_follower_clears_esc_during_stop_hook_continuation_without_new_observation() {
    let mut setup = Setup::new();
    setup.claim();
    let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
    write_jsonl(
        &lead_tx,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "cont"})],
    );
    let env = setup.naming(AGENT_PID);
    let start = parse_provider_event(
        "agy",
        "SessionStart",
        &json!({
            "hookEventName": "SessionStart",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "artifactDirectoryPath": lead_dir.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont"
        }),
        &env,
    );
    apply_as(&setup, &env, &start, "00000000000000000200");
    let pre0 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "artifactDirectoryPath": lead_dir.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont",
            "invocationNum": 0,
            "initialNumSteps": 1
        }),
        &env,
    );
    apply_as(&setup, &env, &pre0, "00000000000000000300");
    let req = last_request(&setup);

    // Normal Stop ends attempt 1 (ResponseFinished at t=400).
    let stop1 = parse_provider_event(
        "agy",
        "Stop",
        &json!({
            "hookEventName": "Stop",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont",
            "terminationReason": "NO_TOOL_CALL",
            "fullyIdle": true,
            "continuationActive": false,
            "executionNum": 0
        }),
        &env,
    );
    apply_as(&setup, &env, &stop1, "00000000000000000400");

    // Stop hook continuation resumes the same turn with PreInvocation(inv=1),
    // writing thinking at t=500 with NO lead observation.
    let pre1 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont",
            "invocationNum": 1,
            "initialNumSteps": 2
        }),
        &env,
    );
    assert!(pre1.observation.is_none());
    apply_as(&setup, &env, &pre1, "00000000000000000500");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    let FollowedTurn::Agy(AgyTurn {
        ref summaries_db,
        ref conv_db,
        ..
    }) = req.turn
    else {
        panic!("expected AgyTurn");
    };
    let summaries = PathBuf::from(summaries_db);
    let conv = PathBuf::from(conv_db.as_ref().unwrap());
    run_sqlite(
        &summaries,
        "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle INTEGER);
         INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_IDLE', 0);",
    );
    run_sqlite(
        &conv,
        "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);
         INSERT INTO steps VALUES (2, 6);",
    );
    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    let followed = follow_every(&setup, &req, Duration::from_millis(10));
    assert_eq!(recorded(&followed), "applied");
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
}
