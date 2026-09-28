use super::*;
use wezterm_attention::query::{ChildCoverage, PaneFacts, PaneScope, read_pane_facts_with_ports};
use wezterm_attention::records::FileRecords;

pub(super) const SESSION: &str = "parent";

pub(super) fn bound(provider: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            provider,
            "SessionStart",
            SESSION,
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}

pub(super) fn child(
    provider: &str,
    name: &str,
    agent: &str,
    agent_type: Option<&str>,
) -> ProviderEvent {
    let mut patch = json!({"agent_id":agent});
    if let Some(agent_type) = agent_type {
        patch["agent_type"] = json!(agent_type);
    }
    match name {
        "SubagentStop" => patch["stop_hook_active"] = json!(false),
        "SubagentStart" => {}
        _ => patch["tool_name"] = json!("Bash"),
    }
    event(provider, name, SESSION, patch)
}

pub(super) fn lead(provider: &str, name: &str) -> ProviderEvent {
    let patch = match name {
        "PreToolUse" => json!({"tool_name":"Bash"}),
        "Stop" => json!({"stop_hook_active":false}),
        _ => json!({}),
    };
    event(provider, name, SESSION, patch)
}

fn set_path(setup: &Setup, provider: &str) -> PathBuf {
    setup.binding_dir(provider, SESSION).join("children.json")
}

/// The binding's live children as `(agent_id, status)`, in the set's order.
pub(super) fn live(setup: &Setup, provider: &str) -> Vec<(String, String)> {
    let Ok(bytes) = fs::read(set_path(setup, provider)) else {
        return vec![];
    };
    let set: Value = serde_json::from_slice(&bytes).expect("child set JSON");
    set["live"]
        .as_array()
        .expect("live children")
        .iter()
        .map(|child| {
            (
                child["agent_id"].as_str().unwrap().to_owned(),
                child["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn running(agent: &str) -> (String, String) {
    (agent.to_owned(), "running".to_owned())
}

fn waiting(agent: &str) -> (String, String) {
    (agent.to_owned(), "waiting".to_owned())
}

pub(super) fn facts(setup: &Setup, provider: &str) -> PaneFacts {
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let scope = PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        launch_id.clone(),
        Some(binding_id(provider, SESSION, launch_id)),
    )
    .unwrap();
    read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap()
}

fn shown(setup: &Setup, provider: &str) -> String {
    let activity: Value = serde_json::from_slice(
        &fs::read(setup.binding_dir(provider, SESSION).join("activity.json")).expect("activity"),
    )
    .expect("activity JSON");
    activity["type"].as_str().unwrap().to_owned()
}

// A child running one long command sends nothing until it finishes. Nothing
// removes it for being quiet, however late the reader looks.
#[test]
fn a_quiet_running_child_stays_counted() {
    for (provider, agent_type) in [("claude", "Explore"), ("codex", "worker")] {
        let mut setup = bound(provider);
        setup.apply(
            &child(provider, "SubagentStart", "child-a", Some(agent_type)),
            "00000000000000000300",
        );
        setup.apply(
            &child(provider, "PreToolUse", "child-a", Some(agent_type)),
            "00000000000000000400",
        );
        setup.clock = FixedClock {
            monotonic: "00000000000000000500",
            unix: "00000099999999999999",
        };
        assert_eq!(
            live(&setup, provider),
            vec![running("child-a")],
            "{provider}"
        );
        let facts = facts(&setup, provider);
        assert_eq!(facts.children.count, 1, "{provider}");
        assert_eq!(facts.children.coverage, ChildCoverage::Known, "{provider}");
    }
}

#[test]
fn a_child_shows_from_its_start_and_leaves_at_its_stop() {
    let setup = bound("claude");
    assert_eq!(
        setup
            .apply(
                &child("claude", "SubagentStart", "child-a", Some("Explore")),
                "00000000000000000300",
            )
            .disposition,
        "applied"
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-a")]);
    setup.apply(
        &child("claude", "SubagentStop", "child-a", Some("Explore")),
        "00000000000000000400",
    );
    assert!(live(&setup, "claude").is_empty());
    assert_eq!(facts(&setup, "claude").children.count, 0);
}

// Claude Code 2.1.283 starts a resumed sub-agent as a new run under the same
// id.
#[test]
fn a_resumed_child_is_counted_again() {
    let setup = bound("claude");
    for (name, order) in [
        ("SubagentStart", "00000000000000000300"),
        ("SubagentStop", "00000000000000000400"),
        ("SubagentStart", "00000000000000000500"),
    ] {
        setup.apply(&child("claude", name, "child-a", Some("Explore")), order);
    }
    assert_eq!(live(&setup, "claude"), vec![running("child-a")]);
}

#[test]
fn an_older_child_event_does_not_overwrite_a_newer_one() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PermissionRequest", "child-a", Some("Explore")),
        "00000000000000000500",
    );
    assert_eq!(
        setup
            .apply(
                &child("claude", "PreToolUse", "child-a", Some("Explore")),
                "00000000000000000400",
            )
            .disposition,
        "ignored"
    );
    assert_eq!(live(&setup, "claude"), vec![waiting("child-a")]);
}

// The provider runs agents of its own. In Claude Code 2.1.283 the stop of one
// carried an empty agent type and no start came before it, and one sent a tool
// event with an agent id and no type at all. Neither is a sub-agent anyone
// asked for.
#[test]
fn an_agent_the_provider_runs_for_itself_is_not_counted() {
    let setup = bound("claude");
    let stop = setup.apply(
        &child("claude", "SubagentStop", "internal", Some("")),
        "00000000000000000300",
    );
    assert_eq!(stop.disposition, "skipped");
    assert!(stop.diagnostic.is_none(), "{:?}", stop.diagnostic);
    assert!(!set_path(&setup, "claude").exists());
    let tool = setup.apply(
        &child("claude", "PreToolUse", "internal", None),
        "00000000000000000400",
    );
    assert_eq!(tool.disposition, "skipped");
    assert_eq!(
        tool.diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("record_invalid")
    );
    assert!(live(&setup, "claude").is_empty());
}

// Whether a child waits for the user comes only from its permission request,
// never from the name of its agent type.
#[test]
fn a_child_whose_type_is_named_permission_is_not_waiting() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PermissionRequest", "child-b", Some("Explore")),
        "00000000000000000300",
    );
    assert_eq!(shown(&setup, "claude"), "notify");
    setup.apply(
        &child("claude", "PreToolUse", "child-b", Some("Explore")),
        "00000000000000000400",
    );
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("permission")),
        "00000000000000000500",
    );
    assert_eq!(
        live(&setup, "claude"),
        vec![running("child-b"), running("child-a")]
    );
    setup.apply(&lead("claude", "PreToolUse"), "00000000000000000600");
    assert_eq!(shown(&setup, "claude"), "thinking");
}

#[test]
fn a_finished_tool_does_not_end_a_childs_wait() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PermissionRequest", "child-a", Some("Explore")),
        "00000000000000000400",
    );
    setup.apply(
        &child("claude", "PostToolUse", "child-a", Some("Explore")),
        "00000000000000000450",
    );
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000460",
    );
    assert_eq!(live(&setup, "claude"), vec![waiting("child-a")]);
    assert_eq!(facts(&setup, "claude").children.waiting, 1);
}

// Codex's parent stop ends the children it covers. That rests on a Codex
// parent stopping only once its children have, which Codex at source
// `985cf47a4` does not enforce (see docs/accepted-limitations.md); a covered
// child that works again
// afterwards is counted and reported.
#[test]
fn a_codex_parent_stop_ends_the_children_it_covers() {
    let setup = bound("codex");
    setup.apply(
        &child("codex", "PreToolUse", "child-a", Some("worker")),
        "00000000000000000300",
    );
    assert_eq!(
        setup
            .apply(&lead("codex", "Stop"), "00000000000000000400")
            .disposition,
        "applied"
    );
    setup.apply(
        &child("codex", "PreToolUse", "child-b", Some("worker")),
        "00000000000000000500",
    );
    assert_eq!(live(&setup, "codex"), vec![running("child-b")]);
    let delayed = setup.apply(
        &child("codex", "PreToolUse", "child-c", Some("worker")),
        "00000000000000000350",
    );
    assert_eq!(delayed.disposition, "ignored");
    let again = setup.apply(
        &child("codex", "PreToolUse", "child-a", Some("worker")),
        "00000000000000000600",
    );
    assert_eq!(again.disposition, "applied");
    assert_eq!(
        again
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("child_active_after_parent_clear")
    );
    assert_eq!(
        live(&setup, "codex"),
        vec![running("child-b"), running("child-a")]
    );
}

// The binding's end record marks where a lifetime ends. A resumed session
// refreshes the binding; children of the ended lifetime are not counted, even
// before any child write has applied that end to the set.
#[test]
fn children_of_an_ended_lifetime_are_never_counted() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000300",
    );
    setup.apply(
        &event("claude", "SessionEnd", SESSION, json!({"reason":"other"})),
        "00000000000000000400",
    );
    let ended = facts(&setup, "claude");
    assert_eq!(ended.children.count, 0);
    assert_eq!(ended.children.coverage, ChildCoverage::Ended);
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            SESSION,
            json!({"source":"resume"}),
        ),
        "00000000000000000500",
    );
    let resumed = facts(&setup, "claude");
    assert_eq!(resumed.children.count, 0);
    assert_eq!(resumed.children.coverage, ChildCoverage::Known);
    setup.apply(
        &child("claude", "SubagentStart", "child-c", Some("Explore")),
        "00000000000000000600",
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-c")]);
    assert_eq!(facts(&setup, "claude").children.count, 1);
}

#[test]
fn a_child_event_after_the_binding_ended_is_ignored() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000300",
    );
    setup.apply(
        &event("claude", "SessionEnd", SESSION, json!({"reason":"other"})),
        "00000000000000000400",
    );
    let late = setup.apply(
        &child("claude", "PreToolUse", "child-b", Some("Explore")),
        "00000000000000000450",
    );
    assert_eq!(late.disposition, "ignored");
    assert!(live(&setup, "claude").is_empty());
}

#[test]
fn an_invalid_child_set_is_moved_aside_and_started_again() {
    let setup = bound("claude");
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    let facts_before = facts(&setup, "claude");
    assert_eq!(facts_before.children.coverage, ChildCoverage::Invalid);
    assert_eq!(
        setup
            .apply(
                &child("claude", "SubagentStart", "child-a", Some("Explore")),
                "00000000000000000300",
            )
            .disposition,
        "applied"
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-a")]);
    let aside = moved_aside(&path);
    assert_eq!(aside.len(), 1);
    assert_eq!(fs::read(&aside[0]).unwrap(), b"{not json");
}

// A stop for a child the set does not hold changes nothing, yet it still
// replaces an invalid set, so the set is moved aside once and the next event
// reads the new one.
#[test]
fn an_event_that_changes_nothing_still_replaces_an_invalid_child_set() {
    let setup = bound("claude");
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    for (order, reported) in [
        ("00000000000000000300", Some("record_invalid")),
        ("00000000000000000400", None),
    ] {
        let stop = setup.apply(
            &child("claude", "SubagentStop", "child-a", Some("Explore")),
            order,
        );
        assert_eq!(stop.disposition, "skipped");
        assert_eq!(
            stop.diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            reported
        );
    }
    assert!(live(&setup, "claude").is_empty());
    assert_eq!(moved_aside(&path).len(), 1);
    assert_eq!(
        facts(&setup, "claude").children.coverage,
        ChildCoverage::Known
    );
}

// Whatever the child set's transition says, a hook that started the set again
// reports the restart instead, and keeps what it replaced.
#[test]
fn a_restarted_child_set_is_reported_instead_of_the_events_own_diagnostic() {
    let setup = bound("claude");
    setup.apply(
        &event("claude", "SessionEnd", SESSION, json!({"reason":"other"})),
        "00000000000000000300",
    );
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    let late = setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        "00000000000000000400",
    );
    assert_eq!(late.disposition, "ignored");
    let diagnostic = late.diagnostic.expect("a diagnostic");
    assert_eq!(diagnostic.code, "record_invalid");
    assert_eq!(diagnostic.context["replaced"]["code"], "binding_conflict");
    assert_eq!(moved_aside(&path).len(), 1);
}

// Moving an invalid set aside moves the file as it stands: a link is moved as
// a link, and what it points to is neither copied nor changed.
#[test]
fn an_invalid_child_set_that_is_a_link_is_moved_aside_as_a_link() {
    let setup = bound("claude");
    let path = set_path(&setup, "claude");
    let target = path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("elsewhere.json");
    fs::write(&target, b"{not json").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000300",
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-a")]);
    assert!(
        !fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let aside = moved_aside(&path);
    assert_eq!(aside.len(), 1);
    assert!(
        fs::symlink_metadata(&aside[0])
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"{not json");
}

// Readers refuse a record larger than `max_json_bytes`, so the writer never
// writes one: an event that would grow the set past it is refused, and the
// set stays as it was. A permission request still shows its notify.
#[test]
fn a_child_that_would_grow_the_set_past_what_readers_accept_is_refused() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "child-0", Some("Explore")),
        "00000000000000000300",
    );
    let path = set_path(&setup, "claude");
    let mut set: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let entry = set["live"][0].clone();
    let limit = wezterm_attention::protocol::manifest()
        .unwrap()
        .limits
        .max_json_bytes;
    let encoded = |set: &Value| serde_json::to_vec(set).unwrap().len() + 1;
    // Entries of one width, so the count that fits is a division.
    let named = |index: usize| {
        let mut next = entry.clone();
        next["agent_id"] = json!(format!("c{index:06}"));
        next
    };
    set["live"] = json!([named(0)]);
    let width = serde_json::to_vec(&named(0)).unwrap().len() + 1;
    let fitting = (limit - encoded(&set)) / width + 1;
    set["live"] = Value::Array((0..fitting).map(named).collect());
    assert!(encoded(&set) <= limit && encoded(&set) + width > limit);
    let mut bytes = serde_json::to_vec(&set).unwrap();
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        facts(&setup, "claude").children.coverage,
        ChildCoverage::Known
    );
    let error = apply_provider_event(
        &child("claude", "SubagentStart", &"n".repeat(200), Some("Explore")),
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    )
    .expect_err("a set past the bound is refused");
    assert_eq!(error.diagnostic.code, "record_invalid");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let asking = setup.apply(
        &child(
            "claude",
            "PermissionRequest",
            &"n".repeat(200),
            Some("Explore"),
        ),
        "00000000000000000500",
    );
    assert_eq!(asking.disposition, "partial");
    assert_eq!(shown(&setup, "claude"), "notify");
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

// A Codex child that works again after its parent's stop removed it is
// reported whichever event brings it back.
#[test]
fn a_child_back_after_a_codex_parent_stop_is_reported_whichever_event_brings_it() {
    for name in ["PreToolUse", "PermissionRequest"] {
        let setup = bound("codex");
        setup.apply(
            &child("codex", "PreToolUse", "child-a", Some("worker")),
            "00000000000000000300",
        );
        setup.apply(&lead("codex", "Stop"), "00000000000000000400");
        let back = setup.apply(
            &child("codex", name, "child-a", Some("worker")),
            "00000000000000000500",
        );
        assert_eq!(back.disposition, "applied", "{name}");
        assert_eq!(
            back.diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("child_active_after_parent_clear"),
            "{name}"
        );
        let set: Value =
            serde_json::from_slice(&fs::read(set_path(&setup, "codex")).unwrap()).unwrap();
        assert_eq!(set["parent_clear"]["removed"], json!([]), "{name}");
    }
}

// A child hook stamped after the lead's end can take the lock before it. The
// set it wrote has not applied that end, and neither readers nor the next
// write count what it holds.
#[test]
fn a_set_written_before_the_end_counts_nothing_after_a_resume() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000300",
    );
    setup.apply(
        &child("claude", "PreToolUse", "child-b", Some("Explore")),
        "00000000000000000450",
    );
    setup.apply(
        &event("claude", "SessionEnd", SESSION, json!({"reason":"other"})),
        "00000000000000000400",
    );
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            SESSION,
            json!({"source":"resume"}),
        ),
        "00000000000000000500",
    );
    assert_eq!(facts(&setup, "claude").children.count, 0);
    // A write that changes nothing else applies the end, and the count the
    // readers showed stays what it was.
    setup.apply(
        &child("claude", "SubagentStop", "child-z", Some("Explore")),
        "00000000000000000550",
    );
    assert!(live(&setup, "claude").is_empty());
    assert_eq!(facts(&setup, "claude").children.count, 0);
    setup.apply(
        &child("claude", "SubagentStart", "child-c", Some("Explore")),
        "00000000000000000600",
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-c")]);
    assert_eq!(facts(&setup, "claude").children.count, 1);
}

// Both readers hold a set naming another provider than its binding invalid,
// so the writer does too: it moves the set aside and starts again rather than
// relabelling the set and keeping what it held.
#[test]
fn a_child_set_naming_another_provider_is_moved_aside_and_started_again() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "foreign-a", Some("Explore")),
        "00000000000000000300",
    );
    let path = set_path(&setup, "claude");
    let mut set: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    set["provider"] = json!("codex");
    fs::write(&path, serde_json::to_vec(&set).unwrap()).unwrap();
    let start = setup.apply(
        &child("claude", "SubagentStart", "child-b", Some("Explore")),
        "00000000000000000400",
    );
    assert_eq!(
        start
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("record_invalid")
    );
    assert_eq!(live(&setup, "claude"), vec![running("child-b")]);
    assert_eq!(moved_aside(&path).len(), 1);
}

// A waiting child in a set naming another provider holds no notify, since no
// reader counts that set.
#[test]
fn a_set_naming_another_provider_holds_no_notify() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PermissionRequest", "child-a", Some("Explore")),
        "00000000000000000400",
    );
    assert_eq!(shown(&setup, "claude"), "notify");
    let path = set_path(&setup, "claude");
    let mut set: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    set["provider"] = json!("codex");
    fs::write(&path, serde_json::to_vec(&set).unwrap()).unwrap();
    setup.apply(&lead("claude", "PreToolUse"), "00000000000000000500");
    assert_eq!(shown(&setup, "claude"), "thinking");
}

// A child the latest Codex parent stop removed, stopping after that stop,
// shows the parent stopped first. A stop stamped before the parent's stop
// only arrived late, and says nothing.
#[test]
fn a_removed_child_that_stops_after_its_parent_is_reported() {
    let setup = bound("codex");
    for (agent, order) in [
        ("child-a", "00000000000000000300"),
        ("child-b", "00000000000000000320"),
    ] {
        setup.apply(&child("codex", "PreToolUse", agent, Some("worker")), order);
    }
    setup.apply(&lead("codex", "Stop"), "00000000000000000400");
    for (agent, order, reported) in [
        ("child-b", "00000000000000000350", None),
        (
            "child-a",
            "00000000000000000500",
            Some("child_active_after_parent_clear"),
        ),
    ] {
        let stop = setup.apply(
            &child("codex", "SubagentStop", agent, Some("worker")),
            order,
        );
        assert_eq!(stop.disposition, "skipped", "{agent}");
        assert_eq!(
            stop.diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code.as_str()),
            reported,
            "{agent}"
        );
    }
}

// A permission request whose notify an activity clear covers still reports
// that it moved an invalid set aside and started again.
#[test]
fn a_restart_is_reported_when_the_requests_notify_is_ignored() {
    let setup = bound("claude");
    setup.apply(&lead("claude", "PreToolUse"), "00000000000000000300");
    prompt_return(&setup.env, "00000000000000000400").expect("prompt return");
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    let asking = setup.apply(
        &child("claude", "PermissionRequest", "child-a", Some("Explore")),
        "00000000000000000350",
    );
    assert_eq!(asking.disposition, "ignored");
    assert_eq!(
        asking
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("record_invalid")
    );
    assert_eq!(moved_aside(&path).len(), 1);
}

// A set that cannot be moved aside is left as it was: a child's own event is
// refused, and a permission request still shows its notify, with native state
// rejected so no consumer is given it.
#[cfg(target_os = "macos")]
#[test]
fn an_invalid_child_set_that_cannot_be_moved_aside_is_left_as_it_was() {
    let setup = bound("claude");
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    let chflags = |flag: &str| {
        assert!(
            Command::new("chflags")
                .arg(flag)
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
    };
    chflags("uchg");
    let started = apply_provider_event(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    let asking = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &child("claude", "PermissionRequest", "child-a", Some("Explore")),
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    chflags("nouchg");
    assert_eq!(
        started
            .expect_err("a set that cannot be moved aside refuses the event")
            .diagnostic
            .code,
        "probe_unavailable"
    );
    assert_eq!(
        asking.persistence.native_state,
        wezterm_attention::lifecycle::outcome::Persistence::Rejected
    );
    assert_eq!(
        asking.result.expect("the request applies").disposition,
        "partial"
    );
    assert_eq!(shown(&setup, "claude"), "notify");
    assert_eq!(fs::read(&path).unwrap(), b"{not json");
    assert!(moved_aside(&path).is_empty());
}

// A hook reports one diagnostic. When the event's lifecycle observation also
// fails, that failure is what makes the hook partial, so it is what the hook
// reports, over what the child set says, on this path as on a child's own.
#[test]
fn a_failed_lifecycle_observation_is_reported_over_the_child_sets_diagnostic() {
    let setup = bound("codex");
    setup.apply(
        &child("codex", "PreToolUse", "child-a", Some("worker")),
        "00000000000000000300",
    );
    setup.apply(&lead("codex", "Stop"), "00000000000000000400");
    let asking = setup.apply(
        &event(
            "codex",
            "PermissionRequest",
            SESSION,
            json!({"agent_id":"child-a","agent_type":"worker","tool_name":5}),
        ),
        "00000000000000000500",
    );
    assert_eq!(asking.disposition, "partial");
    let diagnostic = asking.diagnostic.expect("a diagnostic");
    assert_eq!(
        (diagnostic.code.as_str(), diagnostic.message.as_str()),
        ("record_invalid", "tool_name is missing or too long")
    );
    assert_eq!(
        diagnostic.context["replaced"]["code"],
        "child_active_after_parent_clear"
    );
    assert_eq!(live(&setup, "codex"), vec![waiting("child-a")]);
}

// The same order holds for a child's own event: a failed lifecycle
// observation is reported, and the restart it replaced is kept.
#[test]
fn a_childs_own_event_reports_a_failed_lifecycle_observation_over_a_restart() {
    let setup = bound("claude");
    let path = set_path(&setup, "claude");
    fs::write(&path, b"{not json").unwrap();
    let working = setup.apply(
        &event(
            "claude",
            "PreToolUse",
            SESSION,
            json!({"agent_id":"child-a","agent_type":"Explore","tool_name":5}),
        ),
        "00000000000000000300",
    );
    assert_eq!(working.disposition, "partial");
    let diagnostic = working.diagnostic.expect("a diagnostic");
    assert_eq!(
        (diagnostic.code.as_str(), diagnostic.message.as_str()),
        ("record_invalid", "tool_name is missing or too long")
    );
    assert_eq!(
        diagnostic.context["replaced"]["message"],
        "an invalid child presence set was moved aside and started again"
    );
    assert_eq!(moved_aside(&path).len(), 1);
}

// Three reasons in one hook: the activity's own, the child transition's, and
// the restart of an invalid set. The restart is reported, and the other two
// stay in order in the chain under it.
#[test]
fn a_restart_keeps_the_transitions_and_the_activitys_diagnostics_in_order() {
    let setup = bound("claude");
    setup.apply(&lead("claude", "PreToolUse"), "00000000000000000300");
    fs::write(set_path(&setup, "claude"), b"{not json").unwrap();
    let asking = setup.apply(
        &child("claude", "PermissionRequest", "child-a", None),
        "00000000000000000300",
    );
    let diagnostic = serde_json::to_value(asking.diagnostic.expect("a diagnostic")).unwrap();
    let messages: Vec<_> = [
        "/message",
        "/context/replaced/message",
        "/context/replaced/context/replaced/message",
    ]
    .iter()
    .map(|pointer| diagnostic.pointer(pointer).and_then(Value::as_str))
    .collect();
    assert_eq!(
        messages,
        [
            Some("an invalid child presence set was moved aside and started again"),
            Some("child event has no agent type and follows no start"),
            Some("equal activity order has different content"),
        ]
    );
}

/// The invalid sets moved aside beside `path`.
fn moved_aside(path: &std::path::Path) -> Vec<PathBuf> {
    fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".children.json.invalid.")
        })
        .map(|entry| entry.path())
        .collect()
}

#[test]
fn a_child_set_from_a_newer_writer_is_never_overwritten() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "SubagentStart", "child-a", Some("Explore")),
        "00000000000000000300",
    );
    let path = set_path(&setup, "claude");
    let mut set: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    set["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&set).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let error = apply_provider_event(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    )
    .expect_err("a newer set is refused");
    assert_eq!(error.diagnostic.code, "future_schema");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        facts(&setup, "claude").children.coverage,
        ChildCoverage::Unsupported
    );
}

// The parent's stop is the lead's own activity. A child set it cannot change
// is reported beside it, and the stop still shows. Not every native effect the
// stop selected was written, so its consumer is not told they were.
#[test]
fn a_codex_parent_stop_applies_beside_a_child_set_it_cannot_change() {
    let setup = bound("codex");
    setup.apply(
        &child("codex", "PreToolUse", "child-a", Some("worker")),
        "00000000000000000300",
    );
    let path = set_path(&setup, "codex");
    let mut set: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    set["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&set).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let outcome = wezterm_attention::lifecycle::apply_provider_event_with_outcome(
        &lead("codex", "Stop"),
        &setup.env,
        "00000000000000000400",
        &setup.ports(),
    );
    assert_eq!(
        outcome.persistence.native_state,
        wezterm_attention::lifecycle::outcome::Persistence::Rejected
    );
    assert_eq!(
        outcome.persistence.activity,
        wezterm_attention::lifecycle::outcome::Persistence::Confirmed
    );
    assert!(matches!(
        wezterm_attention::consumer::delivery_bytes(
            &outcome,
            wezterm_attention::hook_content::HookContent::NotRequested,
            wezterm_attention::hook_content::HookContent::NotRequested,
        ),
        Err(wezterm_attention::consumer::NotDispatchedReason::NativeStateRejected)
    ));
    let stop = outcome.result.expect("the stop applies");
    assert_eq!(stop.disposition, "partial");
    assert_eq!(
        stop.diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("future_schema")
    );
    assert_eq!(shown(&setup, "codex"), "stop");
    assert_eq!(fs::read(&path).unwrap(), before);
}

// The real binary, registered as the README shows, from a child's start to
// its stop.
#[test]
fn the_hook_binary_records_a_child_from_start_to_stop() {
    for (provider, agent_type) in [("claude", "Explore"), ("codex", "worker")] {
        let setup = bound(provider);
        for name in ["SubagentStart", "SubagentStop"] {
            let output = run_hook(
                &setup,
                &["hooks", "event", provider, name, "--strict"],
                &payload(
                    provider,
                    name,
                    SESSION,
                    json!({"agent_id":"child-a","agent_type":agent_type,"stop_hook_active":false}),
                ),
            );
            assert!(
                output.status.success(),
                "{provider} {name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = usize::from(name == "SubagentStart");
            assert_eq!(live(&setup, provider).len(), expected, "{provider} {name}");
            assert_eq!(facts(&setup, provider).children.count, expected);
        }
    }
}
