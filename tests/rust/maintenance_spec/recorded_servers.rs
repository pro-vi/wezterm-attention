//! What the socket at a realm's recorded path says about the server that held
//! an incarnation. A server shown to have exited -- nothing listens on its
//! socket file, its GUI process is gone, or no process carries the pane --
//! leaves its panes absent, and the two-observation rule reclaims them. A
//! socket that is gone or replaced with nothing to show the server gone keeps
//! every record, and doctor and sweep report that history once, however much
//! of it there is. A mux that did not answer leaves them incomplete.

use super::pane_retention::{OP_1, OP_2, actions, end_long_ago, end_reason, pane_dir, tree_bytes};
use super::*;
use std::os::unix::fs::PermissionsExt;
use wezterm_attention::protocol::{AttentionError, Diagnostic};
use wezterm_attention::query::{
    PanePresence, ScopeRelation, WindowCheck, WindowCheckReason,
    read_bindings_for_socket_with_ports, read_checked_tab_publications,
};

/// A pane lister whose `wezterm cli list` fails, as it does against a socket
/// nobody answers on.
struct UnansweredPanes;

impl PaneLister for UnansweredPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        Err(AttentionError::new(
            "realm_unavailable",
            "wezterm cli list via /fake/wezterm exited with status 3",
        ))
    }
}

/// Two applies a minute apart with the fixture's clock and `panes`.
fn two_applies(setup: &Setup, panes: &dyn PaneLister) -> (Vec<Vec<Value>>, Vec<Diagnostic>) {
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
            panes,
            Some(&setup.processes),
        )
        .expect("sweep");
        runs.push(result.details);
        diagnostics.extend(found);
    }
    (runs, diagnostics)
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|item| item.code.as_str()).collect()
}

/// Changes the socket file's metadata, and so its incarnation, while the same
/// listener keeps it open: what `chmod` or `touch` on a live socket does.
fn change_socket_metadata(setup: &Setup) {
    let socket = &setup.env["WEZTERM_UNIX_SOCKET"];
    let (before, _) = pane_address(&setup.env).expect("address");
    let mode = fs::metadata(socket).expect("socket").permissions().mode();
    std::thread::sleep(std::time::Duration::from_millis(5));
    fs::set_permissions(socket, fs::Permissions::from_mode(mode ^ 0o070)).expect("chmod");
    fs::set_permissions(socket, fs::Permissions::from_mode(mode)).expect("chmod back");
    let (after, _) = pane_address(&setup.env).expect("address");
    assert_ne!(
        before.incarnation_id, after.incarnation_id,
        "the socket's incarnation must change"
    );
}

/// A CLI run outside any pane, with a fake `wezterm` first on PATH that runs
/// `script`.
fn run_cli(setup: &Setup, script: &str, arguments: &[&str]) -> (Option<i32>, Value) {
    let directory = setup._scratch.0.join("script-bin");
    fs::create_dir_all(&directory).expect("fake bin directory");
    let executable = directory.join("wezterm");
    fs::write(&executable, format!("#!/bin/sh\n{script}\n")).expect("fake wezterm");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("chmod");
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .env("HOME", &setup.env["HOME"])
        .env("WEZTERM_ATTENTION_DIR", &setup.env["WEZTERM_ATTENTION_DIR"])
        .env("PATH", &directory)
        .args(arguments)
        .output()
        .expect("run attention");
    let response = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    (output.status.code(), response)
}

/// Claims and binds `count` more panes, 100 and up, on the fixture's socket.
fn bind_panes(setup: &Setup, count: u32) {
    setup.panes.set(
        std::iter::once(42)
            .chain(100..100 + count)
            .map(|pane| PaneRow {
                pane_id: pane.to_string(),
                tty_name: Some("/dev/ttys888".to_owned()),
            })
            .collect(),
    );
    for index in 0..count {
        let mut env = setup.env.clone();
        env.insert("WEZTERM_PANE".into(), (100 + index).to_string());
        env.insert(
            "WEZTERM_ATTENTION_LAUNCH_ID".into(),
            format!("00000000-0000-4000-8000-{:012}", 900 + index),
        );
        wezterm_attention::claim_launch(&env, &setup.ports()).expect("claim");
        let payload = json!({
            "session_id": format!("session-{index}"),
            "transcript_path":"/tmp/s.jsonl","cwd":"/tmp/p",
            "hook_event_name":"SessionStart","source":"startup"
        });
        let event = parse_provider_event("claude", "SessionStart", &payload, &BTreeMap::new());
        apply_provider_event(&event, &env, "00000000000000000200", &setup.ports()).expect("bind");
    }
}

/// `chmod` on a live socket changes its incarnation, and the server and its
/// panes run on. Whatever the process probe says, that shows nothing gone,
/// so two applies a minute apart end nothing, and the change is reported once
/// as history rather than as a probe that did not answer.
#[test]
fn a_live_socket_whose_metadata_changed_ends_nothing() {
    for presence in [Presence::Present, Presence::Unseen, Presence::Unavailable] {
        let setup = Setup::new();
        setup.claim_and_bind();
        let binding_dir = setup.binding_dir();
        let pane = pane_dir(&setup);
        change_socket_metadata(&setup);
        setup.processes.set(presence);
        let (runs, diagnostics) = two_applies(&setup, &setup.panes);
        assert_eq!(end_reason(&binding_dir), None, "{presence:?}");
        assert!(!pane.join("absence-probe.json").exists(), "{presence:?}");
        for details in &runs {
            assert!(actions(details, "absence").is_empty(), "{details:?}");
        }
        assert!(
            !codes(&diagnostics).contains(&"probe_unavailable"),
            "{presence:?}: {diagnostics:?}"
        );
        assert!(
            codes(&diagnostics).contains(&"incarnation_changed"),
            "{presence:?}: {diagnostics:?}"
        );
    }
}

/// The same for an old pane's whole tree: its reviews survive.
#[test]
fn a_live_socket_whose_metadata_changed_keeps_an_old_panes_tree() {
    for presence in [Presence::Present, Presence::Unseen, Presence::Unavailable] {
        let setup = Setup::new();
        setup.claim_and_bind();
        wezterm_attention::lifecycle::apply_mark_review(&setup.env, "build", false)
            .expect("mark review");
        end_long_ago(&setup);
        let pane = pane_dir(&setup);
        change_socket_metadata(&setup);
        setup.processes.set(presence);
        let before = tree_bytes(&setup.root());
        let (runs, _) = two_applies(&setup, &setup.panes);
        for details in &runs {
            assert!(actions(details, "pane_retention").is_empty(), "{details:?}");
        }
        assert!(pane.join("reviews").exists(), "{presence:?}");
        assert_eq!(tree_bytes(&setup.root()), before, "{presence:?}");
    }
}

/// A new server bound the old path, and the process listing read every
/// process and found none carrying the old pane: that pane is gone.
#[test]
fn a_replaced_socket_with_no_process_left_on_the_pane_is_reclaimed() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let socket = PathBuf::from(&setup.env["WEZTERM_UNIX_SOCKET"]);
    setup.stop_listening();
    fs::remove_file(&socket).expect("remove socket");
    let _new_server = UnixListener::bind(&socket).expect("rebind socket");
    setup.processes.set(Presence::Absent);
    let (runs, _) = two_applies(&setup, &setup.panes);
    assert_eq!(actions(&runs[0], "absence"), [&json!("first_absence")]);
    assert_eq!(actions(&runs[1], "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
}

/// A server that exited and left its socket file behind, as a GUI that quits
/// does: nothing listens on the file, so its listing fails, and that failure
/// is the server's exit, not a probe that did not answer. The pane reads
/// absent, the binding ends after two sightings, and every answer is complete.
#[test]
fn a_socket_file_nobody_listens_on_is_a_server_that_exited() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    setup.stop_listening();
    setup.processes.set(Presence::Unseen);
    let (rows, diagnostics) = read_bindings_with_ports(
        &setup.root(),
        Some(&UnansweredPanes),
        Some(&setup.processes),
    )
    .expect("bindings");
    assert_eq!(rows[0].pane_presence, "verified_absent", "{diagnostics:?}");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let (_, doctor_diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &BTreeMap::new(),
        Some(&UnansweredPanes),
        Some(&setup.processes),
    )
    .expect("doctor");
    assert!(
        !codes(&doctor_diagnostics).contains(&"realm_unavailable"),
        "{doctor_diagnostics:?}"
    );
    let (runs, diagnostics) = two_applies(&setup, &UnansweredPanes);
    assert_eq!(actions(&runs[0], "absence"), [&json!("first_absence")]);
    assert_eq!(actions(&runs[1], "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The same through the CLI, whose `wezterm cli list` fails against the
/// abandoned socket: sweep is complete and exits 0.
#[test]
fn a_socket_file_nobody_listens_on_leaves_sweep_complete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.stop_listening();
    for arguments in [&["sweep", "--json"][..], &["sweep", "--apply", "--json"]] {
        let (code, response) = run_cli(&setup, "exit 3", arguments);
        assert_eq!(code, Some(0), "{arguments:?}: {response}");
        assert_eq!(response["complete"], true, "{arguments:?}: {response}");
    }
}

/// A missing `gui-sock-<pid>` socket whose GUI process has exited: the GUI's
/// local panes ended with it, so the pane is absent, and the binding ends
/// after two sightings although the process listing could not read every
/// process.
#[test]
fn a_gone_gui_socket_whose_process_exited_is_reclaimed() {
    let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("wait");
    let setup = Setup::with_socket_name(&format!("gui-sock-{pid}"));
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Unseen);
    let (runs, diagnostics) = two_applies(&setup, &setup.panes);
    assert_eq!(actions(&runs[0], "absence"), [&json!("first_absence")]);
    assert_eq!(actions(&runs[1], "absence"), [&json!("end")]);
    assert_eq!(end_reason(&binding_dir), Some(json!("sweep_absent")));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The same name with a process that still runs proves nothing: the GUI may
/// still run with its socket removed.
#[test]
fn a_gone_gui_socket_whose_process_runs_is_kept() {
    let setup = Setup::with_socket_name(&format!("gui-sock-{}", std::process::id()));
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let pane = pane_dir(&setup);
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    setup.processes.set(Presence::Unseen);
    let (runs, diagnostics) = two_applies(&setup, &setup.panes);
    for details in &runs {
        assert!(actions(details, "absence").is_empty(), "{details:?}");
    }
    assert_eq!(end_reason(&binding_dir), None);
    assert!(!pane.join("absence-probe.json").exists());
    assert!(
        codes(&diagnostics).contains(&"socket_gone"),
        "{diagnostics:?}"
    );
}

/// Sixty panes under a socket that is gone, with nothing to show the server
/// gone, are sixty panes of kept history. Doctor and sweep report it as one
/// diagnostic that names the incarnation, where it lies, and how many panes
/// it holds, and stay complete.
#[test]
fn many_panes_under_a_gone_socket_leave_doctor_and_sweep_complete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    bind_panes(&setup, 59);
    let (address, _) = pane_address(&setup.env).expect("address");
    fs::remove_file(&setup.env["WEZTERM_UNIX_SOCKET"]).expect("remove socket");
    for arguments in [
        &["doctor", "--json"][..],
        &["sweep", "--json"],
        &["sweep", "--apply", "--json"],
    ] {
        let (code, response) = run_cli(&setup, "printf '[]\\n'", arguments);
        assert_eq!(code, Some(0), "{arguments:?}: {response}");
        assert_eq!(response["complete"], true, "{arguments:?}: {response}");
        let gone: Vec<&Value> = response["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .filter(|item| item["code"] == "socket_gone")
            .collect();
        assert_eq!(gone.len(), 1, "{arguments:?}: {response}");
        assert_eq!(
            gone[0]["context"]["incarnations"],
            json!([{
                "realm_id": address.realm_id,
                "incarnation_id": address.incarnation_id,
                "path": format!("v2/realms/{}/incarnations/{}", address.realm_id, address.incarnation_id),
                "pane_count": 60,
            }]),
            "{arguments:?}: {response}"
        );
        if arguments[0] == "sweep" {
            assert!(
                !response["result"]["details"]
                    .as_array()
                    .expect("details")
                    .iter()
                    .any(|detail| detail["kind"] == "absence"),
                "{response}"
            );
        }
    }
}

/// A mux whose socket carries the incarnation and does not answer leaves its
/// panes unknown: doctor and sweep both say so and exit 1.
#[test]
fn a_mux_that_does_not_answer_leaves_doctor_and_sweep_incomplete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    for arguments in [&["doctor", "--json"][..], &["sweep", "--json"]] {
        let (code, response) = run_cli(&setup, "exit 3", arguments);
        assert_eq!(code, Some(1), "{arguments:?}: {response}");
        assert_eq!(response["complete"], false, "{arguments:?}: {response}");
        assert_eq!(
            response["status"], "unavailable",
            "{arguments:?}: {response}"
        );
    }
}

/// Every reader names a removed socket the same way: `socket_gone`.
#[test]
fn every_reader_calls_a_removed_socket_gone() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let socket = setup.env["WEZTERM_UNIX_SOCKET"].clone();
    write_sourced_tab_order(&root, &socket, 0);
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    let scope = PaneScope::new(
        pane_address(&setup.env).expect("address").0,
        launch_id.clone(),
        Some(binding_id("claude", "session-a", &launch_id)),
    )
    .expect("scope");
    fs::remove_file(&socket).expect("remove socket");
    let (rows, diagnostics) =
        read_bindings_with_ports(&root, Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert_eq!(rows[0].pane_presence, "unavailable");
    assert_eq!(codes(&diagnostics), ["socket_gone"]);
    let facts = read_pane_facts_with_ports(
        &root,
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("inspect");
    assert_eq!(facts.scope_relation, ScopeRelation::Unavailable);
    assert_eq!(codes(&facts.diagnostics), ["socket_gone"]);
    let error = read_bindings_for_socket_with_ports(
        &root,
        &socket,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect_err("bindings --socket");
    assert_eq!(error.diagnostic.code, "socket_gone");
    let lister = WindowLister(|_: &str| Ok(std::collections::BTreeSet::from([0])));
    let (windows, _) = read_checked_tab_publications(&root, &lister, &setup.clock).expect("tabs");
    assert!(
        matches!(
            windows[0].window_check,
            WindowCheck::Unavailable {
                reason: WindowCheckReason::SocketGone,
                ..
            }
        ),
        "{:?}",
        windows[0].window_check
    );
}

/// Inspect agrees with bindings about a server shown to have exited: the
/// scope matches and the pane is absent.
#[test]
fn inspect_reads_a_pane_of_an_exited_server_as_absent() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.stop_listening();
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    let scope = PaneScope::new(
        pane_address(&setup.env).expect("address").0,
        launch_id.clone(),
        Some(binding_id("claude", "session-a", &launch_id)),
    )
    .expect("scope");
    let facts = read_pane_facts_with_ports(
        &setup.root(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&UnansweredPanes),
        Some(&setup.processes),
    )
    .expect("inspect");
    assert_eq!(facts.scope_relation, ScopeRelation::Matched);
    assert_eq!(facts.pane_presence, PanePresence::VerifiedAbsent);
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
}

/// What sweep could not decide about a pane names that pane and its binding,
/// once per run, although an apply looks at the pane twice.
#[test]
fn sweep_names_each_pane_it_could_not_decide_once() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (address, _) = pane_address(&setup.env).expect("address");
    let binding = binding_id(
        "claude",
        "session-a",
        &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
    );
    for operation in [None, Some(OP_1)] {
        let (_, diagnostics) = sweep(
            &setup.root(),
            None,
            operation.is_some(),
            operation,
            &setup.clock,
            &UnansweredPanes,
            Some(&setup.processes),
        )
        .expect("sweep");
        assert_eq!(
            codes(&diagnostics),
            ["realm_unavailable", "probe_unavailable"],
            "{diagnostics:?}"
        );
        for item in &diagnostics {
            assert_eq!(
                item.context,
                BTreeMap::from([
                    ("realm_id".to_owned(), json!(address.realm_id)),
                    ("incarnation_id".to_owned(), json!(address.incarnation_id)),
                    ("pane_id".to_owned(), json!("42")),
                    ("binding_id".to_owned(), json!(binding)),
                ]),
                "{item:?}"
            );
        }
    }
}
