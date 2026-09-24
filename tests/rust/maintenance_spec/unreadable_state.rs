//! A directory in the state tree that cannot be read hides whatever is below
//! it. Every walk says so, rather than answering as if it were empty.

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Makes a directory unreadable for the life of the guard, and readable again
/// on drop so the scratch tree can be removed.
struct Unreadable(PathBuf);

impl Unreadable {
    fn new(path: PathBuf) -> Self {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        Self(path)
    }
}

impl Drop for Unreadable {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn hide_bindings(setup: &Setup) -> Unreadable {
    Unreadable::new(
        setup
            .binding_dir()
            .parent()
            .expect("bindings directory")
            .to_path_buf(),
    )
}

/// A fake `wezterm` first on PATH, so the CLI never reaches a real one.
pub(super) fn fake_wezterm_path(setup: &Setup, rows: &str) -> PathBuf {
    let directory = setup._scratch.0.join("fake-bin");
    fs::create_dir_all(&directory).expect("fake bin directory");
    let executable = directory.join("wezterm");
    fs::write(&executable, format!("#!/bin/sh\nprintf '%s\\n' '{rows}'\n")).expect("fake wezterm");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("chmod");
    directory
}

#[test]
fn doctor_reports_a_directory_it_could_not_read() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let _hidden = hide_bindings(&setup);
    let (result, diagnostics) =
        doctor(&setup.root(), Some(&setup.panes), Some(&setup.processes)).expect("doctor");
    assert!(
        diagnostics.iter().any(|d| d.code == "state_permissions"),
        "{diagnostics:?}"
    );
    let probe = |name: &str| {
        result["probes"]
            .as_array()
            .expect("probes")
            .iter()
            .find(|probe| probe["name"] == name)
            .expect("probe")["status"]
            .clone()
    };
    assert_eq!(probe("state_files"), "finding");
    assert_eq!(probe("permissions"), "finding");
}

#[test]
fn a_realm_wide_listing_says_rows_may_be_missing() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let _hidden = hide_bindings(&setup);
    let (rows, diagnostics) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert!(rows.is_empty());
    assert!(
        diagnostics.iter().any(|d| d.code == "state_permissions"),
        "{diagnostics:?}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .env("PATH", fake_wezterm_path(&setup, r#"[{"pane_id":"42"}]"#))
        .args(["bindings", "--all", "--json"])
        .output()
        .expect("run bindings");
    let response: Value = serde_json::from_slice(&output.stdout).expect("bindings JSON");
    assert_eq!(response["complete"], false, "{response}");
    assert_eq!(response["status"], "findings");
}

#[test]
fn sweep_reports_a_directory_it_could_not_read() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let _hidden = hide_bindings(&setup);
    let (_, diagnostics) = setup.run_sweep(false, None);
    assert!(
        diagnostics.iter().any(|d| d.code == "state_permissions"),
        "{diagnostics:?}"
    );
}
