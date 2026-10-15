//! What doctor asks of the machine, and what it says when it could not ask.

use super::*;
use std::sync::atomic::AtomicUsize;
use wezterm_attention::wezterm::{
    ControllingTerminal, PaneProcessSet, ProcessFacts, ProcessInspector, ProcessListing,
    ProcessRead, ProcessStart,
};

/// Offers one listing of every process, and counts how it is asked. Like the
/// system probe, it answers whether it is available by taking a listing.
pub(super) struct CountingListing {
    pub(super) listings: AtomicUsize,
    pub(super) single_looks: AtomicUsize,
}

impl CountingListing {
    pub(super) fn new() -> Self {
        Self {
            listings: AtomicUsize::new(0),
            single_looks: AtomicUsize::new(0),
        }
    }
}

impl ProcessProbe for CountingListing {
    fn available(&self) -> bool {
        !matches!(self.pane_processes(), ProcessListing::Failed)
    }

    fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
        self.single_looks.fetch_add(1, Ordering::SeqCst);
        Presence::Absent
    }

    fn pane_processes(&self) -> ProcessListing {
        self.listings.fetch_add(1, Ordering::SeqCst);
        ProcessListing::Listed(PaneProcessSet::from_process_listing(""))
    }
}

/// Copies pane 42's claim to other pane ids, as other claimed panes.
fn claim_more_panes(setup: &Setup, panes: &[&str]) {
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let claim: Value = serde_json::from_slice(
        &fs::read(pane_dir(&root, &address).join("claim.json")).expect("claim"),
    )
    .expect("claim JSON");
    for pane in panes {
        let mut other = address.clone();
        other.pane_id = (*pane).to_owned();
        let mut copy = claim.clone();
        copy["address"] = json!(other);
        atomic_replace(&pane_dir(&root, &other).join("claim.json"), &copy).expect("claim copy");
    }
}

/// One process listing answers for every claim. Each listing reads every
/// process's environment and takes tens of milliseconds, so one per claim
/// would make doctor take seconds on a machine with a few hundred claims.
#[test]
fn doctor_takes_one_process_listing_however_many_claims() {
    let setup = Setup::new();
    setup.claim_and_bind();
    claim_more_panes(&setup, &["43", "44", "45"]);
    setup.panes.set(Vec::new());
    let probe = CountingListing::new();
    wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&probe),
        &wezterm_attention::wezterm::SystemProcessInspector,
    )
    .expect("doctor");
    assert_eq!(probe.single_looks.load(Ordering::SeqCst), 0);
    assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
}

fn probe_status(result: &Value, name: &str) -> Value {
    result["probes"]
        .as_array()
        .expect("probes")
        .iter()
        .find(|probe| probe["name"] == name)
        .unwrap_or_else(|| panic!("no {name} probe in {result}"))["status"]
        .clone()
}

/// With no state, no claims and no pane around it, doctor checked nothing,
/// and "healthy" would say it had. Each probe that found nothing to check
/// says unobserved, and `unobserved` lists exactly those probes.
#[test]
fn doctor_on_an_empty_setup_says_what_it_could_not_observe() {
    let setup = Setup::new();
    let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &BTreeMap::new(),
        Some(&setup.panes),
        Some(&setup.processes),
        &wezterm_attention::wezterm::SystemProcessInspector,
    )
    .expect("doctor");
    for name in [
        "permissions",
        "state_files",
        "socket",
        "processes",
        "environment",
    ] {
        assert_eq!(
            probe_status(&result, name),
            "unobserved",
            "{name}: {result}"
        );
        assert!(
            result["unobserved"]
                .as_array()
                .expect("unobserved")
                .contains(&json!(name)),
            "{name}: {result}"
        );
    }
    assert_eq!(probe_status(&result, "versions"), "healthy");
    let nothing_to_check: Vec<Value> = result["probes"]
        .as_array()
        .expect("probes")
        .iter()
        .filter(|probe| probe["status"] == "unobserved")
        .map(|probe| probe["name"].clone())
        .collect();
    assert_eq!(result["unobserved"], json!(nothing_to_check), "{result}");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A platform that does or does not let an agent claim its own pane. Doctor
/// asks it nothing else.
struct SelfClaimPlatform(bool);

impl ProcessInspector for SelfClaimPlatform {
    fn self_claim_supported(&self) -> bool {
        self.0
    }

    fn own_pid(&self) -> i32 {
        0
    }

    fn process(&self, _pid: i32) -> ProcessRead {
        ProcessRead::Unknown
    }

    fn boot_session(&self) -> Option<String> {
        None
    }

    fn terminal_device(&self, _path: &str) -> Option<u64> {
        None
    }
}

/// Run inside a pane whose server identity nothing has published, doctor
/// says so where only a claiming shell could publish it: that is the setup
/// where hooks run and nothing ever shows. Where an agent can claim its own
/// pane, nothing publishes the identity before the first agent starts, and
/// the agent's hook publishes it, so there is nothing yet to check.
#[test]
fn doctor_in_a_pane_nothing_has_published_reports_it_where_no_agent_can_claim() {
    let setup = Setup::new();
    let doctor = |env: &BTreeMap<String, String>, supported: bool| {
        wezterm_attention::maintenance::doctor_with_environment(
            &setup.root(),
            env,
            Some(&setup.panes),
            Some(&setup.processes),
            &SelfClaimPlatform(supported),
        )
        .expect("doctor")
    };
    let mut switched_off = setup.env.clone();
    switched_off.insert(
        "WEZTERM_ATTENTION_ENABLE_SELF_CLAIM".to_owned(),
        "0".to_owned(),
    );
    for (label, env, supported) in [
        ("switched off", &switched_off, true),
        ("unsupported", &setup.env, false),
    ] {
        let (result, diagnostics) = doctor(env, supported);
        assert_eq!(
            probe_status(&result, "environment"),
            "finding",
            "{label}: {result}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::IdentityUnpublished),
            "{label}: {diagnostics:?}"
        );
    }

    let (result, diagnostics) = doctor(&setup.env, true);
    assert_eq!(
        probe_status(&result, "environment"),
        "unobserved",
        "{result}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::IdentityUnpublished),
        "{diagnostics:?}"
    );

    setup.claim_and_bind();
    for (env, supported) in [
        (&setup.env, true),
        (&switched_off, true),
        (&setup.env, false),
    ] {
        let (result, diagnostics) = doctor(env, supported);
        assert_eq!(probe_status(&result, "environment"), "healthy", "{result}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}

/// Offers one listing, parsed from process lines.
struct ListedProcesses(String);

impl ProcessProbe for ListedProcesses {
    fn available(&self) -> bool {
        true
    }

    fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
        Presence::Absent
    }

    fn pane_processes(&self) -> ProcessListing {
        ProcessListing::Listed(PaneProcessSet::from_process_listing(&self.0))
    }
}

/// A Codex session that joins a shared server runs its hooks there, in no
/// pane, so Attention refuses every event and Codex shows none of the
/// refusals. Doctor names such a server when the `CODEX_HOME` it serves has
/// hooks that call Attention, and leaves alone one whose hooks do not, and
/// the desktop app's private server, which no terminal session can join.
#[test]
fn doctor_names_a_codex_shared_server_whose_hooks_call_attention() {
    let setup = Setup::new();
    let homes = setup._scratch.0.join("codex");
    let (calling, other) = (homes.join("calling"), homes.join("other"));
    for (home, hooks) in [
        (
            &calling,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"WEZTERM_ATTENTION_HOST_PID=$PPID exec attention hooks event codex Stop"}]}]}}"#,
        ),
        (&other, r#"{"hooks":{}}"#),
    ] {
        fs::create_dir_all(home).expect("codex home");
        fs::write(home.join("hooks.json"), hooks).expect("hooks");
    }
    let server = |home: &PathBuf| {
        format!(
            "codex app-server --listen unix:// --managed-daemon CODEX_HOME={}",
            home.display()
        )
    };
    let doctor = |listing: String| {
        wezterm_attention::maintenance::doctor_with_environment(
            &setup.root(),
            &setup.env,
            Some(&setup.panes),
            Some(&ListedProcesses(listing)),
            &wezterm_attention::wezterm::SystemProcessInspector,
        )
        .expect("doctor")
    };

    let (result, diagnostics) = doctor(format!("{}\n{}", server(&calling), server(&other)));
    assert_eq!(probe_status(&result, "codex_server"), "finding", "{result}");
    let named: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::SessionDetached)
        .collect();
    assert_eq!(named.len(), 1, "{diagnostics:?}");
    assert_eq!(
        named[0].context.get("codex_home"),
        Some(&json!(calling.display().to_string())),
        "{diagnostics:?}"
    );

    let desktop = format!(
        "codex app-server --analytics-default-enabled CODEX_HOME={}",
        calling.display()
    );
    let (result, diagnostics) = doctor(format!("{}\n{desktop}", server(&other)));
    assert_eq!(probe_status(&result, "codex_server"), "healthy", "{result}");
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::SessionDetached),
        "{diagnostics:?}"
    );

    let (result, _) = doctor(desktop);
    assert_eq!(
        probe_status(&result, "codex_server"),
        "unobserved",
        "{result}"
    );
}

/// Hooks that exist and cannot be read may call Attention, so their server is
/// reported; a listing that failed leaves the probe unavailable, not
/// unobserved, so doctor says its report is incomplete.
#[test]
fn doctor_reports_a_codex_server_it_cannot_rule_out() {
    let setup = Setup::new();
    let unreadable = setup._scratch.0.join("codex-unreadable");
    fs::create_dir_all(unreadable.join("hooks.json")).expect("hooks.json as a directory");
    let listing = format!(
        "codex app-server --listen unix:// --managed-daemon CODEX_HOME={}",
        unreadable.display()
    );
    let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&ListedProcesses(listing)),
        &wezterm_attention::wezterm::SystemProcessInspector,
    )
    .expect("doctor");
    assert_eq!(probe_status(&result, "codex_server"), "finding", "{result}");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::SessionDetached),
        "{diagnostics:?}"
    );

    let (result, _) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&FailedListing),
        &wezterm_attention::wezterm::SystemProcessInspector,
    )
    .expect("doctor");
    assert_eq!(
        probe_status(&result, "codex_server"),
        "unavailable",
        "{result}"
    );
}

struct FailedListing;

impl ProcessProbe for FailedListing {
    fn available(&self) -> bool {
        false
    }

    fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
        Presence::Unavailable
    }

    fn pane_processes(&self) -> ProcessListing {
        ProcessListing::Failed
    }
}

/// The pid, start and boot of the Claude Code process [`OneHost`] runs.
const HOST_PID: i32 = 4321;
const HOST_STARTED_USEC: u32 = 7;
const BOOT: &str = "0f9a7c3e-51b2-4d6e-8a1b-2c3d4e5f6a7b";

/// A process table where only the host at `HOST_PID` can run.
struct OneHost {
    running: bool,
}

impl ProcessInspector for OneHost {
    fn self_claim_supported(&self) -> bool {
        true
    }

    fn own_pid(&self) -> i32 {
        0
    }

    fn process(&self, pid: i32) -> ProcessRead {
        if pid != HOST_PID || !self.running {
            return ProcessRead::Gone;
        }
        ProcessRead::Found(ProcessFacts {
            parent_pid: 1,
            process_group: HOST_PID,
            terminal: ControllingTerminal::Absent,
            terminal_foreground_group: HOST_PID,
            uid: 501,
            start: ProcessStart {
                seconds: 1_700_000_000,
                microseconds: HOST_STARTED_USEC,
            },
            traced: false,
            zombie: false,
        })
    }

    fn boot_session(&self) -> Option<String> {
        Some(BOOT.to_owned())
    }

    fn terminal_device(&self, _path: &str) -> Option<u64> {
        None
    }
}

/// Records `HOST_PID` as the host of the binding of session-a, as a prompt
/// hook that proved it would.
fn record_claude_host(setup: &Setup) {
    let (address, _) = pane_address(&setup.env).expect("address");
    let launch = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    atomic_replace(
        &setup.binding_dir().join("host.json"),
        &json!({
            "kind":"host","schema":3,"address":address,"launch_id":launch,
            "binding_id":binding_id("claude", "session-a", launch),
            "owner_pid":HOST_PID.to_string(),"owner_started_sec":"1700000000",
            "owner_started_usec":HOST_STARTED_USEC.to_string(),
            "owner_boot_session_id":BOOT,"tty_path":"/dev/ttys888",
            "tty_fingerprint":"f".repeat(64),"source_event":"UserPromptSubmit",
            "written_at_unix_ns":"00000000001000000000"
        }),
    )
    .expect("host record");
}

/// The status file fixture `name`, as the host's file for session-a.
fn status_file(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_status")
        .join(format!("{name}.json"));
    let mut file: Value =
        serde_json::from_slice(&fs::read(path).expect("fixture")).expect("fixture JSON");
    file["sessionId"] = json!("session-a");
    file["pid"] = json!(HOST_PID);
    file
}

fn write_status(directory: &Path, file: &Value) {
    let sessions = directory.join("sessions");
    fs::create_dir_all(&sessions).expect("sessions directory");
    fs::write(
        sessions.join(format!("{HOST_PID}.json")),
        serde_json::to_vec(file).expect("status JSON"),
    )
    .expect("status file");
}

/// Esc reaches the tab only through the status file of a running Claude Code
/// session, so doctor reads each one and names the Claude Code version of a
/// file it does not understand.
#[test]
fn doctor_reports_whether_running_claude_sessions_status_files_are_understood() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let doctor = |running: bool| {
        wezterm_attention::maintenance::doctor_with_environment(
            &setup.root(),
            &setup.env,
            Some(&setup.panes),
            Some(&setup.processes),
            &OneHost { running },
        )
        .expect("doctor")
    };
    let mismatches = |diagnostics: &[wezterm_attention::protocol::Diagnostic]| {
        diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::IntegrationVersionMismatch)
            .cloned()
            .collect::<Vec<_>>()
    };
    let (result, _) = doctor(true);
    assert_eq!(
        probe_status(&result, "claude_status"),
        "unobserved",
        "no host is recorded: {result}"
    );
    assert!(
        result["scope"]
            .as_array()
            .expect("scope")
            .contains(&json!("claude_status")),
        "{result}"
    );
    record_claude_host(&setup);
    let (result, _) = doctor(false);
    assert_eq!(
        probe_status(&result, "claude_status"),
        "unobserved",
        "the host has exited: {result}"
    );

    let (result, diagnostics) = doctor(true);
    assert_eq!(
        probe_status(&result, "claude_status"),
        "finding",
        "{result}"
    );
    let found = mismatches(&diagnostics);
    assert_eq!(found.len(), 1, "{diagnostics:?}");
    assert_eq!(found[0].context.get("reason"), Some(&json!("missing")));

    let claude = setup._scratch.0.join(".claude");
    for name in ["busy", "waiting-permission", "idle", "shell"] {
        write_status(&claude, &status_file(name));
        let (result, diagnostics) = doctor(true);
        assert_eq!(
            probe_status(&result, "claude_status"),
            "healthy",
            "{name}: {result}"
        );
        assert!(
            mismatches(&diagnostics).is_empty(),
            "{name}: {diagnostics:?}"
        );
    }

    let mut paused = status_file("busy");
    paused["status"] = json!("paused");
    write_status(&claude, &paused);
    let (result, diagnostics) = doctor(true);
    assert_eq!(
        probe_status(&result, "claude_status"),
        "finding",
        "{result}"
    );
    let found = mismatches(&diagnostics);
    assert_eq!(found.len(), 1, "{diagnostics:?}");
    assert_eq!(
        found[0].context.get("reason"),
        Some(&json!("unknown_state"))
    );
    assert_eq!(
        found[0].context.get("claude_code_version"),
        Some(&json!("2.1.300"))
    );
    assert_eq!(
        found[0].context.get("provider_session_id"),
        Some(&json!("session-a"))
    );
    assert!(
        !serde_json::to_string(&diagnostics)
            .unwrap()
            .contains("paused"),
        "the file's own words stay out of the report: {diagnostics:?}"
    );

    let mut other = status_file("busy");
    other["sessionId"] = json!("session-b");
    write_status(&claude, &other);
    let (result, diagnostics) = doctor(true);
    assert_eq!(
        probe_status(&result, "claude_status"),
        "finding",
        "{result}"
    );
    assert_eq!(
        mismatches(&diagnostics)[0].context.get("reason"),
        Some(&json!("other_session"))
    );
}

/// A session started with `CLAUDE_CONFIG_DIR` keeps its status file there,
/// and its binding records that directory.
#[test]
fn doctor_reads_a_claude_session_s_file_in_its_configuration_directory() {
    let setup = Setup::new();
    wezterm_attention::claim_launch(&setup.env, &setup.ports()).expect("claim");
    let config = setup._scratch.0.join("claude-config");
    let event = parse_provider_event(
        "claude",
        "SessionStart",
        &json!({"session_id":"session-a","cwd":"/tmp/project",
                "hook_event_name":"SessionStart","source":"startup"}),
        &BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), config.display().to_string())]),
    );
    apply_provider_event(&event, &setup.env, "00000000000000000200", &setup.ports()).expect("bind");
    record_claude_host(&setup);
    write_status(&config, &status_file("idle"));
    let (result, _) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&setup.processes),
        &OneHost { running: true },
    )
    .expect("doctor");
    assert_eq!(
        probe_status(&result, "claude_status"),
        "healthy",
        "{result}"
    );
}
