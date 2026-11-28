//! Antigravity CLI (`agy`) lifecycle, subagent lineage, quiet checks,
//! transient prompt/reply delivery, `--resume` lazy binding, clear replay
//! guard, and Esc status-database turn follower integration tests.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::self_claim::{AGENT_LAUNCH, apply_as, install, self_owned_claim, stored_claim};
#[cfg(target_os = "macos")]
use super::turn_follower::{follow_every, last_request, recorded};
use super::*;
use wezterm_attention::consumer::{NotDispatchedReason, delivery_bytes};
use wezterm_attention::hook_content::HookContent;
use wezterm_attention::lifecycle::apply_provider_event_with_outcome;
use wezterm_attention::lifecycle::outcome::Persistence;
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

fn link_agy_child(setup: &Setup, parent: &str, child: &str, role: &str) -> String {
    let brain = setup._scratch.0.join("app_data").join("brain");
    write_agy_child_link(&brain, parent, child, role, "self")
        .to_string_lossy()
        .into_owned()
}

fn agy_hook_cli(
    setup: &Setup,
    event_name: &str,
    payload: &Value,
    extra: &[&str],
) -> std::process::Output {
    let mut args = vec!["hooks", "event", "agy", event_name];
    args.extend_from_slice(extra);
    run_hook(setup, &args, payload)
}

fn run_cli_with_stdin(
    setup: &Setup,
    args: &[&str],
    stdin: &Value,
) -> (std::process::Output, Value) {
    let out = run_hook(setup, args, stdin);
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
    let start_out = agy_hook_cli(&setup, "SessionStart", &start_payload, &["--strict"]);
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
        "executionId": "exec-1",
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
        "executionId": "exec-1",
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
    let stop_flags = [
        "--consumer",
        stop_consumer.to_str().unwrap(),
        "--consumer-timeout-ms",
        "5000",
        "--include-reply",
        "--strict",
    ];
    let stop_out = agy_hook_cli(&setup, "Stop", &stop_payload, &stop_flags);
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

    // After a newer turn writes a newer reply, replaying Turn 1's Stop keeps
    // its original observation_id and suppresses transcript fallback (while
    // populated finalModelOutput still delivers); a late Stop for an earlier
    // executionId also reports absent.
    let turn1_stop_id = stop_delivery["observation_id"].as_str().unwrap().to_owned();
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "All 67 tests pass."}),
            json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "content": "Next turn"}),
        ],
    );
    let prompts_path = setup.binding_dir("agy", LEAD).join("prompts.json");
    let turn1_prompts_bytes = fs::read(&prompts_path).unwrap();
    let mut turn2_pre = pre_inv.clone();
    turn2_pre["executionId"] = json!("exec-2");
    turn2_pre["lastUserInput"] = json!("Next turn");
    turn2_pre["initialNumSteps"] = json!(3);
    assert!(
        agy_hook_cli(&setup, "PreInvocation", &turn2_pre, &["--strict"])
            .status
            .success()
    );
    // Simulate Turn 2's prompts.json write failing (leaving Turn 1 in
    // prompts.json while Turn 2 reached lifecycle.json): subsequent writes must
    // heal Turn 2 into prompts.json before tool traffic can evict Turn 2's
    // prompt from lifecycle.json.
    fs::write(&prompts_path, &turn1_prompts_bytes).unwrap();
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "All 67 tests pass."}),
            json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "content": "Next turn"}),
            json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "Next turn reply."}),
        ],
    );
    assert!(
        agy_hook_cli(&setup, "Stop", &stop_payload, &stop_flags)
            .status
            .success()
    );
    let replay_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(replay_delivery["observation_id"], turn1_stop_id);
    assert_eq!(replay_delivery["reply"], json!({"availability": "absent"}));

    let mut fwd_stop = stop_payload.clone();
    fwd_stop["finalModelOutput"] = json!("Direct payload reply");
    assert!(
        agy_hook_cli(&setup, "Stop", &fwd_stop, &stop_flags)
            .status
            .success()
    );
    let fwd_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(fwd_delivery["observation_id"], turn1_stop_id);
    assert_eq!(
        fwd_delivery["reply"],
        json!({"availability": "available", "text": "Direct payload reply"})
    );

    // A delayed PostToolUse for Turn 1 arriving after Turn 2's prompt must not
    // let Turn 1's Stop or an anonymous Stop claim Turn 2, flip thinking to
    // stop, or forward Turn 2's reply.
    let delayed_turn1_tool = json!({
        "hookEventName": "PostToolUse",
        "conversationId": LEAD,
        "executionId": "exec-1",
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "Summarize the tests",
        "toolCall": {"name": "run_command", "id": "delayed-t1"},
        "error": ""
    });
    assert!(
        agy_hook_cli(&setup, "PostToolUse", &delayed_turn1_tool, &["--strict"])
            .status
            .success()
    );

    let mut stale_stop = stop_payload.clone();
    stale_stop["executionId"] = json!("exec-stale");
    stale_stop["executionNum"] = json!(0);
    let _ = fs::remove_file(&stop_sink);
    let stale_out = agy_hook_cli(&setup, "Stop", &stale_stop, &stop_flags);
    assert!(!stale_out.status.success());
    assert!(
        String::from_utf8_lossy(&stale_out.stderr)
            .contains("turn end was superseded by newer native state")
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert!(!stop_sink.exists());

    let mut anon_stop = stop_payload.clone();
    anon_stop.as_object_mut().unwrap().remove("executionId");
    anon_stop["executionNum"] = json!(0);
    let anon_out = agy_hook_cli(&setup, "Stop", &anon_stop, &stop_flags);
    assert!(!anon_out.status.success());
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert!(!stop_sink.exists());

    // Evict Turn 1's Stop and Turn 2's PromptSubmitted from the 64-entry
    // general pool with Turn 2 tool traffic: even after a delayed Turn 1
    // PostToolUse, replaying Turn 1's Stop is superseded, keeps Turn 2
    // thinking, and suppresses consumer delivery while real Turn 2's Stop
    // positively matches Turn 2's prompt record; after removing lifecycle.json,
    // replaying Turn 1 also stays superseded and suppresses delivery.
    let before_evict: LifecycleSnapshot = serde_json::from_slice(
        &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
    )
    .unwrap();
    let base_mono: u128 = before_evict
        .pools
        .general
        .observations
        .iter()
        .map(|item| item.observed_mono_ns.parse::<u128>().unwrap())
        .max()
        .unwrap();
    for step in 0..33u128 {
        let call_id = format!("evict-{step}");
        for (ev_name, offset) in [("PreToolUse", step * 2 + 1), ("PostToolUse", step * 2 + 2)] {
            let tool_ev = parse_provider_event(
                "agy",
                ev_name,
                &json!({
                    "hookEventName": ev_name,
                    "conversationId": LEAD,
                    "executionId": "exec-2",
                    "transcriptPath": tx_str,
                    "cwd": "/tmp/project",
                    "lastUserInput": "Next turn",
                    "toolCall": {"name": "run_command", "id": call_id},
                    "error": ""
                }),
                &setup.env,
            );
            apply_as(
                &setup,
                &setup.env,
                &tool_ev,
                &format!("{:020}", base_mono + offset),
            );
        }
    }
    let mut delayed_after_evict = delayed_turn1_tool;
    delayed_after_evict["toolCall"]["id"] = json!("delayed-t1-after-evict");
    assert!(
        agy_hook_cli(&setup, "PostToolUse", &delayed_after_evict, &["--strict"])
            .status
            .success()
    );
    let evicted_out = agy_hook_cli(&setup, "Stop", &stop_payload, &stop_flags);
    assert!(!evicted_out.status.success());
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert!(!stop_sink.exists());

    let mut turn2_stop = stop_payload.clone();
    turn2_stop["executionId"] = json!("exec-2");
    turn2_stop["lastUserInput"] = json!("Next turn");
    turn2_stop["executionNum"] = json!(0);
    assert!(
        agy_hook_cli(&setup, "Stop", &turn2_stop, &stop_flags)
            .status
            .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "stop"
    );
    let turn2_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(
        turn2_delivery["reply"],
        json!({"availability": "available", "text": "Next turn reply."})
    );

    // Same-execution continuation after evicting Turn 2's Stop(0), even if a
    // prior partial write had left prompts.json without last_execution_num:
    // the tool traffic heals last_execution_num into prompts.json before
    // Stop(0) is evicted, so replaying Stop(0) stays superseded and leaves
    // continuation 1 thinking without leaking its reply, while genuine Stop(1)
    // completes and delivers it.
    let prompts_path = setup.binding_dir("agy", LEAD).join("prompts.json");
    let strip_last_execution_num = || {
        let mut prompts_json: Value =
            serde_json::from_slice(&fs::read(&prompts_path).unwrap()).unwrap();
        assert_eq!(prompts_json["last_execution_num"], json!(0));
        prompts_json
            .as_object_mut()
            .unwrap()
            .remove("last_execution_num");
        fs::write(&prompts_path, serde_json::to_vec(&prompts_json).unwrap()).unwrap();
    };
    strip_last_execution_num();

    let mut turn2_cont1 = turn2_pre.clone();
    turn2_cont1["invocationNum"] = json!(1);
    turn2_cont1["initialNumSteps"] = json!(4);
    assert!(
        agy_hook_cli(&setup, "PreInvocation", &turn2_cont1, &["--strict"])
            .status
            .success()
    );
    // PreInvocation continuation heals last_execution_num from lifecycle.json;
    // strip it again to also prove subsequent tool writes heal it before
    // evicting Stop(0) from the bounded pool.
    strip_last_execution_num();
    let before_cont_evict: LifecycleSnapshot = serde_json::from_slice(
        &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
    )
    .unwrap();
    let cont_base_mono: u128 = before_cont_evict
        .pools
        .general
        .observations
        .iter()
        .map(|item| item.observed_mono_ns.parse::<u128>().unwrap())
        .max()
        .unwrap();
    for step in 0..33u128 {
        let call_id = format!("cont-evict-{step}");
        for (ev_name, offset) in [("PreToolUse", step * 2 + 1), ("PostToolUse", step * 2 + 2)] {
            let tool_ev = parse_provider_event(
                "agy",
                ev_name,
                &json!({
                    "hookEventName": ev_name,
                    "conversationId": LEAD,
                    "executionId": "exec-2",
                    "transcriptPath": tx_str,
                    "cwd": "/tmp/project",
                    "lastUserInput": "Next turn",
                    "toolCall": {"name": "run_command", "id": call_id},
                    "error": ""
                }),
                &setup.env,
            );
            apply_as(
                &setup,
                &setup.env,
                &tool_ev,
                &format!("{:020}", cont_base_mono + offset),
            );
        }
    }
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "Summarize the tests"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "All 67 tests pass."}),
            json!({"step_index": 2, "type": "USER_INPUT", "status": "DONE", "content": "Next turn"}),
            json!({"step_index": 3, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "Next turn reply."}),
            json!({"step_index": 4, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "Continuation 1 reply."}),
        ],
    );
    let _ = fs::remove_file(&stop_sink);
    let evicted_same_exec = agy_hook_cli(&setup, "Stop", &turn2_stop, &stop_flags);
    assert!(!evicted_same_exec.status.success());
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert!(!stop_sink.exists());

    let mut turn2_stop_cont1 = turn2_stop.clone();
    turn2_stop_cont1["executionNum"] = json!(1);
    assert!(
        agy_hook_cli(&setup, "Stop", &turn2_stop_cont1, &stop_flags)
            .status
            .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "stop"
    );
    let cont1_delivery: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(
        cont1_delivery["reply"],
        json!({"availability": "available", "text": "Continuation 1 reply."})
    );

    fs::remove_file(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap();
    let _ = fs::remove_file(&stop_sink);
    let rebuilt_out = agy_hook_cli(&setup, "Stop", &stop_payload, &stop_flags);
    assert!(!rebuilt_out.status.success());
    assert!(!stop_sink.exists());
    let rebuilt_same_exec = agy_hook_cli(&setup, "Stop", &turn2_stop_cont1, &stop_flags);
    assert!(!rebuilt_same_exec.status.success());
    assert!(!stop_sink.exists());
    // Even if last_execution_num was also lost before lifecycle.json was
    // deleted, replaying a numbered Stop without invocation evidence stays
    // superseded across repeated deliveries.
    let mut lost_ord_prompts: Value =
        serde_json::from_slice(&fs::read(&prompts_path).unwrap()).unwrap();
    lost_ord_prompts
        .as_object_mut()
        .unwrap()
        .remove("last_execution_num");
    fs::write(
        &prompts_path,
        serde_json::to_vec(&lost_ord_prompts).unwrap(),
    )
    .unwrap();
    let _ = fs::remove_file(setup.binding_dir("agy", LEAD).join("lifecycle.json"));
    let lost_both = agy_hook_cli(&setup, "Stop", &turn2_stop_cont1, &stop_flags);
    assert!(!lost_both.status.success());
    assert!(!stop_sink.exists());
    // Replaying the same Stop deduplicates to the Superseded record with
    // absent reply, and a higher executionNum on the rebuilt snapshot still
    // lacks live turn evidence and stays superseded.
    assert!(
        agy_hook_cli(&setup, "Stop", &turn2_stop_cont1, &stop_flags)
            .status
            .success()
    );
    let lost_replay: Value = serde_json::from_slice(&fs::read(&stop_sink).unwrap()).unwrap();
    assert_eq!(lost_replay["turn_end"]["status"], "superseded");
    assert_eq!(lost_replay["reply"], json!({"availability": "absent"}));
    let _ = fs::remove_file(&stop_sink);
    let mut lost_higher = turn2_stop_cont1.clone();
    lost_higher["executionNum"] = json!(2);
    assert!(
        !agy_hook_cli(&setup, "Stop", &lost_higher, &stop_flags)
            .status
            .success()
    );
    assert!(!stop_sink.exists());

    // Regression: loss -> continuation -> PreToolUse (rebuilding lifecycle.json
    // under the same executionId) -> replay of old Stop(1) must STILL stay
    // superseded, leave thinking intact, and not deliver a reply.
    let mut partial_prompts: Value =
        serde_json::from_slice(&fs::read(&prompts_path).unwrap()).unwrap();
    partial_prompts["last_execution_num"] = json!(0);
    partial_prompts
        .as_object_mut()
        .unwrap()
        .remove("execution_history_lost");
    fs::write(&prompts_path, serde_json::to_vec(&partial_prompts).unwrap()).unwrap();
    let _ = fs::remove_file(setup.binding_dir("agy", LEAD).join("lifecycle.json"));
    let mut turn2_cont2 = turn2_pre.clone();
    turn2_cont2["invocationNum"] = json!(2);
    turn2_cont2["initialNumSteps"] = json!(5);
    assert!(
        agy_hook_cli(&setup, "PreInvocation", &turn2_cont2, &["--strict"])
            .status
            .success()
    );
    assert!(
        agy_hook_cli(
            &setup,
            "PreToolUse",
            &json!({
                "hookEventName": "PreToolUse",
                "conversationId": LEAD,
                "executionId": "exec-2",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "Next turn",
                "toolCall": {"name": "run_command", "id": "rebuilt-tool-1"}
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let rebuilt_prompts: Value = serde_json::from_slice(&fs::read(&prompts_path).unwrap()).unwrap();
    assert_eq!(rebuilt_prompts["execution_history_lost"], json!(true));
    let _ = fs::remove_file(&stop_sink);
    assert!(
        !agy_hook_cli(&setup, "Stop", &turn2_stop_cont1, &stop_flags)
            .status
            .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert!(!stop_sink.exists());

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
        "executionId": "exec-2",
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
        agy_hook_cli(&setup, "SessionStart", &start_payload, &["--strict"])
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

    // 2. First PreToolUse re-runs before_thinking quiet check; a second
    // PreToolUse skips it while preserving the thinking quiet note.
    let tool1 = json!({
        "hookEventName": "PreToolUse",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "toolCall": {"name": "run_command", "id": "call-tool-1"}
    });
    assert!(
        agy_hook_cli(
            &setup,
            "PreToolUse",
            &tool1,
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let tool2 = json!({
        "hookEventName": "PreToolUse",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "toolCall": {"name": "run_command", "id": "call-tool-2"}
    });
    assert!(
        agy_hook_cli(
            &setup,
            "PreToolUse",
            &tool2,
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(act["hold_notes"]["wezpup"]["quiet"], json!(true));

    // 3. Intra-turn PreInvocation(inv=1) and Stop(fullyIdle: false) preserve
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

    // Exactly 3 invocations: PreInvocation(0), first PreToolUse, and normal Stop.
    let logged: Vec<Value> = fs::read_to_string(&log_file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(logged.len(), 3, "{logged:?}");
    assert_eq!(logged[0]["phase"], "before_thinking");
    assert_eq!(logged[0]["source_event"], "PreInvocation");
    assert_eq!(
        logged[0]["prompt"],
        json!({"availability": "available", "text": "auto prompt"})
    );
    assert_eq!(logged[1]["phase"], "before_thinking");
    assert_eq!(logged[1]["source_event"], "PreToolUse");
    assert_eq!(logged[2]["phase"], "before_turn_end");
    assert_eq!(logged[2]["source_event"], "Stop");
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
    let brain = setup._scratch.0.join("app_data").join("brain");
    write_agy_child_link(&brain, LEAD, "child-1", "Worker", "self");
    write_agy_child_link(&brain, "child-1", "grand-1", "Leaf", "research");

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
    let c1_dir = link_agy_child(&setup, LEAD, "c1", "worker");
    let c2_dir = link_agy_child(&setup, LEAD, "c2", "worker");
    for (cid, dir, ts) in [
        ("c1", &c1_dir, "00000000000000000300"),
        ("c2", &c2_dir, "00000000000000000350"),
    ] {
        let start = event(
            "agy",
            "SessionStart",
            cid,
            json!({"artifactDirectoryPath": dir, "lastUserInput": ""}),
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
            "artifactDirectoryPath": c1_dir,
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
            "artifactDirectoryPath": c1_dir,
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
            "artifactDirectoryPath": c1_dir,
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
            "artifactDirectoryPath": c1_dir,
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
        let dir = link_agy_child(&setup, LEAD, cid, "worker");
        let ask = event(
            "agy",
            "PreToolUse",
            cid,
            json!({
                "artifactDirectoryPath": dir,
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
    let w3_dir = link_agy_child(&setup, LEAD, "w3", "worker");
    let ask_w3 = event(
        "agy",
        "PreToolUse",
        "w3",
        json!({
            "artifactDirectoryPath": w3_dir,
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
    let c1_dir = link_agy_child(&setup, LEAD, "c1", "worker");
    let child_ask = event(
        "agy",
        "PreToolUse",
        "c1",
        json!({
            "artifactDirectoryPath": c1_dir,
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
            "artifactDirectoryPath": c1_dir,
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

    // 3. Grandchild walks messages/*.json upward through mid-child to the root lead session id.
    let _mid_dir = link_agy_child(&setup, LEAD, "mid-child", "Worker");
    let grand_dir = link_agy_child(&setup, "mid-child", "leaf-grand", "Worker");
    let grand_ev = parse_provider_event(
        "agy",
        "SessionStart",
        &json!({
            "hookEventName": "SessionStart",
            "conversationId": "leaf-grand",
            "artifactDirectoryPath": grand_dir,
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
    let self_c_dir = link_agy_child(&setup, LEAD, "self-c", "worker");
    let start_c = event(
        "agy",
        "SessionStart",
        "self-c",
        json!({"artifactDirectoryPath": self_c_dir, "lastUserInput": ""}),
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
            "artifactDirectoryPath": self_c_dir,
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
    let child_a_dir = link_agy_child(&setup, LEAD, "child-a", "worker");
    let child_b_dir = link_agy_child(&setup, LEAD, "child-b", "worker");
    let start_a = event(
        "agy",
        "SessionStart",
        "child-a",
        json!({"artifactDirectoryPath": child_a_dir, "lastUserInput": ""}),
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
            "artifactDirectoryPath": child_b_dir,
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
            "artifactDirectoryPath": child_b_dir,
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
    let out = run_hook(
        &setup,
        &[
            "hooks",
            "event",
            "claude",
            "Stop",
            "--hold-check",
            &check_arg,
            "--debug",
        ],
        &stop_payload,
    );
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
            "executionId": "exec-cont",
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
            "executionId": "exec-cont",
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

    // Once Stop(0) lands after invocation 0, follower #1 sees its invocation
    // ended and exits Skipped immediately.
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
         INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_RUNNING', 1);",
    );
    run_sqlite(
        &conv,
        "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);",
    );
    let followed_idle = follow_every(&setup, &req, Duration::from_millis(10));
    assert_eq!(recorded(&followed_idle), "skipped");

    // Stop hook continuation resumes the same turn with PreInvocation(inv=1),
    // writing thinking at t=500 with NO lead observation and spawning a new
    // invocation-scoped follower even while follower #1's lock is still held.
    let (address, _) = pane_address(&env).unwrap();
    let lock_path = wezterm_attention::records::turn_follower_lock(
        &state_root(&env).unwrap(),
        &address,
        &env["WEZTERM_ATTENTION_LAUNCH_ID"],
        &req.binding_id(),
    );
    let wezterm_attention::turn_follower::TakenTurn::Taken(held_old) =
        wezterm_attention::turn_follower::take_turn(
            &lock_path,
            &req.turn.key(),
            Duration::from_millis(10),
        )
        .unwrap()
    else {
        panic!("expected lock");
    };
    let started_before = setup.followers.started.lock().unwrap().len();
    let pre1 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "artifactDirectoryPath": lead_dir.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont",
            "invocationNum": 1,
            "initialNumSteps": 2
        }),
        &env,
    );
    assert!(pre1.observation.is_none());
    apply_as(&setup, &env, &pre1, "00000000000000000500");
    drop(held_old);
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert_eq!(
        setup.followers.started.lock().unwrap().len(),
        started_before + 1
    );
    let req2 = last_request(&setup);
    assert_ne!(req2.turn.key(), req.turn.key());
    let FollowedTurn::Agy(ref turn2) = req2.turn else {
        panic!("expected AgyTurn");
    };
    assert_eq!(turn2.initial_step_count, 2);

    // A tool-loop PreInvocation while follower #2 holds the lock does not spawn
    // a duplicate follower.
    let wezterm_attention::turn_follower::TakenTurn::Taken(held_new) =
        wezterm_attention::turn_follower::take_turn(
            &lock_path,
            &req2.turn.key(),
            Duration::from_millis(10),
        )
        .unwrap()
    else {
        panic!("expected lock");
    };
    let before_loop = setup.followers.started.lock().unwrap().len();
    let pre2 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": lead_tx.to_string_lossy(),
            "artifactDirectoryPath": lead_dir.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "cont",
            "invocationNum": 2,
            "initialNumSteps": 3
        }),
        &env,
    );
    apply_as(&setup, &env, &pre2, "00000000000000000550");
    assert_eq!(setup.followers.started.lock().unwrap().len(), before_loop);
    drop(held_new);

    // A superseded Stop from an older execution persisted during continuation 1
    // must keep thinking and must NOT retire follower #2.
    let stale_stop = parse_provider_event(
        "agy",
        "Stop",
        &json!({
            "hookEventName": "Stop",
            "conversationId": LEAD,
            "executionId": "exec-stale",
            "transcriptPath": lead_tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "old",
            "terminationReason": "NO_TOOL_CALL",
            "fullyIdle": true,
            "continuationActive": false,
            "executionNum": 0
        }),
        &env,
    );
    let stale_res = apply_as(&setup, &env, &stale_stop, "00000000000000000600");
    assert_eq!(stale_res.disposition, "ignored", "{stale_res:?}");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    // An old follower from invocation 0 cannot clear continuation 1's thinking
    // even if it already sees IDLE / canceled state; follower #2 clears Esc.
    run_sqlite(
        &summaries,
        "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='agy-lead-1';",
    );
    run_sqlite(&conv, "INSERT INTO steps VALUES (2, 6);");
    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    let stale_follow = follow_every(&setup, &req, Duration::from_millis(10));
    assert_eq!(recorded(&stale_follow), "skipped");
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );

    let followed = follow_every(&setup, &req2, Duration::from_millis(10));
    assert_eq!(recorded(&followed), "applied");
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn agy_follower_ignores_malformed_summary_and_active_killed_step_then_interrupts_on_valid_idle() {
    let mut setup = Setup::new();
    setup.claim();
    let (lead_dir, lead_tx) = agy_brain_dir(&setup, LEAD);
    write_jsonl(
        &lead_tx,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "work"})],
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
            "lastUserInput": "work"
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
            "lastUserInput": "work",
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
    // Poll 1: earlier background tool step was killed (status=6), later step
    // finished (status=3), and summary reports IDLE with not_fully_idle=1
    // (waiting on background subagents) -> Working (sets seen_working=true,
    // must NOT record user_interrupt).
    run_sqlite(
        &summaries,
        "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, status TEXT, not_fully_idle);
         INSERT INTO conversation_summaries VALUES ('agy-lead-1', 'CASCADE_RUN_STATUS_IDLE', 1);",
    );
    run_sqlite(
        &conv,
        "CREATE TABLE steps (idx INTEGER PRIMARY KEY, status INTEGER);
         INSERT INTO steps VALUES (1, 6), (2, 3);",
    );

    let polls = Arc::new(AtomicUsize::new(0));
    let polls_for_hook = Arc::clone(&polls);
    let summaries_for_hook = summaries.clone();
    let conv_for_hook = conv.clone();
    let binding_dir_for_hook = setup.binding_dir("agy", LEAD);
    *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
        if pid == AGENT_PID {
            let n = polls_for_hook.fetch_add(1, Ordering::SeqCst);
            if n == 2 {
                // Still working after active killed-step poll: activity must stay thinking.
                assert_eq!(
                    shown_activity(&binding_dir_for_hook).unwrap()["type"],
                    "thinking"
                );
                // Switch steps to quiescent DONE and corrupt not_fully_idle to TEXT 'true'.
                run_sqlite(
                    &conv_for_hook,
                    "DELETE FROM steps; INSERT INTO steps VALUES (1, 3), (2, 3);",
                );
                run_sqlite(
                    &summaries_for_hook,
                    "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle='true' WHERE conversation_id='agy-lead-1';",
                );
            } else if n == 4 {
                // Embedded-NUL TEXT status must also fail closed without ending the turn.
                assert_eq!(
                    shown_activity(&binding_dir_for_hook).unwrap()["type"],
                    "thinking"
                );
                run_sqlite(
                    &summaries_for_hook,
                    "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE' || char(0) || 'INVALID', not_fully_idle=0 WHERE conversation_id='agy-lead-1';",
                );
            } else if n == 7 {
                // After malformed summary polls, no interrupt may have been recorded.
                assert_eq!(
                    shown_activity(&binding_dir_for_hook).unwrap()["type"],
                    "thinking"
                );
                // Finally deliver valid IDLE + integer 0 -> follower records user_interrupt.
                run_sqlite(
                    &summaries_for_hook,
                    "UPDATE conversation_summaries SET status='CASCADE_RUN_STATUS_IDLE', not_fully_idle=0 WHERE conversation_id='agy-lead-1';",
                );
            }
        }
        None
    }));

    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    let followed = follow_every(&setup, &req, Duration::from_millis(10));
    assert_eq!(recorded(&followed), "applied");
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
    assert!(polls.load(Ordering::SeqCst) >= 8);
}

#[test]
fn delayed_manage_subagents_kill_over_activity_clear_clears_diagnostic_and_rejects_activity_delivery()
 {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );
    let child_a_dir = link_agy_child(&setup, LEAD, "child-a", "worker");
    let start_a = event(
        "agy",
        "SessionStart",
        "child-a",
        json!({"artifactDirectoryPath": child_a_dir, "lastUserInput": ""}),
    );
    assert_eq!(
        setup.apply(&start_a, "00000000000000000300").disposition,
        "applied"
    );
    // Clear lead activity at t=500 while child-a remains live so the delayed
    // PostToolUse at t=400 is covered by activity-clear.
    let interrupt = ProviderEvent::agy_turn_interrupt(LEAD);
    assert_eq!(
        setup.apply(&interrupt, "00000000000000000500").disposition,
        "applied"
    );

    // Delayed PostToolUse(manage_subagents, kill child-a) at t=400 is covered by
    // the t=500 activity clear, so plan_activity returns Ignored + BindingConflict,
    // while StopIds still removes child-a.
    let delayed_kill = event(
        "agy",
        "PostToolUse",
        LEAD,
        json!({
            "toolCall": {
                "name": "manage_subagents",
                "id": "kill-a-covered",
                "args": {"Action": "kill", "ConversationIds": ["child-a"]}
            },
            "error": ""
        }),
    );
    let outcome = apply_provider_event_with_outcome(
        &delayed_kill,
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    let res = outcome.result.as_ref().expect("applied child cleanup");
    assert_eq!(res.disposition, "applied", "{res:?}");
    assert!(
        res.diagnostic.is_none(),
        "stale binding_conflict diagnostic must not remain on an applied cleanup: {res:?}"
    );
    assert_eq!(outcome.persistence.activity, Persistence::Rejected);
    assert_eq!(outcome.persistence.native_state, Persistence::Rejected);
    assert_eq!(
        delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::NotRequested
        ),
        Err(NotDispatchedReason::NativeStateRejected)
    );
    let children = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children["live"].as_array().unwrap().len(), 0);
}

#[test]
fn agy_resume_self_claim_reuses_same_host_replaces_dead_owner_and_refuses_shell_claim() {
    // 1. Same-host self-owned claim is reused without querying the WezTerm mux.
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let mux_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mux_calls_hook = std::sync::Arc::clone(&mux_calls);
    *setup.panes.on_list.lock().unwrap() = Some(std::sync::Arc::new(move || {
        mux_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    setup.panes.set(None);
    let agent_env = setup.agent_env();
    let (_, tx) = agy_brain_dir(&setup, "resume-same-host");
    write_jsonl(
        &tx,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"})],
    );
    let pre = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "resume-same-host",
            "transcriptPath": tx.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "hi",
            "invocationNum": 0
        }),
        &agent_env,
    );
    let res = apply_as(&setup, &agent_env, &pre, "00000000000000000300");
    assert_eq!(res.disposition, "applied", "{res:?}");
    assert_eq!(mux_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        stored_claim(&setup).unwrap()["launch_id"],
        json!(AGENT_LAUNCH)
    );

    // 2. Dead prior agent's self-owned claim is replaced via mux proof on ClaimStale.
    let setup2 = Setup::new();
    let dead_pid = AGENT_PID + 77;
    setup2.processes.set(
        dead_pid,
        ProcessRead::Found(process_facts(
            dead_pid,
            SHELL_PID,
            ControllingTerminal::Device(PANE_TTY_DEVICE),
        )),
    );
    install(&setup2, &self_owned_claim(&setup2, AGENT_LAUNCH, dead_pid));
    setup2.processes.set(dead_pid, ProcessRead::Gone);
    let agent_env2 = setup2.agent_env();
    let (_, tx2) = agy_brain_dir(&setup2, "resume-dead-owner");
    write_jsonl(
        &tx2,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"})],
    );
    let pre2 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "resume-dead-owner",
            "transcriptPath": tx2.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "hi",
            "invocationNum": 0
        }),
        &agent_env2,
    );
    let res2 = apply_as(&setup2, &agent_env2, &pre2, "00000000000000000400");
    assert_eq!(res2.disposition, "applied", "{res2:?}");
    let replaced_claim = stored_claim(&setup2).unwrap();
    assert_ne!(replaced_claim["launch_id"], json!(AGENT_LAUNCH));
    assert_eq!(replaced_claim["owner_pid"], json!(AGENT_PID.to_string()));

    // 3. Existing shell claim without WEZTERM_ATTENTION_LAUNCH_ID refuses without touching mux.
    let setup3 = Setup::new();
    setup3.claim();
    let mux_calls3 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mux_calls3_hook = std::sync::Arc::clone(&mux_calls3);
    *setup3.panes.on_list.lock().unwrap() = Some(std::sync::Arc::new(move || {
        mux_calls3_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    setup3.panes.set(None);
    let agent_env3 = setup3.agent_env();
    let (_, tx3) = agy_brain_dir(&setup3, "resume-shell-claim");
    write_jsonl(
        &tx3,
        &[json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "hi"})],
    );
    let pre3 = parse_provider_event(
        "agy",
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": "resume-shell-claim",
            "transcriptPath": tx3.to_string_lossy(),
            "cwd": "/tmp/project",
            "lastUserInput": "hi",
            "invocationNum": 0
        }),
        &agent_env3,
    );
    let res3 = apply_as(&setup3, &agent_env3, &pre3, "00000000000000000500");
    assert_eq!(res3.disposition, "ignored", "{res3:?}");
    assert_eq!(mux_calls3.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn agy_user_canceled_stop_commits_clear_but_marks_partial_and_suppresses_delivery_when_children_refused()
 {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("agy", "SessionStart", LEAD, json!({})),
        "00000000000000000200",
    );
    setup.apply(
        &event("agy", "PreInvocation", LEAD, json!({"invocationNum": 1})),
        "00000000000000000300",
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    // Write a future-schema children.json so Reconcile refuses the child set.
    let children_path = setup.binding_dir("agy", LEAD).join("children.json");
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].as_str();
    fs::write(
        &children_path,
        serde_json::to_vec(&json!({
            "kind": "children",
            "schema": 999,
            "binding_id": binding_id("agy", LEAD, launch_id),
            "live": []
        }))
        .unwrap(),
    )
    .unwrap();

    let cancel = event(
        "agy",
        "Stop",
        LEAD,
        json!({"terminationReason": "USER_CANCELED", "fullyIdle": true}),
    );
    let outcome = apply_provider_event_with_outcome(
        &cancel,
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    let res = outcome.result.as_ref().expect("partial clear result");
    assert_eq!(res.disposition, "partial", "{res:?}");
    assert_eq!(
        res.diagnostic.as_ref().map(|d| d.code.as_str()),
        Some("future_schema")
    );
    assert!(shown_activity(&setup.binding_dir("agy", LEAD)).is_none());
    assert_eq!(outcome.persistence.activity, Persistence::Confirmed);
    assert_eq!(outcome.persistence.native_state, Persistence::Rejected);
    assert_eq!(
        delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::NotRequested
        ),
        Err(NotDispatchedReason::NativeStateRejected)
    );
}

#[test]
fn agy_quiet_continuation_wakeup_and_unseen_stop_with_checks_json() {
    let setup = Setup::new();
    setup.claim();
    let (_, tx) = agy_brain_dir(&setup, LEAD);
    let tx_str = tx.to_string_lossy().into_owned();
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "human prompt"}),
        ],
    );

    // 1. Human turn ends with an unread stop.
    assert!(
        agy_hook_cli(
            &setup,
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "human prompt"
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert!(
        agy_hook_cli(
            &setup,
            "Stop",
            &json!({
                "hookEventName": "Stop",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "human prompt",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true,
                "continuationActive": false,
                "executionNum": 0
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let human_stop = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    let human_stop_id = human_stop["event_id"].as_str().unwrap().to_owned();
    assert_eq!(human_stop["type"], "stop");

    // 2. Register quiet checks for agy via checks.json (no CLI --quiet-check flag).
    let check_log = setup._scratch.0.join("agy-checks-json-calls.log");
    let check_log_quoted = format!(
        "'{}'",
        check_log.display().to_string().replace('\'', "'\\''")
    );
    let checker = make_executable(
        &setup,
        "agy-checks-json-quiet",
        &format!(
            "/bin/cat >/dev/null\nprintf 'called\\n' >> {check_log_quoted}\nprintf '{{\"quiet\":true,\"answer\":\"controller_submitted\"}}\\n'",
        ),
    );
    let check_calls = || {
        fs::read_to_string(&check_log)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    let prompt_observations = || {
        let snapshot: Value = serde_json::from_slice(
            &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
        )
        .unwrap();
        snapshot["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"] == "prompt_submitted")
            .count()
    };
    let latest_stop_id = || {
        let snapshot: Value = serde_json::from_slice(
            &fs::read(setup.binding_dir("agy", LEAD).join("lifecycle.json")).unwrap(),
        )
        .unwrap();
        snapshot["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|item| item["kind"] == "response_finished")
            .unwrap()["observation_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let checks_path = PathBuf::from(&setup.env["WEZTERM_ATTENTION_DIR"]).join("checks.json");
    fs::write(
        &checks_path,
        serde_json::to_vec(&json!({
            "schema": 1,
            "checks": [
                {"provider": "agy", "event": "PreInvocation", "kind": "quiet", "name": "wezpup", "executable": checker.to_string_lossy()},
                {"provider": "agy", "event": "PreToolUse", "kind": "quiet", "name": "wezpup", "executable": checker.to_string_lossy()},
                {"provider": "agy", "event": "Stop", "kind": "quiet", "name": "wezpup", "executable": checker.to_string_lossy()}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&checks_path, fs::Permissions::from_mode(0o600)).unwrap();

    // 3. Application turn runs PreInvocation(0) -> PreToolUse -> Stop(exec=0)
    // -> Stop-hook continuation PreInvocation(inv=1) -> PreToolUse -> Stop(exec=1)
    // -> tool-free continuation PreInvocation(inv=2) -> Stop(exec=2);
    // every step lands in its expected activity, stays quiet, and keeps the
    // unread human unseen_stop, and each continuation Stop records its own end.
    let mut stop_ids = Vec::new();
    let stop_exec1 = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "executionId": "exec-auto",
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 1
    });
    let stop_exec2 = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "executionId": "exec-auto",
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": false,
        "executionNum": 2
    });
    for (event_name, expected_type, payload) in [
        (
            "PreInvocation",
            "thinking",
            json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
        ),
        (
            "PreToolUse",
            "thinking",
            json!({
                "hookEventName": "PreToolUse",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "toolCall": {"name": "run_command", "id": "call-1"}
            }),
        ),
        (
            "Stop",
            "stop",
            json!({
                "hookEventName": "Stop",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true,
                "continuationActive": false,
                "executionNum": 0
            }),
        ),
        (
            "PreInvocation",
            "thinking",
            json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "invocationNum": 1,
                "initialNumSteps": 2
            }),
        ),
        (
            "PreToolUse",
            "thinking",
            json!({
                "hookEventName": "PreToolUse",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "toolCall": {"name": "run_command", "id": "call-2"}
            }),
        ),
        ("Stop", "stop", stop_exec1.clone()),
        (
            "PreInvocation",
            "thinking",
            json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "invocationNum": 2,
                "initialNumSteps": 3
            }),
        ),
        // Delayed replay of Stop(exec=1) during continuation 2 must not close
        // continuation 2 or replace Stop(exec=1)'s observation id.
        ("Stop", "thinking", stop_exec1),
        ("Stop", "stop", stop_exec2.clone()),
    ] {
        let out = agy_hook_cli(&setup, event_name, &payload, &["--strict"]);
        assert!(
            out.status.success(),
            "{event_name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
        assert_eq!(act["type"], expected_type, "{event_name}: {act}");
        assert_eq!(
            act["hold_notes"]["wezpup"]["quiet"],
            json!(true),
            "{event_name} lost quiet note: {act}"
        );
        assert_eq!(
            act["unseen_stop"]["event_id"],
            json!(human_stop_id),
            "{event_name} lost unseen_stop: {act}"
        );
        if event_name == "Stop" {
            stop_ids.push(latest_stop_id());
        }
    }
    assert_eq!(stop_ids.len(), 4);
    assert_ne!(stop_ids[0], stop_ids[1]);
    assert_eq!(stop_ids[2], stop_ids[1]);
    assert_ne!(stop_ids[3], stop_ids[1]);
    // Immediate replay of Stop(exec=2) still deduplicates to the stored end.
    assert!(
        agy_hook_cli(&setup, "Stop", &stop_exec2, &["--strict"])
            .status
            .success()
    );
    assert_eq!(latest_stop_id(), stop_ids[3]);

    // 4. Confirmed background wakeup (invocationNum=0, transcript ending in SYSTEM_MESSAGE
    // after a completed PLANNER_RESPONSE) over the quiet stop preserves quiet note and
    // unseen_stop via continues_turn without running before_thinking or appending a prompt.
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "auto prompt"}),
            json!({"step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "content": "working"}),
            json!({"step_index": 2, "type": "SYSTEM_MESSAGE", "status": "DONE", "content": "subagent done"}),
        ],
    );
    let wakeup_payload = json!({
        "hookEventName": "PreInvocation",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "invocationNum": 0,
        "initialNumSteps": 3
    });
    let parsed_wakeup = parse_provider_event("agy", "PreInvocation", &wakeup_payload, &setup.env);
    assert!(parsed_wakeup.continues_turn);
    assert!(!parsed_wakeup.is_lead_prompt);
    assert!(parsed_wakeup.observation.is_none());

    let calls_before_wakeup = check_calls();
    let prompts_before_wakeup = prompt_observations();
    let wakeup_out = agy_hook_cli(&setup, "PreInvocation", &wakeup_payload, &["--strict"]);
    assert!(wakeup_out.status.success());
    assert_eq!(check_calls(), calls_before_wakeup);
    assert_eq!(prompt_observations(), prompts_before_wakeup);
    let wakeup_act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(wakeup_act["type"], "thinking");
    assert_eq!(wakeup_act["hold_notes"]["wezpup"]["quiet"], json!(true));
    assert_eq!(wakeup_act["unseen_stop"]["event_id"], json!(human_stop_id));

    // 4b. Lead PostToolUse for manage_subagents kill and kill_all during a quiet
    // turn preserves quiet hold_notes and unseen_stop while stopping the children.
    let quiet_w1_dir = link_agy_child(&setup, LEAD, "quiet-w1", "worker-1");
    let quiet_w2_dir = link_agy_child(&setup, LEAD, "quiet-w2", "worker-2");
    for (child_id, child_dir) in [("quiet-w1", &quiet_w1_dir), ("quiet-w2", &quiet_w2_dir)] {
        assert!(
            agy_hook_cli(
                &setup,
                "SessionStart",
                &json!({
                    "hookEventName": "SessionStart",
                    "conversationId": child_id,
                    "artifactDirectoryPath": child_dir,
                    "cwd": "/tmp/project"
                }),
                &["--strict"],
            )
            .status
            .success()
        );
    }
    let children_before = read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children_before["live"].as_array().unwrap().len(), 2);

    let kill_one_payload = json!({
        "hookEventName": "PostToolUse",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "toolCall": {
            "name": "manage_subagents",
            "id": "call-kill-1",
            "args": {"Action": "kill", "ConversationIds": ["quiet-w1"]}
        },
        "error": ""
    });
    let kill_all_payload = json!({
        "hookEventName": "PostToolUse",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "toolCall": {
            "name": "manage_subagents",
            "id": "call-kill-all",
            "args": {"Action": "kill_all"}
        },
        "error": ""
    });
    let ask_done_payload = json!({
        "hookEventName": "PostToolUse",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "toolCall": {
            "name": "ask_question",
            "id": "call-ask-done",
            "args": {"questions": []}
        },
        "error": ""
    });
    assert!(
        parse_provider_event("agy", "PostToolUse", &kill_one_payload, &setup.env).continues_turn
    );
    assert!(
        parse_provider_event("agy", "PostToolUse", &kill_all_payload, &setup.env).continues_turn
    );
    assert!(
        !parse_provider_event("agy", "PostToolUse", &ask_done_payload, &setup.env).continues_turn
    );

    assert!(
        agy_hook_cli(&setup, "PostToolUse", &kill_one_payload, &["--strict"],)
            .status
            .success()
    );
    assert_eq!(check_calls(), calls_before_wakeup);
    let after_kill_one = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(after_kill_one["type"], "thinking");
    assert_eq!(after_kill_one["hold_notes"]["wezpup"]["quiet"], json!(true));
    assert_eq!(
        after_kill_one["unseen_stop"]["event_id"],
        json!(human_stop_id)
    );
    let children_after_one =
        read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children_after_one["live"].as_array().unwrap().len(), 1);
    assert_eq!(children_after_one["live"][0]["agent_id"], "quiet-w2");

    assert!(
        agy_hook_cli(&setup, "PostToolUse", &kill_all_payload, &["--strict"],)
            .status
            .success()
    );
    assert_eq!(check_calls(), calls_before_wakeup);
    let after_kill_all = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(after_kill_all["type"], "thinking");
    assert_eq!(after_kill_all["hold_notes"]["wezpup"]["quiet"], json!(true));
    assert_eq!(
        after_kill_all["unseen_stop"]["event_id"],
        json!(human_stop_id)
    );
    let children_after_all =
        read_json(setup.binding_dir("agy", LEAD).join("children.json")).unwrap();
    assert_eq!(children_after_all["live"].as_array().unwrap().len(), 0);

    // Acknowledging the unseen_stop event_id succeeds while carried under quiet thinking.
    let (address, _) = pane_address(&setup.env).unwrap();
    let ack = wezterm_attention::lifecycle::acknowledge_activity(
        &state_root(&setup.env).unwrap(),
        &address,
        &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        &human_stop_id,
    )
    .unwrap();
    assert_eq!(ack.disposition.as_str(), "applied");

    // 5. Fail-closed invocationNum=0 with an unreadable/missing transcriptPath must
    // NOT inherit quiet notes from a prior quiet stop.
    assert!(
        agy_hook_cli(
            &setup,
            "Stop",
            &json!({
                "hookEventName": "Stop",
                "conversationId": LEAD,
                "executionId": "exec-auto",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true,
                "continuationActive": false,
                "executionNum": 3
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let unreadable_out = agy_hook_cli(
        &setup,
        "PreInvocation",
        &json!({
            "hookEventName": "PreInvocation",
            "conversationId": LEAD,
            "transcriptPath": "/nonexistent/agy/transcript_full.jsonl",
            "cwd": "/tmp/project",
            "lastUserInput": "new human prompt",
            "invocationNum": 0,
            "initialNumSteps": 3
        }),
        &["--strict"],
    );
    assert!(unreadable_out.status.success());
    let unreadable_act = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(unreadable_act["type"], "thinking");
    assert!(
        unreadable_act.get("hold_notes").is_none(),
        "{unreadable_act}"
    );
    assert!(
        unreadable_act.get("unseen_stop").is_none(),
        "{unreadable_act}"
    );

    // Genuine no-ID turn (PreInvocation(0) with valid USER_INPUT transcript and
    // no executionId) still transitions from thinking to stop on its no-ID Stop.
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "anonymous prompt"}),
        ],
    );
    assert!(
        agy_hook_cli(
            &setup,
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "anonymous prompt",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert!(
        agy_hook_cli(
            &setup,
            "Stop",
            &json!({
                "hookEventName": "Stop",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "anonymous prompt",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true,
                "continuationActive": false,
                "executionNum": 0
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "stop"
    );

    // 6. Omitted-executionNum replay (continuationActive: true) must not reopen
    // on PreInvocation or a delayed PostToolUse, and a stale-execution Stop must
    // not run checks.json Stop checks.
    assert!(
        agy_hook_cli(
            &setup,
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "executionId": "exec-no-ord",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let no_ord_stop = json!({
        "hookEventName": "Stop",
        "conversationId": LEAD,
        "executionId": "exec-no-ord",
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "auto prompt",
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": true,
        "continuationActive": true
    });
    assert!(
        agy_hook_cli(&setup, "Stop", &no_ord_stop, &["--strict"])
            .status
            .success()
    );
    let no_ord_id = latest_stop_id();
    assert!(
        agy_hook_cli(
            &setup,
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "executionId": "exec-no-ord",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "invocationNum": 1,
                "initialNumSteps": 2
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert!(
        agy_hook_cli(
            &setup,
            "PostToolUse",
            &json!({
                "hookEventName": "PostToolUse",
                "conversationId": LEAD,
                "executionId": "exec-no-ord",
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "auto prompt",
                "toolCall": {"name": "run_command", "id": "delayed-no-ord"},
                "error": ""
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let calls_before_stale = check_calls();
    assert!(
        agy_hook_cli(&setup, "Stop", &no_ord_stop, &["--strict"])
            .status
            .success()
    );
    assert_eq!(latest_stop_id(), no_ord_id);
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
    assert_eq!(check_calls(), calls_before_stale);

    let stale_checked = agy_hook_cli(
        &setup,
        "Stop",
        &json!({
            "hookEventName": "Stop",
            "conversationId": LEAD,
            "executionId": "exec-old-checked",
            "transcriptPath": tx_str,
            "cwd": "/tmp/project",
            "lastUserInput": "auto prompt",
            "terminationReason": "NO_TOOL_CALL",
            "fullyIdle": true,
            "continuationActive": false,
            "executionNum": 0
        }),
        &["--strict"],
    );
    assert!(!stale_checked.status.success());
    assert_eq!(check_calls(), calls_before_stale);
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
}

#[test]
fn agy_unclaimed_prompt_holds_unseen_stop_until_first_tool_and_malformed_execution_id_keeps_lead_prompt()
 {
    let setup = Setup::new();
    setup.claim();
    let (_, tx) = agy_brain_dir(&setup, LEAD);
    let tx_str = tx.to_string_lossy().into_owned();
    write_jsonl(
        &tx,
        &[
            json!({"step_index": 0, "type": "USER_INPUT", "status": "DONE", "content": "human turn"}),
        ],
    );

    assert!(
        agy_hook_cli(
            &setup,
            "SessionStart",
            &json!({
                "hookEventName": "SessionStart",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "human turn"
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert!(
        agy_hook_cli(
            &setup,
            "Stop",
            &json!({
                "hookEventName": "Stop",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "human turn",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    let unseen_id = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Unclaiming quiet check: PreInvocation(0) tentatively holds unseen_stop until
    // the turn's unclaimed first PreToolUse strips it.
    let unclaiming = make_executable(&setup, "agy-unclaiming-check", "/bin/cat >/dev/null");
    let check_arg = format!("wezpup={}", unclaiming.display());
    assert!(
        agy_hook_cli(
            &setup,
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "second human turn",
                "invocationNum": 0,
                "initialNumSteps": 1
            }),
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let after_prompt = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(after_prompt["type"], "thinking");
    assert_eq!(after_prompt["unseen_stop"]["event_id"], json!(unseen_id));

    assert!(
        agy_hook_cli(
            &setup,
            "PreToolUse",
            &json!({
                "hookEventName": "PreToolUse",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "second human turn",
                "toolCall": {"name": "run_command", "id": "call-unclaimed-1"}
            }),
            &["--quiet-check", &check_arg, "--strict"],
        )
        .status
        .success()
    );
    let after_tool = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(after_tool["type"], "thinking");
    assert!(after_tool.get("unseen_stop").is_none(), "{after_tool}");

    // Confirmed lead PreInvocation(0) with a malformed optional executionId must
    // still act as a lead prompt: it does not inherit prior quiet thinking notes,
    // still replaces a waiting child's notify, and enters Phase::BeforeThinking
    // (where Purpose::Quiet checks bypass with HistoryUnavailable because
    // observation is None).
    let claiming = make_executable(
        &setup,
        "agy-claiming-check",
        "/bin/cat >/dev/null\nprintf '{\"quiet\":true,\"answer\":\"controller_submitted\"}\\n'",
    );
    let claim_arg = format!("wezpup={}", claiming.display());
    assert!(
        agy_hook_cli(
            &setup,
            "PreInvocation",
            &json!({
                "hookEventName": "PreInvocation",
                "conversationId": LEAD,
                "transcriptPath": tx_str,
                "cwd": "/tmp/project",
                "lastUserInput": "quiet turn",
                "invocationNum": 0,
                "initialNumSteps": 2
            }),
            &["--quiet-check", &claim_arg, "--strict"],
        )
        .status
        .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["hold_notes"]["wezpup"]["quiet"],
        json!(true)
    );

    let bad_exec_payload = json!({
        "hookEventName": "PreInvocation",
        "conversationId": LEAD,
        "transcriptPath": tx_str,
        "cwd": "/tmp/project",
        "lastUserInput": "fresh prompt with bad exec id",
        "executionId": "bad\nexec",
        "invocationNum": 0,
        "initialNumSteps": 3
    });
    let parsed_bad = parse_provider_event("agy", "PreInvocation", &bad_exec_payload, &setup.env);
    assert!(parsed_bad.is_lead_prompt);
    assert!(!parsed_bad.continues_turn);
    assert!(parsed_bad.observation.is_none());
    assert!(parsed_bad.observation_diagnostic.is_some());

    // Over quiet thinking with a claiming check: enters Phase::BeforeThinking,
    // bypasses Purpose::Quiet with history_unavailable because observation is None,
    // and does NOT inherit the prior turn's quiet note.
    let bad_exec_out = agy_hook_cli(
        &setup,
        "PreInvocation",
        &bad_exec_payload,
        &["--quiet-check", &claim_arg, "--debug"],
    );
    assert!(bad_exec_out.status.success());
    let bad_exec_env: Value = serde_json::from_slice(&bad_exec_out.stderr).unwrap();
    assert_eq!(
        bad_exec_env["result"]["hold_checks"][0]["reason"],
        "history_unavailable"
    );
    let after_bad_exec = shown_activity(&setup.binding_dir("agy", LEAD)).unwrap();
    assert_eq!(after_bad_exec["type"], "thinking");
    assert!(
        after_bad_exec.get("hold_notes").is_none(),
        "{after_bad_exec}"
    );

    // Over a waiting child's notify: replaces notify immediately instead of holding.
    let child_dir = link_agy_child(&setup, LEAD, "waiting-child", "worker");
    assert!(
        agy_hook_cli(
            &setup,
            "PreToolUse",
            &json!({
                "hookEventName": "PreToolUse",
                "conversationId": "waiting-child",
                "artifactDirectoryPath": child_dir,
                "lastUserInput": "",
                "toolCall": {"name": "ask_question", "id": "q-wait"}
            }),
            &["--strict"],
        )
        .status
        .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "notify"
    );
    assert!(
        agy_hook_cli(&setup, "PreInvocation", &bad_exec_payload, &[],)
            .status
            .success()
    );
    assert_eq!(
        shown_activity(&setup.binding_dir("agy", LEAD)).unwrap()["type"],
        "thinking"
    );
}
