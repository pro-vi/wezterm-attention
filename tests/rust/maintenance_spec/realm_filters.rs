//! A realm-wide bindings query asks each mux socket for its panes. A filter
//! that rules a realm out rules its socket out too, and the sockets it does
//! ask are asked together, so one slow socket costs one deadline, not one per
//! socket.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

/// Claims pane 42 on a second socket and binds `session` there.
pub(super) fn bind_on_another_socket(
    setup: &Setup,
    name: &str,
    session: &str,
) -> (UnixListener, String) {
    let socket = setup._scratch.0.join(name);
    let listener = UnixListener::bind(&socket).expect("bind second socket");
    let mut env = setup.env.clone();
    env.insert(
        "WEZTERM_UNIX_SOCKET".to_owned(),
        socket.to_string_lossy().into_owned(),
    );
    env.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
        "00000000-0000-4000-8000-000000000703".to_owned(),
    );
    wezterm_attention::claim_launch(&env, &setup.ports()).expect("claim on the second socket");
    let payload = json!({
        "session_id":session,"transcript_path":"/tmp/s.jsonl","cwd":"/tmp/project",
        "hook_event_name":"SessionStart","source":"startup"
    });
    let event = parse_provider_event("claude", "SessionStart", &payload, &BTreeMap::new());
    apply_provider_event(&event, &env, "00000000000000000210", &setup.ports())
        .expect("bind on the second socket");
    let canonical = fs::canonicalize(&socket).expect("canonical socket");
    (listener, canonical.to_string_lossy().into_owned())
}

/// A fake `wezterm` that notes which socket it was asked about, waits, and
/// lists pane 42.
fn logging_wezterm(setup: &Setup, delay: &str) -> (PathBuf, PathBuf) {
    let directory = setup._scratch.0.join("logging-bin");
    fs::create_dir_all(&directory).expect("bin directory");
    let log = setup._scratch.0.join("asked.log");
    let executable = directory.join("wezterm");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$WEZTERM_UNIX_SOCKET\" >> '{}'\nsleep {delay}\nprintf '%s\\n' '[{{\"pane_id\":\"42\"}}]'\n",
            log.display()
        ),
    )
    .expect("fake wezterm");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("chmod");
    (directory, log)
}

fn run_bindings(setup: &Setup, path: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .env("PATH", path)
        .arg("bindings")
        .args(args)
        .output()
        .expect("run bindings");
    serde_json::from_slice(&output.stdout).expect("bindings JSON")
}

#[test]
fn a_realm_filter_never_asks_another_realms_socket() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (_second, second_socket) = bind_on_another_socket(&setup, "second.sock", "session-b");
    let (address, _) = pane_address(&setup.env).expect("address");
    let (bin, log) = logging_wezterm(&setup, "0");
    let response = run_bindings(
        &setup,
        &bin,
        &["--realm", &address.realm_id, "--all", "--json"],
    );
    assert_eq!(response["result"]["returned"], 1, "{response}");
    let asked = fs::read_to_string(&log).expect("asked log");
    assert!(
        !asked.lines().any(|line| line == second_socket),
        "the filtered-out realm's socket was asked: {asked}"
    );
    assert_eq!(asked.lines().count(), 1, "{asked}");

    // A provider filter that no row matches asks no socket at all.
    fs::remove_file(&log).expect("reset log");
    let response = run_bindings(&setup, &bin, &["--provider", "codex", "--all", "--json"]);
    assert_eq!(response["result"]["returned"], 0, "{response}");
    assert!(!log.exists(), "a socket was asked for no row");
}

#[test]
fn slow_sockets_are_asked_together() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (_second, _) = bind_on_another_socket(&setup, "second.sock", "session-b");
    let (bin, log) = logging_wezterm(&setup, "2");
    let started = Instant::now();
    let response = run_bindings(&setup, &bin, &["--all", "--json"]);
    let elapsed = started.elapsed();
    assert_eq!(response["result"]["returned"], 2, "{response}");
    assert_eq!(fs::read_to_string(&log).expect("log").lines().count(), 2);
    // One after the other they take at least 4 s. Together they take one
    // listing's 2 s plus process start-up, which a loaded machine stretches.
    assert!(
        elapsed < std::time::Duration::from_millis(3_800),
        "two 2 s sockets took {elapsed:?}"
    );
}

/// A realm filter still sees a rival in another realm: the question is about
/// the rows it returns, and a conflict is a fact about those rows.
#[test]
fn a_realm_filter_still_reports_a_conflict_with_another_realm() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (_second, _) = bind_on_another_socket(&setup, "second.sock", "session-a");
    let (address, _) = pane_address(&setup.env).expect("address");
    let (bin, _) = logging_wezterm(&setup, "0");
    let response = run_bindings(
        &setup,
        &bin,
        &["--realm", &address.realm_id, "--all", "--json"],
    );
    let rows = response["result"]["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "{response}");
    assert_eq!(rows[0]["binding_health"], "conflicted", "{response}");
}
