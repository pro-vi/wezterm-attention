//! The plugin's own writes: the user's review flag and the acknowledgement of
//! an activity the user has looked at. The plugin is not a process in the
//! pane, so each command names its pane and launch in flags, and runs here
//! with none of the pane's environment.

use super::*;

const LAUNCH: &str = "00000000-0000-4000-8000-000000000401";
const OTHER_LAUNCH: &str = "00000000-0000-4000-8000-000000000499";

fn bound() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "plugin",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}

/// `attention plugin <action>` for the pane the setup claimed, as the plugin
/// runs it: the state root handed over, and nothing of the pane's own.
fn plugin(setup: &Setup, action: &str, launch_id: &str, extra: &[&str]) -> (i32, Value) {
    let (address, _) = pane_address(&setup.env).expect("address");
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .env("HOME", &setup.env["HOME"])
        .env("WEZTERM_ATTENTION_DIR", &setup.env["WEZTERM_ATTENTION_DIR"])
        .args(["plugin", action])
        .args(["--realm-id", &address.realm_id])
        .args(["--incarnation-id", &address.incarnation_id])
        .args(["--pane-id", &address.pane_id])
        .args(["--launch-id", launch_id])
        .args(extra)
        .output()
        .expect("run plugin command");
    let response = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "plugin {action} printed no envelope: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().expect("exit code"), response)
}

fn review_path(setup: &Setup, owner: &str) -> PathBuf {
    let (address, _) = pane_address(&setup.env).unwrap();
    pane_path(&state_root(&setup.env).unwrap(), &address)
        .join("reviews")
        .join(format!(
            "{}.json",
            wezterm_attention::protocol::sha256_hex(owner.as_bytes())
        ))
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("record")).expect("record JSON")
}

/// The current activity's event id, after a Stop.
fn stopped(setup: &Setup, observation: &str) -> String {
    setup.apply(
        &event(
            "claude",
            "Stop",
            "plugin",
            json!({"stop_hook_active":false}),
        ),
        observation,
    );
    let activity = read_json(&setup.binding_dir("claude", "plugin").join("activity.json"));
    assert_eq!(activity["type"], "stop");
    activity["event_id"].as_str().expect("event id").to_owned()
}

#[test]
fn the_plugin_sets_and_clears_the_users_review_on_the_pane_it_names() {
    let setup = bound();
    let (code, response) = plugin(&setup, "set-review", LAUNCH, &[]);
    assert_eq!(code, 0, "{response}");
    assert_eq!(response["command"], "plugin set-review");
    assert_eq!(response["status"], "ok");
    assert_eq!(response["result"]["disposition"], "applied");
    let review = read_json(&review_path(&setup, "user"));
    assert_eq!(review["owner_id"], "user");
    assert_eq!(review["event_id"], response["result"]["event_id"]);

    let (code, response) = plugin(&setup, "clear-review", LAUNCH, &[]);
    assert_eq!(code, 0, "{response}");
    assert_eq!(response["result"]["disposition"], "applied");
    assert!(!review_path(&setup, "user").exists());
    let (_, again) = plugin(&setup, "clear-review", LAUNCH, &[]);
    assert_eq!(again["result"]["disposition"], "skipped");
}

// A reader shows nothing for a pane whose claim is not the launch the pane
// published, so a flag written then would light nothing.
#[test]
fn the_users_review_is_refused_where_the_claim_names_another_launch() {
    let setup = bound();
    let (code, response) = plugin(&setup, "set-review", OTHER_LAUNCH, &[]);
    assert_eq!(code, 1, "{response}");
    assert_eq!(response["diagnostics"][0]["code"], "claim_stale");
    assert!(!review_path(&setup, "user").exists());
}

// The plugin is not a process in the pane: the launch it names is the one the
// pane published, whichever kind of claim holds it.
#[test]
fn a_claim_an_agent_holds_for_itself_takes_the_users_review() {
    let setup = bound();
    self_claim::install(
        &setup,
        &self_claim::self_owned_claim(&setup, LAUNCH, AGENT_PID),
    );
    let (code, response) = plugin(&setup, "set-review", LAUNCH, &[]);
    assert_eq!(code, 0, "{response}");
    assert!(review_path(&setup, "user").exists());
}

// A review is withdrawn only by its owner.
#[test]
fn clearing_the_users_review_leaves_another_owners() {
    let setup = bound();
    apply_mark_review(&setup.env, "build").expect("build's review");
    plugin(&setup, "set-review", LAUNCH, &[]);
    let (code, response) = plugin(&setup, "clear-review", LAUNCH, &[]);
    assert_eq!(code, 0, "{response}");
    assert!(!review_path(&setup, "user").exists());
    assert!(review_path(&setup, "build").exists());
}

#[test]
fn the_users_review_is_written_and_removed_only_under_the_review_locks() {
    let setup = bound();
    let (address, _) = pane_address(&setup.env).unwrap();
    let pane = pane_path(&state_root(&setup.env).unwrap(), &address);
    let owner_lock = pane.join("reviews").join(format!(
        ".{}.lock",
        wezterm_attention::protocol::sha256_hex(b"user")
    ));
    for lock in [pane.join(".claim.lock"), owner_lock.clone()] {
        let (code, response) = with_lock(&lock, Duration::from_secs(5), || {
            Ok(plugin(&setup, "set-review", LAUNCH, &[]))
        })
        .unwrap();
        assert_eq!(code, 1, "{response}");
        assert_eq!(response["diagnostics"][0]["code"], "probe_unavailable");
        assert!(!review_path(&setup, "user").exists());
    }
    plugin(&setup, "set-review", LAUNCH, &[]);
    for lock in [pane.join(".claim.lock"), owner_lock] {
        let (code, _) = with_lock(&lock, Duration::from_secs(5), || {
            Ok(plugin(&setup, "clear-review", LAUNCH, &[]))
        })
        .unwrap();
        assert_eq!(code, 1);
        assert!(review_path(&setup, "user").exists());
    }
}

#[test]
fn the_plugin_acknowledges_the_activity_the_user_saw() {
    let setup = bound();
    let event_id = stopped(&setup, "00000000000000000300");
    let (code, response) = plugin(
        &setup,
        "acknowledge",
        LAUNCH,
        &["--activity-event-id", &event_id],
    );
    assert_eq!(code, 0, "{response}");
    assert_eq!(response["command"], "plugin acknowledge");
    assert_eq!(response["result"]["disposition"], "applied");
    let directory = setup.binding_dir("claude", "plugin");
    let ack = read_json(&directory.join("ack.json"));
    assert_eq!(ack["activity_event_id"], event_id.as_str());
    assert_eq!(
        ack["target"],
        read_json(&directory.join("activity.json"))["target"]
    );
    assert_eq!(ack["event_id"], response["result"]["event_id"]);
    let (_, again) = plugin(
        &setup,
        "acknowledge",
        LAUNCH,
        &["--activity-event-id", &event_id],
    );
    assert_eq!(again["result"]["disposition"], "skipped");
}

// The user saw one activity; a newer one is still unseen, and acknowledging
// the older one must not hide it.
#[test]
fn an_acknowledgement_for_an_activity_that_moved_on_writes_nothing() {
    let setup = bound();
    let seen = stopped(&setup, "00000000000000000300");
    setup.apply(
        &event("claude", "UserPromptSubmit", "plugin", json!({})),
        "00000000000000000400",
    );
    let current = stopped(&setup, "00000000000000000500");
    assert_ne!(seen, current);
    let (code, response) = plugin(
        &setup,
        "acknowledge",
        LAUNCH,
        &["--activity-event-id", &seen],
    );
    assert_eq!(code, 0, "{response}");
    assert_eq!(response["result"]["disposition"], "ignored");
    assert!(
        !setup
            .binding_dir("claude", "plugin")
            .join("ack.json")
            .exists()
    );
}

#[test]
fn an_acknowledgement_is_refused_where_the_claim_names_another_launch() {
    let setup = bound();
    let event_id = stopped(&setup, "00000000000000000300");
    let (code, response) = plugin(
        &setup,
        "acknowledge",
        OTHER_LAUNCH,
        &["--activity-event-id", &event_id],
    );
    assert_eq!(code, 1, "{response}");
    assert_eq!(response["diagnostics"][0]["code"], "claim_stale");
    assert!(
        !setup
            .binding_dir("claude", "plugin")
            .join("ack.json")
            .exists()
    );
}

#[test]
fn an_acknowledgement_is_written_only_under_the_activity_locks() {
    let setup = bound();
    let event_id = stopped(&setup, "00000000000000000300");
    let root = state_root(&setup.env).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    for lock in [
        launch_path(&root, &address, LAUNCH).join(".lock"),
        pane_path(&root, &address).join(".claim.lock"),
    ] {
        let (code, response) = with_lock(&lock, Duration::from_secs(5), || {
            Ok(plugin(
                &setup,
                "acknowledge",
                LAUNCH,
                &["--activity-event-id", &event_id],
            ))
        })
        .unwrap();
        assert_eq!(code, 1, "{response}");
        assert_eq!(response["diagnostics"][0]["code"], "probe_unavailable");
    }
    assert!(
        !setup
            .binding_dir("claude", "plugin")
            .join("ack.json")
            .exists()
    );
}

#[test]
fn the_plugin_command_refuses_a_target_it_cannot_name() {
    let setup = bound();
    let (code, response) = plugin(&setup, "set-review", "not-a-uuid", &[]);
    assert_eq!(code, 2, "{response}");
    assert_eq!(response["status"], "usage_error");
    assert!(!review_path(&setup, "user").exists());
}
