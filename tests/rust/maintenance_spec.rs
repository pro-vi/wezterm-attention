use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::{Value, json};
use uuid::Uuid;
use wezterm_attention::identity::{PaneAddress, pane_address};
use wezterm_attention::lifecycle::{apply_provider_event, binding_id};
use wezterm_attention::maintenance::{
    ABSENCE_INTERVAL_NS, RETENTION_AGE_NS, binding_cap_paths_by_realm, doctor, limit_sweep_preview,
    sweep,
};
use wezterm_attention::providers::parse_provider_event;
use wezterm_attention::query::read_bindings_with_ports;
use wezterm_attention::query::{PaneScope, read_pane_facts_with_ports};
use wezterm_attention::records::{
    FileRecords, atomic_replace, launch_path, pane_path, state_root, with_lock,
};
use wezterm_attention::wezterm::{
    Clock, PaneLister, PaneRow, Presence, ProcessProbe, RuntimePorts, TtyWriter,
};

#[path = "maintenance_spec/bindings_socket.rs"]
mod bindings_socket;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("wm-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct MutableClock {
    monotonic: AtomicU64,
    unix: AtomicU64,
}

impl MutableClock {
    fn set_monotonic(&self, value: u64) {
        self.monotonic.store(value, Ordering::SeqCst);
    }

    fn set_unix(&self, value: u64) {
        self.unix.store(value, Ordering::SeqCst);
    }
}

impl Clock for MutableClock {
    fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(format!("{:020}", self.monotonic.load(Ordering::SeqCst)))
    }

    fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(format!("{:020}", self.unix.load(Ordering::SeqCst)))
    }
}

struct FakeTty {
    path: String,
}

impl TtyWriter for FakeTty {
    fn current_path(&self) -> wezterm_attention::protocol::Result<String> {
        Ok(self.path.clone())
    }

    fn fingerprint(&self, _path: &str) -> wezterm_attention::protocol::Result<String> {
        Ok("f".repeat(64))
    }

    fn write(
        &self,
        _path: &str,
        _data: &[u8],
        _expected_fingerprint: &str,
    ) -> wezterm_attention::protocol::Result<()> {
        Ok(())
    }
}

struct FakePanes(Mutex<Vec<PaneRow>>);

impl FakePanes {
    fn set(&self, rows: Vec<PaneRow>) {
        *self.0.lock().expect("pane rows lock") = rows;
    }
}

impl PaneLister for FakePanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        Ok(self.0.lock().expect("pane rows lock").clone())
    }
}

struct FakeProcesses {
    state: AtomicU8,
    queries: Mutex<Vec<(String, String)>>,
}

impl FakeProcesses {
    fn set(&self, presence: Presence) {
        self.state.store(
            match presence {
                Presence::Present => 1,
                Presence::Absent => 2,
                Presence::Unavailable => 3,
            },
            Ordering::SeqCst,
        );
    }
}

impl ProcessProbe for FakeProcesses {
    fn available(&self) -> bool {
        self.state.load(Ordering::SeqCst) != 3
    }

    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        self.queries
            .lock()
            .expect("process queries lock")
            .push((socket_path.to_owned(), pane_id.to_owned()));
        match self.state.load(Ordering::SeqCst) {
            1 => Presence::Present,
            2 => Presence::Absent,
            _ => Presence::Unavailable,
        }
    }
}

struct Setup {
    _scratch: Scratch,
    _socket: UnixListener,
    env: BTreeMap<String, String>,
    clock: MutableClock,
    tty: FakeTty,
    panes: FakePanes,
    processes: FakeProcesses,
}

impl Setup {
    fn new() -> Self {
        let scratch = Scratch::new();
        let socket_path = scratch.0.join("mux.sock");
        let socket = UnixListener::bind(&socket_path).expect("bind socket");
        let tty = FakeTty {
            path: "/dev/ttys888".to_owned(),
        };
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
                "00000000-0000-4000-8000-000000000701".to_owned(),
            ),
        ]);
        Self {
            _scratch: scratch,
            _socket: socket,
            env,
            clock: MutableClock {
                monotonic: AtomicU64::new(100),
                unix: AtomicU64::new(1_000_000_000),
            },
            tty,
            panes: FakePanes(Mutex::new(vec![PaneRow {
                pane_id: "42".to_owned(),
                tty_name: Some("/dev/ttys888".to_owned()),
            }])),
            processes: FakeProcesses {
                state: AtomicU8::new(1),
                queries: Mutex::new(Vec::new()),
            },
        }
    }

    fn ports(&self) -> RuntimePorts<'_> {
        RuntimePorts {
            clock: &self.clock,
            tty: &self.tty,
            panes: &self.panes,
        }
    }

    fn claim_and_bind(&self) {
        wezterm_attention::claim_launch(&self.env, &self.ports()).expect("claim");
        let payload = json!({
            "session_id":"session-a",
            "transcript_path":"/tmp/session-a.jsonl",
            "cwd":"/tmp/project",
            "hook_event_name":"SessionStart",
            "source":"startup"
        });
        let event = parse_provider_event("claude", "SessionStart", &payload, &BTreeMap::new());
        apply_provider_event(&event, &self.env, "00000000000000000200", &self.ports())
            .expect("bind");
    }

    fn provider_event(
        &self,
        name: &str,
        session: &str,
        patch: Value,
        observation: &str,
    ) -> wezterm_attention::lifecycle::LifecycleResult {
        let mut payload = json!({
            "session_id":session,
            "transcript_path":"/tmp/session.jsonl",
            "cwd":"/tmp/project",
            "hook_event_name":name
        });
        for (key, value) in patch.as_object().expect("event patch") {
            payload[key] = value.clone();
        }
        let event = parse_provider_event("claude", name, &payload, &BTreeMap::new());
        apply_provider_event(&event, &self.env, observation, &self.ports()).expect("provider event")
    }

    fn root(&self) -> PathBuf {
        state_root(&self.env).expect("state root")
    }

    fn binding_dir(&self) -> PathBuf {
        let root = self.root();
        let (address, _) = pane_address(&self.env).expect("address");
        let launch_id = &self.env["WEZTERM_ATTENTION_LAUNCH_ID"];
        launch_path(&root, &address, launch_id)
            .join("bindings")
            .join(binding_id("claude", "session-a", launch_id))
    }

    fn seed_presence(&self, agent: &str, observation: u64, written: u64, status: &str) -> PathBuf {
        let (address, _) = pane_address(&self.env).expect("address");
        let launch_id = &self.env["WEZTERM_ATTENTION_LAUNCH_ID"];
        let binding_id = binding_id("claude", "session-a", launch_id);
        let agent_key = wezterm_attention::protocol::sha256_hex(agent.as_bytes());
        let path = self
            .binding_dir()
            .join("agents")
            .join(format!("{agent_key}.json"));
        atomic_replace(
            &path,
            &json!({
                "kind":"subagent_presence","schema":3,"address":address,
                "launch_id":launch_id,"binding_id":binding_id,"provider":"claude",
                "agent_id":agent,"agent_key":agent_key,"source":"worker","status":status,
                "event_id":Uuid::new_v4().to_string(),
                "observed_mono_ns":format!("{observation:020}"),
                "written_at_unix_ns":format!("{written:020}"),"ttl_ms":600000
            }),
        )
        .expect("write presence");
        path
    }

    fn run_sweep(
        &self,
        apply: bool,
        operation: Option<&str>,
    ) -> (
        wezterm_attention::maintenance::SweepResult,
        Vec<wezterm_attention::protocol::Diagnostic>,
    ) {
        sweep(
            &self.root(),
            None,
            apply,
            operation,
            &self.clock,
            &self.panes,
            Some(&self.processes),
        )
        .expect("sweep")
    }
}

#[test]
fn doctor_reports_embedded_manifest_digest_and_confirmed_binding() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (result, diagnostics) =
        doctor(&setup.root(), Some(&setup.panes), Some(&setup.processes)).expect("doctor");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(result["manifest"]["matches"], true);
    assert_eq!(
        result["manifest"]["embedded_sha256"],
        result["manifest"]["on_disk_sha256"]
    );
    assert_eq!(result["bindings_scanned"], 1);
    let (rows, _) =
        read_bindings_with_ports(&setup.root(), Some(&setup.panes), Some(&setup.processes))
            .expect("bindings");
    assert_eq!(rows[0].pane_presence, "present");
    assert_eq!(rows[0].reader_confidence, "confirmed");
}

#[test]
fn doctor_reports_a_future_claim_without_any_binding() {
    let setup = Setup::new();
    wezterm_attention::claim_launch(&setup.env, &setup.ports()).expect("claim");
    let (address, _) = pane_address(&setup.env).expect("address");
    let claim_path = pane_path(&setup.root(), &address).join("claim.json");
    let mut claim: Value =
        serde_json::from_slice(&fs::read(&claim_path).expect("claim")).expect("claim JSON");
    claim["schema"] = json!(999);
    fs::write(
        &claim_path,
        serde_json::to_vec(&claim).expect("future claim JSON"),
    )
    .expect("write future claim");
    let (result, diagnostics) =
        doctor(&setup.root(), Some(&setup.panes), Some(&setup.processes)).expect("doctor");
    assert!(diagnostics.iter().any(|item| item.code == "future_schema"));
    assert!(
        result["probes"]
            .as_array()
            .expect("probes")
            .iter()
            .any(|probe| { probe["name"] == "versions" && probe["status"] == "finding" })
    );
}

#[test]
fn sweep_preview_writes_nothing_and_two_absences_end_one_binding() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.clock.set_monotonic(300);
    let pane = pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0);
    let probe = pane.join("absence-probe.json");
    let preview = setup.run_sweep(false, None).0;
    assert!(
        preview
            .details
            .iter()
            .any(|detail| detail["action"] == "first_absence")
    );
    assert!(!probe.exists());
    let operation_one = "00000000-0000-4000-8000-000000000711";
    setup.run_sweep(true, Some(operation_one));
    assert!(probe.exists());
    setup
        .clock
        .set_monotonic(300 + ABSENCE_INTERVAL_NS as u64 - 1);
    let too_soon = setup
        .run_sweep(true, Some("00000000-0000-4000-8000-000000000712"))
        .0;
    assert!(
        too_soon
            .details
            .iter()
            .any(|detail| detail["action"] == "too_soon")
    );
    assert!(!setup.binding_dir().join("end.json").exists());
    setup.clock.set_monotonic(300 + ABSENCE_INTERVAL_NS as u64);
    let ended = setup
        .run_sweep(true, Some("00000000-0000-4000-8000-000000000713"))
        .0;
    assert!(ended.details.iter().any(|detail| detail["action"] == "end"));
    assert!(setup.binding_dir().join("end.json").exists());
}

#[test]
fn unavailable_process_probe_never_counts_as_absence() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Unavailable);
    let (_, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000714"));
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "probe_unavailable")
    );
    let pane = pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0);
    assert!(!pane.join("absence-probe.json").exists());
}

#[test]
fn absence_process_negative_is_scoped_to_socket_and_pane() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.run_sweep(false, None);
    let expected_socket = fs::canonicalize(&setup.env["WEZTERM_UNIX_SOCKET"])
        .expect("socket path")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        *setup
            .processes
            .queries
            .lock()
            .expect("process queries lock"),
        vec![(expected_socket, "42".to_owned())]
    );
}

#[test]
fn live_pane_clears_the_first_absence_probe() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.panes.set(Vec::new());
    setup.processes.set(Presence::Absent);
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000715"));
    let pane = pane_path(&setup.root(), &pane_address(&setup.env).expect("address").0);
    assert!(pane.join("absence-probe.json").exists());
    setup.panes.set(vec![PaneRow {
        pane_id: "42".to_owned(),
        tty_name: Some("/dev/ttys888".to_owned()),
    }]);
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000716"));
    assert!(!pane.join("absence-probe.json").exists());
}

#[test]
fn compaction_advances_floor_before_delete_and_replays_operation() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let old = setup.seed_presence("old-child", 300, 1, "stopped");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    let preview = setup.run_sweep(false, None).0;
    assert!(old.exists());
    assert!(
        preview
            .details
            .iter()
            .any(|detail| detail["action"] == "advance_floor")
    );
    let operation = "00000000-0000-4000-8000-000000000717";
    setup.run_sweep(true, Some(operation));
    let floor = setup.binding_dir().join("agents-floor.json");
    let floor_bytes = fs::read(&floor).expect("floor record");
    assert!(!old.exists());
    let delayed = setup.seed_presence("delayed-child", 250, 1, "stopped");
    let replay = setup.run_sweep(true, Some(operation)).0;
    assert!(!delayed.exists());
    assert_eq!(fs::read(&floor).expect("floor record"), floor_bytes);
    assert!(
        replay
            .details
            .iter()
            .any(|detail| detail["action"] == "replay_floor")
    );
}

#[test]
fn compaction_apply_revalidates_reactivated_child() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let child = setup.seed_presence("child-a", 300, 1, "stopped");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    assert!(
        setup
            .run_sweep(false, None)
            .0
            .details
            .iter()
            .any(|detail| detail["action"] == "advance_floor")
    );
    let mut active: Value =
        serde_json::from_slice(&fs::read(&child).expect("presence")).expect("presence JSON");
    active["status"] = json!("active");
    active["event_id"] = json!(Uuid::new_v4().to_string());
    active["observed_mono_ns"] = json!("00000000000000000400");
    active["written_at_unix_ns"] = json!(format!("{:020}", RETENTION_AGE_NS as u64 + 2));
    atomic_replace(&child, &active).expect("reactivate child");
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000718"));
    assert!(child.exists());
    assert!(!setup.binding_dir().join("agents-floor.json").exists());
}

#[test]
fn negative_wall_age_reports_clock_skew_and_preserves_child() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let child = setup.seed_presence("future-child", 300, 9_000_000_000, "stopped");
    setup.clock.set_unix(2_000_000_000);
    let (_, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000719"));
    assert!(child.exists());
    assert!(diagnostics.iter().any(|item| item.code == "clock_skew"));
    assert!(!setup.binding_dir().join("agents-floor.json").exists());
}

#[test]
fn binding_history_cap_is_calculated_per_realm() {
    let two_realms = BTreeMap::from([
        (
            "realm-a".to_owned(),
            (0..251)
                .map(|index| {
                    (
                        format!("{index:020}"),
                        PathBuf::from(format!("/a/{index}")),
                        false,
                    )
                })
                .collect(),
        ),
        (
            "realm-b".to_owned(),
            (0..251)
                .map(|index| {
                    (
                        format!("{index:020}"),
                        PathBuf::from(format!("/b/{index}")),
                        false,
                    )
                })
                .collect(),
        ),
    ]);
    assert!(binding_cap_paths_by_realm(&two_realms).is_empty());
    let one_realm = BTreeMap::from([(
        "realm-a".to_owned(),
        (0..501)
            .map(|index| {
                (
                    format!("{index:020}"),
                    PathBuf::from(format!("/a/{index}")),
                    false,
                )
            })
            .collect(),
    )]);
    assert_eq!(binding_cap_paths_by_realm(&one_realm).len(), 1);
}

#[test]
fn equal_order_active_child_blocks_the_whole_cap_group() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.clock.set_unix(10_000_000_000);
    setup.seed_presence("tie-stopped", 300, 10_000_000_000, "stopped");
    setup.seed_presence("tie-active", 300, 10_000_000_000, "active");
    for index in 0..499 {
        setup.seed_presence(
            &format!("later-{index}"),
            301 + index,
            10_000_000_000,
            "stopped",
        );
    }
    let (result, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000720"));
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "binding_conflict")
    );
    assert!(
        !result
            .details
            .iter()
            .any(|detail| detail["action"] == "advance_floor")
    );
    assert!(!setup.binding_dir().join("agents-floor.json").exists());
}

#[test]
fn old_noncurrent_binding_is_pruned_but_current_binding_is_preserved() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.provider_event(
        "PreToolUse",
        "session-a",
        json!({"tool_name":"Bash","tool_use_id":"retained-fact"}),
        "00000000000000000250",
    );
    let old_dir = setup.binding_dir();
    assert!(old_dir.join("lifecycle.json").exists());
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
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let current_dir = launch_path(&root, &address, launch_id)
        .join("bindings")
        .join(binding_id("claude", "session-b", launch_id));
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000721"));
    assert!(!old_dir.exists());
    assert!(current_dir.exists());
}

#[test]
fn unknown_binding_file_and_future_child_are_preserved() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let child = setup.seed_presence("future-child", 300, 1, "stopped");
    let mut future: Value =
        serde_json::from_slice(&fs::read(&child).expect("child")).expect("child JSON");
    future["schema"] = json!(999);
    fs::write(&child, serde_json::to_vec(&future).expect("future JSON"))
        .expect("write future child");
    let unknown = setup.binding_dir().join("unknown.state");
    fs::write(&unknown, b"preserve").expect("write unknown state");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    let (_, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000722"));
    assert!(child.exists());
    assert!(unknown.exists());
    assert!(
        diagnostics
            .iter()
            .any(|item| matches!(item.code.as_str(), "future_schema" | "record_invalid"))
    );
    assert!(!setup.binding_dir().join("agents-floor.json").exists());
}

#[test]
fn every_emitted_diagnostic_literal_is_declared_by_the_manifest() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let declared = &wezterm_attention::protocol::manifest()
        .expect("manifest")
        .enums
        .diagnostic_codes;
    let mut emitted = BTreeMap::new();
    assert_eq!(
        declared,
        &wezterm_attention::protocol::EMITTED_DIAGNOSTIC_CODES
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    for entry in fs::read_dir(source_root).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("source text");
        for marker in ["AttentionError::new(", "diagnostic("] {
            for tail in source.split(marker).skip(1) {
                let tail = tail.trim_start();
                let Some(tail) = tail.strip_prefix('"') else {
                    continue;
                };
                let Some(end) = tail.find('"') else { continue };
                emitted.insert(tail[..end].to_owned(), path.clone());
            }
        }
    }
    emitted.insert(
        "bad_usage".to_owned(),
        PathBuf::from("AttentionError::usage"),
    );
    emitted.insert(
        "record_invalid".to_owned(),
        PathBuf::from("AttentionError::record_json"),
    );
    assert!(!emitted.is_empty());
    for (code, path) in emitted {
        assert!(
            declared.contains(&code),
            "{code} from {} is not in the manifest",
            path.display()
        );
    }
}

#[test]
fn foreign_retention_floor_never_deletes_children() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let active = setup.seed_presence("active", 300, 10_000_000_000, "active");
    let stopped = setup.seed_presence("stopped", 301, 1, "stopped");
    let operation = "00000000-0000-4000-8000-000000000723";
    let (address, _) = pane_address(&setup.env).expect("address");
    atomic_replace(
        &setup.binding_dir().join("agents-floor.json"),
        &json!({
            "kind":"subagent_retention_floor","schema":3,"address":address,
            "launch_id":"00000000-0000-4000-8000-000000000999",
            "binding_id":"f".repeat(64),"floor_mono_ns":"00000009999999999999",
            "operation_id":operation
        }),
    )
    .expect("write foreign floor");
    setup.clock.set_unix(10_000_000_000);
    let (_, diagnostics) = setup.run_sweep(true, Some(operation));
    assert!(active.exists());
    assert!(stopped.exists());
    assert!(diagnostics.iter().any(|item| item.code == "record_invalid"));
}

struct SignalingPanes {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    calls: AtomicU8,
}

impl PaneLister for SignalingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.wait();
            self.release.wait();
        }
        Ok(Vec::new())
    }
}

#[test]
fn sweep_apply_uses_the_binding_reread_after_selection() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Absent);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let panes = SignalingPanes {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        calls: AtomicU8::new(0),
    };
    let binding_path = setup.binding_dir().join("binding.json");
    let root = setup.root();
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            sweep(
                &root,
                None,
                true,
                Some("00000000-0000-4000-8000-000000000724"),
                &setup.clock,
                &panes,
                Some(&setup.processes),
            )
            .expect("sweep")
        });
        entered.wait();
        let mut binding: Value =
            serde_json::from_slice(&fs::read(&binding_path).expect("binding record"))
                .expect("binding JSON");
        binding["observed_mono_ns"] = json!("00000000000000000999");
        atomic_replace(&binding_path, &binding).expect("rewrite binding during selection window");
        release.wait();
        worker.join().expect("sweep worker")
    });
    let (result, diagnostics) = result;
    assert!(
        !pane_path(&root, &pane_address(&setup.env).expect("address").0)
            .join("absence-probe.json")
            .exists()
    );
    assert!(diagnostics.iter().any(|item| {
        item.code == "record_invalid" && item.message.contains("changed before sweep apply")
    }));
    assert!(
        !result
            .details
            .iter()
            .any(|detail| detail["action"] == "first_absence")
    );
}

#[test]
fn missing_claim_preserves_old_binding_history() {
    let setup = Setup::new();
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
    let (address, _) = pane_address(&setup.env).expect("address");
    fs::remove_file(pane_path(&setup.root(), &address).join("claim.json")).expect("remove claim");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    let (result, _) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000725"));
    assert!(old_dir.exists());
    assert!(result.details.iter().any(|detail| {
        detail["kind"] == "binding_selection" && detail["action"] == "unavailable"
    }));
}

#[test]
fn malformed_claim_is_contained_to_its_binding_selection() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (address, _) = pane_address(&setup.env).expect("address");
    fs::write(
        pane_path(&setup.root(), &address).join("claim.json"),
        b"not json",
    )
    .expect("corrupt claim");
    let (result, diagnostics) = setup.run_sweep(false, None);
    assert!(diagnostics.iter().any(|item| item.code == "record_invalid"));
    assert!(result.details.iter().any(|detail| {
        detail["kind"] == "binding_selection" && detail["action"] == "unavailable"
    }));
}

#[test]
fn doctor_rejects_a_valid_record_at_the_wrong_depth() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let (address, _) = pane_address(&setup.env).expect("address");
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let binding_id = binding_id("claude", "session-a", launch_id);
    let agent_key = wezterm_attention::protocol::sha256_hex(b"misplaced");
    atomic_replace(
        &pane_path(&setup.root(), &address)
            .join("agents")
            .join(format!("{agent_key}.json")),
        &json!({
            "kind":"subagent_presence","schema":3,"address":address,
            "launch_id":launch_id,"binding_id":binding_id,"provider":"claude",
            "agent_id":"misplaced","agent_key":agent_key,"source":"worker","status":"stopped",
            "event_id":"00000000-0000-4000-8000-000000000726",
            "observed_mono_ns":"00000000000000000300",
            "written_at_unix_ns":"00000000001000000000","ttl_ms":600000
        }),
    )
    .expect("write misplaced record");
    let (_, diagnostics) =
        doctor(&setup.root(), Some(&setup.panes), Some(&setup.processes)).expect("doctor");
    assert!(diagnostics.iter().any(|item| item.code == "record_invalid"));
}

#[test]
fn sweep_never_follows_a_symlinked_binding_tree_outside_the_state_root() {
    use std::os::unix::fs::symlink;

    let setup = Setup::new();
    setup.claim_and_bind();
    let binding_dir = setup.binding_dir();
    let bindings = binding_dir
        .parent()
        .expect("bindings directory")
        .to_path_buf();
    let outside = setup._scratch.0.join("outside-bindings");
    fs::rename(&bindings, &outside).expect("move bindings outside state root");
    symlink(&outside, &bindings).expect("link bindings outside state root");
    let result = setup
        .run_sweep(true, Some("00000000-0000-4000-8000-000000000727"))
        .0;
    assert_eq!(result.scanned, 0);
    assert!(
        outside
            .join(binding_dir.file_name().expect("binding id"))
            .exists()
    );
}

#[test]
fn retention_preserves_unknown_files_inside_an_old_binding() {
    let setup = Setup::new();
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
    let unknown = old_dir.join("agents").join("unknown.state");
    fs::create_dir_all(unknown.parent().expect("agents directory")).expect("create agents");
    fs::write(&unknown, b"preserve").expect("write unknown state");
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    let (_, diagnostics) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000728"));
    assert!(old_dir.exists());
    assert!(unknown.exists());
    assert!(diagnostics.iter().any(|item| item.code == "record_invalid"));
}

#[test]
fn a_session_resumed_in_a_new_pane_conflicts_only_while_both_panes_live() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let mut resumed = address.clone();
    resumed.pane_id = "99".to_owned();
    let launch_id = "00000000-0000-4000-8000-000000000702";
    let resumed_binding = binding_id("claude", "session-a", launch_id);
    atomic_replace(
        &launch_path(&root, &resumed, launch_id)
            .join("bindings")
            .join(&resumed_binding)
            .join("binding.json"),
        &json!({
            "kind":"binding","schema":3,"address":resumed,"launch_id":launch_id,
            "binding_id":resumed_binding,"event_id":Uuid::new_v4().to_string(),
            "provider":"claude","provider_session_id":"session-a","start_source":"resume",
            "observed_mono_ns":"00000000000000000300",
            "written_at_unix_ns":"00000000001000000000","writer_version":"2.0.0"
        }),
    )
    .expect("write resumed binding");

    setup.panes.set(vec![
        PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some("/dev/ttys888".to_owned()),
        },
        PaneRow {
            pane_id: "99".to_owned(),
            tty_name: Some("/dev/ttys889".to_owned()),
        },
    ]);
    let (rows, diagnostics) =
        read_bindings_with_ports(&root, Some(&setup.panes), Some(&setup.processes))
            .expect("bindings with both panes live");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.binding_health == "conflicted"));
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "binding_conflict")
    );

    // The old pane is gone. One live claim remains, so it is not a conflict.
    setup.panes.set(vec![PaneRow {
        pane_id: "42".to_owned(),
        tty_name: Some("/dev/ttys888".to_owned()),
    }]);
    setup.processes.set(Presence::Absent);
    let (rows, diagnostics) =
        read_bindings_with_ports(&root, Some(&setup.panes), Some(&setup.processes))
            .expect("bindings after the old pane is gone");
    assert_eq!(rows.len(), 2);
    let gone = rows
        .iter()
        .find(|row| row.address.pane_id == "99")
        .expect("resumed pane row");
    assert_eq!(gone.pane_presence, "verified_absent");
    let live = rows
        .iter()
        .find(|row| row.address.pane_id == "42")
        .expect("live pane row");
    assert_eq!(live.pane_presence, "present");
    assert_eq!(live.binding_health, "valid");
    assert!(
        !diagnostics
            .iter()
            .any(|item| item.code == "binding_conflict")
    );
}

fn collection_details(details: &[Value]) -> Vec<&Value> {
    details
        .iter()
        .filter(|detail| detail["kind"] == "projection_collection")
        .collect()
}

fn plant_flat_files(root: &std::path::Path, pane_id: &str) {
    fs::write(root.join(pane_id), "stop\n").expect("write marker");
    fs::write(root.join(format!("{pane_id}.agents")), "{}\n").expect("write agents");
    fs::write(root.join(format!("{pane_id}.ack")), "{}\n").expect("write ack");
    fs::write(root.join(format!("{pane_id}.review")), "{}\n").expect("write review");
}

#[test]
fn flat_orphan_preview_lists_without_removing() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    plant_flat_files(&root, "42");
    let preview = setup.run_sweep(false, None).0;
    let collections = collection_details(&preview.details);
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0]["pane_id"], "42");
    assert!(root.join("42").exists());
    assert!(root.join("42.agents").exists());
    assert!(root.join("42.ack").exists());
}

#[test]
fn flat_orphan_apply_removes_marker_agents_and_ack() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    plant_flat_files(&root, "42");
    let operation = "00000000-0000-4000-8000-000000000801";
    let applied = setup.run_sweep(true, Some(operation)).0;
    let collections = collection_details(&applied.details);
    assert_eq!(collections.len(), 1);
    assert!(!root.join("42").exists());
    assert!(!root.join("42.agents").exists());
    assert!(!root.join("42.ack").exists());
    assert!(root.join("42.review").exists());
    let again = setup.run_sweep(true, Some(operation)).0;
    assert!(collection_details(&again.details).is_empty());
}

#[test]
fn a_review_flag_survives_collection() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    plant_flat_files(&root, "42");
    setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000802"));
    assert!(root.join("42.review").exists());
}

#[test]
fn third_party_marker_without_a_claim_is_never_collected() {
    let setup = Setup::new();
    let root = setup.root();
    fs::create_dir_all(&root).expect("create state root");
    fs::write(root.join("99"), "stop\n").expect("write third-party marker");
    let (preview, diagnostics) = setup.run_sweep(false, None);
    assert!(collection_details(&preview.details).is_empty());
    assert!(diagnostics.is_empty());
    assert!(root.join("99").exists());
}

#[test]
fn an_ambiguous_scalar_id_is_refused_not_collected() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let claim_path = pane_path(&root, &address).join("claim.json");
    let mut claim: Value =
        serde_json::from_slice(&fs::read(&claim_path).expect("claim")).expect("claim JSON");
    let other = PaneAddress {
        realm_id: "e".repeat(64),
        incarnation_id: address.incarnation_id.clone(),
        pane_id: address.pane_id.clone(),
    };
    claim["address"] = json!(other);
    atomic_replace(&pane_path(&root, &other).join("claim.json"), &claim).expect("second claim");
    plant_flat_files(&root, "42");
    let (_, diagnostics) = setup.run_sweep(false, None);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "binding_conflict" && item.context["pane_id"] == "42")
    );
    assert!(root.join("42").exists());
}

#[test]
fn an_undecidable_claim_leaves_the_file_alone() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let claim_path = pane_path(&root, &address).join("claim.json");
    let mut claim: Value =
        serde_json::from_slice(&fs::read(&claim_path).expect("claim")).expect("claim JSON");
    claim["schema"] = json!(999);
    fs::write(
        &claim_path,
        serde_json::to_vec(&claim).expect("future claim JSON"),
    )
    .expect("write future claim");
    plant_flat_files(&root, "42");
    let (_, diagnostics) = setup.run_sweep(false, None);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "record_invalid" && item.context["pane_id"] == "42")
    );
    assert!(root.join("42").exists());
}

#[test]
fn a_symlinked_marker_is_refused() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let target = root.join("elsewhere");
    fs::write(&target, "stop\n").expect("write symlink target");
    symlink(&target, root.join("42")).expect("symlink marker");
    let (_, diagnostics) = setup.run_sweep(false, None);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "record_invalid" && item.context["pane_id"] == "42")
    );
    assert!(
        root.join("42")
            .symlink_metadata()
            .expect("symlink")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn an_incomplete_claim_walk_does_not_grant_collection() {
    for start_is_symlink in [false, true] {
        let setup = Setup::new();
        setup.claim_and_bind();
        let root = setup.root();
        plant_flat_files(&root, "42");
        if start_is_symlink {
            let realms = root.join("v2/realms");
            let outside = root.join("realms-target");
            fs::rename(&realms, &outside).expect("move realms");
            symlink(&outside, &realms).expect("symlink realms");
        } else {
            let hidden = root.join("hidden-realm");
            fs::create_dir_all(&hidden).expect("hidden realm");
            symlink(&hidden, root.join("v2/realms").join("link")).expect("symlink realm");
        }
        let (preview, diagnostics) = setup.run_sweep(false, None);
        assert!(
            collection_details(&preview.details).is_empty(),
            "start_is_symlink={start_is_symlink}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "record_invalid" || item.code == "probe_unavailable" }),
            "start_is_symlink={start_is_symlink} {diagnostics:?}"
        );
        assert!(root.join("42").exists());
        let applied = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000804"));
        assert!(collection_details(&applied.0.details).is_empty());
        assert!(root.join("42").exists());
    }
}

#[test]
fn a_replaced_marker_is_refused_not_collected() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    plant_flat_files(&root, "42");
    let (address, _) = pane_address(&setup.env).expect("address");
    let lock = pane_path(&root, &address).join(".claim.lock");
    let locked = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (details, diagnostics) = std::thread::scope(|scope| {
        let holder = scope.spawn(|| {
            with_lock(&lock, std::time::Duration::from_secs(5), || {
                locked.wait();
                release.wait();
                Ok(())
            })
            .expect("hold claim lock")
        });
        locked.wait();
        let sweep =
            scope.spawn(|| setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000805")));
        std::thread::sleep(std::time::Duration::from_millis(250));
        let next = root.join("42.agents.next");
        fs::write(&next, "thinking\n").expect("write replacement");
        fs::rename(&next, root.join("42.agents")).expect("replace agents");
        release.wait();
        holder.join().expect("holder");
        sweep.join().expect("sweep")
    });
    assert!(collection_details(&details.details).is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "record_invalid" && item.message.contains("changed")),
        "{diagnostics:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("42")).expect("marker"),
        "stop\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("42.agents")).expect("agents"),
        "thinking\n"
    );
    assert!(root.join("42.ack").exists());
}

fn write_tab_order(root: &Path, window_id: u64, marker_ids: &[&str]) -> PathBuf {
    let tabs = root.join("tabs");
    fs::create_dir_all(&tabs).expect("create tabs directory");
    let path = tabs.join(format!("{window_id}.json"));
    let ids: Vec<Value> = marker_ids.iter().map(|id| json!(id)).collect();
    let listed: Vec<Value> = if ids.is_empty() {
        Vec::new()
    } else {
        vec![json!({"marker_ids": ids, "number": 1, "text": "tab"})]
    };
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "published_at_ms": 1, "schema": 1, "tabs": listed, "window_id": window_id
        }))
        .expect("tab order JSON"),
    )
    .expect("write tab order");
    path
}

fn tab_order_detail(details: &[Value], window_id: u64) -> &Value {
    details
        .iter()
        .find(|detail| detail["kind"] == "tab_order_collection" && detail["window_id"] == window_id)
        .unwrap_or_else(|| panic!("no tab order detail for window {window_id}"))
}

#[test]
fn sweep_collects_a_tab_order_only_when_every_pane_it_names_is_verified_absent() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Absent);
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let v2 = |pane: &str| format!("v2:{}:{}:{pane}", address.realm_id, address.incarnation_id);

    // Pane 42 is in the mux listing; 43 and 44 are not, and the process probe
    // says they are gone.
    let live = write_tab_order(&root, 7, &[&v2("42"), &v2("43")]);
    let dead = write_tab_order(&root, 8, &[&v2("43"), &v2("44")]);
    let v1 = write_tab_order(&root, 9, &["17"]);
    let empty = write_tab_order(&root, 10, &[]);

    let (preview, _) = setup.run_sweep(false, None);
    assert_eq!(tab_order_detail(&preview.details, 7)["action"], "keep");
    assert_eq!(tab_order_detail(&preview.details, 7)["reason"], "present");
    assert_eq!(tab_order_detail(&preview.details, 8)["action"], "collect");
    assert_eq!(tab_order_detail(&preview.details, 8)["path"], "tabs/8.json");
    assert_eq!(
        tab_order_detail(&preview.details, 9)["reason"],
        "no_address"
    );
    // A window with no tabs has closed; an empty order is the bar's last draw.
    assert_eq!(tab_order_detail(&preview.details, 10)["action"], "collect");
    assert!(
        live.exists() && dead.exists() && v1.exists() && empty.exists(),
        "a preview writes nothing"
    );

    let (applied, _) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000721"));
    assert_eq!(tab_order_detail(&applied.details, 8)["action"], "collected");
    assert_eq!(
        tab_order_detail(&applied.details, 10)["action"],
        "collected"
    );
    assert!(
        !dead.exists() && !empty.exists(),
        "the dead windows' orders are collected"
    );
    assert!(live.exists() && v1.exists(), "everything else stays");
}

#[test]
fn sweep_keeps_a_tab_order_whose_panes_could_not_be_probed() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Unavailable);
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let unknown = write_tab_order(
        &root,
        11,
        &[&format!(
            "v2:{}:{}:43",
            address.realm_id, address.incarnation_id
        )],
    );
    let (applied, _) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000722"));
    assert_eq!(tab_order_detail(&applied.details, 11)["action"], "keep");
    assert_eq!(
        tab_order_detail(&applied.details, 11)["reason"],
        "unavailable"
    );
    assert!(unknown.exists());
}

#[test]
fn a_realm_filtered_sweep_leaves_tab_orders_alone() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Absent);
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let dead = write_tab_order(
        &root,
        12,
        &[&format!(
            "v2:{}:{}:43",
            address.realm_id, address.incarnation_id
        )],
    );
    let (result, _) = sweep(
        &root,
        Some(&address.realm_id),
        true,
        Some("00000000-0000-4000-8000-000000000723"),
        &setup.clock,
        &setup.panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    assert!(
        result
            .details
            .iter()
            .all(|detail| detail["kind"] != "tab_order_collection")
    );
    assert!(dead.exists());
}

#[test]
fn leftover_preview_keeps_every_collection_row() {
    let mut details = Vec::new();
    for pane in 0..51 {
        details.push(json!({
            "kind": "projection_collection",
            "pane_id": pane.to_string(),
            "paths": [pane.to_string()],
        }));
    }
    for index in 0..51 {
        details.push(json!({
            "kind": "absence",
            "binding_id": index.to_string(),
            "action": "replay_end",
        }));
    }
    let (shown, total) = limit_sweep_preview(details.clone(), false);
    assert_eq!(total, 102);
    assert_eq!(
        shown
            .iter()
            .filter(|detail| detail["kind"] == "projection_collection")
            .count(),
        51
    );
    assert_eq!(
        shown
            .iter()
            .filter(|detail| detail["kind"] == "absence")
            .count(),
        50
    );
    let (all, all_total) = limit_sweep_preview(details, true);
    assert_eq!(all_total, 102);
    assert_eq!(all.len(), 102);
}

#[test]
fn an_empty_state_root_answers_completely() {
    let setup = Setup::new();
    fs::create_dir_all(setup.root()).expect("create state root");
    let (result, diagnostics) = setup.run_sweep(false, None);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(result.details.is_empty());
}

#[test]
fn a_present_row_from_an_incomplete_bindings_answer_inspects_completely() {
    let setup = Setup::new();
    setup.claim_and_bind();
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let launch_id = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    let other_id = "d".repeat(64);
    let other_dir = launch_path(&root, &address, &launch_id)
        .join("bindings")
        .join(&other_id);
    fs::create_dir_all(&other_dir).expect("create extra binding");
    atomic_replace(
        &other_dir.join("binding.json"),
        &json!({
            "kind":"binding","schema":3,"address":address,"launch_id":launch_id,
            "binding_id":other_id,"event_id":"00000000-0000-4000-8000-000000000702",
            "provider":"claude","provider_session_id":"session-b",
            "start_source":"startup","observed_mono_ns":"00000000000000000702",
            "written_at_unix_ns":"00000000001000000000","writer_version":"2.0.0"
        }),
    )
    .expect("write extra binding");
    let bindings = Command::new(env!("CARGO_BIN_EXE_attention"))
        .env_clear()
        .envs(&setup.env)
        .args(["bindings", "--json", "--limit", "1"])
        .output()
        .expect("run bindings");
    let envelope: Value = serde_json::from_slice(&bindings.stdout).expect("bindings JSON");
    assert_eq!(envelope["complete"], false);
    assert_eq!(envelope["result"]["truncated"], true);
    let (rows, _) =
        read_bindings_with_ports(&root, Some(&setup.panes), Some(&setup.processes)).expect("rows");
    let present = rows
        .iter()
        .find(|row| row.current && row.pane_presence == "present")
        .expect("present current row");
    let scope = PaneScope::new(
        present.address.clone(),
        present.launch_id.clone(),
        Some(present.binding_id.clone()),
    )
    .expect("inspect scope");
    let facts = read_pane_facts_with_ports(
        &root,
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("inspect");
    assert!(facts.complete());
    assert_eq!(
        facts.pane_presence,
        wezterm_attention::query::PanePresence::Present
    );
}
