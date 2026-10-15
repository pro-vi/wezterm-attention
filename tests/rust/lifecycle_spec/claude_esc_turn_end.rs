//! As of Claude Code 2.1.300 a turn the user stops with Esc runs no hook; its
//! end shows only in the session's status file, which goes from working to no
//! turn. The turn follower reads that file and records the end.

use super::self_claim::{apply_as, records, start};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use wezterm_attention::claude_status::Foreign;
use wezterm_attention::turn_follower::{
    ClaudeTurn, FollowRequest, Followed, FollowedTurn, follow_turn,
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
            // One poll past the script reads its last state; the next finds
            // the agent gone.
            None if poll == script.len() => None,
            None => Some(ProcessRead::Gone),
        }
    }));
    let (sent, received) = std::sync::mpsc::channel();
    let followed = thread::scope(|scope| {
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
    });
    *setup.processes.on_read.lock().unwrap() = None;
    followed
}

fn general_observations(setup: &Setup) -> Vec<Value> {
    let Ok(bytes) = fs::read(setup.binding_dir("claude", SESSION).join("lifecycle.json")) else {
        return Vec::new();
    };
    let snapshot: Value = serde_json::from_slice(&bytes).expect("lifecycle JSON");
    snapshot["pools"]["general"]["observations"]
        .as_array()
        .expect("observations")
        .clone()
}

fn interrupts(setup: &Setup) -> Vec<Value> {
    general_observations(setup)
        .into_iter()
        .filter(|item| item["kind"] == "user_interrupt")
        .collect()
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

fn recorded(followed: &Followed) -> &str {
    let Followed::Recorded(result) = followed else {
        panic!("the follower recorded nothing: {followed:?}");
    };
    result.disposition.as_str()
}

/// Shared scenario "Esc after working".
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

/// A follower that has not seen the turn working records nothing, and keeps
/// following until its agent exits.
#[test]
fn no_turn_without_an_earlier_working_reading_records_nothing() {
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

/// Shared scenario "Stop came first", and the same for `StopFailure`.
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

/// Shared scenario "Next prompt right after Esc": the next prompt's hook took
/// its stamp before the follower wrote, and reaches the writer after it.
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
