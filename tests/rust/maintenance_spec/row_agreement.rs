//! `bindings` lists a row and `inspect` reads it in depth; a consumer moves
//! from one to the other, so the row's assessment must read the same in both.

use super::*;
use wezterm_attention::query::{BindingHealth, PaneFacts, ReaderConfidence, ScopeRelation};

fn current_scope(setup: &Setup, session: &str) -> PaneScope {
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    PaneScope::new(
        pane_address(&setup.env).expect("address").0,
        launch_id.clone(),
        Some(binding_id("claude", session, &launch_id)),
    )
    .expect("scope")
}

fn inspect(setup: &Setup, scope: &PaneScope) -> PaneFacts {
    read_pane_facts_with_ports(
        &setup.root(),
        scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("inspect")
}

/// The bindings row for `scope`, and inspect's answer, must carry the same
/// health and confidence. Returns inspect's answer for further checks.
fn assert_agree(setup: &Setup, scope: &PaneScope) -> PaneFacts {
    let (rows, _) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    let row = rows
        .iter()
        .find(|row| Some(row.binding_id.as_str()) == scope.binding_id())
        .expect("bindings row for the scope");
    let facts = inspect(setup, scope);
    assert_eq!(
        serde_json::to_value(facts.binding_health).expect("health"),
        json!(row.binding_health),
        "binding_health differs; inspect diagnostics: {:?}",
        facts.diagnostics
    );
    assert_eq!(
        serde_json::to_value(facts.reader_confidence).expect("confidence"),
        json!(row.reader_confidence),
        "reader_confidence differs"
    );
    assert_eq!(
        serde_json::to_value(facts.pane_presence).expect("presence"),
        json!(row.pane_presence)
    );
    facts
}

/// A record beside the binding that cannot be read is a diagnostic about that
/// record. It does not make the binding invalid, and it does not stop a reader
/// from acting on a pane that is present and currently bound.
#[test]
fn an_unreadable_record_beside_the_binding_is_a_diagnostic_not_binding_health() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let scope = current_scope(&setup, "session-a");
    let (address, _) = pane_address(&setup.env).expect("address");
    let reviews = pane_path(&setup.root(), &address).join("reviews");
    fs::create_dir_all(&reviews).expect("reviews");
    fs::write(reviews.join(format!("{}.json", "a".repeat(64))), "not json").expect("review");
    let facts = assert_agree(&setup, &scope);
    assert_eq!(facts.binding_health, BindingHealth::Valid);
    assert_eq!(facts.reader_confidence, ReaderConfidence::Confirmed);
    assert!(!facts.complete(), "the unreadable review is still reported");
    assert!(facts.diagnostics.iter().any(|d| d.code == "record_invalid"));

    fs::write(setup.binding_dir().join("agents"), "not a directory").expect("agents");
    let facts = assert_agree(&setup, &scope);
    assert_eq!(facts.binding_health, BindingHealth::Valid);
    assert_eq!(facts.reader_confidence, ReaderConfidence::Confirmed);
}

/// An unreadable end record is part of the binding itself, and both answers
/// call the binding invalid for it.
#[test]
fn an_unreadable_end_record_is_invalid_health_in_both_answers() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let scope = current_scope(&setup, "session-a");
    fs::write(setup.binding_dir().join("end.json"), "not json").expect("end");
    let facts = assert_agree(&setup, &scope);
    assert_eq!(facts.binding_health, BindingHealth::Invalid);
}

/// A provider session bound at two live pane addresses is a conflict whichever
/// command looks at it.
#[test]
fn a_conflicted_row_reads_conflicted_through_inspect() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let mut other = address.clone();
    other.pane_id = "99".to_owned();
    let launch_id = "00000000-0000-4000-8000-000000000702";
    let other_binding = binding_id("claude", "session-a", launch_id);
    atomic_replace(
        &launch_path(&root, &other, launch_id)
            .join("bindings")
            .join(&other_binding)
            .join("binding.json"),
        &json!({
            "kind":"binding","schema":3,"address":other,"launch_id":launch_id,
            "binding_id":other_binding,"event_id":Uuid::new_v4().to_string(),
            "provider":"claude","provider_session_id":"session-a","start_source":"resume",
            "observed_mono_ns":"00000000000000000300",
            "written_at_unix_ns":"00000000001000000000","writer_version":"2.0.0"
        }),
    )
    .expect("write the second binding");
    setup.panes.set(vec![
        PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some("/dev/ttys888".to_owned()),
        },
        PaneRow {
            pane_id: "99".to_owned(),
            tty_name: Some("/dev/ttys889".to_owned()),
        },
    ]);
    let facts = assert_agree(&setup, &current_scope(&setup, "session-a"));
    assert_eq!(facts.binding_health, BindingHealth::Conflicted);

    // The other pane is gone: history, not a rival.
    setup.panes.set(vec![PaneRow {
        pane_id: "42".to_owned(),
        tty_name: Some("/dev/ttys888".to_owned()),
    }]);
    setup.processes.set(Presence::Absent);
    let facts = assert_agree(&setup, &current_scope(&setup, "session-a"));
    assert_eq!(facts.binding_health, BindingHealth::Valid);
}

/// A scope whose launch is no longer the pane's claim is stale, and a stale
/// row is not a broken one: `bindings` lists it as valid and not current.
#[test]
fn a_stale_launch_is_unconfirmed_not_invalid() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let stale = PaneScope::new(
        pane_address(&setup.env).expect("address").0,
        "00000000-0000-4000-8000-000000000799".to_owned(),
        None,
    )
    .expect("scope");
    let facts = inspect(&setup, &stale);
    assert_eq!(facts.scope_relation, ScopeRelation::LaunchChanged);
    assert_eq!(facts.binding_health, BindingHealth::Valid);
    assert_eq!(facts.reader_confidence, ReaderConfidence::Unconfirmed);
}

struct SlowPanes;

impl PaneLister for SlowPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        std::thread::sleep(std::time::Duration::from_millis(150));
        Ok(vec![PaneRow {
            pane_id: "42".to_owned(),
            tty_name: None,
        }])
    }
}

/// An inspect that took seconds says which phase took them, as a bindings
/// answer does: the pane listing, the process probe, or the record reads.
#[test]
fn inspect_says_where_its_time_went() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let facts = read_pane_facts_with_ports(
        &setup.root(),
        &current_scope(&setup, "session-a"),
        &FileRecords,
        &setup.clock,
        Some(&SlowPanes),
        Some(&setup.processes),
    )
    .expect("inspect");
    let value = serde_json::to_value(&facts).expect("facts serialize");
    let timing = &value["timing_ms"];
    for phase in ["pane_list", "process_list", "records"] {
        assert!(timing[phase].is_u64(), "{phase}: {timing}");
    }
    assert!(timing["pane_list"].as_u64().unwrap() >= 150, "{timing}");
    assert!(timing["records"].as_u64().unwrap() < 150, "{timing}");
    assert_eq!(timing["process_list"], 0, "the pane was listed: {timing}");
}
