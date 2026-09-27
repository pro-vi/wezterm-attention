//! A saved stop folds the binding's spent child records into its retention
//! floor and removes them, so, unless a record holds the floor back, the
//! plugin's poll reads about as many child records as ran within one presence
//! lifetime of the latest stop, not every child the session ever started.

use super::*;
use std::os::unix::fs::PermissionsExt;

const PROVIDERS: [(&str, &str); 2] = [("claude", "Explore"), ("codex", "worker")];

/// One presence lifetime after the fixture clock's first reading, and one
/// nanosecond past it.
const LIFETIME_LATER: &str = "00000000612345678900";
const PAST_LIFETIME: &str = "00000000612345678901";

fn bound(provider: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            provider,
            "SessionStart",
            "parent",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(provider, "UserPromptSubmit", "parent", json!({})),
        "00000000000000000300",
    );
    setup
}

fn child(provider: &str, name: &str, agent: &str, agent_type: &str) -> ProviderEvent {
    let mut patch = json!({"agent_id":agent,"agent_type":agent_type});
    match name {
        "SubagentStop" => patch["stop_hook_active"] = json!(false),
        _ => patch["tool_name"] = json!("Bash"),
    }
    event(provider, name, "parent", patch)
}

fn at(unix: &'static str) -> FixedClock {
    FixedClock {
        monotonic: "00000000000000000100",
        unix,
    }
}

fn order(value: u64) -> String {
    format!("{value:020}")
}

/// Runs one child from its first tool call to its stop, and returns the
/// stop's disposition.
fn run_child(setup: &Setup, provider: &str, agent_type: &str, agent: &str, start: u64) -> String {
    setup.apply(
        &child(provider, "PreToolUse", agent, agent_type),
        &order(start),
    );
    setup
        .apply(
            &child(provider, "SubagentStop", agent, agent_type),
            &order(start + 5),
        )
        .disposition
        .as_str()
        .to_owned()
}

fn presence_path(setup: &Setup, provider: &str, agent: &str) -> PathBuf {
    setup
        .binding_dir(provider, "parent")
        .join("agents")
        .join(format!(
            "{}.json",
            wezterm_attention::protocol::sha256_hex(agent.as_bytes())
        ))
}

fn floor(setup: &Setup, provider: &str) -> Option<Value> {
    let path = setup
        .binding_dir(provider, "parent")
        .join("agents-floor.json");
    fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("floor JSON"))
}

#[test]
fn a_child_stop_folds_spent_children_into_the_floor() {
    for (provider, agent_type) in PROVIDERS {
        let mut setup = bound(provider);
        run_child(&setup, provider, agent_type, "child-a", 400);
        run_child(&setup, provider, agent_type, "child-b", 410);

        // A stopped child still fences its own late events for one whole
        // lifetime after it was written.
        setup.clock = at(LIFETIME_LATER);
        assert_eq!(
            run_child(&setup, provider, agent_type, "child-c", 500),
            "applied"
        );
        assert!(presence_path(&setup, provider, "child-a").exists());
        assert!(presence_path(&setup, provider, "child-b").exists());
        assert!(floor(&setup, provider).is_none(), "{provider}");

        setup.clock = at(PAST_LIFETIME);
        assert_eq!(
            run_child(&setup, provider, agent_type, "child-d", 600),
            "applied"
        );
        assert!(!presence_path(&setup, provider, "child-a").exists());
        assert!(!presence_path(&setup, provider, "child-b").exists());
        assert!(presence_path(&setup, provider, "child-c").exists());
        assert!(presence_path(&setup, provider, "child-d").exists());
        let floor = floor(&setup, provider).expect("floor written");
        assert_eq!(floor["floor_mono_ns"], order(415), "{provider}");
        assert!(Uuid::parse_str(floor["operation_id"].as_str().unwrap()).is_ok());

        // The floor fences the removed children: a late event of one is
        // refused, as its own stopped record refused it before.
        let late = setup.apply(
            &child(provider, "PreToolUse", "child-a", agent_type),
            &order(404),
        );
        assert_eq!(late.disposition, "ignored", "{provider}");
        assert!(!presence_path(&setup, provider, "child-a").exists());
    }
}

// The lead's stop ends the turn its children ran in, so it compacts as a
// child's stop does. Claude's Stop is lead activity; Codex's also clears the
// children it leaves behind.
#[test]
fn a_lead_stop_folds_spent_children_into_the_floor() {
    for (provider, agent_type) in PROVIDERS {
        let mut setup = bound(provider);
        run_child(&setup, provider, agent_type, "child-a", 400);
        setup.clock = at(PAST_LIFETIME);
        let stop = setup.apply(
            &event(
                provider,
                "Stop",
                "parent",
                json!({"stop_hook_active":false}),
            ),
            &order(500),
        );
        assert_eq!(stop.disposition, "applied", "{provider}");
        assert!(!presence_path(&setup, provider, "child-a").exists());
        assert_eq!(
            floor(&setup, provider).expect("floor written")["floor_mono_ns"],
            order(405)
        );
    }
}

// A child blocked on a permission prompt sends nothing that would refresh its
// presence, and its wait has no time limit. While it can still hold the
// pane's notify, compaction keeps it, and every spent child after it.
#[test]
fn a_waiting_child_outlasts_compaction() {
    for (provider, agent_type) in PROVIDERS {
        let mut setup = bound(provider);
        run_child(&setup, provider, agent_type, "child-old", 350);
        setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            &order(400),
        );
        run_child(&setup, provider, agent_type, "child-after", 450);
        setup.clock = at("00000086412345678900");
        run_child(&setup, provider, agent_type, "child-b", 500);
        assert!(!presence_path(&setup, provider, "child-old").exists());
        assert!(presence_path(&setup, provider, "child-a").exists());
        assert!(presence_path(&setup, provider, "child-after").exists());
        assert_eq!(
            floor(&setup, provider).expect("floor written")["floor_mono_ns"],
            order(355)
        );

        let tool = if provider == "claude" {
            "Task"
        } else {
            "wait_agent"
        };
        let held = setup.apply(
            &event(provider, "PreToolUse", "parent", json!({"tool_name":tool})),
            &order(600),
        );
        assert_eq!(held.disposition, "ignored", "{provider}");
    }
}

// Once the pane's activity is newer than the child's request, the child
// holds nothing, and it is spent like any other.
#[test]
fn a_waiting_child_behind_newer_activity_is_spent() {
    for (provider, agent_type) in PROVIDERS {
        let mut setup = bound(provider);
        setup.apply(
            &child(provider, "PermissionRequest", "child-a", agent_type),
            &order(400),
        );
        setup.apply(
            &event(provider, "UserPromptSubmit", "parent", json!({})),
            &order(450),
        );
        setup.clock = at(PAST_LIFETIME);
        run_child(&setup, provider, agent_type, "child-b", 500);
        assert!(!presence_path(&setup, provider, "child-a").exists());
    }
}

// Compaction is maintenance: a fence it cannot read stops it, and the stop
// that ran it is saved and reported as saved all the same.
#[test]
fn a_failed_compaction_leaves_the_stop_saved() {
    let mut setup = bound("claude");
    run_child(&setup, "claude", "Explore", "child-a", 400);
    let activity = setup.binding_dir("claude", "parent").join("activity.json");
    fs::write(&activity, b"{").expect("corrupt activity");
    setup.clock = at(PAST_LIFETIME);
    assert_eq!(
        run_child(&setup, "claude", "Explore", "child-b", 500),
        "applied"
    );
    let stopped: Value = serde_json::from_slice(
        &fs::read(presence_path(&setup, "claude", "child-b")).expect("child-b"),
    )
    .expect("child-b JSON");
    assert_eq!(stopped["status"], "stopped");
    assert!(presence_path(&setup, "claude", "child-a").exists());
    assert!(floor(&setup, "claude").is_none());
}

// Only `*.json` entries are records, as for every reader. A file an editor or
// the Finder left in agents/, or a directory, is not read and does not stop
// the binding's compaction, which would otherwise freeze with nothing to say so.
#[test]
fn a_file_that_is_not_a_record_does_not_stop_compaction() {
    let mut setup = bound("claude");
    run_child(&setup, "claude", "Explore", "child-a", 400);
    let agents = setup.binding_dir("claude", "parent").join("agents");
    fs::write(agents.join(".DS_Store"), b"finder").expect("write .DS_Store");
    fs::write(agents.join("notes.json.bak"), b"{").expect("write backup");
    fs::create_dir(agents.join("cache")).expect("create directory");
    // Dot-prefixed names the plugin never lists: macOS's AppleDouble file and
    // an editor's lock link. Neither can be a record, whose name is its key.
    let key = wezterm_attention::protocol::sha256_hex(b"child-a");
    fs::write(agents.join(format!("._{key}.json")), b"{").expect("write AppleDouble file");
    std::os::unix::fs::symlink("nowhere", agents.join(format!(".#{key}.json")))
        .expect("create lock link");
    setup.clock = at(PAST_LIFETIME);
    run_child(&setup, "claude", "Explore", "child-b", 500);
    assert!(!presence_path(&setup, "claude", "child-a").exists());
    assert!(agents.join(".DS_Store").exists() && agents.join("notes.json.bak").exists());
    assert!(agents.join("cache").is_dir());
    assert_eq!(
        floor(&setup, "claude").expect("floor written")["floor_mono_ns"],
        order(405)
    );
}

// A child record that cannot be read may be one the floor must not pass, so
// the floor stays where it is. A writer's own temporary file is not a record
// and does not stop it.
#[test]
fn an_unreadable_child_record_holds_the_floor() {
    let mut setup = bound("claude");
    run_child(&setup, "claude", "Explore", "child-a", 400);
    run_child(&setup, "claude", "Explore", "child-b", 410);
    let agents = setup.binding_dir("claude", "parent").join("agents");
    let leftover = agents.join(format!(
        ".{}.json.{}",
        wezterm_attention::protocol::sha256_hex(b"child-x"),
        Uuid::new_v4()
    ));
    fs::write(&leftover, b"{").expect("leftover");
    let unreadable = presence_path(&setup, "claude", "child-b");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod");
    setup.clock = at(PAST_LIFETIME);
    run_child(&setup, "claude", "Explore", "child-c", 500);
    assert!(presence_path(&setup, "claude", "child-a").exists());
    assert!(floor(&setup, "claude").is_none());

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600)).expect("chmod");
    run_child(&setup, "claude", "Explore", "child-d", 600);
    assert!(!presence_path(&setup, "claude", "child-a").exists());
    assert!(!unreadable.exists());
    assert!(
        leftover.exists(),
        "a temporary file is not compaction's to remove"
    );
    assert_eq!(
        floor(&setup, "claude").expect("floor written")["floor_mono_ns"],
        order(415)
    );
}
