//! As of Codex 0.162.0 a turn that ends on a model error runs no hook; its
//! end is only in the session rollout, as a `task_complete` that carries an
//! `error`. The turn follower a Codex prompt hook starts reads the rollout and
//! records that end.

use super::self_claim::{apply_as, install, records, start};
use super::*;
use std::sync::atomic::Ordering;
use wezterm_attention::codex_rollout::{
    FollowRequest, Followed, Refusal, Rollout, TurnSignal, follow_turn, take_turn,
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
    let prompt = event(
        "codex",
        "UserPromptSubmit",
        SESSION,
        json!({"prompt":"go","turn_id":turn,"transcript_path":rollout}),
    );
    let result = apply_as(setup, &setup.naming(AGENT_PID), &prompt, observation);
    // A prompt after a prompt repeats the badge, which is skipped.
    assert!(
        ["applied", "skipped"].contains(&result.disposition.as_str()),
        "{result:?}"
    );
    result
}

/// A pane its shell claimed, where Codex session "s" started and a prompt
/// for `TURN` was admitted from a hook that names its agent, and the follower
/// request that prompt made. The clock then reads later than every hook.
fn prompted(setup: &mut Setup) -> FollowRequest {
    setup.claim();
    let result = apply_as(
        setup,
        &setup.naming(AGENT_PID),
        &start("codex", SESSION),
        "00000000000000000200",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
    prompt(setup, TURN, "00000000000000000300");
    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    setup
        .followers
        .started
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the prompt asked for a follower")
}

/// Run the follower to its end. One that would wait for ever, as a follower
/// that misses its turn's end does, is stopped by its host exiting, and the
/// test fails.
fn follow(setup: &Setup, request: &FollowRequest) -> Followed {
    let (sent, received) = std::sync::mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let _ = sent.send(follow_turn(request, &setup.env, &setup.ports(), POLL));
        });
        match received.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result.expect("the follower runs"),
            Err(_) => {
                setup.processes.set(AGENT_PID, ProcessRead::Gone);
                let _ = received.recv();
                panic!("the follower did not finish within 5 s");
            }
        }
    })
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

/// Every record under the state root; the follower's lock is not one.
fn state(setup: &Setup) -> BTreeMap<PathBuf, String> {
    records(setup)
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

fn recorded(followed: &Followed) -> &str {
    let Followed::Recorded(result) = followed else {
        panic!("the follower recorded nothing: {followed:?}");
    };
    result.disposition.as_str()
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
    assert_eq!(end["source_version"], "0.162.0");
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
    assert_eq!(activity["observed_mono_ns"], "00000000000000000900");
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
    for (path, text) in state(&setup) {
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
// follower leaves without writing.
#[test]
fn an_end_without_an_error_records_nothing() {
    let aborted = line(
        "event_msg",
        json!({"type":"turn_aborted","turn_id":TURN,"reason":"interrupted"}),
    );
    let child = line(
        "event_msg",
        json!({"type":"task_complete","turn_id":TURN,"root_turn_id":"another-root",
               "error":{"message":MESSAGE,"codex_error_info":"other"}}),
    );
    for (name, lines, ended) in [
        (
            "successful task_complete",
            vec![finished(TURN)],
            TurnSignal::Finished,
        ),
        ("turn_aborted", vec![aborted], TurnSignal::Aborted),
        (
            "another turn started",
            vec![started(NEXT_TURN)],
            TurnSignal::OtherTurnStarted,
        ),
        (
            "a child turn's errored record, then the lead's own end",
            vec![child, finished(TURN)],
            TurnSignal::Finished,
        ),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        let before = state(&setup);
        append(&rollout_path(&setup), &lines);
        let followed = follow(&setup, &request);
        let Followed::Ended(signal) = &followed else {
            panic!("{name}: {followed:?}");
        };
        assert_eq!(*signal, ended, "{name}");
        assert!(state(&setup) == before, "{name}");
    }
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
    let activity = activity(&setup);
    assert_eq!(activity["type"], "thinking");
    assert!(activity.get("label").is_none(), "{activity}");
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
    let before = state(&setup);
    setup.clock = FixedClock {
        monotonic: "00000000000000000950",
        unix: "00000000099999999950",
    };
    assert_eq!(recorded(&follow(&setup, &request)), "skipped");
    assert!(state(&setup) == before);
}

#[test]
fn a_turn_its_own_hook_ended_is_not_recorded_again() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let stop = event(
        "codex",
        "Stop",
        SESSION,
        json!({"stop_hook_active":false,"turn_id":TURN}),
    );
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &stop,
        "00000000000000000500",
    );
    assert!(
        ["applied", "skipped"].contains(&result.disposition.as_str()),
        "{result:?}"
    );
    let before = state(&setup);
    append(
        &rollout_path(&setup),
        &[failed(TURN, json!("server_overloaded"))],
    );
    assert_eq!(recorded(&follow(&setup, &request)), "skipped");
    assert!(state(&setup) == before);
    assert!(attempt_outcomes(&setup).is_empty());
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
        let before = state(&setup);
        append(
            &rollout_path(&setup),
            &[failed(TURN, json!("server_overloaded"))],
        );
        let followed = follow(&setup, &request);
        assert!(expected(&followed), "{name}: {followed:?}");
        assert!(state(&setup) == before, "{name}");
    }
}

#[test]
fn one_follower_follows_a_turn_and_the_next_turn_waits_for_it() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let lock = follower_lock(&setup);
    let held = take_turn(&lock, TURN).expect("lock").expect("taken");
    assert!(matches!(
        follow(&setup, &request),
        Followed::AlreadyFollowed
    ));

    // A follower for the next turn waits for the one that holds the lock.
    let mut next = request.clone();
    next.turn_id = NEXT_TURN.to_owned();
    next.offset = fs::metadata(rollout_path(&setup)).unwrap().len();
    append(
        &rollout_path(&setup),
        &[started(NEXT_TURN), failed(NEXT_TURN, json!("other"))],
    );
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    // The newest turn the binding observed is still the first one, so the
    // next turn's end is kept as superseded; what matters here is that the
    // follower waited and then followed.
    assert!(matches!(follow(&setup, &next), Followed::Recorded(_)));
    releaser.join().unwrap();
    assert_eq!(fs::read_to_string(&lock).unwrap(), NEXT_TURN);
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
    request.rollout = other.display().to_string();
    request.offset = 0;
    let before = state(&setup);
    assert!(matches!(
        follow(&setup, &request),
        Followed::Refused(Refusal::OtherSession)
    ));
    assert!(state(&setup) == before);
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

// A hook can claim only the events Codex registers, and `task_complete` is
// none of them, so only the follower records a turn end from that source.
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
            turn_id: TURN.to_owned(),
            host_pid: AGENT_PID,
            host_started_sec: agent.start.seconds,
            host_started_usec: agent.start.microseconds,
            host_boot_session_id: BOOT_SESSION.to_owned(),
            rollout: rollout_path(&setup).display().to_string(),
            offset: fs::metadata(rollout_path(&setup)).unwrap().len(),
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
            "a Claude prompt",
            "claude",
            Box::new(|setup| {
                let rollout = rollout_path(setup);
                append(&rollout, &[header(SESSION)]);
                let prompt = event(
                    "claude",
                    "UserPromptSubmit",
                    SESSION,
                    json!({"prompt":"go","transcript_path":rollout}),
                );
                (setup.naming(AGENT_PID), prompt)
            }),
        ),
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
    use super::super::host_facet::real_processes::Pane;
    use super::*;
    use std::time::Instant;

    /// The followers of the pane in `directory`: pid and process group.
    fn followers(directory: &std::path::Path) -> Vec<(i32, i32)> {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,pgid=,command="])
            .output()
            .expect("run ps");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|row| {
                row.contains("hooks follow-turn") && row.contains(&*directory.to_string_lossy())
            })
            .filter_map(|row| {
                let mut fields = row.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect()
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(50));
        }
    }

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
