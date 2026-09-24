//! The monotonic clock restarts at boot, so a binding recorded before a reboot
//! carries a larger stamp than anything written after it. An end that sweep
//! writes for such a binding after the reboot still ends it, for every reader.

use super::pane_retention::{OP_1, OP_2, OP_3, OP_4, actions, pane_dir};
use super::*;

const OP_5: &str = "00000000-0000-4000-8000-000000000915";

/// Uptime when the binding was recorded, before the reboot.
const BEFORE_REBOOT: &str = "09000000000000000000";
/// Uptime of the first sweep after the reboot.
const AFTER_REBOOT: u64 = 5_000;

/// Binds pane 42 before the reboot, then lets two sweeps after it end the
/// binding. Returns the end record sweep wrote.
fn end_after_a_reboot(setup: &Setup) -> Value {
    wezterm_attention::claim_launch(&setup.env, &setup.ports()).expect("claim");
    setup.provider_event(
        "SessionStart",
        "session-a",
        json!({"source":"startup"}),
        BEFORE_REBOOT,
    );
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(AFTER_REBOOT);
    let (first, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(
        actions(&first.details, "absence"),
        [&json!("first_absence")]
    );
    setup
        .clock
        .set_monotonic(AFTER_REBOOT + ABSENCE_INTERVAL_NS as u64);
    let (second, _) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(actions(&second.details, "absence"), [&json!("end")]);
    read_json(&setup.binding_dir().join("end.json"))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read record")).expect("record JSON")
}

fn inspect(setup: &Setup, env: &BTreeMap<String, String>) -> wezterm_attention::query::PaneFacts {
    let root = setup.root();
    let (address, _) = pane_address(env).expect("address");
    let launch_id = env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    let scope = PaneScope::new(address, launch_id, None).expect("inspect scope");
    read_pane_facts_with_ports(
        &root,
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("inspect")
}

#[test]
fn an_end_written_after_a_reboot_ends_the_binding_for_sweep_bindings_and_inspect() {
    let setup = Setup::new();
    let end = end_after_a_reboot(&setup);

    setup
        .clock
        .set_monotonic(AFTER_REBOOT + 2 * ABSENCE_INTERVAL_NS as u64);
    let (third, _) = setup.run_sweep(true, Some(OP_3));
    assert_eq!(
        actions(&third.details, "absence"),
        [&json!("already_ended")]
    );
    assert_eq!(
        read_json(&setup.binding_dir().join("end.json"))["event_id"],
        end["event_id"],
        "an ended binding's end is not written again"
    );

    let (rows, _) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].binding_phase, "ended");

    let facts = inspect(&setup, &setup.env);
    assert_eq!(
        facts.binding.as_ref().expect("binding row").binding_phase,
        "ended"
    );
    assert_eq!(
        facts.binding_end.availability,
        wezterm_attention::query::RecordAvailability::Present
    );
}

/// Pane retention counts its own two sightings after the end, and the end
/// here is from the new boot, so they follow it and the tree goes.
#[test]
fn a_pane_ended_after_a_reboot_is_retained_by_the_usual_rule() {
    let setup = Setup::new();
    end_after_a_reboot(&setup);
    let pane = pane_dir(&setup);
    setup
        .clock
        .set_unix(RETENTION_AGE_NS as u64 + 1_000_000_001);
    setup
        .clock
        .set_monotonic(AFTER_REBOOT + 2 * ABSENCE_INTERVAL_NS as u64);
    let (first, _) = setup.run_sweep(true, Some(OP_4));
    assert_eq!(
        actions(&first.details, "pane_retention"),
        [&json!("first_absence")]
    );
    setup
        .clock
        .set_monotonic(AFTER_REBOOT + 3 * ABSENCE_INTERVAL_NS as u64);
    let (second, _) = setup.run_sweep(true, Some(OP_5));
    assert_eq!(
        actions(&second.details, "pane_retention"),
        [&json!("prune")]
    );
    assert!(!pane.exists());
}

/// The same session resumed in another pane is not a conflict with a binding
/// that sweep ended, even while the ended pane is listed again.
#[test]
fn a_binding_ended_after_a_reboot_is_no_rival_for_its_session() {
    let setup = Setup::new();
    end_after_a_reboot(&setup);
    setup.panes.set(
        ["42", "99"]
            .into_iter()
            .map(|pane| PaneRow {
                pane_id: pane.to_owned(),
                tty_name: Some("/dev/ttys888".to_owned()),
            })
            .collect(),
    );
    setup.processes.set(Presence::Present);
    let mut resumed = setup.env.clone();
    resumed.insert("WEZTERM_PANE".into(), "99".into());
    resumed.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        "00000000-0000-4000-8000-000000000702".into(),
    );
    wezterm_attention::claim_launch(&resumed, &setup.ports()).expect("claim pane 99");
    let payload = json!({
        "session_id":"session-a","transcript_path":"/tmp/session.jsonl",
        "cwd":"/tmp/project","hook_event_name":"SessionStart","source":"resume"
    });
    let event = parse_provider_event("claude", "SessionStart", &payload, &BTreeMap::new());
    apply_provider_event(&event, &resumed, "00000000000000009000", &setup.ports())
        .expect("bind pane 99");

    let (rows, diagnostics) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.binding_health == "valid"),
        "{rows:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|item| item.code == "binding_conflict")
    );

    let (_, socket_rows, _) = wezterm_attention::query::read_bindings_for_socket_with_ports(
        &setup.root(),
        &setup.env["WEZTERM_UNIX_SOCKET"],
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("bindings --socket");
    assert!(
        socket_rows.iter().all(|row| row.binding_health == "valid"),
        "{socket_rows:?}"
    );

    let facts = inspect(&setup, &resumed);
    assert_eq!(
        facts.binding_health,
        wezterm_attention::query::BindingHealth::Valid
    );
}
