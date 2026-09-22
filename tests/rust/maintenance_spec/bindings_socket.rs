use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use wezterm_attention::query::read_bindings_for_socket_with_ports;
use wezterm_attention::records::{RecordIdentity, RecordRead, read_record, read_record_typed};
use wezterm_attention::wezterm::PaneProcessSet;

fn query(
    setup: &Setup,
) -> (
    wezterm_attention::query::BindingQueryScope,
    Vec<wezterm_attention::query::BindingRow>,
    Vec<wezterm_attention::protocol::Diagnostic>,
) {
    read_bindings_for_socket_with_ports(
        &state_root(&setup.env).unwrap(),
        &setup.env["WEZTERM_UNIX_SOCKET"],
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .unwrap()
}

#[test]
fn bindings_socket_empty_keeps_scope_and_creates_no_state() {
    let setup = Setup::new();
    let (scope, rows, diagnostics) = query(&setup);
    assert!(rows.is_empty() && diagnostics.is_empty());
    assert_eq!(scope.realm_id, pane_address(&setup.env).unwrap().0.realm_id);
    assert!(!state_root(&setup.env).unwrap().exists());
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args([
            "bindings",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["complete"], true);
    assert_eq!(
        response["result"]["scope"],
        serde_json::to_value(scope).unwrap()
    );
    assert!(!state_root(&setup.env).unwrap().exists());
}

#[test]
fn bindings_socket_selects_one_incarnation_and_accepts_symlink() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = state_root(&setup.env).unwrap();
    let unrelated = root
        .join("v2/realms")
        .join("a".repeat(64))
        .join("binding.json");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    fs::write(&unrelated, b"invalid unrelated record").unwrap();
    let (scope, rows, diagnostics) = query(&setup);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].current && diagnostics.is_empty());
    let alias = setup._scratch.0.join("alias.sock");
    symlink(&setup.env["WEZTERM_UNIX_SOCKET"], &alias).unwrap();
    let (aliased, aliased_rows, problems) = read_bindings_for_socket_with_ports(
        &root,
        alias.to_str().unwrap(),
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .unwrap();
    assert_eq!(aliased, scope);
    assert_eq!(aliased_rows.len(), 1);
    assert!(problems.is_empty());
}

#[test]
fn typed_reads_distinguish_missing_io_invalid_and_future_without_changing_wrapper() {
    let scratch = Scratch::new();
    let file = scratch.0.join("record");
    let identity = RecordIdentity::unscoped();
    assert!(matches!(
        read_record_typed(&file, Some("claim"), &identity),
        RecordRead::Missing
    ));
    fs::create_dir(&file).unwrap();
    assert!(matches!(
        read_record_typed(&file, Some("claim"), &identity),
        RecordRead::Unavailable(_)
    ));
    // A directory in a record's place is never opened as bytes, so the failure is
    // that the record could not be inspected, not that its contents were bad.
    // This assertion used to read `record_invalid`, which contradicted the
    // `Unavailable` variant asserted two lines above it.
    assert_eq!(
        read_record(&file, Some("claim"), &identity)
            .unwrap_err()
            .diagnostic
            .code,
        "probe_unavailable"
    );
    fs::remove_dir(&file).unwrap();
    fs::write(&file, "invalid").unwrap();
    assert!(matches!(
        read_record_typed(&file, Some("claim"), &identity),
        RecordRead::Invalid(_)
    ));
    fs::write(&file, r#"{"schema":99}"#).unwrap();
    assert!(matches!(
        read_record_typed(&file, Some("claim"), &identity),
        RecordRead::Unsupported(_)
    ));
}

#[test]
fn bindings_socket_read_failures_are_not_empty_success() {
    let setup = Setup::new();
    let root = state_root(&setup.env).unwrap();
    let (scope, _, _) = query(&setup);
    let selected = root
        .join("v2/realms")
        .join(scope.realm_id)
        .join("incarnations")
        .join(scope.incarnation_id);
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, "not a directory").unwrap();
    let (_, rows, diagnostics) = query(&setup);
    assert!(rows.is_empty());
    assert!(!diagnostics.is_empty());
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args([
            "bindings",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["complete"],
        false
    );
}

#[test]
fn bindings_socket_rebirth_discards_rows() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let original = setup.env["WEZTERM_UNIX_SOCKET"].clone();
    let alias = setup._scratch.0.join("alias.sock");
    let replacement_path = setup._scratch.0.join("replacement.sock");
    let _replacement = UnixListener::bind(&replacement_path).unwrap();
    symlink(&original, &alias).unwrap();
    struct Retarget {
        alias: PathBuf,
        replacement: PathBuf,
    }
    impl PaneLister for Retarget {
        fn list(&self, _: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
            fs::remove_file(&self.alias).unwrap();
            symlink(&self.replacement, &self.alias).unwrap();
            Ok(vec![PaneRow {
                pane_id: "42".into(),
                tty_name: None,
            }])
        }
    }
    let probe = Retarget {
        alias: alias.clone(),
        replacement: replacement_path,
    };
    let error = read_bindings_for_socket_with_ports(
        &state_root(&setup.env).unwrap(),
        alias.to_str().unwrap(),
        Some(&probe),
        None,
    )
    .unwrap_err();
    assert_eq!(error.diagnostic.code, "incarnation_changed");
    assert_eq!(error.exit_code, 1);
}

#[test]
fn socket_queries_never_autostart_and_preserve_legacy_transport() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let executable = setup._scratch.0.join("wezterm");
    let log = setup._scratch.0.join("argv");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 1\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .env("WEZTERM_EXECUTABLE", &executable)
        .args([
            "bindings",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["complete"],
        false
    );
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .any(|arg| arg == "--no-auto-start")
    );
}

#[test]
fn socket_selector_conflicts_are_usage_errors_and_resolution_is_incomplete() {
    let setup = Setup::new();
    for args in [
        vec!["bindings", "--socket", "/missing", "--realm", "bad"],
        vec!["bindings", "--socket", "relative"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .envs(&setup.env)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args(["bindings", "--socket", "/missing", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["complete"], false);
    assert!(response["result"].get("scope").is_none());
    assert!(!state_root(&setup.env).unwrap().exists());
}

#[test]
fn publish_socket_writes_identity_to_selected_tty() {
    // This is the only test that drives the real CLI all the way to a real pty
    // and checks that bytes arrived. It used to be the body of an
    // alias-equivalence test; when the alias went, the equivalence assertion
    // went with it correctly and this proof went with it by accident.
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    let setup = Setup::new();
    let (mut master, mut slave) = (0, 0);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let tty = wezterm_attention::wezterm::tty_path_from_fd(slave.as_raw_fd()).unwrap();
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let executable = setup._scratch.0.join("wezterm");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            json!([{"pane_id":"42", "tty_name":tty}])
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .env("WEZTERM_EXECUTABLE", &executable)
        .args([
            "hooks",
            "publish",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut bytes = Vec::new();
    let error = master.read_to_end(&mut bytes).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(
        !bytes.is_empty(),
        "publication wrote nothing to the selected tty"
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "ok");
}

#[test]
fn publish_rejects_the_removed_realm_selector() {
    // `--realm` selects a realm id on `bindings` and `sweep`. It was also a
    // path-valued alias for `--socket` on `publish`, which gave one flag two
    // opposite meanings. The separation is asserted, not merely absent.
    let setup = Setup::new();
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args([
            "hooks",
            "publish",
            "--realm",
            &setup.env["WEZTERM_UNIX_SOCKET"],
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn socket_truncation_is_explicit_and_legacy_shape_is_preserved() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.provider_event(
        "SessionStart",
        "session-b",
        json!({"source":"clear"}),
        "00000000000000000300",
    );
    let executable = setup._scratch.0.join("wezterm");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' '[{\"pane_id\":\"42\"}]'\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    for socket_mode in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_attention"));
        command
            .env_clear()
            .envs(&setup.env)
            .env("WEZTERM_EXECUTABLE", &executable)
            .args(["bindings", "--limit", "1", "--json"]);
        if socket_mode {
            command.args(["--socket", &setup.env["WEZTERM_UNIX_SOCKET"]]);
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["complete"], false);
        assert_eq!(response["result"]["scanned"], 2);
        assert_eq!(response["result"]["returned"], 1);
        assert_eq!(response["result"].get("scope").is_some(), socket_mode);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "attention bindings: returned 1 of 2; use --all"
        );
    }
}

/// The stderr line fires only when rows were dropped: not for `--all`, not for
/// a limit the matches fit under, and not for a filter that left few rows.
#[test]
fn only_a_query_that_dropped_rows_says_so_on_stderr() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.provider_event(
        "SessionStart",
        "session-b",
        json!({"source":"clear"}),
        "00000000000000000300",
    );
    let executable = setup._scratch.0.join("wezterm");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' '[{\"pane_id\":\"42\"}]'\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    for (args, complete) in [
        (vec!["--all"], true),
        (vec!["--limit", "2"], true),
        (vec!["--limit", "1", "--provider", "codex"], true),
        (vec!["--limit", "1"], false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .envs(&setup.env)
            .env("WEZTERM_EXECUTABLE", &executable)
            .args(["bindings", "--json"])
            .args(&args)
            .output()
            .unwrap();
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["complete"], complete, "{args:?}");
        assert_eq!(response["result"]["truncated"], !complete, "{args:?}");
        assert_eq!(
            output.stderr.is_empty(),
            complete,
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A realm-wide answer with more diagnostics than the envelope shows is still
/// complete: the rows are all there, and the counts say what was dropped.
#[test]
fn realm_wide_diagnostics_are_counted_and_never_make_the_rows_incomplete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = state_root(&setup.env).unwrap();
    let address = pane_address(&setup.env).unwrap().0;
    let launch = launch_path(&root, &address, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]);
    for index in 0..60 {
        let bad = launch
            .join("bindings")
            .join(format!("{index:064x}"))
            .join("binding.json");
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(bad, "invalid").unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args(["bindings", "--json", "--all"])
        .output()
        .unwrap();
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "findings");
    assert_eq!(response["complete"], true);
    assert_eq!(response["result"]["truncated"], false);
    assert_eq!(response["result"]["diagnostic_count"], 50);
    // The sixty unreadable records plus whatever the unscoped probe reports.
    assert!(response["result"]["total_diagnostic_count"].as_u64().unwrap() >= 60);
    assert_eq!(response["diagnostics"].as_array().unwrap().len(), 50);
    assert!(output.stderr.is_empty());

    // Scoped to the socket, one unanswered probe is still incomplete.
    fs::remove_file(
        root.join("v2/realms")
            .join(&address.realm_id)
            .join("realm.json"),
    )
    .unwrap();
    let (_, rows, diagnostics) = query(&setup);
    assert_eq!(rows.len(), 1);
    assert!(diagnostics.iter().any(|d| d.code == "probe_unavailable"));
}

#[test]
fn selected_invalid_future_and_unreadable_binding_records_degrade_the_query() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = state_root(&setup.env).unwrap();
    let address = pane_address(&setup.env).unwrap().0;
    let launch = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let binding = launch_path(&root, &address, launch)
        .join("bindings")
        .join(binding_id("claude", "session-a", launch))
        .join("binding.json");
    for bytes in [b"invalid".as_slice(), br#"{"schema":99}"#.as_slice()] {
        fs::write(&binding, bytes).unwrap();
        let (_, rows, diagnostics) = query(&setup);
        assert!(rows.is_empty() && !diagnostics.is_empty());
    }
    fs::remove_file(&binding).unwrap();
    fs::create_dir(&binding).unwrap();
    // A directory named binding.json must be read as a failed selected record,
    // rather than recursively treated as an empty binding inventory.
    let (_, rows, diagnostics) = query(&setup);
    assert!(rows.is_empty() && !diagnostics.is_empty());
}

#[test]
fn socket_query_does_not_call_a_skipped_symlink_complete() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = state_root(&setup.env).unwrap();
    let (scope, _, _) = query(&setup);
    let selected = root
        .join("v2/realms")
        .join(scope.realm_id)
        .join("incarnations")
        .join(scope.incarnation_id);
    symlink(
        "/does-not-exist-synthetic",
        selected.join("unknown-subtree"),
    )
    .unwrap();
    let (_, rows, diagnostics) = query(&setup);
    assert_eq!(rows.len(), 1);
    assert!(!diagnostics.is_empty());
}

#[test]
fn invalid_state_root_is_incomplete_in_socket_mode() {
    let setup = Setup::new();
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .env("WEZTERM_ATTENTION_DIR", "relative")
        .args([
            "bindings",
            "--socket",
            &setup.env["WEZTERM_UNIX_SOCKET"],
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["complete"], false);
}

#[test]
fn a_record_failure_cannot_hide_another_rows_unavailable_probe() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = state_root(&setup.env).unwrap();
    let address = pane_address(&setup.env).unwrap().0;
    let launch = launch_path(&root, &address, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]);
    let bad = launch
        .join("bindings")
        .join("0".repeat(64))
        .join("binding.json");
    fs::create_dir_all(bad.parent().unwrap()).unwrap();
    fs::write(bad, "invalid").unwrap();
    fs::remove_file(
        root.join("v2/realms")
            .join(&address.realm_id)
            .join("realm.json"),
    )
    .unwrap();
    let (_, rows, diagnostics) = query(&setup);
    assert_eq!(rows.len(), 1);
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));
    assert!(diagnostics.iter().any(|d| d.code == "probe_unavailable"));
}

/// Offers one listing of every process, and counts how it is asked.
struct ListingProcesses {
    listing: String,
    listings: AtomicUsize,
    single_looks: AtomicUsize,
}

impl ListingProcesses {
    fn new(listing: String) -> Self {
        Self {
            listing,
            listings: AtomicUsize::new(0),
            single_looks: AtomicUsize::new(0),
        }
    }
}

impl ProcessProbe for ListingProcesses {
    fn available(&self) -> bool {
        true
    }

    fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
        self.single_looks.fetch_add(1, Ordering::SeqCst);
        Presence::Unavailable
    }

    fn pane_processes(&self) -> Option<PaneProcessSet> {
        self.listings.fetch_add(1, Ordering::SeqCst);
        Some(PaneProcessSet::from_process_listing(&self.listing))
    }
}

fn query_with(
    setup: &Setup,
    processes: &ListingProcesses,
) -> Vec<wezterm_attention::query::BindingRow> {
    read_bindings_for_socket_with_ports(
        &state_root(&setup.env).unwrap(),
        &setup.env["WEZTERM_UNIX_SOCKET"],
        Some(&setup.panes),
        Some(processes),
    )
    .unwrap()
    .1
}

#[test]
fn a_pane_the_mux_no_longer_lists_is_answered_from_the_process_listing() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.panes.set(Vec::new());

    let nobody = ListingProcesses::new("zsh HOME=/nowhere".to_owned());
    let rows = query_with(&setup, &nobody);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pane_presence, "verified_absent");
    assert_eq!(nobody.listings.load(Ordering::SeqCst), 1);
    assert_eq!(nobody.single_looks.load(Ordering::SeqCst), 0);

    let address = pane_address(&setup.env).unwrap().0;
    // Presence is asked at the socket path the realm record holds, which is the
    // canonical one.
    let realm: Value = serde_json::from_slice(
        &fs::read(
            state_root(&setup.env)
                .unwrap()
                .join("v2/realms")
                .join(&address.realm_id)
                .join("realm.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let still_running = ListingProcesses::new(format!(
        "zsh WEZTERM_UNIX_SOCKET={} WEZTERM_PANE={}",
        realm["socket_path"].as_str().unwrap(),
        address.pane_id
    ));
    let rows = query_with(&setup, &still_running);
    assert_eq!(rows[0].pane_presence, "present");
    assert_eq!(still_running.single_looks.load(Ordering::SeqCst), 0);
}
