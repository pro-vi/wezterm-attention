//! A pane's records outlive the pane. When its mux server is gone, or the
//! pane closed, sweep ends its binding by the two-observation absence rule,
//! and once that binding has been over for the retention age, sweep --apply
//! removes the pane's whole tree. Reads never delete anything.

use super::lock_scope::LockCheckingPanes;
use super::*;

const OP_1: &str = "00000000-0000-4000-8000-000000000911";
const OP_2: &str = "00000000-0000-4000-8000-000000000912";
const OP_3: &str = "00000000-0000-4000-8000-000000000913";
const OP_4: &str = "00000000-0000-4000-8000-000000000914";

fn pane_dir(setup: &Setup) -> PathBuf {
    pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0)
}

fn actions<'a>(details: &'a [Value], kind: &str) -> Vec<&'a Value> {
    details
        .iter()
        .filter(|detail| detail["kind"] == kind)
        .map(|detail| &detail["action"])
        .collect()
}

fn end_reason(binding_dir: &Path) -> Option<Value> {
    let end = fs::read(binding_dir.join("end.json")).ok()?;
    Some(serde_json::from_slice::<Value>(&end).expect("end JSON")["reason"].clone())
}

/// The server that owned the pane is gone and its socket file with it, and no
/// process carries the socket and pane id. Nothing can list the pane again,
/// and that counts as absence -- observed twice, a minute apart, before the
/// binding ends.
#[test]
fn a_binding_on_a_vanished_socket_ends_after_two_observations() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(1_000);
    let (first, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(
        actions(&first.details, "absence"),
        [&json!("first_absence")]
    );
    assert_eq!(
        end_reason(&binding_dir),
        None,
        "one sighting never ends a binding"
    );
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (second, _) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(actions(&second.details, "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
}

/// A new server bound the same path: the old incarnation's panes are gone.
#[test]
fn a_binding_whose_socket_has_a_new_server_ends_after_two_observations() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let socket = PathBuf::from(&setup.env["WEZTERM_UNIX_SOCKET"]);
    fs::remove_file(&socket).expect("remove socket");
    let _new_server = UnixListener::bind(&socket).expect("rebind socket");
    setup.clock.set_monotonic(1_000);
    let (first, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(
        actions(&first.details, "absence"),
        [&json!("first_absence")]
    );
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (second, _) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(actions(&second.details, "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
}

/// A process still running with the pane's socket and id means the server
/// may be alive with its socket file removed; that is not absence.
#[test]
fn a_process_still_on_a_vanished_socket_is_not_absence() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let pane = pane_dir(&setup);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Present);
    let (result, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(actions(&result.details, "absence"), [&json!("unavailable")]);
    assert!(!pane.join("absence-probe.json").exists());
}

/// A vanished socket alone does not show the server gone: it may still run
/// with its socket file deleted. Without a process probe that answers "no
/// process carries this pane" -- the probe failed, or there is none -- the
/// pane is unavailable, however many sweeps see the socket missing.
#[test]
fn a_vanished_socket_without_a_process_answer_never_ends_a_binding() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let pane = pane_dir(&setup);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Unavailable);
    let probes: [Option<&dyn ProcessProbe>; 2] = [Some(&setup.processes), None];
    for (step, (operation, processes)) in [OP_1, OP_2, OP_3, OP_4]
        .into_iter()
        .zip(probes.into_iter().cycle())
        .enumerate()
    {
        setup
            .clock
            .set_monotonic(1_000 + step as u64 * ABSENCE_INTERVAL_NS as u64);
        let (result, diagnostics) = sweep(
            &setup.root(),
            None,
            true,
            Some(operation),
            &setup.clock,
            &setup.panes,
            processes,
        )
        .expect("sweep");
        assert_eq!(actions(&result.details, "absence"), [&json!("unavailable")]);
        assert!(diagnostics.iter().any(|d| d.code == "probe_unavailable"));
    }
    assert_eq!(end_reason(&binding_dir), None);
    assert!(!pane.join("absence-probe.json").exists());
}

/// The same holds for removing an old pane's tree: two applies a minute apart
/// with the socket gone and no process answer keep every record.
#[test]
fn a_vanished_socket_without_a_process_answer_keeps_an_old_panes_tree() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    let pane = pane_dir(&setup);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Unavailable);
    let probes: [Option<&dyn ProcessProbe>; 2] = [Some(&setup.processes), None];
    for processes in probes {
        let before = tree_bytes(&setup.root());
        for (step, operation) in [OP_1, OP_2].into_iter().enumerate() {
            setup
                .clock
                .set_monotonic(1_000 + step as u64 * ABSENCE_INTERVAL_NS as u64);
            let (result, _) = sweep(
                &setup.root(),
                None,
                true,
                Some(operation),
                &setup.clock,
                &setup.panes,
                processes,
            )
            .expect("sweep");
            assert_eq!(
                actions(&result.details, "pane_retention"),
                [&json!("unavailable")]
            );
        }
        assert!(pane.join("claim.json").exists(), "the pane tree is kept");
        assert_eq!(tree_bytes(&setup.root()), before);
    }
}

/// Readers report a vanished socket as unavailable, and write nothing.
#[test]
fn readers_report_a_vanished_socket_as_unavailable_and_change_nothing() {
    let setup = Setup::new();
    setup.claim_and_bind();
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    let before = tree_bytes(&setup.root());
    let (rows, diagnostics) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert_eq!(rows[0].pane_presence, "unavailable");
    assert!(diagnostics.iter().any(|d| d.code == "probe_unavailable"));
    setup.doctor();
    setup.run_sweep(false, None);
    assert_eq!(tree_bytes(&setup.root()), before);
}

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                result.insert(path.clone(), fs::read(&path).expect("read"));
            }
        }
    }
    result
}

/// Ends the fixture's binding at unix time 1 and moves the wall clock past
/// the retention age.
fn end_long_ago(setup: &Setup) {
    setup.clock.set_unix(1);
    setup.provider_event(
        "SessionEnd",
        "session-a",
        json!({"reason":"other"}),
        "00000000000000000300",
    );
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
}

fn sweep_with(setup: &Setup, panes: &dyn PaneLister, operation: Option<&str>) -> Vec<Value> {
    sweep(
        &setup.root(),
        None,
        operation.is_some(),
        operation,
        &setup.clock,
        panes,
        Some(&setup.processes),
    )
    .expect("sweep")
    .0
    .details
}

#[test]
fn a_closed_panes_tree_is_removed_only_by_apply_after_two_observations() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    setup.processes.set(Presence::Absent);
    let panes = LockCheckingPanes::for_setup(&setup);
    let pane = pane_dir(&setup);

    let preview = sweep_with(&setup, &panes, None);
    assert_eq!(
        actions(&preview, "pane_retention"),
        [&json!("first_absence")]
    );
    assert!(
        !pane.join("absence-probe.json").exists(),
        "a preview writes nothing"
    );

    setup.clock.set_monotonic(1_000);
    let first = sweep_with(&setup, &panes, Some(OP_1));
    assert_eq!(actions(&first, "pane_retention"), [&json!("first_absence")]);
    assert!(pane.join("claim.json").exists());

    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64 - 1);
    let soon = sweep_with(&setup, &panes, Some(OP_2));
    assert_eq!(actions(&soon, "pane_retention"), [&json!("too_soon")]);
    assert!(pane.exists());

    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let preview = sweep_with(&setup, &panes, None);
    assert_eq!(actions(&preview, "pane_retention"), [&json!("prune")]);
    assert!(pane.exists(), "a preview removes nothing");

    let second = sweep_with(&setup, &panes, Some(OP_3));
    assert_eq!(actions(&second, "pane_retention"), [&json!("prune")]);
    assert!(!pane.exists(), "the whole pane tree is gone");
    assert_eq!(panes.asked_under_lock.load(Ordering::SeqCst), 0);
}

#[test]
fn a_present_pane_is_never_pruned_however_old_its_binding() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    for (step, operation) in [OP_1, OP_2, OP_3].into_iter().enumerate() {
        setup
            .clock
            .set_monotonic(1_000 + step as u64 * ABSENCE_INTERVAL_NS as u64);
        let (result, _) = setup.run_sweep(true, Some(operation));
        assert_eq!(
            actions(&result.details, "pane_retention"),
            [&json!("present")]
        );
    }
    assert!(pane_dir(&setup).join("claim.json").exists());
}

#[test]
fn a_binding_ended_recently_keeps_its_pane() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.clock.set_unix(1);
    setup.provider_event(
        "SessionEnd",
        "session-a",
        json!({"reason":"other"}),
        "00000000000000000300",
    );
    setup.clock.set_unix(RETENTION_AGE_NS as u64 - 1);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    for (step, operation) in [OP_1, OP_2].into_iter().enumerate() {
        setup
            .clock
            .set_monotonic(1_000 + step as u64 * ABSENCE_INTERVAL_NS as u64);
        let (result, _) = setup.run_sweep(true, Some(operation));
        assert!(actions(&result.details, "pane_retention").is_empty());
    }
    assert!(pane_dir(&setup).join("claim.json").exists());
}

/// An absence probe written before the binding ended belongs to the binding's
/// own absence, and a pane may have come back since without anything
/// clearing it. Removing the pane takes two sightings after the end.
#[test]
fn a_probe_from_before_the_end_does_not_count_toward_removal() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (address, _) = pane_address(&setup.env).expect("address");
    atomic_replace(
        &pane_dir(&setup).join("absence-probe.json"),
        &json!({"kind":"absence_probe","schema":3,"address":address,
            "operation_id":"00000000-0000-4000-8000-000000000910",
            "observed_mono_ns":"00000000000000000250"}),
    )
    .expect("old probe");
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(
        actions(&result.details, "pane_retention"),
        [&json!("first_absence")]
    );
    assert!(pane_dir(&setup).exists());
}

/// A file sweep does not recognise stays, and so does the tree around it.
#[test]
fn an_unknown_file_keeps_the_pane_tree() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    let unknown = pane_dir(&setup).join("notes.txt");
    fs::write(&unknown, "keep me").expect("unknown file");
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, diagnostics) = setup.run_sweep(true, Some(OP_2));
    assert!(unknown.exists());
    assert!(pane_dir(&setup).join("claim.json").exists());
    assert!(
        diagnostics.iter().any(|d| d.code == "record_invalid"),
        "{diagnostics:?}"
    );
    assert!(!actions(&result.details, "pane_retention").contains(&&json!("prune")));
}

/// The monotonic clock restarts at boot. A probe taken before a restart reads
/// as later than now; it cannot be measured against, so the count starts
/// again rather than waiting for the new clock to pass the old one.
#[test]
fn a_probe_from_before_a_restart_starts_the_count_again() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let pane = pane_dir(&setup);
    let (address, _) = pane_address(&setup.env).expect("address");
    atomic_replace(
        &pane.join("absence-probe.json"),
        &json!({"kind":"absence_probe","schema":3,"address":address,
            "operation_id":"00000000-0000-4000-8000-000000000910",
            "observed_mono_ns":"00000009000000000000"}),
    )
    .expect("probe from before a restart");
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(5_000);
    let (result, _) = setup.run_sweep(true, Some(OP_1));
    assert_eq!(
        actions(&result.details, "absence"),
        [&json!("first_absence")]
    );
    let probe: Value =
        serde_json::from_slice(&fs::read(pane.join("absence-probe.json")).expect("probe"))
            .expect("probe JSON");
    assert_eq!(probe["observed_mono_ns"], "00000000000000005000");
    assert_eq!(probe["operation_id"], OP_1);
}

/// An operation id is how a retried apply recognises its own earlier work.
/// A caller with nothing to retry need not invent one: each apply without an
/// id gets a fresh one, so two runs a minute apart are two observations.
#[test]
fn an_apply_without_an_operation_id_uses_a_fresh_one() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(1_000);
    let (first, _) = setup.run_sweep(true, None);
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (second, _) = setup.run_sweep(true, None);
    assert_eq!(actions(&second.details, "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
    let ids: Vec<String> = [first.operation_id, second.operation_id]
        .into_iter()
        .map(|id| id.expect("an apply reports its operation id"))
        .collect();
    assert_ne!(ids[0], ids[1]);
    for id in &ids {
        assert_eq!(Uuid::parse_str(id).expect("UUID").to_string(), *id);
    }
    // A preview has no operation.
    assert_eq!(setup.run_sweep(false, None).0.operation_id, None);
}
