//! Finding a provider session's other bindings -- for `bindings --socket` and
//! `inspect` -- reads the session index rather than walking every binding.
//! The index is derived state: a store written before it existed answers the
//! same by the walk, and `sweep --apply` completes it.

use super::pane_retention::{OP_1, OP_2, OP_3, actions, end_long_ago, pane_dir};
use super::*;
use wezterm_attention::query::read_bindings_for_socket_with_ports;
use wezterm_attention::records::{session_entry_path, session_index_path};

fn entry_of(setup: &Setup, pane_id: &str, session: &str, launch_id: &str) -> PathBuf {
    let (mut address, _) = pane_address(&setup.env).expect("address");
    address.pane_id = pane_id.to_owned();
    session_entry_path(
        &setup.root(),
        "claude",
        session,
        &address,
        launch_id,
        &binding_id("claude", session, launch_id),
    )
}

fn own_entry(setup: &Setup) -> PathBuf {
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    entry_of(setup, "42", "session-a", launch_id)
}

/// Writes a binding the way a writer from before the session index did:
/// the record alone, with no entry.
fn write_bare_binding(setup: &Setup, pane_id: &str, launch_id: &str, session: &str) {
    let (mut address, _) = pane_address(&setup.env).expect("address");
    address.pane_id = pane_id.to_owned();
    let binding = binding_id("claude", session, launch_id);
    atomic_replace(
        &launch_path(&setup.root(), &address, launch_id)
            .join("bindings")
            .join(&binding)
            .join("binding.json"),
        &json!({
            "kind":"binding","schema":3,"address":address,"launch_id":launch_id,
            "binding_id":binding,"event_id":Uuid::new_v4().to_string(),
            "provider":"claude","provider_session_id":session,"start_source":"resume",
            "observed_mono_ns":"00000000000000000300",
            "written_at_unix_ns":"00000000001000000000","writer_version":"1.0.0"
        }),
    )
    .expect("write bare binding");
}

/// Every pane the store names is listed, so a rival of session-a is live.
fn list_panes(setup: &Setup, panes: &[&str]) {
    setup.panes.set(
        panes
            .iter()
            .map(|pane| PaneRow {
                pane_id: (*pane).to_owned(),
                tty_name: Some("/dev/ttys888".to_owned()),
            })
            .collect(),
    );
}

/// The answers the session lookup feeds: every `bindings --socket` row and
/// diagnostic, and inspect's health for the fixture's own pane.
fn answers(setup: &Setup) -> (Value, String) {
    let (_, rows, diagnostics) = read_bindings_for_socket_with_ports(
        &setup.root(),
        &setup.env["WEZTERM_UNIX_SOCKET"],
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("bindings --socket");
    let (address, _) = pane_address(&setup.env).expect("address");
    let scope = PaneScope::new(
        address,
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        None,
    )
    .expect("scope");
    let facts = read_pane_facts_with_ports(
        &setup.root(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("inspect");
    (
        json!({"rows": rows, "diagnostics": diagnostics}),
        serde_json::to_value(facts.binding_health)
            .expect("health")
            .as_str()
            .expect("health text")
            .to_owned(),
    )
}

#[test]
fn a_bind_names_its_binding_in_the_index_of_a_store_it_started() {
    let setup = Setup::new();
    setup.claim_and_bind();
    assert!(session_index_path(&setup.root()).exists());
    let entry: Value =
        serde_json::from_slice(&fs::read(own_entry(&setup)).expect("entry")).expect("entry JSON");
    let (address, _) = pane_address(&setup.env).expect("address");
    assert_eq!(entry["kind"], "session_binding");
    assert_eq!(entry["address"], json!(address));
    assert_eq!(entry["launch_id"], setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]);
}

/// A store whose bindings came before the index, some with entries and some
/// without, answers by the walk; after `sweep --apply` it answers from the
/// index. Both answers are the same, a live rival in another pane included.
#[test]
fn the_indexed_answer_is_the_walked_answer_on_a_mixed_store() {
    let setup = Setup::new();
    setup.claim_and_bind();
    fs::remove_file(session_index_path(&setup.root())).expect("unmark the index");
    write_bare_binding(
        &setup,
        "99",
        "00000000-0000-4000-8000-000000000702",
        "session-a",
    );
    write_bare_binding(
        &setup,
        "98",
        "00000000-0000-4000-8000-000000000703",
        "session-b",
    );
    list_panes(&setup, &["42", "98", "99"]);

    let walked = answers(&setup);
    assert_eq!(walked.1, "conflicted", "the rival is found: {walked:?}");

    let (_, diagnostics) = setup.run_sweep(true, Some(OP_1));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(session_index_path(&setup.root()).exists());
    assert!(
        entry_of(
            &setup,
            "99",
            "session-a",
            "00000000-0000-4000-8000-000000000702"
        )
        .exists()
    );
    assert_eq!(answers(&setup), walked);
}

/// Once the index is marked complete, the lookup reads it and nothing else:
/// a binding it does not name is not looked for.
#[test]
fn a_complete_index_is_all_the_session_lookup_reads() {
    let setup = Setup::new();
    setup.claim_and_bind();
    write_bare_binding(
        &setup,
        "99",
        "00000000-0000-4000-8000-000000000702",
        "session-a",
    );
    list_panes(&setup, &["42", "99"]);
    assert_eq!(answers(&setup).1, "valid");
    fs::remove_file(session_index_path(&setup.root())).expect("unmark the index");
    assert_eq!(answers(&setup).1, "conflicted");
}

/// A sweep that could not read a binding leaves the index unmarked, so
/// readers keep walking.
#[test]
fn an_unreadable_binding_leaves_the_index_unmarked() {
    let setup = Setup::new();
    setup.claim_and_bind();
    fs::remove_file(session_index_path(&setup.root())).expect("unmark the index");
    let binding = setup.binding_dir().join("binding.json");
    fs::write(&binding, b"{").expect("break the binding");
    setup.run_sweep(true, Some(OP_1));
    assert!(!session_index_path(&setup.root()).exists());
}

#[test]
fn pane_retention_removes_the_entries_of_the_tree_it_removes() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    let entry = own_entry(&setup);
    assert!(entry.exists());
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_2));
    assert!(entry.exists(), "one sighting removes nothing");
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, _) = setup.run_sweep(true, Some(OP_3));
    assert_eq!(
        actions(&result.details, "pane_retention"),
        [&json!("prune")]
    );
    assert!(!pane_dir(&setup).exists());
    assert!(!entry.exists());
}

#[test]
fn binding_retention_removes_the_entry_of_the_binding_it_removes() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let old_entry = own_entry(&setup);
    setup.clock.set_unix(1);
    setup.provider_event(
        "SessionEnd",
        "session-a",
        json!({"reason":"other"}),
        "00000000000000000300",
    );
    setup.provider_event(
        "SessionStart",
        "session-b",
        json!({"source":"resume"}),
        "00000000000000000400",
    );
    let current_entry = entry_of(
        &setup,
        "42",
        "session-b",
        &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
    );
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    setup.run_sweep(true, Some(OP_1));
    assert!(!setup.binding_dir().exists());
    assert!(!old_entry.exists());
    assert!(current_entry.exists());
}

/// A bind whose entry cannot be written leaves no binding behind, so an index
/// marked complete still names every binding the store holds.
#[test]
fn a_bind_that_cannot_write_its_entry_leaves_no_binding() {
    let setup = Setup::new();
    setup.claim_and_bind();
    list_panes(&setup, &["42", "99"]);
    let launch_id = "00000000-0000-4000-8000-000000000702";
    // A directory at the entry's path makes the entry write fail.
    let entry = entry_of(&setup, "99", "session-a", launch_id);
    fs::create_dir_all(&entry).expect("block the entry");
    let mut env = setup.env.clone();
    env.insert("WEZTERM_PANE".to_owned(), "99".to_owned());
    env.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
        launch_id.to_owned(),
    );
    wezterm_attention::claim_launch(&env, &setup.ports()).expect("claim pane 99");
    let payload = json!({
        "session_id":"session-a",
        "transcript_path":"/tmp/session-a.jsonl",
        "cwd":"/tmp/project",
        "hook_event_name":"SessionStart",
        "source":"startup"
    });
    let event = parse_provider_event("claude", "SessionStart", &payload, &BTreeMap::new());
    assert!(apply_provider_event(&event, &env, "00000000000000000300", &setup.ports()).is_err());
    fs::remove_dir(&entry).expect("unblock the entry");

    let (address, _) = pane_address(&env).expect("address");
    let binding = launch_path(&setup.root(), &address, launch_id)
        .join("bindings")
        .join(binding_id("claude", "session-a", launch_id))
        .join("binding.json");
    assert!(!binding.exists(), "a binding the index does not name");
    let indexed = answers(&setup);
    fs::remove_file(session_index_path(&setup.root())).expect("unmark the index");
    assert_eq!(answers(&setup), indexed);
}
