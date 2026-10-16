//! As of Claude Code 2.1.300 a turn the user stops with Esc runs no hook; the
//! session's status file shows its end, going from working to no turn. The
//! turn follower reads that file and records the end.

use super::self_claim::{apply_as, records, start};
use super::turn_follower::{follow_every, last_request, recorded};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wezterm_attention::claude_status::Foreign;
use wezterm_attention::turn_follower::{
    ClaudeTurn, FollowRequest, Followed, FollowedTurn, TakenTurn, take_turn,
};

const SESSION: &str = "s";
const POLL: Duration = Duration::from_millis(10);

/// One state of the status file a script sets, or no file.
#[derive(Clone)]
enum Status {
    File(Value),
    Text(&'static [u8]),
    Removed,
}

/// The status file of session "s" in the agent's process, in state `state`,
/// changed at `updated_at` on Claude Code's clock.
fn status(state: &str, updated_at: u64) -> Status {
    Status::File(json!({
        "pid":AGENT_PID,"sessionId":SESSION,"cwd":"/tmp/project","version":"2.1.300",
        "kind":"interactive","entrypoint":"cli","status":state,
        "statusUpdatedAt":updated_at
    }))
}

fn waiting(waiting_for: &str, updated_at: u64) -> Status {
    let Status::File(mut file) = status("waiting", updated_at) else {
        unreachable!()
    };
    file["waitingFor"] = json!(waiting_for);
    Status::File(file)
}

fn status_path(setup: &Setup) -> PathBuf {
    setup
        ._scratch
        .0
        .join(".claude/sessions")
        .join(format!("{AGENT_PID}.json"))
}

fn set_status(path: &std::path::Path, status: &Status) {
    match status {
        Status::File(file) => {
            fs::create_dir_all(path.parent().expect("sessions directory")).expect("sessions");
            // Replaced whole, as a reader sees a rewrite that completed.
            let next = path.with_extension("tmp");
            fs::write(&next, serde_json::to_vec(file).expect("status JSON")).expect("status");
            fs::rename(&next, path).expect("status in place");
        }
        Status::Text(text) => fs::write(path, text).expect("status text"),
        Status::Removed => {
            let _ = fs::remove_file(path);
        }
    }
}

/// Apply Claude Code's hook `name` from its agent, observed at `observation`.
fn hook(setup: &Setup, name: &str, payload: Value, observation: &str) -> Value {
    let result = apply_as(
        setup,
        &setup.naming(AGENT_PID),
        &event("claude", name, SESSION, payload),
        observation,
    );
    assert!(
        ["applied", "skipped"].contains(&result.disposition.as_str()),
        "{name}: {result:?}"
    );
    serde_json::to_value(&result).expect("result JSON")
}

/// The lead's latest prompt as the prompt record names it.
fn latest_prompt(setup: &Setup) -> Value {
    let prompts: Value = serde_json::from_slice(
        &fs::read(setup.binding_dir("claude", SESSION).join("prompts.json")).expect("prompts"),
    )
    .expect("prompts JSON");
    prompts["latest"].clone()
}

/// A request to follow the lead's latest prompt, as the prompt hook makes it.
fn request_for_latest(setup: &Setup) -> FollowRequest {
    let latest = latest_prompt(setup);
    let agent = setup.processes.facts(AGENT_PID);
    FollowRequest {
        address: pane_address(&setup.env).expect("address").0,
        launch_id: setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        session_id: SESSION.to_owned(),
        host_pid: AGENT_PID,
        host_started_sec: agent.start.seconds,
        host_started_usec: agent.start.microseconds,
        host_boot_session_id: BOOT_SESSION.to_owned(),
        turn: FollowedTurn::Claude(ClaudeTurn {
            prompt_observation_id: latest["observation_id"].as_str().unwrap().to_owned(),
            prompt_observed_mono_ns: latest["observed_mono_ns"].as_str().unwrap().to_owned(),
            status_file: status_path(setup).display().to_string(),
        }),
    }
}

/// A pane its shell claimed, where Claude Code session "s" started and a
/// prompt was admitted at 300 from a hook that names its agent, and the
/// request to follow that prompt. The clock then reads later than every hook.
fn prompted(setup: &mut Setup) -> FollowRequest {
    setup.claim();
    let result = apply_as(
        setup,
        &setup.naming(AGENT_PID),
        &start("claude", SESSION),
        "00000000000000000200",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
    hook(
        setup,
        "UserPromptSubmit",
        json!({"prompt":"go"}),
        "00000000000000000300",
    );
    setup.clock = FixedClock {
        monotonic: "00000000000000000900",
        unix: "00000000099999999900",
    };
    request_for_latest(setup)
}

/// Run the follower, with the status file set to `script[i]` as poll `i`
/// begins. Once the script has run out the agent exits, so a follower still
/// following then leaves as `HostGone`.
fn follow_script(setup: &Setup, request: &FollowRequest, script: &[Status]) -> Followed {
    follow_script_then(setup, request, script, Arc::new(|| None))
}

/// [`follow_script`], where `at_write` answers the agent's process read that
/// follows the last poll of the script: the one a write takes under the locks.
fn follow_script_then(
    setup: &Setup,
    request: &FollowRequest,
    script: &[Status],
    at_write: Arc<dyn Fn() -> Option<ProcessRead> + Send + Sync>,
) -> Followed {
    let path = status_path(setup);
    if let Some(first) = script.first() {
        set_status(&path, first);
    }
    let polls = Arc::new(AtomicUsize::new(0));
    let script = Arc::new(script.to_vec());
    let hook_path = path.clone();
    *setup.processes.on_read.lock().unwrap() = Some(Arc::new(move |pid| {
        if pid != AGENT_PID {
            return None;
        }
        let poll = polls.fetch_add(1, Ordering::SeqCst);
        match script.get(poll) {
            Some(status) => {
                set_status(&hook_path, status);
                None
            }
            // The read after the script's last poll is a write's, under its
            // locks, or the next poll's, which reads the last state again;
            // the read after that finds the agent gone.
            None if poll == script.len() => at_write(),
            None => Some(ProcessRead::Gone),
        }
    }));
    let followed = follow_every(setup, request, POLL);
    *setup.processes.on_read.lock().unwrap() = None;
    followed
}

/// The interrupts the lifecycle file in `binding` holds; none when there is
/// no file yet.
fn interrupts_in(binding: &std::path::Path) -> Vec<Value> {
    let Ok(bytes) = fs::read(binding.join("lifecycle.json")) else {
        return Vec::new();
    };
    let snapshot: Value = serde_json::from_slice(&bytes).expect("lifecycle JSON");
    snapshot["pools"]["general"]["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .filter(|item| item["kind"] == "user_interrupt")
        .cloned()
        .collect()
}

fn interrupts(setup: &Setup) -> Vec<Value> {
    interrupts_in(&setup.binding_dir("claude", SESSION))
}

fn read_json(path: PathBuf) -> Option<Value> {
    fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("record JSON"))
}

/// The activity the tab shows: the binding's activity when it is newer than
/// the activity-clear watermark.
fn shown_activity(setup: &Setup) -> Option<String> {
    let binding = setup.binding_dir("claude", SESSION);
    let activity = read_json(binding.join("activity.json"))?;
    let clear = read_json(binding.join("activity-clear.json"));
    let order = activity["observed_mono_ns"].as_str().unwrap_or("");
    if clear.is_some_and(|clear| order <= clear["observed_mono_ns"].as_str().unwrap_or("")) {
        return None;
    }
    activity["type"].as_str().map(str::to_owned)
}

#[test]
fn claude_idle_after_busy_records_user_interrupt_clear() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    assert_eq!(shown_activity(&setup).as_deref(), Some("thinking"));
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[status("busy", 100), status("idle", 200)]
        )),
        "applied"
    );
    let ends = interrupts(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    let end = &ends[0];
    assert_eq!(end["source_event"], "session_status");
    assert_eq!(end["source_version"], "2.1.300");
    assert_eq!(end["actor"]["kind"], "lead");
    assert!(end.get("correlation").is_none(), "{end}");
    assert_eq!(end["turn_end"]["status"], "recorded");
    assert_eq!(end["turn_end"]["held"], false);
    assert_eq!(end["turn_end"]["hold_checks"], json!([]));
    // Just after the prompt and its thinking, not at the follower's reading.
    assert_eq!(end["observed_mono_ns"], "00000000000000000301");
    assert_eq!(shown_activity(&setup), None);
    let clear = read_json(
        setup
            .binding_dir("claude", SESSION)
            .join("activity-clear.json"),
    )
    .expect("clear");
    assert_eq!(clear["observed_mono_ns"], "00000000000000000301");
}

#[test]
fn every_change_from_working_to_no_turn_records_the_interrupt() {
    for (name, script) in [
        (
            "shell after busy",
            vec![status("busy", 100), status("shell", 200)],
        ),
        (
            "idle after a permission prompt",
            vec![waiting("permission prompt", 100), status("idle", 200)],
        ),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        assert_eq!(
            recorded(&follow_script(&setup, &request, &script)),
            "applied",
            "{name}"
        );
        assert_eq!(interrupts(&setup).len(), 1, "{name}");
        assert_eq!(shown_activity(&setup), None, "{name}");
    }
}

#[test]
fn esc_at_an_approval_prompt_clears_its_notify() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    hook(
        &setup,
        "PermissionRequest",
        json!({"tool_name":"Bash"}),
        "00000000000000000400",
    );
    assert_eq!(shown_activity(&setup).as_deref(), Some("notify"));
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[waiting("permission prompt", 100), status("idle", 200)]
        )),
        "applied"
    );
    assert_eq!(
        interrupts(&setup)[0]["observed_mono_ns"],
        "00000000000000000401"
    );
    assert_eq!(shown_activity(&setup), None);
}

/// A follower that never reads an end newer than a working reading records
/// nothing, and keeps following until its agent exits: an end with no working
/// reading before it, an end no newer than it, a dialog, or a status it does
/// not know.
#[test]
fn no_end_newer_than_a_working_reading_records_nothing() {
    let cases: Vec<(&str, Vec<Status>)> = vec![
        (
            "idle from the start",
            vec![status("idle", 200), status("idle", 300)],
        ),
        (
            "idle no newer than the working reading",
            vec![status("busy", 200), status("idle", 200)],
        ),
        (
            "waiting at a dialog",
            vec![status("busy", 100), waiting("dialog open", 200)],
        ),
        (
            "an unknown state",
            vec![status("busy", 100), status("paused", 200)],
        ),
    ];
    for (name, script) in cases {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        let before = records(&setup);
        let followed = follow_script(&setup, &request, &script);
        assert!(
            matches!(followed, Followed::HostGone),
            "{name}: {followed:?}"
        );
        assert!(records(&setup) == before, "{name}");
        assert_eq!(
            shown_activity(&setup).as_deref(),
            Some("thinking"),
            "{name}"
        );
    }
}

/// A read that lands while Claude Code rewrites the file is skipped, not
/// taken as the end of the follow.
#[test]
fn a_reading_during_a_rewrite_is_skipped() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[
                status("busy", 100),
                Status::Text(b"{\"pid\":4000,\"sessionId\":\"s\",\"sta"),
                status("idle", 200)
            ]
        )),
        "applied"
    );
    assert_eq!(interrupts(&setup).len(), 1);
}

#[test]
fn a_status_file_that_is_gone_or_another_s_ends_the_follow_with_no_write() {
    let mut other = status("idle", 200);
    if let Status::File(file) = &mut other {
        file["sessionId"] = json!("other");
    }
    for (name, script, expected) in [
        (
            "another session's file",
            vec![status("busy", 100), other],
            Followed::StatusForeign(Foreign::OtherSession),
        ),
        (
            "the file is gone",
            vec![status("busy", 100), Status::Removed],
            Followed::StatusGone,
        ),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        let before = records(&setup);
        let followed = follow_script(&setup, &request, &script);
        assert_eq!(format!("{followed:?}"), format!("{expected:?}"), "{name}");
        assert!(records(&setup) == before, "{name}");
    }
}

/// A turn whose `Stop` or `StopFailure` was recorded first keeps that end.
#[test]
fn a_claude_turn_its_stop_ended_is_not_interrupted() {
    for (name, payload) in [
        ("Stop", json!({"stop_hook_active":false})),
        ("StopFailure", json!({"error":"server_error"})),
    ] {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        hook(&setup, name, payload, "00000000000000000500");
        let before = records(&setup);
        assert_eq!(
            recorded(&follow_script(
                &setup,
                &request,
                &[status("busy", 100), status("idle", 200)]
            )),
            "skipped",
            "{name}"
        );
        assert!(records(&setup) == before, "{name}");
        assert!(interrupts(&setup).is_empty(), "{name}");
    }
}

/// A `Stop` hook that blocks lets Claude Code go on with the same turn, so an
/// Esc later in it ends the turn there.
#[test]
fn an_esc_after_a_blocked_stop_is_recorded() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    hook(
        &setup,
        "Stop",
        json!({"stop_hook_active":false}),
        "00000000000000000500",
    );
    hook(
        &setup,
        "PreToolUse",
        json!({"tool_name":"Bash","tool_use_id":"call-1"}),
        "00000000000000000600",
    );
    assert_eq!(shown_activity(&setup).as_deref(), Some("thinking"));
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[status("busy", 100), status("idle", 200)]
        )),
        "applied"
    );
    let ends = interrupts(&setup);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0]["observed_mono_ns"], "00000000000000000601");
    assert_eq!(shown_activity(&setup), None);
}

/// The next prompt's hook took its stamp before the follower wrote, and
/// reaches the writer after it.
#[test]
fn next_prompt_after_esc_still_shows() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[status("busy", 100), status("idle", 200)]
        )),
        "applied"
    );
    hook(
        &setup,
        "UserPromptSubmit",
        json!({"prompt":"again"}),
        "00000000000000000850",
    );
    assert_eq!(shown_activity(&setup).as_deref(), Some("thinking"));
    let ends = interrupts(&setup);
    assert!(
        ends[0]["observed_mono_ns"].as_str().unwrap() < "00000000000000000850",
        "{ends:?}"
    );
}

#[test]
fn a_newer_prompt_ends_the_follow_with_no_write() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    hook(
        &setup,
        "UserPromptSubmit",
        json!({"prompt":"again"}),
        "00000000000000000400",
    );
    let before = records(&setup);
    let followed = follow_script(
        &setup,
        &request,
        &[status("busy", 100), status("idle", 200)],
    );
    assert!(matches!(followed, Followed::PromptReplaced), "{followed:?}");
    assert!(records(&setup) == before);
}

#[test]
fn two_followers_of_one_prompt_record_one_interrupt() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let script = [status("busy", 100), status("idle", 200)];
    assert_eq!(
        recorded(&follow_script(&setup, &request, &script)),
        "applied"
    );
    let before = records(&setup);
    assert_eq!(
        recorded(&follow_script(&setup, &request, &script)),
        "skipped"
    );
    assert!(records(&setup) == before);
    assert_eq!(interrupts(&setup).len(), 1);
}

#[test]
fn the_follower_writes_nothing_once_its_host_or_binding_is_gone() {
    type Arrange = Box<dyn Fn(&Setup)>;
    let cases: Vec<(&str, Arrange)> = vec![
        (
            "another process holds the host's pid",
            Box::new(|setup| {
                setup
                    .processes
                    .change(AGENT_PID, |agent| agent.start.microseconds += 1)
            }),
        ),
        (
            "the binding ended",
            Box::new(|setup| {
                let end = event("claude", "SessionEnd", SESSION, json!({"reason":"other"}));
                apply_as(
                    setup,
                    &setup.naming(AGENT_PID),
                    &end,
                    "00000000000000000500",
                );
            }),
        ),
        (
            "the pane was claimed for another launch",
            Box::new(|setup| {
                let mut claim = super::self_claim::stored_claim(setup).expect("claim");
                claim["launch_id"] = json!("00000000-0000-4000-8000-000000000999");
                super::self_claim::install(setup, &claim);
            }),
        ),
    ];
    for (name, arrange) in cases {
        let mut setup = Setup::new();
        let request = prompted(&mut setup);
        arrange(&setup);
        let before = records(&setup);
        let followed = follow_script(
            &setup,
            &request,
            &[status("busy", 100), status("idle", 200)],
        );
        assert!(
            matches!(followed, Followed::HostGone | Followed::BindingChanged),
            "{name}: {followed:?}"
        );
        assert!(records(&setup) == before, "{name}");
    }
}

#[test]
fn follow_request_binding_id_derives_from_provider() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let launch = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    assert_eq!(request.binding_id(), binding_id("claude", SESSION, launch));
    assert_ne!(request.binding_id(), binding_id("codex", SESSION, launch));
}

#[test]
fn an_admitted_claude_prompt_asks_for_one_follower_of_it() {
    let mut setup = Setup::new();
    let expected = prompted(&mut setup);
    assert_eq!(setup.followers.started.lock().unwrap().len(), 1);
    // The prompt as the writer stored it, not the hook's draft of it.
    let request = last_request(&setup);
    assert_eq!(request, expected);
    let FollowedTurn::Claude(turn) = &request.turn else {
        panic!("not a Claude turn: {request:?}");
    };
    assert_eq!(turn.prompt_observed_mono_ns, "00000000000000000300");
    assert_eq!(
        PathBuf::from(&turn.status_file),
        setup._scratch.0.join(".claude/sessions/4000.json")
    );
}

/// With `CLAUDE_CONFIG_DIR` set, the status file is in that directory.
#[test]
fn a_claude_prompt_follows_the_status_file_in_its_configuration_directory() {
    let setup = Setup::new();
    setup.claim();
    apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &start("claude", SESSION),
        "00000000000000000200",
    );
    let config = setup._scratch.0.join("claude-config");
    let prompt = parse_provider_event(
        "claude",
        "UserPromptSubmit",
        &payload(
            "claude",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":"go"}),
        ),
        &BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), config.display().to_string())]),
    );
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &prompt,
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
    let FollowedTurn::Claude(turn) = last_request(&setup).turn else {
        panic!("not a Claude turn");
    };
    assert_eq!(
        PathBuf::from(turn.status_file),
        config.join("sessions/4000.json")
    );
}

#[test]
fn a_claude_prompt_with_nothing_to_follow_asks_for_no_follower() {
    type Arrange = Box<dyn Fn(&Setup) -> (BTreeMap<String, String>, ProviderEvent)>;
    let cases: Vec<(&str, Arrange)> = vec![
        (
            "a hook that names no agent",
            Box::new(|setup| {
                (
                    setup.env.clone(),
                    event(
                        "claude",
                        "UserPromptSubmit",
                        SESSION,
                        json!({"prompt":"go"}),
                    ),
                )
            }),
        ),
        (
            "a sub-agent's prompt",
            Box::new(|setup| {
                (
                    setup.naming(AGENT_PID),
                    event(
                        "claude",
                        "UserPromptSubmit",
                        SESSION,
                        json!({"prompt":"go","agent_id":"child-a","agent_type":"worker"}),
                    ),
                )
            }),
        ),
        (
            "a CLAUDE_CONFIG_DIR the hook could not keep",
            Box::new(|setup| {
                let mut env = setup.naming(AGENT_PID);
                env.insert("CLAUDE_CONFIG_DIR".to_owned(), "relative/claude".to_owned());
                let prompt = parse_provider_event(
                    "claude",
                    "UserPromptSubmit",
                    &payload(
                        "claude",
                        "UserPromptSubmit",
                        SESSION,
                        json!({"prompt":"go"}),
                    ),
                    &env,
                );
                (env, prompt)
            }),
        ),
        (
            "a hook with no home and no configuration directory",
            Box::new(|setup| {
                let mut env = setup.naming(AGENT_PID);
                env.remove("HOME");
                (
                    env,
                    event(
                        "claude",
                        "UserPromptSubmit",
                        SESSION,
                        json!({"prompt":"go"}),
                    ),
                )
            }),
        ),
    ];
    for (name, arrange) in cases {
        let setup = Setup::new();
        setup.claim();
        apply_as(
            &setup,
            &setup.naming(AGENT_PID),
            &start("claude", SESSION),
            "00000000000000000200",
        );
        let (env, prompt) = arrange(&setup);
        let result = apply_as(&setup, &env, &prompt, "00000000000000000300");
        assert!(
            ["applied", "skipped"].contains(&result.disposition.as_str()),
            "{name}: {result:?}"
        );
        assert!(setup.followers.started.lock().unwrap().is_empty(), "{name}");
    }
}

#[test]
fn a_claude_follower_that_cannot_start_leaves_the_prompt_recorded() {
    let setup = Setup::new();
    setup.claim();
    apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &start("claude", SESSION),
        "00000000000000000200",
    );
    setup
        .followers
        .refuses
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = apply_as(
        &setup,
        &setup.naming(AGENT_PID),
        &event(
            "claude",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":"go"}),
        ),
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "applied", "{result:?}");
    assert_eq!(
        result.diagnostic.as_ref().map(|found| found.code),
        Some(DiagnosticCode::ProbeUnavailable),
        "{result:?}"
    );
    assert_eq!(shown_activity(&setup).as_deref(), Some("thinking"));
}

/// The write checks again under the launch and claim locks that the prompt
/// it follows is still the lead's latest, so a prompt recorded after the
/// follower's last reading leaves nothing written. The newer prompt is put in
/// place at the host read the write makes under its locks, before that check.
#[test]
fn the_write_checks_the_prompt_again_under_its_locks() {
    let script = [status("busy", 100), status("idle", 200)];
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let prompts = setup.binding_dir("claude", SESSION).join("prompts.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&prompts).unwrap()).unwrap();
    record["latest"]["observation_id"] = json!(Uuid::new_v4().to_string());
    record["latest"]["observed_mono_ns"] = json!("00000000000000000400");
    let newer = serde_json::to_vec(&record).unwrap();
    let followed = follow_script_then(
        &setup,
        &request,
        &script,
        Arc::new(move || {
            fs::write(&prompts, &newer).expect("a newer prompt");
            None
        }),
    );
    assert_eq!(recorded(&followed), "skipped");
    assert!(interrupts(&setup).is_empty());
    assert_eq!(shown_activity(&setup).as_deref(), Some("thinking"));
}

/// The write checks again under its locks that the follower's host still
/// runs, so a host that exited after the follower's last reading leaves
/// nothing written.
#[test]
fn the_write_checks_the_host_again_under_its_locks() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let before = records(&setup);
    let followed = follow_script_then(
        &setup,
        &request,
        &[status("busy", 100), status("idle", 200)],
        Arc::new(|| Some(ProcessRead::Gone)),
    );
    let Followed::Recorded(result) = &followed else {
        panic!("{followed:?}");
    };
    assert_eq!(result.disposition.as_str(), "ignored", "{result:?}");
    assert_eq!(
        result
            .diagnostic
            .as_ref()
            .map(|found| found.message.as_str()),
        Some("the turn's agent process is gone")
    );
    assert!(records(&setup) == before);
}

/// Tool traffic can evict the prompt's own observation from the pool while
/// its turn runs; the prompt record's stamp still says the turn is open.
#[test]
fn a_turn_whose_prompt_left_the_pool_is_still_interrupted() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    for call in 0..65 {
        hook(
            &setup,
            "PreToolUse",
            json!({"tool_name":"Bash","tool_use_id":format!("call-{call}")}),
            &format!("{:020}", 400 + call),
        );
    }
    let snapshot: Value = serde_json::from_slice(
        &fs::read(setup.binding_dir("claude", SESSION).join("lifecycle.json")).unwrap(),
    )
    .unwrap();
    assert!(
        !snapshot["pools"]["general"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "prompt_submitted"),
        "the prompt is still in the pool"
    );
    assert_eq!(
        recorded(&follow_script(
            &setup,
            &request,
            &[status("busy", 100), status("idle", 200)]
        )),
        "applied"
    );
    // Just after the turn's newest record.
    assert_eq!(
        interrupts(&setup)[0]["observed_mono_ns"],
        "00000000000000000465"
    );
    assert_eq!(shown_activity(&setup), None);
}

/// A Claude end needs two readings, so a follower that another prompt's
/// follower keeps out of the lock reads once and writes nothing.
#[test]
fn a_claude_follower_kept_out_of_the_lock_writes_nothing() {
    let mut setup = Setup::new();
    let request = prompted(&mut setup);
    let launch = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let lock = wezterm_attention::records::turn_follower_lock(
        &state_root(&setup.env).expect("state root"),
        &pane_address(&setup.env).expect("address").0,
        launch,
        &binding_id("claude", SESSION, launch),
    );
    let TakenTurn::Taken(_other) = take_turn(&lock, "another-prompt", POLL).expect("lock") else {
        panic!("the lock is free");
    };
    let before = records(&setup);
    let followed = follow_script(
        &setup,
        &request,
        &[status("busy", 100), status("idle", 200)],
    );
    assert!(
        matches!(followed, Followed::AlreadyFollowed),
        "{followed:?}"
    );
    assert!(records(&setup) == before);
}

/// A real agent in a real pseudo-terminal runs Claude Code's session-start
/// and prompt hooks as Attention registers them, so the follower the prompt
/// hook leaves is a real process reading a real status file.
#[cfg(target_os = "macos")]
mod real_follower {
    use super::super::host_facet::real_processes::{Pane, followers, wait_for};
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_real_prompt_hook_leaves_a_follower_that_records_the_interrupt() {
        let mut pane = Pane::new();
        let directory = pane.dir().to_owned();
        for (name, event, patch) in [
            (
                "claude-start.json",
                "SessionStart",
                json!({"source":"startup"}),
            ),
            (
                "claude-prompt.json",
                "UserPromptSubmit",
                json!({"prompt":"go"}),
            ),
        ] {
            fs::write(
                directory.join(name),
                serde_json::to_vec(&payload("claude", event, "real", patch)).unwrap(),
            )
            .unwrap();
        }
        // As of Claude Code 2.1.300 the status file reads busy before a
        // prompt's hooks run.
        // The prompt hook's output goes through a pipe, so the agent goes on
        // only once every writer of that pipe has closed it.
        fs::write(
            directory.join("agent.sh"),
            concat!(
                "mkdir -p \"$HOME/.claude/sessions\"\n",
                "printf '{\"pid\":%s,\"sessionId\":\"real\",\"status\":\"busy\",",
                "\"statusUpdatedAt\":100,\"version\":\"2.1.300\"}' $$ ",
                "> \"$HOME/.claude/sessions/$$.json\"\n",
                "sh -c 'WEZTERM_ATTENTION_HOST_PID=$PPID exec \"$ATTENTION\" ",
                "hooks event claude SessionStart --strict --debug' ",
                "< \"$OUT/claude-start.json\" > \"$OUT/start.out\" 2> \"$OUT/start.err\"\n",
                "echo $? > \"$OUT/start.status\"\n",
                "sh -c 'WEZTERM_ATTENTION_HOST_PID=$PPID exec \"$ATTENTION\" ",
                "hooks event claude UserPromptSubmit --strict --debug' ",
                "< \"$OUT/claude-prompt.json\" 2>&1 | cat > \"$OUT/prompt.out\"\n",
                "echo $$ > \"$OUT/agent.tmp\" && mv \"$OUT/agent.tmp\" \"$OUT/agent.pid\"\n",
                "sleep 60\n",
            ),
        )
        .unwrap();
        pane.start_shell("sh \"$OUT/agent.sh\"", None);
        let agent = pane.agent();

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

        let claim = pane.claim();
        let launch = claim["launch_id"].as_str().unwrap().to_owned();
        let env: BTreeMap<String, String> = pane.env().into_iter().collect();
        let binding = wezterm_attention::records::binding_dir(
            &state_root(&env).unwrap(),
            &pane_address(&env).unwrap().0,
            &launch,
            &binding_id("claude", "real", &launch),
        );
        // Esc: Claude Code rewrites the file to say no turn is working.
        let status = directory.join(format!(".claude/sessions/{agent}.json"));
        let next = status.with_extension("tmp");
        fs::write(
            &next,
            format!(
                "{{\"pid\":{agent},\"sessionId\":\"real\",\"status\":\"idle\",\
                 \"statusUpdatedAt\":200,\"version\":\"2.1.300\"}}"
            ),
        )
        .unwrap();
        let written = Instant::now();
        fs::rename(&next, &status).unwrap();
        let interrupts = || interrupts_in(&binding);
        wait_for("the follower to record the interrupt", || {
            !interrupts().is_empty()
        });
        // Within two of its one-second polls.
        assert!(
            written.elapsed() < Duration::from_secs(2),
            "{:?}",
            written.elapsed()
        );
        let end = &interrupts()[0];
        assert_eq!(end["source_event"], "session_status");
        assert_eq!(end["turn_end"]["status"], "recorded");
        let activity: Value =
            serde_json::from_slice(&fs::read(binding.join("activity.json")).unwrap()).unwrap();
        let clear: Value =
            serde_json::from_slice(&fs::read(binding.join("activity-clear.json")).unwrap())
                .unwrap();
        assert!(
            activity["observed_mono_ns"].as_str() <= clear["observed_mono_ns"].as_str(),
            "{activity} {clear}"
        );
        wait_for("the follower to leave", || followers(&directory).is_empty());
        pane.kill_shell();
    }
}
