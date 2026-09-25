use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;
use wezterm_attention::identity::pane_address;
use wezterm_attention::lifecycle::{
    apply_mark_activity, apply_mark_clear, apply_mark_review, apply_provider_event, binding_id,
    prompt_return,
};
use wezterm_attention::observations::LifecycleSnapshot;
use wezterm_attention::providers::{ProviderAction, ProviderEvent, parse_provider_event};
use wezterm_attention::query::read_bindings;
use wezterm_attention::records::{atomic_replace, launch_path, pane_path, state_root, with_lock};
use wezterm_attention::wezterm::{
    Clock, ControllingTerminal, PaneLister, PaneRow, ProcessFacts, ProcessInspector, ProcessRead,
    ProcessStart, RuntimePorts, TtyWriter,
};

#[path = "support/executables.rs"]
mod executables;

#[path = "lifecycle_spec/hook_consumer.rs"]
mod hook_consumer;

#[path = "lifecycle_spec/pane_facts.rs"]
mod pane_facts;

#[path = "lifecycle_spec/consumer_recipes.rs"]
mod consumer_recipes;

#[path = "lifecycle_spec/untrusted_text.rs"]
mod untrusted_text;

#[path = "lifecycle_spec/session_starts.rs"]
mod session_starts;

#[path = "lifecycle_spec/turn_endings.rs"]
mod turn_endings;

#[path = "lifecycle_spec/metadata_fields.rs"]
mod metadata_fields;

#[path = "lifecycle_spec/mark_clear.rs"]
mod mark_clear;

#[path = "lifecycle_spec/self_claim.rs"]
mod self_claim;

struct Scratch(PathBuf);

#[test]
fn c2_plain_text_marker_is_left_alone() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "repair",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let marker = state_root(&setup.env).unwrap().join("42");
    fs::write(&marker, "thinking\n").unwrap();
    let result = apply_provider_event(
        &event("claude", "Stop", "repair", json!({})),
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    assert!(
        result.is_ok(),
        "third-party marker blocked the v2 write: {result:?}"
    );
    assert_eq!(fs::read(&marker).unwrap(), b"thinking\n");
}

impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("wl-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct FixedClock {
    monotonic: &'static str,
    unix: &'static str,
}

impl Clock for FixedClock {
    fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.monotonic.to_owned())
    }

    fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.unix.to_owned())
    }
}

struct FakeTty {
    path: String,
    fingerprint: String,
    writes: Mutex<Vec<Vec<u8>>>,
}

impl FakeTty {
    fn new() -> Self {
        Self {
            path: "/dev/ttys777".to_owned(),
            fingerprint: "f".repeat(64),
            writes: Mutex::new(Vec::new()),
        }
    }
}

impl TtyWriter for FakeTty {
    fn current_path(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.path.clone())
    }

    fn controlling_path(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.path.clone())
    }

    fn fingerprint(&self, _path: &str) -> wezterm_attention::protocol::Result<String> {
        Ok(self.fingerprint.clone())
    }

    fn write(
        &self,
        _path: &str,
        data: &[u8],
        _expected_fingerprint: &str,
    ) -> wezterm_attention::protocol::Result<()> {
        self.writes.lock().expect("writes lock").push(data.to_vec());
        Ok(())
    }
}

/// The panes a mux lists, or `None` for a listing that fails.
struct FakePanes {
    rows: Mutex<Option<Vec<PaneRow>>>,
    /// Runs as each listing is taken, before it answers.
    on_list: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl FakePanes {
    fn new(rows: Vec<PaneRow>) -> Self {
        Self {
            rows: Mutex::new(Some(rows)),
            on_list: Mutex::new(None),
        }
    }

    fn set(&self, rows: Option<Vec<PaneRow>>) {
        *self.rows.lock().expect("rows lock") = rows;
    }
}

impl PaneLister for FakePanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        let hook = self.on_list.lock().expect("hook lock").clone();
        if let Some(hook) = hook {
            hook();
        }
        self.rows.lock().expect("rows lock").clone().ok_or_else(|| {
            wezterm_attention::protocol::AttentionError::new(
                "realm_unavailable",
                "synthetic listing failure",
            )
        })
    }
}

/// The process ids a fake agent tree uses: the shell that started the agent,
/// the agent, and the hook the agent runs.
const SHELL_PID: i32 = 3000;
const AGENT_PID: i32 = 4000;
const HOOK_PID: i32 = 5000;
const USER_ID: u32 = 501;
const PANE_TTY_DEVICE: u64 = 0x1000_0777;
const BOOT_SESSION: &str = "0f9a7c3e-51b2-4d6e-8a1b-2c3d4e5f6a7b";

fn process_facts(pid: i32, parent_pid: i32, terminal: ControllingTerminal) -> ProcessFacts {
    ProcessFacts {
        pid,
        parent_pid,
        process_group: pid,
        terminal,
        terminal_foreground_group: AGENT_PID,
        uid: USER_ID,
        start: ProcessStart {
            seconds: 1_700_000_000,
            microseconds: u32::try_from(pid).expect("small pid"),
        },
        traced: false,
        zombie: false,
    }
}

type ReadHook = std::sync::Arc<dyn Fn(i32) -> Option<ProcessRead> + Send + Sync>;

/// A process table an agent's hook reads. By default the agent leads the
/// pane terminal's foreground job, and its hook runs detached from any
/// terminal, the way Claude Code starts one.
struct FakeProcesses {
    supported: Mutex<bool>,
    own: Mutex<i32>,
    table: Mutex<BTreeMap<i32, ProcessRead>>,
    boot: Mutex<Option<String>>,
    devices: Mutex<BTreeMap<String, u64>>,
    /// Runs on every process read, with the pid read; an answer it gives
    /// replaces the table's.
    on_read: Mutex<Option<ReadHook>>,
}

impl FakeProcesses {
    fn new(tty_path: &str) -> Self {
        let table = BTreeMap::from([
            (
                SHELL_PID,
                ProcessRead::Found(process_facts(
                    SHELL_PID,
                    1,
                    ControllingTerminal::Device(PANE_TTY_DEVICE),
                )),
            ),
            (
                AGENT_PID,
                ProcessRead::Found(process_facts(
                    AGENT_PID,
                    SHELL_PID,
                    ControllingTerminal::Device(PANE_TTY_DEVICE),
                )),
            ),
            (
                HOOK_PID,
                ProcessRead::Found(process_facts(
                    HOOK_PID,
                    AGENT_PID,
                    ControllingTerminal::Absent,
                )),
            ),
        ]);
        Self {
            supported: Mutex::new(true),
            own: Mutex::new(HOOK_PID),
            table: Mutex::new(table),
            boot: Mutex::new(Some(BOOT_SESSION.to_owned())),
            devices: Mutex::new(BTreeMap::from([(tty_path.to_owned(), PANE_TTY_DEVICE)])),
            on_read: Mutex::new(None),
        }
    }

    fn set(&self, pid: i32, read: ProcessRead) {
        self.table.lock().expect("table lock").insert(pid, read);
    }

    fn facts(&self, pid: i32) -> ProcessFacts {
        match self.table.lock().expect("table lock").get(&pid) {
            Some(ProcessRead::Found(facts)) => facts.clone(),
            other => panic!("pid {pid} is not a live process here: {other:?}"),
        }
    }

    fn change(&self, pid: i32, change: impl FnOnce(&mut ProcessFacts)) {
        let mut facts = self.facts(pid);
        change(&mut facts);
        self.set(pid, ProcessRead::Found(facts));
    }
}

impl ProcessInspector for FakeProcesses {
    fn self_claim_supported(&self) -> bool {
        *self.supported.lock().expect("supported lock")
    }

    fn own_pid(&self) -> i32 {
        *self.own.lock().expect("own lock")
    }

    fn process(&self, pid: i32) -> ProcessRead {
        let hook = self.on_read.lock().expect("hook lock").clone();
        if let Some(read) = hook.and_then(|hook| hook(pid)) {
            return read;
        }
        self.table
            .lock()
            .expect("table lock")
            .get(&pid)
            .cloned()
            .unwrap_or(ProcessRead::Gone)
    }

    fn boot_session(&self) -> Option<String> {
        self.boot.lock().expect("boot lock").clone()
    }

    fn terminal_device(&self, path: &str) -> Option<u64> {
        self.devices
            .lock()
            .expect("devices lock")
            .get(path)
            .copied()
    }
}

struct Setup {
    _scratch: Scratch,
    _socket: UnixListener,
    env: BTreeMap<String, String>,
    tty: FakeTty,
    panes: FakePanes,
    processes: FakeProcesses,
    clock: FixedClock,
}

impl Setup {
    fn new() -> Self {
        let scratch = Scratch::new();
        let socket_path = scratch.0.join("mux.sock");
        let socket = UnixListener::bind(&socket_path).expect("bind disposable socket");
        let tty = FakeTty::new();
        let panes = FakePanes::new(vec![PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some(tty.path.clone()),
        }]);
        let processes = FakeProcesses::new(&tty.path);
        let env = BTreeMap::from([
            ("HOME".to_owned(), scratch.0.to_string_lossy().into_owned()),
            (
                "WEZTERM_ATTENTION_DIR".to_owned(),
                scratch.0.join("state").to_string_lossy().into_owned(),
            ),
            (
                "WEZTERM_UNIX_SOCKET".to_owned(),
                socket_path.to_string_lossy().into_owned(),
            ),
            ("WEZTERM_PANE".to_owned(), "42".to_owned()),
            (
                "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
                "00000000-0000-4000-8000-000000000401".to_owned(),
            ),
        ]);
        Self {
            _scratch: scratch,
            _socket: socket,
            env,
            tty,
            panes,
            processes,
            clock: FixedClock {
                monotonic: "00000000000000000100",
                unix: "00000000012345678900",
            },
        }
    }

    fn ports(&self) -> RuntimePorts<'_> {
        RuntimePorts {
            clock: &self.clock,
            tty: &self.tty,
            panes: &self.panes,
            processes: &self.processes,
        }
    }

    /// The environment an agent's hook runs in when its agent was started
    /// without a claim: no launch id, and the agent's pid asserted by the
    /// hook entry.
    fn agent_env(&self) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
        env.insert(
            "WEZTERM_ATTENTION_HOST_PID".to_owned(),
            AGENT_PID.to_string(),
        );
        env
    }

    fn claim(&self) {
        wezterm_attention::claim_launch(&self.env, &self.ports()).expect("claim succeeds");
    }

    fn apply(
        &self,
        event: &ProviderEvent,
        observation: &str,
    ) -> wezterm_attention::lifecycle::LifecycleResult {
        apply_provider_event(event, &self.env, observation, &self.ports()).expect("event applies")
    }

    fn binding_dir(&self, provider: &str, session: &str) -> PathBuf {
        let root = state_root(&self.env).expect("state root");
        let (address, _) = pane_address(&self.env).expect("address");
        let launch_id = &self.env["WEZTERM_ATTENTION_LAUNCH_ID"];
        launch_path(&root, &address, launch_id)
            .join("bindings")
            .join(binding_id(provider, session, launch_id))
    }
}

fn rust_command(setup: &Setup) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_attention"));
    command.env_clear().envs(&setup.env);
    command
}

#[test]
fn same_claim_event_reaches_snapshot() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "facts",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let tool = event(
        "codex",
        "PreToolUse",
        "facts",
        json!({"tool_name":"shell", "tool_use_id":"call-1"}),
    );
    setup.apply(&tool, "00000000000000000300");
    let directory = setup.binding_dir("codex", "facts");
    let before: Value =
        serde_json::from_slice(&fs::read(directory.join("activity.json")).unwrap()).unwrap();
    let first: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(directory.join("lifecycle.json")).unwrap()).unwrap();
    assert_eq!(first.pools.general.observations.len(), 1);
    setup.apply(&tool, "00000000000000000400");
    let after: Value =
        serde_json::from_slice(&fs::read(directory.join("activity.json")).unwrap()).unwrap();
    assert_eq!(before, after, "facts must not refresh an equal badge");
    let second: LifecycleSnapshot =
        serde_json::from_slice(&fs::read(directory.join("lifecycle.json")).unwrap()).unwrap();
    assert_eq!(
        first, second,
        "native retry retains exact observation identity"
    );
}

#[test]
fn lifecycle_manifest_and_typed_union_agree() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/observations.json")).unwrap();
    let protocol = wezterm_attention::protocol::manifest().unwrap();
    let mut kinds = BTreeSet::new();
    for case in fixture["raw_cases"].as_array().unwrap() {
        let value: Value = serde_json::from_str(case["raw"].as_str().unwrap()).unwrap();
        assert_eq!(
            wezterm_attention::protocol::parse_record_value(&value, protocol).as_str(),
            case["expected"].as_str().unwrap(),
            "{}",
            case["id"]
        );
    }
    for case in fixture["cases"].as_array().unwrap() {
        let verdict = wezterm_attention::protocol::parse_record_value(&case["value"], protocol);
        assert_eq!(
            verdict.as_str(),
            case["expected"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        if verdict == wezterm_attention::protocol::Verdict::Valid {
            let typed: LifecycleSnapshot = serde_json::from_value(case["value"].clone()).unwrap();
            assert_eq!(serde_json::to_value(&typed).unwrap(), case["value"]);
            for item in typed
                .pools
                .requests
                .observations
                .iter()
                .chain(&typed.pools.general.observations)
            {
                kinds.insert(
                    serde_json::to_value(item).unwrap()["kind"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
    }
    assert_eq!(kinds, protocol.lifecycle_variants.keys().cloned().collect());
    assert_eq!(kinds.len(), 14);
    assert_eq!(protocol.limits.lifecycle_pool_max_count, 64);
    assert_eq!(protocol.limits.lifecycle_pool_max_bytes, 122_880);
    assert_eq!(protocol.limits.lifecycle_observation_max_bytes, 2_048);
    assert_eq!(protocol.limits.lifecycle_max_json_bytes, 262_144);
    assert_eq!(protocol.limits.lifecycle_envelope_max_bytes, 16_384);
    assert_eq!(protocol.limits.lifecycle_max_depth, 8);
}

#[test]
fn observation_pools_preserve_each_other_and_fence_evicted_receipts() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/observations.json")).unwrap();
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_value(fixture["cases"][14]["value"].clone()).unwrap();
    let request_pool = snapshot.pools.requests.clone();
    let generic: LifecycleSnapshot =
        serde_json::from_value(fixture["cases"][1]["value"].clone()).unwrap();
    let template = generic.pools.general.observations[0].clone();
    for index in 0..130 {
        let mut item = template.clone();
        item.observation_id = Uuid::new_v4().to_string();
        item.observed_mono_ns = format!("{:020}", 1000 + index);
        assert!(snapshot.reduce(item).unwrap());
        assert_eq!(snapshot.pools.requests, request_pool);
    }
    assert_eq!(snapshot.pools.general.observations.len(), 64);
    assert_eq!(
        snapshot.pools.general.retention_floor_mono_ns.as_deref(),
        Some("00000000000000001065")
    );
    let retained = snapshot.clone();
    let mut delayed = template.clone();
    delayed.observed_mono_ns = "00000000000000001065".to_owned();
    assert!(!snapshot.reduce(delayed).unwrap());
    assert_eq!(snapshot, retained);
    let mut oversized = template;
    oversized.source_version = Some("x".repeat(2048));
    assert!(snapshot.reduce(oversized).is_err());
    assert_eq!(snapshot, retained);
}

#[test]
fn malformed_child_identity_never_becomes_lead() {
    for bad in [Value::Null, json!(17), json!(""), json!("bad\nchild")] {
        let parsed = event(
            "codex",
            "PreToolUse",
            "facts",
            json!({"tool_name":"shell", "agent_id":bad}),
        );
        assert_eq!(parsed.action, ProviderAction::Ignored);
        assert!(parsed.observation.is_none());
    }
}

#[test]
fn rich_rejection_preserves_legacy_contract() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "facts",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let result = setup.apply(
        &event(
            "codex",
            "PreToolUse",
            "facts",
            json!({"tool_name":"shell", "tool_use_id":17}),
        ),
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "partial");
    assert!(
        setup
            .binding_dir("codex", "facts")
            .join("activity.json")
            .exists()
    );
    assert!(
        !setup
            .binding_dir("codex", "facts")
            .join("lifecycle.json")
            .exists()
    );
}

#[test]
fn tty_presence_is_not_execution_identity() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "facts",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let directory = setup.binding_dir("codex", "facts");
    let result = apply_provider_event(
        &event("codex", "PreToolUse", "facts", json!({"tool_name":"shell"})),
        &setup.agent_env(),
        "00000000000000000300",
        &setup.ports(),
    )
    .expect("a refusal is a result");
    assert_eq!(result.disposition, "ignored");
    assert_eq!(
        result.diagnostic.as_ref().map(|item| item.code.as_str()),
        Some("claim_stale")
    );
    assert!(!directory.join("activity.json").exists());
    assert!(!directory.join("lifecycle.json").exists());
}

#[test]
fn real_cli_tool_snapshot_reaches_installed_wezterm() {
    let setup = Setup::new();
    setup.claim();
    let started = run_hook(
        &setup,
        &["hooks", "event", "codex", "SessionStart", "--strict"],
        &payload(
            "codex",
            "SessionStart",
            "cli-facts",
            json!({"source":"startup"}),
        ),
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let tool = run_hook(
        &setup,
        &["hooks", "event", "codex", "PreToolUse", "--strict"],
        &payload(
            "codex",
            "PreToolUse",
            "cli-facts",
            json!({"tool_name":"shell", "tool_use_id":"cli-call"}),
        ),
    );
    assert!(
        tool.status.success(),
        "{}",
        String::from_utf8_lossy(&tool.stderr)
    );
    assert!(tool.stdout.is_empty());
    let (address, _) = pane_address(&setup.env).unwrap();
    let wire =
        json!({"wire":2,"address":address,"launch_id":setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]});
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let result = setup._scratch.0.join("wezterm-result");
    let wezterm = executables::resolve("wezterm");
    let output = Command::new(&wezterm)
        .env_clear()
        .env("PATH", executables::child_path(&[], &[&wezterm]))
        .env("WEZTERM_ATTENTION_SMOKE_RESULT", &result)
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env(
            "WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_DIR",
            &setup.env["WEZTERM_ATTENTION_DIR"],
        )
        .env("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE", wire.to_string())
        .args(["--config-file"])
        .arg(root.join("tests/lua/wezterm_protocol_smoke.lua"))
        .args(["show-keys", "--lua"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let evidence = fs::read_to_string(result).unwrap();
    assert!(evidence.starts_with("ok -"), "{evidence}");
}

#[test]
fn lifecycle_raw_read_is_bounded_before_decode() {
    use wezterm_attention::protocol::{bounded_lifecycle_json, manifest};
    let limit = manifest().unwrap().limits.lifecycle_max_json_bytes;
    assert!(!bounded_lifecycle_json(&vec![b' '; limit + 1]));
    assert!(!bounded_lifecycle_json(b"[[[[[[[[[]]]]]]]]]"));
    assert!(bounded_lifecycle_json(br#"{"quoted":"[[[[[[[[[[[[["}"#));
    let setup = Setup::new();
    let file = setup._scratch.0.join("lifecycle.json");
    fs::write(&file, vec![b' '; limit + 1]).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    assert!(
        wezterm_attention::records::read_record(
            &file,
            Some("lifecycle_snapshot"),
            &wezterm_attention::records::RecordIdentity::pane(&address)
        )
        .is_err()
    );
}

#[test]
fn same_key_order_conflict_and_equal_time_eviction() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/observations.json")).unwrap();
    let mut snapshot: LifecycleSnapshot =
        serde_json::from_value(fixture["cases"][1]["value"].clone()).unwrap();
    let mut item = snapshot.pools.general.observations.remove(0);
    item.correlation = Some(wezterm_attention::observations::NativeCorrelation {
        tool_call_id: Some("stable".into()),
        ..Default::default()
    });
    assert!(snapshot.reduce(item.clone()).unwrap());
    let mut older = item.clone();
    older.observed_mono_ns = "00000000000000000001".into();
    older.source_version = Some("old".into());
    assert!(!snapshot.reduce(older).unwrap());
    let mut conflict = item.clone();
    conflict.source_version = Some("conflict".into());
    assert!(snapshot.reduce(conflict).is_err());
    for _ in 0..63 {
        let mut sibling = item.clone();
        sibling.observation_id = Uuid::new_v4().to_string();
        sibling.correlation = None;
        assert!(snapshot.reduce(sibling).unwrap());
    }
    // A full pool evicts everything at its oldest instant. When that instant
    // is the candidate's own, the candidate goes too, so nothing is stored
    // and the full pool stays as it was.
    let full = snapshot.clone();
    let mut sibling = item.clone();
    sibling.observation_id = Uuid::new_v4().to_string();
    sibling.correlation = None;
    assert!(!snapshot.reduce(sibling).unwrap());
    assert_eq!(snapshot, full);
    assert_eq!(snapshot.pools.general.observations.len(), 64);
    assert!(snapshot.pools.general.retention_floor_mono_ns.is_none());
    assert!(snapshot.pools.requests.retention_floor_mono_ns.is_none());
}

#[test]
fn tool_post_failure_and_future_snapshot_preserve_badge_identity() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "tool-phases",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "claude",
            "PreToolUse",
            "tool-phases",
            json!({"tool_name":"Bash","tool_use_id":"tool-1"}),
        ),
        "00000000000000000300",
    );
    let directory = setup.binding_dir("claude", "tool-phases");
    let badge = fs::read(directory.join("activity.json")).unwrap();
    for (name, order) in [
        ("PostToolUse", "00000000000000000400"),
        ("PostToolUseFailure", "00000000000000000500"),
    ] {
        let result = setup.apply(&event("claude", name, "tool-phases", json!({"tool_name":"Bash","tool_use_id":"tool-1","tool_response":"SYNTHETIC-PRIVATE-SENTINEL"})), order);
        assert_eq!(result.disposition, "applied");
        assert_eq!(fs::read(directory.join("activity.json")).unwrap(), badge);
    }
    let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
    assert!(!raw.contains("SYNTHETIC-PRIVATE-SENTINEL"));
    let snapshot: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        snapshot["pools"]["general"]["observations"][1]["is_error"],
        true
    );
    let mut future = snapshot;
    future["schema"] = json!(4);
    let future_bytes = serde_json::to_vec(&future).unwrap();
    fs::write(directory.join("lifecycle.json"), &future_bytes).unwrap();
    let result = setup.apply(
        &event(
            "claude",
            "PreToolUse",
            "tool-phases",
            json!({"tool_name":"Read","tool_use_id":"tool-2"}),
        ),
        "00000000000000000600",
    );
    assert_eq!(result.disposition, "partial");
    assert_eq!(
        fs::read(directory.join("lifecycle.json")).unwrap(),
        future_bytes
    );
    assert_eq!(fs::read(directory.join("activity.json")).unwrap(), badge);
}

#[test]
fn request_observations_are_passive_and_namespaced() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "requests",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "claude",
            "PermissionRequest",
            "requests",
            json!({"tool_name":"Bash"}),
        ),
        "00000000000000000300",
    );
    let directory = setup.binding_dir("claude", "requests");
    let badge = fs::read(directory.join("activity.json")).unwrap();
    for (index, (name, patch)) in [
        ("PermissionDenied", json!({"tool_name":"Bash","tool_use_id":"denied-1","permission_mode":"auto"})),
        ("Elicitation", json!({"mcp_server_name":"server-a","elicitation_id":"same-id","mode":"form"})),
        ("ElicitationResult", json!({"mcp_server_name":"server-b","elicitation_id":"same-id","action":"accept"})),
        ("Notification", json!({"notification_type":"elicitation_url_dialog","message":"SYNTHETIC-PRIVATE-SENTINEL"})),
    ].into_iter().enumerate() {
        let result = setup.apply(&event("claude", name, "requests", patch), &format!("{:020}", 400 + index));
        assert_eq!(result.disposition, "applied");
        assert_eq!(fs::read(directory.join("activity.json")).unwrap(), badge);
    }
    let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
    assert!(!raw.contains("SYNTHETIC-PRIVATE-SENTINEL"));
    let snapshot: LifecycleSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(snapshot.pools.requests.observations.len(), 5);
}

#[test]
fn async_post_only_publication_requires_exact_success_receipt() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "async",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let patch = json!({"tool_name":"request_user_input_async","tool_use_id":"question-call","turn_id":"turn-a","tool_response":"{\"accepted\":true}"});
    let result = setup.apply(
        &event("codex", "PostToolUse", "async", patch.clone()),
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "applied");
    let directory = setup.binding_dir("codex", "async");
    assert!(!directory.join("activity.json").exists());
    let before = fs::read(directory.join("lifecycle.json")).unwrap();
    for receipt in [
        json!([{"type":"input_text","text":"{\"accepted\":true}"}]),
        json!("{\"accepted\":false}"),
        json!("{\"accepted\":true,\"answer\":\"private\"}"),
        json!(null),
    ] {
        let mut rejected = patch.clone();
        rejected["tool_response"] = receipt;
        let result = setup.apply(
            &event("codex", "PostToolUse", "async", rejected),
            "00000000000000000400",
        );
        assert_eq!(result.disposition, "partial");
        assert_eq!(fs::read(directory.join("lifecycle.json")).unwrap(), before);
    }
}

#[test]
fn attempt_failure_retry_and_settling_do_not_end_a_binding() {
    for provider in ["claude", "codex", "pi"] {
        let setup = Setup::new();
        setup.claim();
        let (start, start_patch) = if provider == "pi" {
            ("session_start", json!({"start_source":"startup"}))
        } else {
            ("SessionStart", json!({"source":"startup"}))
        };
        setup.apply(
            &event(provider, start, "run", start_patch),
            "00000000000000000200",
        );
        let cases = match provider {
            "claude" => vec![
                (
                    "UserPromptSubmit",
                    json!({"prompt":"SYNTHETIC-PRIVATE-SENTINEL"}),
                ),
                (
                    "StopFailure",
                    json!({"error":"rate_limit","error_details":"SYNTHETIC-PRIVATE-SENTINEL"}),
                ),
                (
                    "PreToolUse",
                    json!({"tool_name":"Read","tool_use_id":"retry"}),
                ),
                ("Stop", json!({"stop_hook_active":true})),
            ],
            "codex" => vec![
                (
                    "UserPromptSubmit",
                    json!({"turn_id":"turn-1","prompt":"SYNTHETIC-PRIVATE-SENTINEL"}),
                ),
                ("Interrupt", json!({"turn_id":"turn-1"})),
                (
                    "PreToolUse",
                    json!({"tool_name":"shell","tool_use_id":"retry","turn_id":"turn-2"}),
                ),
                ("Stop", json!({"stop_hook_active":false,"turn_id":"turn-2"})),
            ],
            _ => vec![
                ("input", json!({"source":"extension"})),
                (
                    "message_end",
                    json!({"role":"assistant","stop_reason":"aborted"}),
                ),
                ("agent_start", json!({})),
                ("agent_settled", json!({})),
            ],
        };
        for (index, (name, patch)) in cases.into_iter().enumerate() {
            let result = setup.apply(
                &event(provider, name, "run", patch),
                &format!("{:020}", 300 + index),
            );
            // A prompt already tints the pane `thinking`, so the first tool call
            // of the same turn repeats it and is skipped rather than republished.
            assert!(
                matches!(result.disposition.as_str(), "applied" | "skipped"),
                "{provider}:{name} {}",
                result.disposition
            );
            assert!(!setup.binding_dir(provider, "run").join("end.json").exists());
        }
        let raw =
            fs::read_to_string(setup.binding_dir(provider, "run").join("lifecycle.json")).unwrap();
        assert!(!raw.contains("SYNTHETIC-PRIVATE-SENTINEL"));
        assert!(raw.contains("prompt_submitted"));
        assert!(raw.contains(if provider == "pi" {
            "run_settled"
        } else {
            "response_finished"
        }));
        assert!(raw.contains(if provider == "codex" {
            "user_interrupt"
        } else {
            "attempt_outcome"
        }));
    }
    for patch in [
        json!({"role":"user","stop_reason":"error"}),
        json!({"role":"assistant","stop_reason":"stop"}),
    ] {
        assert_eq!(
            event("pi", "message_end", "run", patch).action,
            ProviderAction::Ignored
        );
    }
}

#[test]
fn actual_pi_runner_dispatches_through_the_real_writer() {
    let setup = Setup::new();
    setup.claim();
    let bridge_root = setup._scratch.0.join("bridge");
    fs::create_dir_all(bridge_root.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_attention"),
        bridge_root.join("bin/attention"),
    )
    .unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let node = executables::resolve("node");
    let mut command = Command::new(&node);
    command
        .env_clear()
        .envs(&setup.env)
        .env("PATH", executables::child_path(&[], &[&node]))
        .env("WEZTERM_ATTENTION_ROOT", &bridge_root)
        .arg(root.join("tests/javascript/pi_lifecycle_runtime.mjs"));
    if let Ok(runtime) = std::env::var("ATTENTION_PI_RUNTIME_ROOT") {
        command.env("ATTENTION_PI_RUNTIME_ROOT", runtime);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw =
        fs::read_to_string(setup.binding_dir("pi", "pi-runtime").join("lifecycle.json")).unwrap();
    assert!(!raw.contains("SYNTHETIC-PRIVATE-SENTINEL"));
    let snapshot: Value = serde_json::from_str(&raw).unwrap();
    let observations = snapshot["pools"]["general"]["observations"]
        .as_array()
        .unwrap();
    assert_eq!(observations.len(), 8);
    assert!(
        observations
            .iter()
            .any(|item| item["kind"] == "compaction_attempted" && item["trigger"] == "threshold")
    );
    assert!(
        observations
            .iter()
            .any(|item| item["kind"] == "compaction_succeeded" && item["trigger"] == "manual")
    );
    assert!(observations.iter().any(|item| item["kind"] == "tool_result"
        && item["is_error"] == true
        && item["tool_class"] == "generic"));
    assert!(
        observations
            .iter()
            .any(|item| item["kind"] == "attempt_outcome" && item["outcome"] == "aborted")
    );
    assert!(
        !setup
            .binding_dir("pi", "pi-runtime")
            .join("end.json")
            .exists(),
        "reload keeps the binding"
    );
}

#[test]
fn compaction_is_not_agent_completion() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        setup.claim();
        setup.apply(
            &event(
                provider,
                "SessionStart",
                "compact",
                json!({"source":"startup"}),
            ),
            "00000000000000000200",
        );
        setup.apply(
            &event(
                provider,
                "PreToolUse",
                "compact",
                json!({"tool_name":"Read", "tool_use_id":"work"}),
            ),
            "00000000000000000300",
        );
        let directory = setup.binding_dir(provider, "compact");
        let badge = fs::read(directory.join("activity.json")).unwrap();
        for (name, order) in [
            ("PostCompact", "00000000000000000400"),
            ("PreCompact", "00000000000000000500"),
        ] {
            let result = setup.apply(
                &event(provider, name, "compact", json!({"trigger":"manual"})),
                order,
            );
            assert_eq!(result.disposition, "applied");
            assert_eq!(fs::read(directory.join("activity.json")).unwrap(), badge);
            assert!(!directory.join("end.json").exists());
            assert!(!directory.join("agents-clear.json").exists());
        }
        let before = fs::read(directory.join("lifecycle.json")).unwrap();
        setup.apply(
            &event(
                provider,
                "SessionStart",
                "compact",
                json!({"source":"compact"}),
            ),
            "00000000000000000600",
        );
        assert_eq!(
            fs::read(directory.join("lifecycle.json")).unwrap(),
            before,
            "compact metadata does not rebind evidence"
        );
        let bad = setup.apply(
            &event(
                provider,
                "PreCompact",
                "compact",
                json!({"trigger":"overflow"}),
            ),
            "00000000000000000700",
        );
        assert_eq!(bad.disposition, "partial");
        assert_eq!(fs::read(directory.join("lifecycle.json")).unwrap(), before);
    }
    assert_eq!(
        event("pi", "session_compact_failed", "compact", json!({})).action,
        ProviderAction::Ignored
    );
}

#[test]
fn byte_pressure_is_pool_local_with_maximal_valid_metadata() {
    use wezterm_attention::observations::{
        Actor, NativeCorrelation, ObservationBody, ResultSurface, ToolClass,
    };
    let cases: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/observations.json")).unwrap();
    for requests in [false, true] {
        let mut snapshot: LifecycleSnapshot =
            serde_json::from_value(cases["cases"][1]["value"].clone()).unwrap();
        snapshot.provider = "claude".into();
        let mut item = snapshot.pools.general.observations[0].clone();
        item.source_version = Some("v".repeat(256));
        let agent_id = "a".repeat(256);
        item.actor = Actor::Child {
            agent_key: wezterm_attention::protocol::sha256_hex(agent_id.as_bytes()),
            agent_id,
        };
        item.correlation = Some(NativeCorrelation {
            tool_call_id: Some("t".repeat(256)),
            turn_id: Some("n".repeat(256)),
            message_id: Some("m".repeat(256)),
            ..Default::default()
        });
        if requests {
            item.source_event = "PermissionRequest".into();
            item.body = ObservationBody::ApprovalRequested {
                tool_name: Some("x".repeat(256)),
            };
        } else {
            item.source_event = "PostToolUse".into();
            item.body = ObservationBody::ToolResult {
                tool_name: "x".repeat(256),
                tool_class: ToolClass::Generic,
                question_mode: None,
                result_surface: ResultSurface::SuccessHook,
                is_error: Some(false),
                interrupted: Some(false),
            };
        }
        let size = serde_json::to_vec(&item).unwrap().len();
        assert!(
            size <= 2048 && size * 64 > 122880,
            "metadata must exercise bytes before count: {size}"
        );
        let other = if requests {
            snapshot.pools.general.clone()
        } else {
            snapshot.pools.requests.clone()
        };
        for index in 0..80 {
            let mut candidate = item.clone();
            candidate.observation_id = Uuid::new_v4().to_string();
            candidate.observed_mono_ns = format!("{:020}", 1000 + index);
            candidate.correlation.as_mut().unwrap().tool_call_id =
                Some(format!("{}{:010}", "t".repeat(246), index));
            snapshot.reduce(candidate).unwrap();
        }
        let (changed, unchanged) = if requests {
            (&snapshot.pools.requests, &snapshot.pools.general)
        } else {
            (&snapshot.pools.general, &snapshot.pools.requests)
        };
        assert_eq!(unchanged, &other);
        assert!(changed.observations.len() < 64 && changed.retention_floor_mono_ns.is_some());
        let value = serde_json::to_value(&snapshot).unwrap();
        wezterm_attention::protocol::validate_record(&value, Some("lifecycle_snapshot")).unwrap();
    }
}

#[test]
fn native_codex_async_tool_hooks_reach_the_snapshot() {
    // This drives a real Codex checkout, which is an external source tree rather
    // than something the repository can provide. It used to fall back to one
    // developer's clone path, so it passed there and failed everywhere else.
    // Absent the variable it now says what it needs and stops, rather than
    // failing the gate on a machine that simply has no Codex source.
    // An empty value is absent. `env::var` returns Ok("") for it while the gate's
    // own `-n` test calls it unset, so without this the gate announces SKIPPED
    // and then fails here.
    let Some(codex_source) = std::env::var("ATTENTION_CODEX_SOURCE")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!(
            "SKIPPED native_codex_async_tool_hooks_reach_the_snapshot: set ATTENTION_CODEX_SOURCE to a Codex checkout to run it"
        );
        return;
    };
    let setup = Setup::new();
    setup.claim();
    let bridge = setup._scratch.0.join("native-bridge");
    fs::create_dir_all(bridge.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_attention"),
        bridge.join("bin/attention"),
    )
    .unwrap();
    let node = executables::resolve("node");
    // The probe this runs spawns codex itself, so it has to be reachable on the
    // PATH handed to the child, not only on the PATH that started cargo.
    let codex = executables::resolve("codex");
    let output = Command::new(&node)
        .env_clear()
        .envs(&setup.env)
        .env("PATH", executables::child_path(&[], &[&node, &codex]))
        // Hand the probe the executable that was selected here. A PATH cannot
        // carry two independent selections: either directory on it may contain
        // both program names, so letting the probe search again can hand it a
        // different codex than this test chose.
        .env("ATTENTION_TEST_CODEX", &codex)
        .env("WEZTERM_ATTENTION_ROOT", &bridge)
        .env("ATTENTION_CODEX_SOURCE", codex_source)
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/javascript/lifecycle_contact_probe.mjs"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let session = report["session_id"].as_str().unwrap();
    let record: Value = serde_json::from_slice(
        &fs::read(setup.binding_dir("codex", session).join("lifecycle.json")).unwrap(),
    )
    .unwrap();
    assert!(
        record["pools"]["requests"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "tool_result" && item["question_mode"] == "nonblocking")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let result_path = setup._scratch.0.join("native-consumer-smoke");
    let (address, _) = pane_address(&setup.env).unwrap();
    let wire =
        json!({"wire":2,"address":address,"launch_id":setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]});
    let wezterm = executables::resolve("wezterm");
    let output = Command::new(&wezterm)
        .env_clear()
        .env("PATH", executables::child_path(&[], &[&wezterm]))
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env("WEZTERM_ATTENTION_SMOKE_RESULT", &result_path)
        .env(
            "WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_DIR",
            &setup.env["WEZTERM_ATTENTION_DIR"],
        )
        .env("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE", wire.to_string())
        .env("WEZTERM_ATTENTION_LIFECYCLE_SCENARIO", "publication")
        .arg("--config-file")
        .arg(root.join("tests/lua/wezterm_protocol_smoke.lua"))
        .args(["show-keys", "--lua"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result = fs::read_to_string(result_path).unwrap();
    assert!(result.starts_with("ok -"), "{result}");
}

#[test]
#[ignore = "requires test-only xterm/headless; the full gate supplies its module path"]
fn native_codex_queued_input_is_not_blocked_by_a_pending_question() {
    // This drives a real Codex checkout, which is an external source tree rather
    // than something the repository can provide. It used to fall back to one
    // developer's clone path, so it passed there and failed everywhere else.
    // Absent the variable it now says what it needs and stops, rather than
    // failing the gate on a machine that simply has no Codex source.
    // An empty value is absent. `env::var` returns Ok("") for it while the gate's
    // own `-n` test calls it unset, so without this the gate announces SKIPPED
    // and then fails here.
    let Some(codex_source) = std::env::var("ATTENTION_CODEX_SOURCE")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!(
            "SKIPPED native_codex_queued_input_is_not_blocked_by_a_pending_question: set ATTENTION_CODEX_SOURCE to a Codex checkout to run it"
        );
        return;
    };
    let setup = Setup::new();
    setup.claim();
    let bridge = setup._scratch.0.join("native-ui-bridge");
    fs::create_dir_all(bridge.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_attention"),
        bridge.join("bin/attention"),
    )
    .unwrap();
    let module = std::env::var("ATTENTION_XTERM_MODULE")
        .expect("set the disposable xterm/headless module path");
    let node = executables::resolve("node");
    // The probe this runs spawns codex itself, so it has to be reachable on the
    // PATH handed to the child, not only on the PATH that started cargo.
    let codex = executables::resolve("codex");
    let output = Command::new(&node)
        .env_clear()
        .envs(&setup.env)
        .env("PATH", executables::child_path(&[], &[&node, &codex]))
        // Hand the probe the executable that was selected here. A PATH cannot
        // carry two independent selections: either directory on it may contain
        // both program names, so letting the probe search again can hand it a
        // different codex than this test chose.
        .env("ATTENTION_TEST_CODEX", &codex)
        .env("WEZTERM_ATTENTION_ROOT", &bridge)
        .env("ATTENTION_NATIVE_UI", "1")
        .env("ATTENTION_CODEX_SOURCE", codex_source)
        .env("ATTENTION_XTERM_MODULE", module)
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/javascript/lifecycle_contact_probe.mjs"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ui_question_and_queue"], true);
    assert_eq!(report["queue_drained"], true);
    assert_eq!(report["exact_prompt_deliveries"], true);
}

#[test]
fn every_active_lifecycle_row_reaches_the_production_writer() {
    let registry: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/contact-cases.json")).unwrap();
    for row in registry["rows"].as_array().unwrap() {
        for case in row["cases"].as_array().unwrap() {
            let setup = Setup::new();
            setup.claim();
            let provider = case["provider"].as_str().unwrap();
            let (start, patch) = if provider == "pi" {
                ("session_start", json!({"start_source":"startup"}))
            } else {
                ("SessionStart", json!({"source":"startup"}))
            };
            setup.apply(
                &event(provider, start, "coverage", patch),
                "00000000000000000200",
            );
            let name = case["event"].as_str().unwrap();
            let mut patch = case["patch"].clone();
            patch["prompt"] = json!("SYNTHETIC-PRIVATE-SENTINEL");
            patch["tool_input"] = json!({"private":"SYNTHETIC-PRIVATE-SENTINEL"});
            let output = run_hook(
                &setup,
                &["hooks", "event", provider, name, "--strict"],
                &payload(provider, name, "coverage", patch),
            );
            assert!(
                output.status.success(),
                "{} {provider} {name}: {}",
                row["id"],
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            let directory = setup.binding_dir(provider, "coverage");
            let raw = fs::read_to_string(directory.join("lifecycle.json")).unwrap();
            assert!(!raw.contains("SYNTHETIC-PRIVATE-SENTINEL"));
            let snapshot: LifecycleSnapshot = serde_json::from_str(&raw).unwrap();
            let observation = snapshot
                .pools
                .requests
                .observations
                .iter()
                .chain(&snapshot.pools.general.observations)
                .find(|item| item.body.kind() == case["kind"].as_str().unwrap())
                .unwrap();
            assert_eq!(observation.source_event, name);
            // A case with an agent id is a subagent's event.
            let from_child = case["patch"].get("agent_id").is_some();
            if from_child && name == "PreToolUse" {
                assert!(
                    !directory.join("activity.json").exists(),
                    "child work cannot become lead activity"
                );
            }
            if from_child && name == "PermissionRequest" {
                let activity: Value =
                    serde_json::from_slice(&fs::read(directory.join("activity.json")).unwrap())
                        .unwrap();
                assert_eq!(
                    activity["type"], "notify",
                    "a child waiting for permission waits for the user"
                );
            }
            if case["patch"]["notification_type"] == "elicitation_url_dialog" {
                assert!(
                    !directory.join("activity.json").exists(),
                    "new URL notice cannot create a badge"
                );
            }
        }
    }
}

#[test]
fn launch_rotation_after_resolution_cannot_add_old_execution_facts() {
    struct PausingClock {
        entered: std::sync::Barrier,
        released: std::sync::Barrier,
    }
    impl Clock for PausingClock {
        fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
            Ok("00000000000000000300".into())
        }
        fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
            self.entered.wait();
            self.released.wait();
            Ok("00000000012345678900".into())
        }
    }
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("codex", "SessionStart", "old", json!({"source":"startup"})),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "codex",
            "PreToolUse",
            "old",
            json!({"tool_name":"shell","tool_use_id":"first"}),
        ),
        "00000000000000000250",
    );
    let path = setup.binding_dir("codex", "old").join("lifecycle.json");
    let before = fs::read(&path).unwrap();
    let clock = PausingClock {
        entered: std::sync::Barrier::new(2),
        released: std::sync::Barrier::new(2),
    };
    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            let ports = RuntimePorts {
                clock: &clock,
                tty: &setup.tty,
                panes: &setup.panes,
                processes: &setup.processes,
            };
            apply_provider_event(
                &event(
                    "codex",
                    "PreToolUse",
                    "old",
                    json!({"tool_name":"shell","tool_use_id":"delayed"}),
                ),
                &setup.env,
                "00000000000000000300",
                &ports,
            )
            .unwrap()
        });
        clock.entered.wait();
        let mut newer = setup.env.clone();
        newer.insert(
            "WEZTERM_ATTENTION_LAUNCH_ID".into(),
            Uuid::new_v4().to_string(),
        );
        let newer_clock = FixedClock {
            monotonic: "00000000000000001000",
            unix: "00000000012345678900",
        };
        let ports = RuntimePorts {
            clock: &newer_clock,
            tty: &setup.tty,
            panes: &setup.panes,
            processes: &setup.processes,
        };
        let claimed = wezterm_attention::claim_launch(&newer, &ports);
        clock.released.wait();
        claimed.unwrap();
        let result = pending.join().unwrap();
        assert_eq!(result.disposition, "partial");
        assert_eq!(result.diagnostic.unwrap().code, "claim_stale");
    });
    assert_eq!(fs::read(path).unwrap(), before);
}

fn run_hook(setup: &Setup, arguments: &[&str], payload: &Value) -> std::process::Output {
    let mut child = rust_command(setup)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(
            serde_json::to_string(payload)
                .expect("payload JSON")
                .as_bytes(),
        )
        .expect("write hook payload");
    child.wait_with_output().expect("wait for hook")
}

fn payload(provider: &str, event: &str, session: &str, patch: Value) -> Value {
    let mut value = match provider {
        "pi" => json!({
            "session_id": session,
            "session_file": "/tmp/pi.jsonl",
            "cwd": "/tmp/project",
            "model": "model-a"
        }),
        _ => json!({
            "session_id": session,
            "transcript_path": "/tmp/session.jsonl",
            "cwd": "/tmp/project",
            "hook_event_name": event,
            "model": "model-a"
        }),
    };
    if let (Some(target), Some(source)) = (value.as_object_mut(), patch.as_object()) {
        for (key, item) in source {
            target.insert(key.clone(), item.clone());
        }
    }
    value
}

fn event(provider: &str, name: &str, session: &str, patch: Value) -> ProviderEvent {
    parse_provider_event(
        provider,
        name,
        &payload(provider, name, session, patch),
        &BTreeMap::new(),
    )
}

fn load_fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/providers")
        .join(format!("{name}.json"));
    serde_json::from_slice(&fs::read(path).expect("read provider fixture")).expect("fixture JSON")
}

#[test]
fn provider_fixtures_equal_the_closed_action_vocabulary() {
    let mut seen = BTreeSet::new();
    for provider in ["claude", "codex", "pi"] {
        let fixture = load_fixture(provider);
        for case in fixture["cases"].as_array().expect("fixture cases") {
            let mut payload = fixture["base"].clone();
            if provider != "pi" {
                payload["hook_event_name"] = case["event"].clone();
            }
            if let (Some(target), Some(patch)) = (
                payload.as_object_mut(),
                case.get("patch").and_then(Value::as_object),
            ) {
                for (key, value) in patch {
                    target.insert(key.clone(), value.clone());
                }
            }
            let environment: BTreeMap<String, String> = case
                .get("env")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or("").to_owned()))
                .collect();
            let parsed = parse_provider_event(
                provider,
                case["event"].as_str().expect("event name"),
                &payload,
                &environment,
            );
            assert_eq!(
                parsed.action.as_str(),
                case["expected"].as_str().expect("expected action"),
                "{provider}:{}",
                case["id"]
            );
            assert_eq!(
                parsed.activity_type.as_deref(),
                case.get("activity_type").and_then(Value::as_str),
                "{provider}:{} activity",
                case["id"]
            );
            if let Some(expected) = case.get("diagnostic").and_then(Value::as_str) {
                assert_eq!(
                    parsed.diagnostic.as_ref().map(|item| item.code.as_str()),
                    Some(expected)
                );
            }
            seen.insert(parsed.action.as_str());
        }
    }
    let declared: BTreeSet<_> = ProviderAction::ALL
        .iter()
        .map(|action| action.as_str())
        .collect();
    assert_eq!(seen, declared);
}

#[test]
fn hook_description_is_exhaustive_read_only_and_pins_public_fields() {
    use wezterm_attention::providers::{HookRegistration, describe_hooks};
    let contact: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/contact-cases.json")).unwrap();
    for provider in ["claude", "codex", "pi"] {
        let fixture = load_fixture(provider);
        let description = describe_hooks(provider).unwrap();
        for hook in &description.native_hooks {
            let callback = &hook.arguments[3];
            let mut cases = Vec::new();
            for case in fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|case| case["event"] == *callback)
            {
                let mut payload = fixture["base"].clone();
                if provider != "pi" {
                    payload["hook_event_name"] = json!(callback);
                }
                if let Some(patch) = case["patch"].as_object() {
                    for (k, v) in patch {
                        payload[k] = v.clone();
                    }
                }
                cases.push(parse_provider_event(
                    provider,
                    callback,
                    &payload,
                    &BTreeMap::new(),
                ));
            }
            for case in contact["rows"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row["cases"].as_array().unwrap())
            {
                if case["provider"] == provider && case["event"] == *callback {
                    cases.push(event(
                        provider,
                        callback,
                        "description-fixture",
                        case["patch"].clone(),
                    ));
                }
            }
            assert!(!cases.is_empty(), "missing {provider}/{callback} fixture");
            assert_eq!(
                cases
                    .iter()
                    .any(|case| case.action != ProviderAction::Ignored),
                hook.registration == HookRegistration::Register,
                "{provider}/{callback}"
            );
            assert!(hook.requires_launch_identity);
            assert_eq!(&hook.arguments[..3], &["hooks", "event", provider]);
        }
        let output = Command::new(env!("CARGO_BIN_EXE_attention"))
            .env_clear()
            .env("WEZTERM_ATTENTION_DIR", "invalid-relative-root")
            .args(["hooks", "describe", "--provider", provider, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["schema"], 1);
        assert_eq!(response["complete"], true);
        assert_eq!(
            response["result"],
            serde_json::to_value(&description).unwrap()
        );
        assert_eq!(
            response["result"].get("extension_entrypoint").is_some(),
            provider == "pi"
        );
        for row in response["result"]["native_hooks"].as_array().unwrap() {
            assert_eq!(
                row.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                vec![
                    "arguments",
                    "evidence",
                    "native_event",
                    "registration",
                    "requires_launch_identity"
                ]
            );
        }
    }
    assert!(describe_hooks("unknown").is_err());
    let pi = describe_hooks("pi").unwrap();
    assert!(
        pi.native_hooks
            .iter()
            .any(|row| row.native_event == "wezterm-attention:mark" && row.arguments[3] == "bus")
    );
    for (provider, callback) in [
        ("claude", "Interrupt"),
        ("codex", "StopFailure"),
        ("pi", "Stop"),
    ] {
        assert_eq!(
            event(provider, callback, "unsupported", json!({})).action,
            ProviderAction::Ignored
        );
    }
}

#[test]
fn nested_startup_conflicts_and_nested_resume_replaces() {
    let setup = Setup::new();
    setup.claim();
    let initial = event(
        "claude",
        "SessionStart",
        "session-a",
        json!({"source":"startup"}),
    );
    assert_eq!(
        setup.apply(&initial, "00000000000000000200").disposition,
        "applied"
    );
    let nested = event(
        "claude",
        "SessionStart",
        "session-b",
        json!({"source":"startup"}),
    );
    let conflict = setup.apply(&nested, "00000000000000000300");
    assert_eq!(conflict.disposition, "conflict");
    assert_eq!(
        conflict.diagnostic.as_ref().map(|item| item.code.as_str()),
        Some("binding_conflict")
    );
    let resume = event(
        "claude",
        "SessionStart",
        "session-b",
        json!({"source":"resume"}),
    );
    assert_eq!(
        setup.apply(&resume, "00000000000000000400").disposition,
        "replaced"
    );
    let (rows, _) = read_bindings(&state_root(&setup.env).expect("state root")).expect("bindings");
    assert!(
        rows.iter()
            .any(|row| row.provider_session_id == "session-b" && row.current)
    );
}

#[test]
fn prompt_return_clears_lead_only_and_newer_hook_reactivates() {
    let setup = Setup::new();
    setup.claim();
    let binding = event(
        "claude",
        "SessionStart",
        "session-a",
        json!({"source":"startup"}),
    );
    setup.apply(&binding, "00000000000000000200");
    let thinking = event(
        "claude",
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash"}),
    );
    setup.apply(&thinking, "00000000000000000300");
    let child = event(
        "claude",
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash","agent_id":"child-a","agent_type":"Explore"}),
    );
    setup.apply(&child, "00000000000000000350");
    let root = state_root(&setup.env).expect("state root");
    let marker = root.join("42");
    assert!(!marker.exists());
    assert!(!root.join("42.agents").exists());
    let binding_dir = setup.binding_dir("claude", "session-a");
    let activity: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("activity.json")).expect("activity"))
            .expect("activity JSON");
    assert_eq!(activity["type"], "thinking");
    assert!(
        binding_dir
            .join("agents")
            .join(format!(
                "{}.json",
                wezterm_attention::protocol::sha256_hex(b"child-a")
            ))
            .exists()
    );

    let cleared = prompt_return(&setup.env, "00000000000000000400").expect("prompt return");
    assert_eq!(cleared.disposition, "applied");
    assert!(binding_dir.join("activity-clear.json").exists());
    assert!(
        binding_dir
            .join("agents")
            .read_dir()
            .expect("agents directory")
            .next()
            .is_some()
    );
    assert!(!binding_dir.join("end.json").exists());
    assert!(!binding_dir.join("agents-clear.json").exists());
    assert!(!marker.exists());
    assert!(!root.join("42.agents").exists());

    assert_eq!(
        setup.apply(&thinking, "00000000000000000500").disposition,
        "applied"
    );
    assert!(!marker.exists());
    let activity: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("activity.json")).expect("activity"))
            .expect("activity JSON");
    assert!(activity.get("publication_id").is_none());
}

#[test]
fn stopped_child_fences_an_older_inflight_tool_event() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let stopped = event(
        "claude",
        "SubagentStop",
        "session-a",
        json!({"agent_id":"child-a","agent_type":"Explore","stop_hook_active":false}),
    );
    setup.apply(&stopped, "00000000000000000500");
    let active = event(
        "claude",
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash","agent_id":"child-a","agent_type":"Explore"}),
    );
    assert_eq!(
        setup.apply(&active, "00000000000000000400").disposition,
        "ignored"
    );
    let presence: Value = serde_json::from_slice(
        &fs::read(
            setup
                .binding_dir("claude", "session-a")
                .join("agents")
                .join(format!(
                    "{}.json",
                    wezterm_attention::protocol::sha256_hex(b"child-a")
                )),
        )
        .expect("presence"),
    )
    .expect("presence JSON");
    assert_eq!(presence["status"], "stopped");
}

#[test]
fn codex_parent_stop_clears_children_with_the_same_observation() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "thread-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "codex",
            "PreToolUse",
            "thread-a",
            json!({"tool_name":"shell","agent_id":"child-a","agent_type":"worker"}),
        ),
        "00000000000000000300",
    );
    setup.apply(
        &event(
            "codex",
            "PreToolUse",
            "thread-a",
            json!({"tool_name":"shell","agent_id":"child-b","agent_type":"worker"}),
        ),
        "00000000000000000350",
    );
    let stop = event(
        "codex",
        "Stop",
        "thread-a",
        json!({"stop_hook_active":false}),
    );
    assert_eq!(
        setup.apply(&stop, "00000000000000000400").disposition,
        "applied"
    );
    let binding_dir = setup.binding_dir("codex", "thread-a");
    let clear: Value = serde_json::from_slice(
        &fs::read(binding_dir.join("agents-clear.json")).expect("clear record"),
    )
    .expect("clear JSON");
    assert_eq!(clear["observed_mono_ns"], "00000000000000000400");
    assert!(
        !state_root(&setup.env)
            .expect("state root")
            .join("42.agents")
            .exists()
    );
}

#[test]
fn duplicate_codex_stop_keeps_a_child_newer_than_the_surviving_activity() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "codex",
            "SessionStart",
            "thread-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let stop = event(
        "codex",
        "Stop",
        "thread-a",
        json!({"stop_hook_active":false}),
    );
    setup.apply(&stop, "00000000000000000300");
    setup.apply(
        &event(
            "codex",
            "PreToolUse",
            "thread-a",
            json!({"tool_name":"shell","agent_id":"child-a","agent_type":"worker"}),
        ),
        "00000000000000000400",
    );
    setup.apply(&stop, "00000000000000000500");
    let clear: Value = serde_json::from_slice(
        &fs::read(
            setup
                .binding_dir("codex", "thread-a")
                .join("agents-clear.json"),
        )
        .expect("agents clear"),
    )
    .expect("agents clear JSON");
    assert_eq!(clear["observed_mono_ns"], "00000000000000000300");
    assert!(
        setup
            .binding_dir("codex", "thread-a")
            .join("agents")
            .join(format!(
                "{}.json",
                wezterm_attention::protocol::sha256_hex(b"child-a")
            ))
            .exists()
    );
    assert!(
        !state_root(&setup.env)
            .expect("state root")
            .join("42.agents")
            .exists()
    );
}

#[test]
fn malformed_agent_type_is_ignored_and_long_paths_use_the_path_limit() {
    let root = parse_provider_event(
        "claude",
        "PreToolUse",
        &payload(
            "claude",
            "PreToolUse",
            "session-a",
            json!({"tool_name":"Bash","agent_type":7}),
        ),
        &BTreeMap::new(),
    );
    assert_eq!(root.action, ProviderAction::Activity);
    let child = parse_provider_event(
        "claude",
        "PreToolUse",
        &payload(
            "claude",
            "PreToolUse",
            "session-a",
            json!({"tool_name":"Bash","agent_id":"child-a","agent_type":""}),
        ),
        &BTreeMap::new(),
    );
    assert_eq!(child.action, ProviderAction::ChildActive);
    assert_eq!(child.agent_type, None);
    let long_cwd = format!("/tmp/{}", "d".repeat(300));
    let start = parse_provider_event(
        "claude",
        "SessionStart",
        &payload(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup","cwd":long_cwd}),
        ),
        &BTreeMap::new(),
    );
    assert_eq!(start.action, ProviderAction::Binding);
    assert_eq!(start.cwd.as_ref().map(String::len), Some(305));
}

#[test]
fn stale_pi_clear_preserves_newer_activity_and_review() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    let clear = event("pi", "bus", "pi-a", json!({"state":"clear"}));
    setup.apply(&clear, "00000000000000000300");
    setup.apply(
        &event("pi", "agent_start", "pi-a", json!({})),
        "00000000000000000400",
    );
    setup.apply(
        &event("pi", "bus", "pi-a", json!({"state":"review"})),
        "00000000000000000450",
    );
    let result = setup.apply(&clear, "00000000000000000250");
    assert_eq!(result.disposition, "ignored");
    let root = state_root(&setup.env).expect("state root");
    assert!(!root.join("42").exists());
    let (address, _) = pane_address(&setup.env).expect("address");
    let review = pane_path(&root, &address).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"pi-bus")
    ));
    assert!(review.exists());
}

#[test]
fn covered_activity_never_creates_the_flat_projection() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event("pi", "agent_start", "pi-a", json!({})),
        "00000000000000000300",
    );
    setup.apply(
        &event("pi", "bus", "pi-a", json!({"state":"clear"})),
        "00000000000000000500",
    );
    let result = setup.apply(
        &event("pi", "agent_settled", "pi-a", json!({})),
        "00000000000000000400",
    );
    assert_eq!(result.disposition, "ignored");
    assert!(
        !state_root(&setup.env)
            .expect("state root")
            .join("42")
            .exists()
    );
}

#[test]
fn delayed_pi_clear_cannot_remove_a_new_launch_review() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    let pane = pane_path(&root, &address);
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    let launch = launch_path(&root, &address, &launch_id);
    let old_payload = payload("pi", "bus", "pi-a", json!({"state":"clear"}));

    let (child, review_before) = with_lock(&launch.join(".lock"), Duration::from_secs(2), || {
        let mut child = rust_command(&setup)
            .args(["hooks", "event", "pi", "bus", "--strict", "--debug"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn delayed clear");
        child
            .stdin
            .take()
            .expect("clear stdin")
            .write_all(
                serde_json::to_string(&old_payload)
                    .expect("payload JSON")
                    .as_bytes(),
            )
            .expect("write delayed clear");
        let lock = fs::canonicalize(launch.join(".lock")).expect("canonical lock path");
        let mut opened = false;
        for _ in 0..60 {
            if process_has_open(child.id(), &lock) {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(opened, "delayed clear opened the held launch lock");

        let next_launch = "00000000-0000-4000-8000-000000000499";
        let claim_path = pane.join("claim.json");
        let mut claim: Value =
            serde_json::from_slice(&fs::read(&claim_path).expect("read old claim"))
                .expect("claim JSON");
        claim["launch_id"] = json!(next_launch);
        claim["observed_mono_ns"] = json!("00000000000000000300");
        atomic_replace(&claim_path, &claim).expect("replace claim");
        let mut next_env = setup.env.clone();
        next_env.insert(
            "WEZTERM_ATTENTION_LAUNCH_ID".to_owned(),
            next_launch.to_owned(),
        );
        apply_provider_event(
            &event(
                "pi",
                "session_start",
                "pi-b",
                json!({"start_source":"startup"}),
            ),
            &next_env,
            "00000000000000000400",
            &setup.ports(),
        )
        .expect("start next launch");
        apply_provider_event(
            &event("pi", "bus", "pi-b", json!({"state":"review"})),
            &next_env,
            "00000000000000000500",
            &setup.ports(),
        )
        .expect("write next review");
        let review_path = pane.join("reviews").join(format!(
            "{}.json",
            wezterm_attention::protocol::sha256_hex(b"pi-bus")
        ));
        Ok((child, fs::read(review_path).expect("read next review")))
    })
    .expect("release delayed clear");
    let output = child.wait_with_output().expect("wait for delayed clear");
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stderr).expect("debug envelope");
    assert_eq!(envelope["result"]["disposition"], "ignored");
    assert_eq!(envelope["result"]["diagnostic"]["code"], "claim_stale");
    let review_path = pane.join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"pi-bus")
    ));
    assert_eq!(
        fs::read(review_path).expect("review remains"),
        review_before
    );
}

#[test]
fn future_review_is_never_deleted_or_replaced() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    let pane = pane_path(&root, &address);
    let owner_key = wezterm_attention::protocol::sha256_hex(b"pi-bus");
    let review_path = pane.join("reviews").join(format!("{owner_key}.json"));
    let future = serde_json::to_vec(&json!({
        "kind":"review","schema":999,"address":address,
        "owner_id":"pi-bus","owner_key":owner_key,
        "event_id":"00000000-0000-4000-8000-000000000498"
    }))
    .expect("future review JSON");
    fs::create_dir_all(review_path.parent().expect("review parent")).expect("review directory");
    fs::write(&review_path, &future).expect("future review");

    let replace_error = apply_provider_event(
        &event("pi", "bus", "pi-a", json!({"state":"review"})),
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    )
    .expect_err("future review cannot be replaced");
    assert_eq!(replace_error.diagnostic.code, "future_schema");
    assert_eq!(
        fs::read(&review_path).expect("review after replace"),
        future
    );

    let delete_error = apply_mark_clear(&setup.env, "pi-bus", "00000000000000000400")
        .expect_err("future review cannot be deleted");
    assert_eq!(delete_error.diagnostic.code, "future_schema");
    assert_eq!(fs::read(review_path).expect("review after delete"), future);
}

#[test]
fn fresh_manual_activity_is_fenced_by_an_existing_clear() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event("pi", "bus", "pi-a", json!({"state":"clear"})),
        "00000000000000000400",
    );
    let result = apply_mark_activity(
        &setup.env,
        "thinking",
        "manual",
        None,
        None,
        None,
        "00000000000000000300",
        "00000000012345678900",
    )
    .expect("manual mark decision");
    assert_eq!(result.disposition, "ignored");
    assert!(
        !setup
            .binding_dir("pi", "pi-a")
            .join("activity.json")
            .exists()
    );
    assert!(
        !state_root(&setup.env)
            .expect("state root")
            .join("42")
            .exists()
    );
}

/// Whether process `pid` holds `path` open. Linux lists a process's
/// descriptors under /proc; macOS has no /proc, so there lsof answers.
fn process_has_open(pid: u32, path: &std::path::Path) -> bool {
    let descriptors = PathBuf::from(format!("/proc/{pid}/fd"));
    if descriptors.is_dir() {
        return fs::read_dir(descriptors).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| fs::read_link(entry.path()).is_ok_and(|target| target == path))
        });
    }
    let output = Command::new(executables::resolve("lsof"))
        .args(["-a", "-p", &pid.to_string(), "-Fn"])
        .output()
        .expect("inspect open files");
    let name = format!("n{}", path.display());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line == name)
}

// macOS answers ttyname on an open /dev/tty with "/dev/tty" itself, a clone
// device owned by root, which must not become a pane identity. Linux answers
// with the real /dev/pts path, so the case this test guards does not arise
// there, and util-linux script takes different arguments.
#[cfg(target_os = "macos")]
#[test]
fn real_macos_controlling_tty_path_is_rejected_in_a_pty_child() {
    const CHILD: &str = "WEZTERM_ATTENTION_REAL_TTY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let writer = wezterm_attention::wezterm::SystemTtyWriter;
        let path = writer
            .controlling_path()
            .expect("/dev/tty opens in pty child");
        let error = writer
            .fingerprint(&path)
            .expect_err("macOS /dev/tty clone must not establish pane identity");
        assert_eq!(error.diagnostic.code, "unsafe_tty");
        return;
    }
    let executable = std::env::current_exe().expect("current test executable");
    // The child needs the pty `script` allocates, never the harness's own stdin:
    // inheriting it makes this test depend on whatever else is reading the
    // terminal. `script` still allocates the pty with stdin closed.
    let output = Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            executable.to_str().expect("test path"),
            "--exact",
            "real_macos_controlling_tty_path_is_rejected_in_a_pty_child",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .output()
        .expect("run pty child");
    assert!(
        output.status.success(),
        "pty child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn every_review_writer_obeys_claim_and_owner_locks() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    let pane = pane_path(&root, &address);
    let owner_key = wezterm_attention::protocol::sha256_hex(b"pi-bus");
    let owner_lock = pane.join("reviews").join(format!(".{owner_key}.lock"));
    let review = event("pi", "bus", "pi-a", json!({"state":"review"}));
    let error = with_lock(&owner_lock, Duration::from_secs(1), || {
        apply_provider_event(&review, &setup.env, "00000000000000000300", &setup.ports())
    })
    .expect_err("Pi review must honor the owner lock");
    assert_eq!(error.diagnostic.code, "probe_unavailable");
    let review_path = pane.join("reviews").join(format!("{owner_key}.json"));
    assert!(!review_path.exists());

    let claim_lock = pane.join(".claim.lock");
    let error = with_lock(&claim_lock, Duration::from_secs(1), || {
        apply_mark_review(&setup.env, "pi-bus")
    })
    .expect_err("manual review must honor the claim lock");
    assert_eq!(error.diagnostic.code, "probe_unavailable");
    assert!(!review_path.exists());
}

#[test]
fn same_session_resume_reopens_an_older_end() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "claude",
            "SessionEnd",
            "session-a",
            json!({"reason":"other"}),
        ),
        "00000000000000000300",
    );
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"resume"}),
        ),
        "00000000000000000400",
    );
    let (rows, _) = read_bindings(&state_root(&setup.env).expect("state root")).expect("bindings");
    assert_eq!(rows[0].binding_phase, "active");
    let second_end = event(
        "claude",
        "SessionEnd",
        "session-a",
        json!({"reason":"other"}),
    );
    assert_eq!(
        setup.apply(&second_end, "00000000000000000500").disposition,
        "applied"
    );
    let (rows, _) = read_bindings(&state_root(&setup.env).expect("state root")).expect("bindings");
    assert_eq!(rows[0].binding_phase, "ended");
}

#[test]
fn non_start_without_a_claim_is_stale_for_every_provider() {
    let mut setup = Setup::new();
    setup.env = setup.agent_env();
    for event in [
        event(
            "claude",
            "PreToolUse",
            "session-a",
            json!({"tool_name":"Bash"}),
        ),
        event(
            "codex",
            "PreToolUse",
            "thread-a",
            json!({"tool_name":"shell"}),
        ),
        event("pi", "agent_start", "pi-a", json!({})),
    ] {
        let result = setup.apply(&event, "00000000000000000200");
        assert_eq!(result.disposition, "ignored");
        assert_eq!(
            result.diagnostic.as_ref().map(|item| item.code.as_str()),
            Some("claim_stale")
        );
    }
}

#[test]
fn pi_review_and_clear_share_the_current_binding_without_ending_it() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            "pi-a",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000200",
    );
    let review = event("pi", "bus", "pi-a", json!({"state":"review"}));
    assert_eq!(
        setup.apply(&review, "00000000000000000300").disposition,
        "applied"
    );
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    let review_path = pane_path(&root, &address).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"pi-bus")
    ));
    assert!(review_path.exists());
    let clear = event("pi", "bus", "pi-a", json!({"state":"clear"}));
    assert_eq!(
        setup.apply(&clear, "00000000000000000400").disposition,
        "applied"
    );
    assert!(!review_path.exists());
    let binding_dir = setup.binding_dir("pi", "pi-a");
    assert!(binding_dir.join("activity-clear.json").exists());
    assert!(!binding_dir.join("end.json").exists());
}

#[test]
fn manual_mark_targets_the_launch_and_a_duplicate_is_skipped() {
    let setup = Setup::new();
    setup.claim();
    let first = apply_mark_activity(
        &setup.env,
        "notify",
        "manual",
        Some(7),
        Some("ready"),
        Some(60_000),
        "00000000000000000200",
        "00000000012345678900",
    )
    .expect("manual mark");
    assert_eq!(first.disposition, "applied");
    let root = state_root(&setup.env).expect("state root");
    let marker = root.join("42");
    assert!(!marker.exists());
    let unchanged = apply_mark_activity(
        &setup.env,
        "notify",
        "manual",
        Some(7),
        Some("ready"),
        Some(60_000),
        "00000000000000000250",
        "00000000012345678900",
    )
    .expect("unchanged duplicate manual mark");
    assert_eq!(unchanged.disposition, "skipped");
    let duplicate = apply_mark_activity(
        &setup.env,
        "notify",
        "manual",
        Some(7),
        Some("ready"),
        Some(60_000),
        "00000000000000000300",
        "00000000012345678900",
    )
    .expect("duplicate manual mark");
    assert_eq!(duplicate.disposition, "skipped");
    assert!(!marker.exists());
    assert_eq!(
        apply_mark_activity(
            &setup.env,
            "stop",
            "manual",
            None,
            None,
            None,
            "00000000000000000400",
            "00000000012345678900",
        )
        .expect("changed manual mark")
        .disposition,
        "applied"
    );

    assert_eq!(
        apply_mark_review(&setup.env, "manual")
            .expect("set review")
            .disposition,
        "applied"
    );
    let (address, _) = pane_address(&setup.env).expect("address");
    let review = pane_path(&root, &address).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"manual")
    ));
    assert!(review.exists());
    apply_mark_clear(&setup.env, "manual", "00000000000000000500").expect("clear review");
    assert!(!review.exists());
}

#[test]
fn hooks_event_debug_uses_stderr_and_lifecycle_errors_are_non_strict() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let payload = payload(
        "claude",
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash"}),
    );
    let debug = run_hook(
        &setup,
        &["hooks", "event", "claude", "PreToolUse", "--debug"],
        &payload,
    );
    assert!(debug.status.success());
    assert!(debug.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&debug.stderr).expect("debug envelope on stderr");
    assert_eq!(envelope["status"], "ok");

    let pointer = setup
        .binding_dir("claude", "session-a")
        .parent()
        .and_then(std::path::Path::parent)
        .expect("launch path")
        .join("current-binding.json");
    let mut foreign: Value =
        serde_json::from_slice(&fs::read(&pointer).expect("pointer")).expect("pointer JSON");
    foreign["address"]["pane_id"] = json!("99");
    atomic_replace(&pointer, &foreign).expect("write foreign pointer");
    let non_strict = run_hook(
        &setup,
        &["hooks", "event", "claude", "PreToolUse"],
        &payload,
    );
    assert!(non_strict.status.success());
    assert!(String::from_utf8_lossy(&non_strict.stderr).contains("record_invalid"));
    let strict = run_hook(
        &setup,
        &["hooks", "event", "claude", "PreToolUse", "--strict"],
        &payload,
    );
    assert_eq!(strict.status.code(), Some(1));
}

#[test]
fn hook_observation_is_captured_before_waiting_for_stdin() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let payload = payload(
        "claude",
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash"}),
    );
    let before = wezterm_attention::identity::monotonic_ns20()
        .expect("monotonic before spawn")
        .parse::<u128>()
        .expect("numeric monotonic");
    let mut child = rust_command(&setup)
        .args(["hooks", "event", "claude", "PreToolUse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn delayed hook");
    std::thread::sleep(Duration::from_millis(800));
    let delivered = wezterm_attention::identity::monotonic_ns20()
        .expect("monotonic before delivery")
        .parse::<u128>()
        .expect("numeric monotonic");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(
            serde_json::to_string(&payload)
                .expect("payload JSON")
                .as_bytes(),
        )
        .expect("deliver hook payload");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(output.status.success());
    let activity: Value = serde_json::from_slice(
        &fs::read(
            setup
                .binding_dir("claude", "session-a")
                .join("activity.json"),
        )
        .expect("activity"),
    )
    .expect("activity JSON");
    let observed = activity["observed_mono_ns"]
        .as_str()
        .expect("observation")
        .parse::<u128>()
        .expect("numeric observation");
    assert!(observed >= before);
    assert!(observed + 400_000_000 < delivered);
}

#[test]
fn prompt_return_failure_does_not_block_tty_publication() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "session-a",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let pointer = setup
        .binding_dir("claude", "session-a")
        .parent()
        .and_then(std::path::Path::parent)
        .expect("launch path")
        .join("current-binding.json");
    let mut foreign: Value =
        serde_json::from_slice(&fs::read(&pointer).expect("pointer")).expect("pointer JSON");
    foreign["address"]["pane_id"] = json!("99");
    atomic_replace(&pointer, &foreign).expect("write foreign pointer");
    let mut master = 0;
    let mut slave = 0;
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
    let stdin = unsafe { fs::File::from_raw_fd(libc::dup(slave)) };
    let output = rust_command(&setup)
        .args(["hooks", "publish", "--json"])
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("publish with failed prompt return");
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("publish envelope");
    assert_eq!(envelope["result"]["published"], 1);
    assert!(
        envelope["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["code"] == "record_invalid"))
    );
    let mut buffer = [0_u8; 8192];
    let count = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
    assert!(count > 0);
    assert!(
        buffer[..count as usize]
            .windows(b"WEZTERM_PANE".len())
            .any(|window| window == b"WEZTERM_PANE")
    );
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

#[test]
fn bindings_rejects_invalid_realm_and_provider_filters() {
    let setup = Setup::new();
    for arguments in [
        ["bindings", "--realm", "NOTHEX", "--json"],
        ["bindings", "--provider", "bogus", "--json"],
    ] {
        let output = rust_command(&setup)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run bindings filter probe");
        assert_eq!(output.status.code(), Some(2));
        let envelope: Value = serde_json::from_slice(&output.stdout).expect("usage envelope");
        assert_eq!(envelope["status"], "usage_error");
        assert_eq!(envelope["diagnostics"][0]["code"], "bad_usage");
    }
}

#[test]
fn stop_after_an_acknowledged_stop_publishes_a_fresh_event_id() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "relight",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    let first = setup.apply(
        &event("claude", "Stop", "relight", json!({})),
        "00000000000000000300",
    );
    assert_eq!(first.disposition, "applied");
    let acknowledged = first
        .event_id
        .clone()
        .expect("first stop publishes an event id");
    let binding_dir = setup.binding_dir("claude", "relight");
    let activity: Value = serde_json::from_slice(
        &fs::read(binding_dir.join("activity.json")).expect("read activity"),
    )
    .expect("activity JSON");
    // The plugin acknowledges a focused pane by naming the publication it showed;
    // it never writes activity-clear.json.
    atomic_replace(
        &binding_dir.join("ack.json"),
        &json!({
            "kind": "acknowledgement",
            "schema": activity["schema"],
            "address": activity["address"],
            "launch_id": activity["launch_id"],
            "target": activity["target"],
            "activity_event_id": acknowledged,
            "event_id": "00000000-0000-4000-8000-000000000501",
        }),
    )
    .expect("write acknowledgement");
    let second = setup.apply(
        &event("claude", "Stop", "relight", json!({})),
        "00000000000000000400",
    );
    assert_eq!(second.disposition, "applied");
    assert_ne!(second.event_id.as_deref(), Some(acknowledged.as_str()));
    assert!(!binding_dir.join("activity-clear.json").exists());
}

#[test]
fn manual_mark_after_an_acknowledged_mark_publishes_a_fresh_event_id() {
    let setup = Setup::new();
    setup.claim();
    let first = apply_mark_activity(
        &setup.env,
        "notify",
        "manual",
        None,
        None,
        None,
        "00000000000000000200",
        "00000000012345678900",
    )
    .expect("manual mark");
    assert_eq!(first.disposition, "applied");
    let acknowledged = first.event_id.clone().expect("mark publishes an event id");
    let root = state_root(&setup.env).expect("state root");
    let (address, _) = pane_address(&setup.env).expect("address");
    let launch = launch_path(&root, &address, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]);
    let activity: Value =
        serde_json::from_slice(&fs::read(launch.join("activity.json")).expect("read activity"))
            .expect("activity JSON");
    atomic_replace(
        &launch.join("ack.json"),
        &json!({
            "kind": "acknowledgement",
            "schema": activity["schema"],
            "address": activity["address"],
            "launch_id": activity["launch_id"],
            "target": activity["target"],
            "activity_event_id": acknowledged,
            "event_id": "00000000-0000-4000-8000-000000000502",
        }),
    )
    .expect("write acknowledgement");
    let second = apply_mark_activity(
        &setup.env,
        "notify",
        "manual",
        None,
        None,
        None,
        "00000000000000000300",
        "00000000012345678900",
    )
    .expect("repeat manual mark");
    assert_eq!(second.disposition, "applied");
    assert_ne!(second.event_id.as_deref(), Some(acknowledged.as_str()));
}

// Characterization, not an endorsement. A duplicate activity leaves
// `observed_mono_ns` at the older value, so an older event that commits later
// still wins against a newer observation it should have lost to. The fence
// cannot simply be advanced here: `observed_mono_ns` is also the subagent-clear
// watermark a parent stop writes, so advancing it would clear children that
// started after the stop. See "One timestamp field carries three roles" in
// docs/accepted-limitations.md.
#[test]
fn a_deduplicated_activity_does_not_advance_the_ordering_fence() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "fence",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup.apply(
        &event("claude", "PreToolUse", "fence", json!({"tool_name":"Bash"})),
        "00000000000000000300",
    );
    let repeat = setup.apply(
        &event("claude", "UserPromptSubmit", "fence", json!({})),
        "00000000000000000500",
    );
    assert_eq!(repeat.disposition, "skipped");
    let binding_dir = setup.binding_dir("claude", "fence");
    let fenced: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("activity.json")).expect("activity"))
            .expect("activity JSON");
    assert_eq!(fenced["observed_mono_ns"], json!("00000000000000000300"));
    // Consequence: the older Stop still publishes over the newer prompt.
    let stale = setup.apply(
        &event("claude", "Stop", "fence", json!({})),
        "00000000000000000400",
    );
    assert_eq!(stale.disposition, "applied");
    let final_activity: Value =
        serde_json::from_slice(&fs::read(binding_dir.join("activity.json")).expect("activity"))
            .expect("activity JSON");
    assert_eq!(final_activity["type"], "stop");
}
