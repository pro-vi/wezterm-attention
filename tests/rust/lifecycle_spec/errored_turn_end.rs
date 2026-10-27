//! As of Codex 0.162.0 a turn that ends on a model error runs no hook; its
//! end is only in the session rollout, as a `task_complete` that carries an
//! `error`. The turn follower a lead Codex hook starts reads the rollout from
//! that hook's turn through the lead turns after it, and records the first end
//! on a model error.

use super::self_claim::{apply_as, install, records, start};
use super::turn_follower::{follow_every, last_request, recorded};
use super::*;
use std::sync::atomic::Ordering;
use wezterm_attention::codex_rollout::{Refusal, Rollout, TurnSignal};
use wezterm_attention::turn_follower::{
    CodexTurn, FollowRequest, Followed, FollowedTurn, TakenTurn, take_turn,
};

const SESSION: &str = "s";
const TURN: &str = "turn-1";
const NEXT_TURN: &str = "turn-2";
const POLL: Duration = Duration::from_millis(10);
const MESSAGE: &str = "Selected model is at capacity. Please try a different model.";

fn rollout_path(setup: &Setup) -> PathBuf {
    setup
        ._scratch
        .0
        .join(format!("rollout-2026-10-12T00-00-00-{SESSION}.jsonl"))
}

fn line(kind: &str, payload: Value) -> String {
    format!(
        "{}\n",
        json!({"timestamp":"2026-10-12T00:00:00.000Z","type":kind,"payload":payload})
    )
}

fn header(session: &str) -> String {
    line(
        "session_meta",
        json!({"id":session,"cli_version":"0.162.0","cwd":"/tmp/project"}),
    )
}

fn started(turn: &str) -> String {
    line(
        "event_msg",
        json!({"type":"task_started","turn_id":turn,"root_turn_id":turn}),
    )
}

fn failed(turn: &str, info: Value) -> String {
    line(
        "event_msg",
        json!({"type":"task_complete","turn_id":turn,"root_turn_id":turn,
               "last_agent_message":null,"error":{"message":MESSAGE,"codex_error_info":info}}),
    )
}

fn finished(turn: &str) -> String {
    line(
        "event_msg",
        json!({"type":"task_complete","turn_id":turn,"root_turn_id":turn,
               "last_agent_message":"done"}),
    )
}

fn filler() -> String {
    line("event_msg", json!({"type":"thread_goal_updated"}))
}

fn append(path: &std::path::Path, lines: &[String]) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the rollout");
    for text in lines {
        file.write_all(text.as_bytes())
            .expect("append to the rollout");
    }
}

fn prompt(
    setup: &Setup,
    turn: &str,
    observation: &str,
) -> wezterm_attention::lifecycle::LifecycleResult {
    let rollout = rollout_path(setup);
    if !rollout.exists() {
        append(&rollout, &[header(SESSION)]);
    }
    append(&rollout, &[started(turn)]);
    hook(
        setup,
        "UserPromptSubmit",
        json!({"prompt":"go","turn_id":turn,"transcript_path":rollout}),
        observation,
    )
}

/// Apply Codex's hook `name` from its agent, observed at `observation`. A
/// hook that repeats the standing badge, as a prompt after a prompt does, is
/// skipped.
fn hook(
    setup: &Setup,
    name: &str,
    payload: Value,
    observation: &str,
) -> wezterm_attention::lifecycle::LifecycleResult {
    let result = apply_as(
        setup,
        &setup.naming(AGENT_PID),
        &event("codex", name, SESSION, payload),
        observation,
    );
    assert!(
        ["applied", "skipped"].contains(&result.disposition.as_str()),
        "{name}: {result:?}"
    );
    result
}

/// `TURN`'s `Stop` hook, observed at `observation`.
fn stop(setup: &Setup, observation: &str) {
    hook(
        setup,
        "Stop",
        json!({"stop_hook_active":false,"turn_id":TURN}),
        observation,
    );
}

/// The Codex turn `request` follows.
fn codex_turn(request: &mut FollowRequest) -> &mut CodexTurn {
    match &mut request.turn {
        FollowedTurn::Codex(turn) => turn,
        other @ FollowedTurn::Claude(_) => panic!("not a Codex turn: {other:?}"),
    }
}

/// A pane its shell claimed, where Codex session "s" started from a hook that
/// names its agent.
fn session_started(setup: &Setup) {
    setup.claim();
    let result = apply_as(
        setup,
        &setup.naming(AGENT_PID),
        &start("codex", SESSION),
        "00000000000000000200",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
}

/// [`session_started`], then a prompt for `TURN` admitted from a hook that
/// names its agent, and the follower request that prompt made. The clock then
/// reads later than every hook.
fn prompted(setup: &mut Setup) -> FollowRequest {
    session_started(setup);
    prompt(setup, TURN, "00000000000000000300");
    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    last_request(setup)
}

/// [`follow_every`] at `POLL`.
fn follow(setup: &Setup, request: &FollowRequest) -> Followed {
    follow_every(setup, request, POLL)
}

/// The attempt outcomes the lifecycle file in `binding` holds; none when
/// there is no file yet.
fn attempt_outcomes_in(binding: &std::path::Path) -> Vec<Value> {
    let Ok(bytes) = fs::read(binding.join("lifecycle.json")) else {
        return Vec::new();
    };
    let snapshot: Value = serde_json::from_slice(&bytes).expect("lifecycle JSON");
    snapshot["pools"]["general"]["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .filter(|item| item["kind"] == "attempt_outcome")
        .cloned()
        .collect()
}

fn activity_in(binding: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(binding.join("activity.json")).expect("activity"))
        .expect("activity JSON")
}

fn attempt_outcomes(setup: &Setup) -> Vec<Value> {
    attempt_outcomes_in(&setup.binding_dir("codex", SESSION))
}

fn activity(setup: &Setup) -> Value {
    activity_in(&setup.binding_dir("codex", SESSION))
}

fn follower_lock(setup: &Setup) -> PathBuf {
    let launch = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    wezterm_attention::records::turn_follower_lock(
        &state_root(&setup.env).expect("state root"),
        &pane_address(&setup.env).expect("address").0,
        launch,
        &binding_id("codex", SESSION, launch),
    )
}

#[test]
fn errored_task_complete_records_codex_attempt_outcome_turn_end() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[filler(), failed(TURN, json!("server_overloaded")), filler()],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");

    let outcomes = attempt_outcomes(&setup);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    let end = &outcomes[0];
    assert_eq!(end["source_event"], "task_complete");
    assert!(end.get("source_version").is_none(), "{end}");
    assert_eq!(end["outcome"], "failed");
    assert_eq!(end["error_code"], "server_overloaded");
    assert!(end.get("error_category").is_none(), "{end}");
    assert_eq!(end["actor"]["kind"], "lead");
    assert_eq!(end["correlation"]["turn_id"], TURN);
    assert_eq!(end["turn_end"]["status"], "recorded");
    assert_eq!(end["turn_end"]["held"], false);
    let activity = activity(&setup);
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["source"], "codex");
    assert_eq!(activity["label"], "server_overloaded");
    // Ordered just after the turn's prompt, its last record, not at the
    // follower's later reading.
    assert_eq!(activity["observed_mono_ns"], "00000000000000000301");
    assert_eq!(end["observed_mono_ns"], "00000000000000000301");
}

#[test]
fn the_end_keeps_its_turn_s_order_when_a_later_record_holds_the_slot() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    // A child's permission prompt, after the turn's last lead record.
    hook(
        &setup,
        "PermissionRequest",
        json!({"tool_name":"shell","agent_id":"child-a","agent_type":"worker"}),
        "00000000000000000860",
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    assert_eq!(
        attempt_outcomes(&setup)[0]["observed_mono_ns"],
        "00000000000000000301"
    );
    let activity = activity(&setup);
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["label"], "server_overloaded");
    assert_eq!(activity["observed_mono_ns"], "00000000000000000861");
}

#[test]
fn the_end_follows_a_prompt_typed_while_the_turn_ran() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    // A second prompt of the same turn, kept only as the latest prompt.
    hook(
        &setup,
        "UserPromptSubmit",
        json!({"prompt":"also","turn_id":TURN,"transcript_path":rollout_path(&setup)}),
        "00000000000000000600",
    );
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    assert_eq!(
        attempt_outcomes(&setup)[0]["observed_mono_ns"],
        "00000000000000000601"
    );
    assert_eq!(activity(&setup)["observed_mono_ns"], "00000000000000000601");
}

#[test]
fn the_notify_takes_the_slot_after_a_later_clear() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    wezterm_attention::lifecycle::apply_mark_clear(&setup.env, "codex", "00000000000000000700")
        .expect("clear");
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let activity = activity(&setup);
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["observed_mono_ns"], "00000000000000000701");
}

#[test]
fn error_message_text_never_persisted() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    for (path, text) in records(&setup) {
        assert!(
            !text.contains("at capacity"),
            "{} holds the error message",
            path.display()
        );
    }
}

// Codex names an error as a string, or as an object with one key. Only a
// safe label is kept; any other code is left out and the turn end kept.
#[test]
fn the_error_code_is_codex_s_name_when_it_is_a_safe_label() {
    for (info, code) in [
        (
            json!({"usage_limit_exceeded": {"resets_at": 1}}),
            Some("usage_limit_exceeded"),
        ),
        (json!("bad\u{7}code"), None),
        (json!({"a":1,"b":2}), None),
        (json!(7), None),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        append(&rollout_path(&setup), &[failed(TURN, info.clone())]);
        assert_eq!(recorded(&follow(&setup, &request)), "applied", "{info}");
        let end = &attempt_outcomes(&setup)[0];
        assert_eq!(end["error_code"].as_str(), code, "{info}: {end}");
        assert_eq!(end["turn_end"]["status"], "recorded");
        let activity = activity(&setup);
        assert_eq!(activity["type"], "notify");
        assert_eq!(activity["label"].as_str(), code, "{info}: {activity}");
    }
}

// Every other end is recorded by the turn's own hook, or by nothing: the
// follower goes on without writing.
#[test]
fn an_end_without_an_error_records_nothing_and_the_follower_goes_on() {
    let aborted = line(
        "event_msg",
        json!({"type":"turn_aborted","turn_id":TURN,"reason":"interrupted"}),
    );
    let child = line(
        "event_msg",
        json!({"type":"task_complete","turn_id":TURN,"root_turn_id":"another-root",
               "error":{"message":MESSAGE,"codex_error_info":"other"}}),
    );
    for (name, ended) in [
        ("successful task_complete", vec![finished(TURN)]),
        ("turn_aborted", vec![aborted]),
        ("another turn started", vec![]),
        (
            "a child turn's errored record, then the lead's own end",
            vec![child, finished(TURN)],
        ),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        // As of Codex 0.163.0 a turn that continues a goal runs no prompt hook,
        // and one that fails before its first tool call runs none at all, so
        // the binding records nothing of the next turn.
        let mut lines = ended;
        lines.extend([
            started(NEXT_TURN),
            failed(NEXT_TURN, json!("server_overloaded")),
        ]);
        append(&rollout_path(&setup), &lines);
        assert_eq!(recorded(&follow(&setup, &request)), "applied", "{name}");
        let ends = attempt_outcomes(&setup);
        assert_eq!(ends.len(), 1, "{name}: {ends:?}");
        assert_eq!(ends[0]["correlation"]["turn_id"], NEXT_TURN, "{name}");
        assert_eq!(ends[0]["turn_end"]["status"], "recorded", "{name}");
        // Just after the last record of the turn it was carried from.
        assert_eq!(
            ends[0]["observed_mono_ns"], "00000000000000000301",
            "{name}"
        );
        let activity = activity(&setup);
        assert_eq!(activity["type"], "notify", "{name}");
        assert_eq!(activity["label"], "server_overloaded", "{name}");
        assert_eq!(
            fs::read_to_string(follower_lock(&setup)).unwrap(),
            NEXT_TURN,
            "{name}"
        );
    }
}

#[test]
fn a_carried_turn_s_failure_after_a_later_turn_s_prompt_is_superseded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[
            finished(TURN),
            started(NEXT_TURN),
            failed(NEXT_TURN, json!("server_overloaded")),
        ],
    );
    // The prompt of the turn after it reached the writer before the follower
    // read that end.
    prompt(&setup, "turn-3", "00000000000000000400");
    assert_eq!(recorded(&follow(&setup, &request)), "ignored");
    let ends = attempt_outcomes(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["correlation"]["turn_id"], NEXT_TURN);
    assert_eq!(ends[0]["turn_end"]["status"], "superseded");
    assert_eq!(activity(&setup)["type"], "thinking");
}

#[test]
fn a_goal_turn_after_a_stop_and_a_turn_without_hooks_is_recorded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    stop(&setup, "00000000000000000500");
    // Two goal turns follow; neither runs a hook, and the second fails.
    append(
        &rollout_path(&setup),
        &[
            finished(TURN),
            started(NEXT_TURN),
            finished(NEXT_TURN),
            started("turn-3"),
            failed("turn-3", json!("server_overloaded")),
        ],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let ends = attempt_outcomes(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["correlation"]["turn_id"], "turn-3");
    assert_eq!(ends[0]["turn_end"]["status"], "recorded");
    assert_eq!(ends[0]["observed_mono_ns"], "00000000000000000501");
    assert_eq!(activity(&setup)["type"], "notify");
}

#[test]
fn a_prompt_starts_a_follower_while_a_leaving_one_holds_the_lock() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    // The turn's follower holds the lock between two polls. Codex has ended
    // the turn on an error and sent the next queued prompt.
    let TakenTurn::Taken(held) = take_turn(&follower_lock(&setup), TURN, POLL).expect("lock")
    else {
        panic!("the lock is free");
    };
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    prompt(&setup, NEXT_TURN, "00000000000000000400");
    let mut next = last_request(&setup);
    assert_eq!(codex_turn(&mut next).turn_id, NEXT_TURN);
    // That follower records its turn's end and leaves, without reading the
    // next turn's start; the next turn's follower then takes the lock.
    drop(held);
    assert_eq!(recorded(&follow(&setup, &request)), "ignored");
    append(
        &rollout_path(&setup),
        &[failed(NEXT_TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &next)), "applied");
    assert_eq!(activity(&setup)["type"], "notify");
}

#[test]
fn a_tool_hook_starts_the_follower_of_a_turn_that_ran_no_prompt_hook() {
    let setup = Setup::new();
    session_started(&setup);
    // As of Codex 0.163.0 a turn that continues a goal runs no prompt hook;
    // its tool hooks name it.
    let rollout = rollout_path(&setup);
    append(&rollout, &[header(SESSION), started(TURN)]);
    hook(
        &setup,
        "PreToolUse",
        json!({"tool_name":"shell","tool_use_id":"call-1","turn_id":TURN,
               "transcript_path":rollout}),
        "00000000000000000300",
    );
    let mut request = last_request(&setup);
    assert_eq!(codex_turn(&mut request).turn_id, TURN);
    assert_eq!(activity(&setup)["type"], "thinking");
    append(&rollout, &[failed(TURN, json!("server_overloaded"))]);
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let ends = attempt_outcomes(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["error_code"], "server_overloaded");
    assert_eq!(ends[0]["turn_end"]["status"], "recorded");
    let activity = activity(&setup);
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["label"], "server_overloaded");
}

#[test]
fn a_hook_starts_no_follower_while_one_holds_the_lock_nor_for_a_child() {
    let mut setup = Setup::new();
    prompted(&mut setup);
    let rollout = rollout_path(&setup);
    let started = || setup.followers.started.lock().unwrap().len();
    let tool = |call: &str, observation: &str| {
        hook(
            &setup,
            "PreToolUse",
            json!({"tool_name":"shell","tool_use_id":call,"turn_id":TURN,
                   "transcript_path":rollout}),
            observation,
        );
    };
    let TakenTurn::Taken(held) = take_turn(&follower_lock(&setup), TURN, POLL).expect("lock")
    else {
        panic!("the lock is free");
    };
    tool("call-1", "00000000000000000400");
    assert_eq!(started(), 1);
    drop(held);
    hook(
        &setup,
        "PreToolUse",
        json!({"tool_name":"shell","tool_use_id":"call-2","turn_id":"child-turn",
               "agent_id":"child-a","agent_type":"worker","transcript_path":rollout}),
        "00000000000000000500",
    );
    assert_eq!(started(), 1);
    tool("call-3", "00000000000000000600");
    assert_eq!(started(), 2);
}

#[test]
fn stale_task_complete_never_overwrites_newer_turn() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    // The next turn's prompt reached the writer before the follower did.
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    prompt(&setup, NEXT_TURN, "00000000000000000400");
    assert_eq!(recorded(&follow(&setup, &request)), "ignored");
    let end = &attempt_outcomes(&setup)[0];
    assert_eq!(end["turn_end"]["status"], "superseded");
    assert_eq!(end["correlation"]["turn_id"], TURN);
    // Before the next turn's prompt, after its own.
    assert_eq!(end["observed_mono_ns"], "00000000000000000301");
    let activity = activity(&setup);
    assert_eq!(activity["type"], "thinking");
    assert!(activity.get("label").is_none(), "{activity}");
}

#[test]
fn an_end_orders_before_a_prompt_that_began_after_it() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    // The next prompt's hook took its stamp after Codex ended the turn and
    // before the follower read the end, and reaches the writer after it.
    let followers = setup.followers.started.lock().unwrap().len();
    prompt(&setup, NEXT_TURN, "00000000000000000850");
    assert_eq!(activity(&setup)["type"], "thinking");
    assert_eq!(setup.followers.started.lock().unwrap().len(), followers + 1);
}

#[test]
fn a_prompt_whose_thinking_loses_the_slot_is_still_followed() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    // A child's permission prompt stands in the slot when the follower
    // writes, so the turn's notify is stamped after it.
    hook(
        &setup,
        "PermissionRequest",
        json!({"tool_name":"shell","agent_id":"child-a","agent_type":"worker"}),
        "00000000000000000860",
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    // The next prompt's hook took its stamp before that notify and writes
    // after it: its thinking loses the slot, and its turn is still followed.
    let rollout = rollout_path(&setup);
    append(&rollout, &[started(NEXT_TURN)]);
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":"go","turn_id":NEXT_TURN,"transcript_path":rollout}),
        ),
        "00000000000000000850",
    );
    assert_eq!(result.disposition, "ignored", "{result:?}");
    let mut next = last_request(&setup);
    assert_eq!(codex_turn(&mut next).turn_id, NEXT_TURN);
    append(&rollout, &[failed(NEXT_TURN, json!("server_overloaded"))]);
    // The same notify already stands, so writing it again is skipped.
    assert_eq!(recorded(&follow(&setup, &next)), "skipped");
    let ends = attempt_outcomes(&setup);
    let end = ends
        .iter()
        .find(|end| end["correlation"]["turn_id"] == NEXT_TURN)
        .expect("the next turn's end");
    assert_eq!(end["turn_end"]["status"], "recorded");
    assert_eq!(activity(&setup)["type"], "notify");
}

#[test]
fn a_turn_failing_after_a_superseded_failure_is_recorded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    prompt(&setup, NEXT_TURN, "00000000000000000400");
    assert_eq!(recorded(&follow(&setup, &request)), "ignored");
    let mut next = last_request(&setup);
    assert_eq!(codex_turn(&mut next).turn_id, NEXT_TURN);
    append(
        &rollout_path(&setup),
        &[failed(NEXT_TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &next)), "applied");
    let ends = attempt_outcomes(&setup);
    let end = ends
        .iter()
        .find(|end| end["correlation"]["turn_id"] == NEXT_TURN)
        .expect("the next turn's end");
    assert_eq!(end["turn_end"]["status"], "recorded");
    let activity = activity(&setup);
    assert_eq!(activity["type"], "notify");
    assert_eq!(activity["label"], "server_overloaded");
}

#[test]
fn replayed_task_complete_is_idempotent_by_turn_id() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let before = records(&setup);
    setup.clock = FixedClock {
        monotonic: "00000000000000000950",
        unix: "00000000099999999950",
    };
    assert_eq!(recorded(&follow(&setup, &request)), "skipped");
    assert!(records(&setup) == before);
}

#[test]
fn a_turn_its_own_hook_ended_is_not_recorded_again() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    stop(&setup, "00000000000000000500");
    let before = records(&setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "skipped");
    assert!(records(&setup) == before);
    assert!(attempt_outcomes(&setup).is_empty());
}

#[test]
fn a_prompt_typed_into_a_turn_after_its_stop_lets_its_failure_be_recorded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    stop(&setup, "00000000000000000500");
    // The Stop blocked and Codex went on with the turn. A prompt typed into
    // it carries the turn's id as of Codex 0.162.0, so only the prompt record
    // keeps it.
    hook(
        &setup,
        "UserPromptSubmit",
        json!({"prompt":"and this","turn_id":TURN,"transcript_path":rollout_path(&setup)}),
        "00000000000000000600",
    );
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let ends = attempt_outcomes(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["turn_end"]["status"], "recorded");
    assert_eq!(ends[0]["observed_mono_ns"], "00000000000000000601");
    assert_eq!(activity(&setup)["type"], "notify");
}

#[test]
fn a_later_turn_s_prompt_after_a_stop_does_not_reopen_the_turn() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    stop(&setup, "00000000000000000500");
    // As of Codex 0.163.0 a post-turn compaction that hits the usage limit
    // ends the turn on an error after a Stop that did not block.
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("usage_limit_exceeded"))],
    );
    prompt(&setup, NEXT_TURN, "00000000000000000600");
    // A second prompt of the next turn is kept only as the latest prompt.
    hook(
        &setup,
        "UserPromptSubmit",
        json!({"prompt":"and this","turn_id":NEXT_TURN,"transcript_path":rollout_path(&setup)}),
        "00000000000000000700",
    );
    let before = records(&setup);
    assert_eq!(recorded(&follow(&setup, &request)), "skipped");
    assert!(records(&setup) == before);
    assert!(attempt_outcomes(&setup).is_empty());
}

#[test]
fn a_turn_that_went_on_after_its_stop_and_failed_is_recorded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    // As of Codex 0.162.0 a Stop hook that blocks makes Codex continue the
    // same turn.
    stop(&setup, "00000000000000000500");
    hook(
        &setup,
        "PreToolUse",
        json!({"tool_name":"shell","tool_use_id":"call-1","turn_id":TURN}),
        "00000000000000000600",
    );
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "applied");
    let ends = attempt_outcomes(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["turn_end"]["status"], "recorded");
    assert_eq!(ends[0]["observed_mono_ns"], "00000000000000000601");
    assert_eq!(activity(&setup)["type"], "notify");
}

#[test]
fn the_follower_writes_nothing_once_its_host_or_binding_is_gone() {
    type Arrange = Box<dyn Fn(&Setup)>;
    type Expect = fn(&Followed) -> bool;
    let host_gone: Expect = |followed| matches!(followed, Followed::HostGone);
    let binding_changed: Expect = |followed| matches!(followed, Followed::BindingChanged);
    let cases: Vec<(&str, Arrange, Expect)> = vec![
        (
            "the host has exited",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Gone)),
            host_gone,
        ),
        (
            "the host cannot be read",
            Box::new(|setup| setup.processes.set(AGENT_PID, ProcessRead::Unknown)),
            host_gone,
        ),
        (
            "another process holds the host's pid",
            Box::new(|setup| {
                setup
                    .processes
                    .change(AGENT_PID, |agent| agent.start.microseconds += 1)
            }),
            host_gone,
        ),
        (
            "the binding ended",
            Box::new(|setup| {
                let end = event("codex", "SessionEnd", SESSION, json!({"reason":"other"}));
                apply_as(
                    setup,
                    &setup.naming(AGENT_PID),
                    &end,
                    "00000000000000000500",
                );
            }),
            binding_changed,
        ),
        (
            "another session took the binding over",
            Box::new(|setup| {
                let resumed = event("codex", "SessionStart", "other", json!({"source":"resume"}));
                let result = apply_as(
                    setup,
                    &setup.naming(AGENT_PID),
                    &resumed,
                    "00000000000000000500",
                );
                assert_eq!(result.disposition, "replaced", "{result:?}");
            }),
            binding_changed,
        ),
        (
            "the pane was claimed for another launch",
            Box::new(|setup| {
                let mut claim = super::self_claim::stored_claim(setup).expect("claim");
                claim["launch_id"] = json!("00000000-0000-4000-8000-000000000999");
                install(setup, &claim);
            }),
            binding_changed,
        ),
    ];
    for (name, arrange, expected) in cases {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        arrange(&setup);
        let before = records(&setup);
        append(
            &rollout_path(&setup),
            &[failed(TURN, json!("server_overloaded"))],
        );
        let followed = follow(&setup, &request);
        assert!(expected(&followed), "{name}: {followed:?}");
        assert!(records(&setup) == before, "{name}");
    }
}

#[test]
fn one_follower_follows_a_turn_and_the_next_turn_waits_for_it() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let lock = follower_lock(&setup);
    let TakenTurn::Taken(held) = take_turn(&lock, TURN, POLL).expect("lock") else {
        panic!("the lock is free");
    };
    assert!(matches!(
        follow(&setup, &request),
        Followed::AlreadyFollowed
    ));

    // A follower for the next turn waits for the one that holds the lock.
    let mut next = request.clone();
    codex_turn(&mut next).turn_id = NEXT_TURN.to_owned();
    codex_turn(&mut next).offset = fs::metadata(rollout_path(&setup)).unwrap().len();
    append(
        &rollout_path(&setup),
        &[started(NEXT_TURN), failed(NEXT_TURN, json!("other"))],
    );
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    // The binding recorded nothing for the next turn, so its end is skipped;
    // what matters is that the follower waited and then followed. Its wait,
    // counted in its 100 ms polls, outlasts the 300 ms the holder stays.
    assert!(matches!(
        follow_every(&setup, &next, Duration::from_millis(100)),
        Followed::Recorded(_)
    ));
    releaser.join().unwrap();
    assert_eq!(fs::read_to_string(&lock).unwrap(), NEXT_TURN);
}

#[test]
fn a_follower_kept_out_by_a_later_turn_s_still_records_its_end() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    prompt(&setup, NEXT_TURN, "00000000000000000400");
    // The later turn's follower keeps the lock for all of the wait.
    let TakenTurn::Taken(_later) =
        take_turn(&follower_lock(&setup), NEXT_TURN, POLL).expect("lock")
    else {
        panic!("the lock is free");
    };
    assert_eq!(recorded(&follow(&setup, &request)), "ignored");
    let end = &attempt_outcomes(&setup)[0];
    assert_eq!(end["correlation"]["turn_id"], TURN);
    assert_eq!(end["turn_end"]["status"], "superseded");
}

#[test]
fn a_rollout_that_is_not_the_session_s_is_refused() {
    let setup = Setup::new();
    let directory = &setup._scratch.0;
    let other_name = directory.join("rollout-2026-10-12T00-00-00-other.jsonl");
    fs::write(&other_name, header(SESSION)).unwrap();
    let other_session = directory.join("rollout-2026-10-12T00-00-01-s.jsonl");
    fs::write(&other_session, header("other")).unwrap();
    let not_a_header = directory.join("rollout-2026-10-12T00-00-02-s.jsonl");
    fs::write(&not_a_header, started(TURN)).unwrap();
    let link = directory.join("rollout-2026-10-12T00-00-03-s.jsonl");
    let real = directory.join("real-rollout-s.jsonl");
    fs::write(&real, header(SESSION)).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let not_a_file = directory.join("rollout-2026-10-12T00-00-04-s.jsonl");
    fs::create_dir(&not_a_file).unwrap();
    for (path, refusal) in [
        (&other_name, Refusal::OtherName),
        (&other_session, Refusal::OtherSession),
        (&not_a_header, Refusal::OtherSession),
        (&link, Refusal::NotOpened),
        (&not_a_file, Refusal::NotRegularFile),
    ] {
        assert_eq!(
            Rollout::open(path, SESSION, 0).err(),
            Some(refusal),
            "{}",
            path.display()
        );
    }
    // A FIFO with no writer is refused at once, not held open until one comes.
    let fifo = directory.join("rollout-2026-10-12T00-00-05-s.jsonl");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `name` is a live NUL-terminated path for the call to read.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sent, received) = std::sync::mpsc::channel();
    let reader = fifo.clone();
    thread::spawn(move || sent.send(Rollout::open(&reader, SESSION, 0).err()));
    let refused = received.recv_timeout(Duration::from_secs(5));
    if refused.is_err() {
        // Release the blocked open before failing.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    assert_eq!(refused.ok().flatten(), Some(Refusal::NotRegularFile));
}

#[test]
fn a_follower_handed_another_session_s_rollout_writes_nothing() {
    let mut setup = Setup::new();
    let mut request = prompted(&mut setup);
    let other = setup._scratch.0.join("rollout-2026-10-12T00-00-09-s.jsonl");
    fs::write(
        &other,
        format!("{}{}", header("other"), failed(TURN, json!("other"))),
    )
    .unwrap();
    codex_turn(&mut request).rollout = other.display().to_string();
    codex_turn(&mut request).offset = 0;
    let before = records(&setup);
    assert!(matches!(
        follow(&setup, &request),
        Followed::RolloutRefused(Refusal::OtherSession)
    ));
    assert!(records(&setup) == before);
}

#[test]
fn a_rollout_that_shrinks_or_is_replaced_is_left() {
    let setup = Setup::new();
    let path = rollout_path(&setup);
    append(&path, &[header(SESSION), started(TURN), filler()]);
    let length = fs::metadata(&path).unwrap().len();
    let mut rollout = Rollout::open(&path, SESSION, length).expect("open");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(length - 4)
        .unwrap();
    assert_eq!(rollout.next_signal(TURN), Err(Refusal::Shrunk));

    let mut rollout = Rollout::open(&path, SESSION, 0).expect("open");
    let replacement = setup._scratch.0.join("replacement.jsonl");
    fs::write(&replacement, header(SESSION)).unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(rollout.next_signal(TURN), Err(Refusal::Replaced));
}

#[test]
fn a_line_is_read_only_once_complete_and_an_overlong_one_is_skipped() {
    let setup = Setup::new();
    let path = rollout_path(&setup);
    append(&path, &[header(SESSION)]);
    let mut rollout = Rollout::open(&path, SESSION, 0).expect("open");
    assert_eq!(rollout.next_signal(TURN), Ok(None));

    let mut overlong = failed(TURN, json!("other"));
    overlong.pop();
    overlong.push_str(&" ".repeat(4 << 20));
    overlong.push('\n');
    append(&path, &[overlong]);
    assert_eq!(
        rollout.next_signal(TURN),
        Ok(None),
        "the overlong line is skipped"
    );

    let mut partial = failed(TURN, json!("server_overloaded"));
    partial.pop();
    append(&path, &[partial]);
    assert_eq!(rollout.next_signal(TURN), Ok(None), "no newline yet");
    append(&path, &["\n".to_owned()]);
    assert_eq!(
        rollout.next_signal(TURN),
        Ok(Some(TurnSignal::Failed {
            error_code: Some("server_overloaded".to_owned())
        }))
    );
}

// The manifest's `native_hooks` do not declare `task_complete`, and a hook
// event they do not declare is ignored, so only the follower records a turn
// end from that source.
#[test]
fn hook_channel_refuses_task_complete_source_event() {
    let parsed = parse_provider_event(
        "codex",
        "task_complete",
        &payload(
            "codex",
            "task_complete",
            SESSION,
            json!({"turn_id":TURN,"error":{"codex_error_info":"other"}}),
        ),
        &BTreeMap::new(),
    );
    assert_eq!(parsed.action, ProviderAction::Ignored, "{parsed:?}");
    assert!(parsed.observation.is_none());
}

#[test]
fn an_admitted_codex_prompt_asks_for_one_follower_of_its_turn() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    assert_eq!(setup.followers.started.lock().unwrap().len(), 1);
    let agent = setup.processes.facts(AGENT_PID);
    let (address, _) = pane_address(&setup.env).expect("address");
    assert_eq!(
        request,
        FollowRequest {
            address,
            launch_id: setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
            session_id: SESSION.to_owned(),
            host_pid: AGENT_PID,
            host_started_sec: agent.start.seconds,
            host_started_usec: agent.start.microseconds,
            host_boot_session_id: BOOT_SESSION.to_owned(),
            turn: FollowedTurn::Codex(CodexTurn {
                turn_id: TURN.to_owned(),
                rollout: rollout_path(&setup).display().to_string(),
                offset: fs::metadata(rollout_path(&setup)).unwrap().len(),
            }),
        }
    );
}

#[test]
fn a_prompt_with_nothing_to_follow_asks_for_no_follower() {
    type Arrange = Box<dyn Fn(&Setup) -> (BTreeMap<String, String>, ProviderEvent)>;
    let codex_prompt = |setup: &Setup, patch: Value| {
        let rollout = rollout_path(setup);
        append(&rollout, &[header(SESSION)]);
        let mut fields = json!({"prompt":"go","turn_id":TURN,"transcript_path":rollout});
        for (key, value) in patch.as_object().unwrap() {
            fields[key] = value.clone();
        }
        event("codex", "UserPromptSubmit", SESSION, fields)
    };
    let cases: Vec<(&str, &str, Arrange)> = vec![
        (
            "a hook that names no agent",
            "codex",
            Box::new(move |setup| (setup.env.clone(), codex_prompt(setup, json!({})))),
        ),
        (
            "a prompt with no turn id",
            "codex",
            Box::new(move |setup| {
                let mut prompt = codex_prompt(setup, json!({}));
                if let Some(draft) = prompt.observation.as_mut() {
                    draft.correlation = None;
                }
                (setup.naming(AGENT_PID), prompt)
            }),
        ),
        (
            "a prompt with no transcript",
            "codex",
            Box::new(move |setup| {
                (
                    setup.naming(AGENT_PID),
                    codex_prompt(setup, json!({"transcript_path":null})),
                )
            }),
        ),
        (
            "a transcript that does not exist",
            "codex",
            Box::new(move |setup| {
                (
                    setup.naming(AGENT_PID),
                    codex_prompt(
                        setup,
                        json!({"transcript_path":"/nonexistent/rollout-s.jsonl"}),
                    ),
                )
            }),
        ),
    ];
    for (name, provider, arrange) in cases {
        let setup = Setup::new();
        setup.claim();
        apply_as(
            &setup,
            &setup.naming(AGENT_PID),
            &start(provider, SESSION),
            "00000000000000000200",
        );
        let (env, prompt) = arrange(&setup);
        let result = apply_as(&setup, &env, &prompt, "00000000000000000300");
        assert_eq!(result.disposition, "applied", "{name}: {result:?}");
        assert!(setup.followers.started.lock().unwrap().is_empty(), "{name}");
    }
}

#[test]
fn a_follower_that_cannot_start_leaves_the_prompt_recorded() {
    let setup = Setup::new();
    setup.claim();
    apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &start("codex", SESSION),
        "00000000000000000200",
    );
    setup.followers.refuses.store(true, Ordering::SeqCst);
    let result = prompt(&setup, TURN, "00000000000000000300");
    assert_eq!(result.disposition, "applied", "{result:?}");
    assert_eq!(
        result.diagnostic.as_ref().map(|found| found.code),
        Some(DiagnosticCode::ProbeUnavailable),
        "{result:?}"
    );
    assert_eq!(activity(&setup)["type"], "thinking");
}

/// A real agent in a real pseudo-terminal runs Codex's session-start and
/// prompt hooks as Codex 0.162.0 registers them, so the follower the prompt
/// hook leaves is a real process reading the kernel's process table.
#[cfg(target_os = "macos")]
mod real_follower {
    use super::super::host_facet::real_processes::{Pane, followers, wait_for};
    use super::*;

    #[test]
    fn a_real_prompt_hook_leaves_a_follower_that_records_the_errored_end() {
        let mut pane = Pane::new();
        let directory = pane.dir().to_owned();
        let rollout = directory.join("rollout-2026-10-12T00-00-00-real.jsonl");
        fs::write(&rollout, format!("{}{}", header("real"), started(TURN))).unwrap();
        fs::write(
            directory.join("prompt.json"),
            serde_json::to_vec(&payload(
                "codex",
                "UserPromptSubmit",
                "real",
                json!({"prompt":"go","turn_id":TURN,"transcript_path":rollout}),
            ))
            .unwrap(),
        )
        .unwrap();
        // The prompt hook's output goes through a pipe, as Codex reads it: the
        // agent goes on only once every writer of that pipe has closed it.
        fs::write(
            directory.join("agent.sh"),
            concat!(
                "sh -c 'WEZTERM_ATTENTION_HOST_PID=$PPID exec \"$ATTENTION\" ",
                "hooks event codex SessionStart --strict --debug' ",
                "< \"$OUT/payload.json\" > \"$OUT/start.out\" 2> \"$OUT/start.err\"\n",
                "echo $? > \"$OUT/start.status\"\n",
                "sh -c 'WEZTERM_ATTENTION_HOST_PID=$PPID exec \"$ATTENTION\" ",
                "hooks event codex UserPromptSubmit --strict --debug' ",
                "< \"$OUT/prompt.json\" 2>&1 | cat > \"$OUT/prompt.out\"\n",
                "echo $$ > \"$OUT/agent.tmp\" && mv \"$OUT/agent.tmp\" \"$OUT/agent.pid\"\n",
                "sleep 60\n",
            ),
        )
        .unwrap();
        pane.start_shell("sh \"$OUT/agent.sh\"", None);
        pane.agent();

        // The hook has returned and closed its output; its follower runs on,
        // leading a process group of its own.
        let running = followers(&directory);
        assert_eq!(
            running.len(),
            1,
            "{running:?}; prompt hook: {}",
            fs::read_to_string(directory.join("prompt.out")).unwrap_or_default()
        );
        let (pid, group) = running[0];
        assert_eq!(pid, group, "the follower leads its own process group");

        append(
            &rollout,
            &[filler(), failed(TURN, json!("server_overloaded"))],
        );
        let claim = pane.claim();
        let launch = claim["launch_id"].as_str().unwrap().to_owned();
        let env: BTreeMap<String, String> = pane.env().into_iter().collect();
        let binding = wezterm_attention::records::binding_dir(
            &state_root(&env).unwrap(),
            &pane_address(&env).unwrap().0,
            &launch,
            &binding_id("codex", "real", &launch),
        );
        wait_for("the follower to record the errored end", || {
            !attempt_outcomes_in(&binding).is_empty()
        });
        let end = &attempt_outcomes_in(&binding)[0];
        assert_eq!(end["error_code"], "server_overloaded");
        assert_eq!(end["turn_end"]["status"], "recorded");
        let activity = activity_in(&binding);
        assert_eq!(activity["type"], "notify");
        assert_eq!(activity["label"], "server_overloaded");
        wait_for("the follower to leave", || followers(&directory).is_empty());
        pane.kill_shell();
    }
}
