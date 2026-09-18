use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use wezterm_attention::query::read_bindings_for_socket_with_ports;
use wezterm_attention::records::{RecordIdentity, RecordRead, read_record, read_record_typed};

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
    }
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
