use std::collections::BTreeMap;
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::{Value, json};
use uuid::Uuid;
use wezterm_attention::identity::pane_address;
use wezterm_attention::lifecycle::{apply_provider_event, binding_id};
use wezterm_attention::maintenance::{
    ABSENCE_INTERVAL_NS, RETENTION_AGE_NS, binding_cap_paths_by_realm, doctor, sweep,
};
use wezterm_attention::providers::parse_provider_event;
use wezterm_attention::query::read_bindings_with_ports;
use wezterm_attention::records::{atomic_replace, launch_path, pane_path, state_root};
use wezterm_attention::wezterm::{
    Clock, PaneLister, PaneRow, Presence, ProcessProbe, RuntimePorts, TtyWriter,
};

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
                "kind":"subagent_presence","schema":2,"address":address,
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
            "kind":"subagent_retention_floor","schema":2,"address":address,
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
            "kind":"subagent_presence","schema":2,"address":address,
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
