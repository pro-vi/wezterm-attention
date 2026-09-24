//! Each test here holds one guard on a path where sweep --apply deletes or
//! overwrites something. Removing the guard turns its test red.

use super::*;

const OP_1: &str = "00000000-0000-4000-8000-000000000931";
const OP_2: &str = "00000000-0000-4000-8000-000000000932";

/// A realm-filtered sweep collects only flat markers whose claim is in that
/// realm.
#[test]
fn a_realm_filtered_sweep_leaves_another_realms_flat_markers() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    plant_flat_files(&root, "42");
    let (result, _) = sweep(
        &root,
        Some(&"e".repeat(64)),
        true,
        Some(OP_1),
        &setup.clock,
        &setup.panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    assert!(collection_details(&result.details).is_empty());
    for name in ["42", "42.agents", "42.ack"] {
        assert!(root.join(name).exists(), "{name} was collected");
    }
}

/// A symlinked `agents/` would aim child compaction at files elsewhere.
#[test]
fn compaction_never_deletes_through_a_symlinked_agents_directory() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let old = setup.seed_presence("old-child", 300, 1, "stopped");
    let agents = setup.binding_dir().join("agents");
    let outside = setup._scratch.0.join("outside-agents");
    fs::rename(&agents, &outside).expect("move agents outside");
    symlink(&outside, &agents).expect("link agents");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    let (_, diagnostics) = setup.run_sweep(true, Some(OP_1));
    assert!(outside.join(old.file_name().expect("child name")).exists());
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));
}

/// A child left below the floor by an earlier operation is removed only when
/// that same operation is replayed; another operation leaves it, since
/// readers already ignore it and removing it would be a new deletion.
#[test]
fn a_child_below_the_floor_is_removed_only_by_a_replay_of_its_operation() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.seed_presence("old-child", 300, 1, "stopped");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    setup.run_sweep(true, Some(OP_1));
    let delayed = setup.seed_presence("delayed-child", 250, 1, "stopped");
    setup.run_sweep(true, Some(OP_2));
    assert!(delayed.exists(), "another operation left the covered child");
}

/// Lists no panes, and on its second call records a hook's end for the
/// binding, as a SessionEnd landing while sweep decides would.
struct EndingPanes {
    end_path: PathBuf,
    end: Value,
    calls: AtomicU8,
}

impl PaneLister for EndingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            atomic_replace(&self.end_path, &self.end).expect("hook end");
        }
        Ok(Vec::new())
    }
}

/// Sweep ends a binding only if no newer end has been recorded; it never
/// overwrites a hook's own end with its absence verdict.
#[test]
fn sweep_never_overwrites_a_newer_end() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    let observation = 1_000 + ABSENCE_INTERVAL_NS as u64;
    setup.clock.set_monotonic(observation);
    let binding: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("binding.json")).expect("binding"))
            .expect("binding JSON");
    let end = json!({
        "kind":"binding_end","schema":3,"address":binding["address"],
        "launch_id":binding["launch_id"],"binding_id":binding["binding_id"],
        "reason":"session_end","event_id":Uuid::new_v4().to_string(),
        "observed_mono_ns":format!("{:020}", observation + 5),
        "written_at_unix_ns":"00000000001000000000"
    });
    let panes = EndingPanes {
        end_path: binding_dir.join("end.json"),
        end: end.clone(),
        calls: AtomicU8::new(0),
    };
    sweep(
        &setup.root(),
        None,
        true,
        Some(OP_2),
        &setup.clock,
        &panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    let stored: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("end.json")).expect("end"))
            .expect("end JSON");
    assert_eq!(stored, end, "the hook's end survives");
}

/// Ends session-a long ago and starts session-b, so session-a's binding is
/// old history. Returns session-a's binding directory.
fn old_history(setup: &Setup) -> PathBuf {
    setup.claim_and_bind();
    let old_dir = setup.binding_dir();
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
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    old_dir
}

/// A symlink inside a binding keeps it, even one named like a record and
/// pointing at a valid one.
#[test]
fn retention_keeps_a_binding_holding_a_symlink() {
    let setup = Setup::new();
    let old_dir = old_history(&setup);
    let outside = setup._scratch.0.join("outside-end.json");
    fs::rename(old_dir.join("end.json"), &outside).expect("move end outside");
    symlink(&outside, old_dir.join("end.json")).expect("link end");
    setup.run_sweep(true, Some(OP_1));
    assert!(old_dir.exists());
}

/// A record in a binding directory that names another binding is not that
/// binding's state, and the directory is kept.
#[test]
fn retention_keeps_a_binding_holding_another_bindings_record() {
    let setup = Setup::new();
    let old_dir = old_history(&setup);
    let mut foreign: Value =
        serde_json::from_slice(&fs::read(old_dir.join("end.json")).expect("end"))
            .expect("end JSON");
    foreign["kind"] = json!("activity_clear");
    foreign["binding_id"] = json!("d".repeat(64));
    foreign.as_object_mut().expect("object").remove("reason");
    foreign
        .as_object_mut()
        .expect("object")
        .remove("written_at_unix_ns");
    fs::write(
        old_dir.join("activity-clear.json"),
        serde_json::to_vec(&foreign).expect("JSON"),
    )
    .expect("foreign record");
    let (_, diagnostics) = setup.run_sweep(true, Some(OP_1));
    assert!(old_dir.exists());
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));
}

/// Lists no panes, and on its first call starts a new launch in the pane, as
/// an agent started while sweep decides would.
struct RelaunchingPanes {
    claim_path: PathBuf,
    calls: AtomicU8,
}

impl PaneLister for RelaunchingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut claim: Value =
                serde_json::from_slice(&fs::read(&self.claim_path).expect("claim"))
                    .expect("claim JSON");
            claim["launch_id"] = json!("00000000-0000-4000-8000-000000000799");
            atomic_replace(&self.claim_path, &claim).expect("new launch");
        }
        Ok(Vec::new())
    }
}

/// A pane claimed again while sweep probed it is not the pane sweep decided
/// to remove.
#[test]
fn pane_retention_keeps_a_pane_claimed_again_during_the_decision() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.clock.set_unix(1);
    setup.provider_event(
        "SessionEnd",
        "session-a",
        json!({"reason":"other"}),
        "00000000000000000300",
    );
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    let pane = pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0);
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    assert!(pane.join("absence-probe.json").exists());
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let panes = RelaunchingPanes {
        claim_path: pane.join("claim.json"),
        calls: AtomicU8::new(0),
    };
    let (result, diagnostics) = sweep(
        &setup.root(),
        None,
        true,
        Some(OP_2),
        &setup.clock,
        &panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    assert!(pane.join("claim.json").exists(), "{:?}", result.details);
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));
}
