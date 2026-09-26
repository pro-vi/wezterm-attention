//! A pane's records outlive the pane. When its mux server is gone, or the
//! pane closed, sweep ends its binding by the two-observation absence rule,
//! and once that binding has been over for the retention age, sweep --apply
//! removes the pane's whole tree. Reads never delete anything.

use super::lock_scope::LockCheckingPanes;
use super::*;

pub(super) const OP_1: &str = "00000000-0000-4000-8000-000000000911";
pub(super) const OP_2: &str = "00000000-0000-4000-8000-000000000912";
pub(super) const OP_3: &str = "00000000-0000-4000-8000-000000000913";
pub(super) const OP_4: &str = "00000000-0000-4000-8000-000000000914";

pub(super) fn pane_dir(setup: &Setup) -> PathBuf {
    pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0)
}

pub(super) fn actions<'a>(details: &'a [Value], kind: &str) -> Vec<&'a Value> {
    details
        .iter()
        .filter(|detail| detail["kind"] == kind)
        .map(|detail| &detail["action"])
        .collect()
}

pub(super) fn end_reason(binding_dir: &Path) -> Option<Value> {
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

/// A process still running with the pane's socket and id means the server
/// may be alive with its socket file removed; that is not absence, and there
/// is nothing to decide about the binding.
#[test]
fn a_process_still_on_a_vanished_socket_is_not_absence() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let pane = pane_dir(&setup);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Present);
    let (result, _) = setup.run_sweep(true, Some(OP_1));
    assert!(actions(&result.details, "absence").is_empty());
    assert!(!pane.join("absence-probe.json").exists());
}

/// A vanished socket alone does not show the server gone: it may still run
/// with its socket file deleted. Without a process probe that answers "no
/// process carries this pane" -- the probe failed, or there is none -- the
/// records are kept however many sweeps see the socket missing. That is the
/// recorded history, reported as the gone socket, not a probe that did not
/// answer.
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
        assert!(actions(&result.details, "absence").is_empty());
        assert!(!diagnostics.iter().any(|d| d.code == "probe_unavailable"));
        assert!(diagnostics.iter().any(|d| d.code == "socket_gone"));
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
            assert!(actions(&result.details, "pane_retention").is_empty());
        }
        assert!(pane.join("claim.json").exists(), "the pane tree is kept");
        assert_eq!(tree_bytes(&setup.root()), before);
    }
}

/// Readers report a vanished socket as unavailable, and write nothing. The
/// diagnostic says the server's socket is gone; no probe failed to answer.
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
    assert!(diagnostics.iter().any(|d| d.code == "socket_gone"));
    assert!(!diagnostics.iter().any(|d| d.code == "probe_unavailable"));
    setup.doctor();
    setup.run_sweep(false, None);
    assert_eq!(tree_bytes(&setup.root()), before);
}

pub(super) fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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
pub(super) fn end_long_ago(setup: &Setup) {
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

/// A preview decides nothing, so every step it takes is answered from one
/// pane listing per socket and one process listing: here the tab order and
/// the old pane's retention both ask about pane 42. An apply takes a fresh
/// look for each decision.
#[test]
fn a_preview_lists_each_socket_and_the_processes_once() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    let (address, _) = pane_address(&setup.env).expect("address");
    let marker = format!("v2:{}:{}:42", address.realm_id, address.incarnation_id);
    write_tab_order(&setup.root(), 7, &[&marker]);
    let count = |apply: bool, operation: Option<&str>| {
        let panes = LockCheckingPanes::for_setup(&setup);
        let processes = super::doctor_probes::CountingListing::new();
        let (result, _) = sweep(
            &setup.root(),
            None,
            apply,
            operation,
            &setup.clock,
            &panes,
            Some(&processes),
        )
        .expect("sweep");
        assert_eq!(
            actions(&result.details, "pane_retention"),
            [&json!("first_absence")]
        );
        (
            panes.asked.load(Ordering::SeqCst),
            processes.listings.load(Ordering::SeqCst)
                + processes.single_looks.load(Ordering::SeqCst),
        )
    };
    assert_eq!(count(false, None), (1, 1), "a preview lists once");
    assert_eq!(
        count(true, Some(OP_1)),
        (2, 2),
        "an apply looks per decision"
    );
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

/// Plugin builds that cleared reviews themselves moved a review aside to
/// `<review>.<session>.clear` while they cleared it. One left by a crash is
/// the remains of a write, like a temporary file, and does not keep an old
/// pane's tree.
#[test]
fn a_review_left_mid_clear_does_not_keep_the_pane_tree() {
    let setup = Setup::new();
    setup.claim_and_bind();
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    let reviews = pane_dir(&setup).join("reviews");
    fs::create_dir_all(&reviews).expect("reviews");
    fs::write(
        reviews.join(format!("{}.json.table0x600003a0c0c0.clear", "a".repeat(64))),
        "{}",
    )
    .expect("review left mid-clear");
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, diagnostics) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(
        actions(&result.details, "pane_retention"),
        [&json!("prune")],
        "{diagnostics:?}"
    );
    assert!(!pane_dir(&setup).exists());
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

/// A process that lives until dropped, running `program` with exactly
/// `environment` and waiting on stdin.
struct Carrier(std::process::Child);

impl Carrier {
    fn spawn(program: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Self {
        let mut command = Command::new(program);
        command
            .args(arguments)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (name, value) in environment {
            command.env(name, value);
        }
        Self(command.spawn().expect("spawn carrier"))
    }
}

impl Drop for Carrier {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Two applies a minute apart that ask the machine's own process listing.
fn real_sweeps(
    setup: &Setup,
) -> (
    Vec<Vec<Value>>,
    Vec<wezterm_attention::protocol::Diagnostic>,
) {
    let mut runs = Vec::new();
    let mut diagnostics = Vec::new();
    for (step, operation) in [OP_1, OP_2].into_iter().enumerate() {
        setup
            .clock
            .set_monotonic(1_000 + step as u64 * ABSENCE_INTERVAL_NS as u64);
        let (result, found) = sweep(
            &setup.root(),
            None,
            true,
            Some(operation),
            &setup.clock,
            &setup.panes,
            Some(&wezterm_attention::wezterm::SystemProcessProbe),
        )
        .expect("sweep");
        runs.push(result.details);
        diagnostics.extend(found);
    }
    (runs, diagnostics)
}

/// macOS hides the environment of its own system binaries from every reader,
/// so a pane where only a system program runs carries nothing the process
/// listing can see. With the pane's socket gone, not seeing it is not seeing
/// it gone: the listing could not read every process. The pane is kept, and
/// the gone socket is reported as the server's state, not as a probe that
/// did not answer.
#[test]
fn a_pane_whose_process_hides_its_environment_is_not_absent_when_its_socket_vanishes() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let pane = pane_dir(&setup);
    let socket = fs::canonicalize(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("socket path");
    let _shell = Carrier::spawn(
        Path::new("/bin/sleep"),
        &["60"],
        &[
            (
                "WEZTERM_UNIX_SOCKET",
                socket.to_str().expect("UTF-8 socket"),
            ),
            ("WEZTERM_PANE", "42"),
        ],
    );
    fs::remove_file(&socket).expect("remove socket");
    let (runs, diagnostics) = real_sweeps(&setup);
    for details in &runs {
        assert!(actions(details, "absence").is_empty(), "{diagnostics:?}");
    }
    assert_eq!(end_reason(&binding_dir), None);
    assert!(!pane.join("absence-probe.json").exists());
    assert!(
        !diagnostics.iter().any(|d| d.code == "probe_unavailable"),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics.iter().any(|d| d.code == "socket_gone"),
        "{diagnostics:?}"
    );
}

/// The realm record keeps the socket path resolved, and a process keeps it as
/// WezTerm was configured to spell it, which may pass through a symlinked
/// directory. The spelling does not decide whether the process is the pane's.
#[test]
fn a_process_naming_the_socket_through_a_symlinked_directory_keeps_its_pane() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let socket = PathBuf::from(&setup.env["WEZTERM_UNIX_SOCKET"]);
    let linked = setup._scratch.0.join("linked");
    symlink(&setup._scratch.0, &linked).expect("link the socket directory");
    let spelled = linked.join("mux.sock");
    let spelled = spelled.to_str().expect("UTF-8 socket");
    let _agent = Carrier::spawn(
        Path::new(env!("CARGO_BIN_EXE_attention")),
        &["hooks", "event", "claude", "Stop"],
        &[("WEZTERM_UNIX_SOCKET", spelled), ("WEZTERM_PANE", "42")],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while wezterm_attention::wezterm::SystemProcessProbe.presence(spelled, "42")
        != Presence::Present
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the carrier never showed up"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    fs::remove_file(&socket).expect("remove socket");
    let (runs, diagnostics) = real_sweeps(&setup);
    for details in &runs {
        assert!(actions(details, "absence").is_empty(), "{diagnostics:?}");
    }
    assert_eq!(end_reason(&binding_dir), None);
}

/// A socket file that is gone, with nothing to show its server gone, keeps
/// its panes' records. That is the recorded history, not a probe that failed
/// to answer, so doctor and sweep still give a complete answer and exit 0,
/// with the gone socket among their findings.
#[test]
fn a_gone_socket_leaves_doctor_and_sweep_complete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (address, _) = pane_address(&setup.env).expect("address");
    let marker = format!("v2:{}:{}:42", address.realm_id, address.incarnation_id);
    write_tab_order(&setup.root(), 5, &[&marker]);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    let outside_a_pane: BTreeMap<_, _> = setup
        .env
        .iter()
        .filter(|(name, _)| matches!(name.as_str(), "HOME" | "WEZTERM_ATTENTION_DIR"))
        .collect();
    let path = super::unreadable_state::fake_wezterm_path(&setup, "[]");
    for arguments in [
        &["doctor", "--json"][..],
        &["sweep", "--json"],
        &["sweep", "--apply", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .envs(outside_a_pane.clone())
            .env("PATH", &path)
            .args(arguments)
            .output()
            .expect("run attention");
        let response: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
        assert_eq!(output.status.code(), Some(0), "{arguments:?}: {response}");
        assert_eq!(response["complete"], true, "{arguments:?}: {response}");
        assert_eq!(response["status"], "findings", "{arguments:?}: {response}");
        let codes: Vec<&Value> = response["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|d| &d["code"])
            .collect();
        assert!(codes.contains(&&json!("socket_gone")), "{response}");
        if arguments[0] == "doctor" {
            let socket = response["result"]["probes"]
                .as_array()
                .expect("probes")
                .iter()
                .find(|probe| probe["name"] == "socket")
                .expect("socket probe")
                .clone();
            assert_eq!(socket["status"], "finding", "{response}");
        }
    }
    assert!(setup.root().join("tabs/5.json").exists());
}

/// `mark review`, `mark clear` and Pi's review events each take a lock beside
/// the review they write, `reviews/.<owner key>.lock`, and leave it there.
/// That lock is the writer's own, and does not keep an old pane's tree.
#[test]
fn a_pane_where_reviews_were_marked_and_cleared_is_removed_once_old() {
    let setup = Setup::new();
    setup.claim_and_bind();
    wezterm_attention::lifecycle::apply_mark_review(&setup.env, "build").expect("mark review");
    wezterm_attention::lifecycle::apply_mark_clear(&setup.env, "build", "00000000000000000250")
        .expect("mark clear");
    let lock = pane_dir(&setup).join("reviews").join(format!(
        ".{}.lock",
        wezterm_attention::protocol::sha256_hex(b"build")
    ));
    assert!(lock.exists(), "the review lock stays behind");
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, diagnostics) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(
        actions(&result.details, "pane_retention"),
        [&json!("prune")],
        "{diagnostics:?}"
    );
    assert!(!pane_dir(&setup).exists());
}

/// A review is a record sweep recognises, kept in the pane's own reviews
/// directory, so one still standing goes with the old pane it flags rather
/// than keeping its tree.
#[test]
fn a_pane_still_flagged_for_review_is_removed_once_old() {
    let setup = Setup::new();
    setup.claim_and_bind();
    wezterm_attention::lifecycle::apply_mark_review(&setup.env, "build").expect("mark review");
    let review = pane_dir(&setup).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"build")
    ));
    assert!(review.exists(), "the review stands");
    end_long_ago(&setup);
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(1_000);
    setup.run_sweep(true, Some(OP_1));
    setup
        .clock
        .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
    let (result, diagnostics) = setup.run_sweep(true, Some(OP_2));
    assert_eq!(
        actions(&result.details, "pane_retention"),
        [&json!("prune")],
        "{diagnostics:?}"
    );
    assert!(!pane_dir(&setup).exists());
}

/// Only the name the review writer uses is its lock. A lock-like file of any
/// other name, or in another directory, is unknown state and keeps the tree.
#[test]
fn a_lock_like_file_the_review_writer_does_not_leave_keeps_the_pane_tree() {
    let key = "a".repeat(64);
    for relative in [
        format!("reviews/.{}.lock", "A".repeat(64)),
        format!("reviews/.{}.lock", &key[..63]),
        format!("reviews/{key}.lock"),
        format!(".{key}.lock"),
        format!("reviews/nested/.{key}.lock"),
    ] {
        let setup = Setup::new();
        setup.claim_and_bind();
        end_long_ago(&setup);
        setup.panes.set(Vec::new());
        setup.processes.set(Presence::Absent);
        let planted = pane_dir(&setup).join(&relative);
        fs::create_dir_all(planted.parent().expect("parent")).expect("create parent");
        fs::write(&planted, "").expect("plant lock-like file");
        setup.clock.set_monotonic(1_000);
        setup.run_sweep(true, Some(OP_1));
        setup
            .clock
            .set_monotonic(1_000 + ABSENCE_INTERVAL_NS as u64);
        let (result, _) = setup.run_sweep(true, Some(OP_2));
        assert!(planted.exists(), "{relative} was removed");
        assert!(
            !actions(&result.details, "pane_retention").contains(&&json!("prune")),
            "{relative} did not keep the tree"
        );
    }
}
