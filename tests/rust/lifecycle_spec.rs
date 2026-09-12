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
    apply_mark_activity, apply_mark_review, apply_provider_event, binding_id, prompt_return,
};
use wezterm_attention::observations::LifecycleSnapshot;
use wezterm_attention::providers::{ProviderAction, ProviderEvent, parse_provider_event};
use wezterm_attention::query::read_bindings;
use wezterm_attention::records::{atomic_replace, launch_path, pane_path, state_root, with_lock};
use wezterm_attention::wezterm::{Clock, PaneLister, PaneRow, RuntimePorts, TtyWriter};

struct Scratch(PathBuf);

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

#[derive(Default)]
struct FakePanes(Vec<PaneRow>);

impl PaneLister for FakePanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        Ok(self.0.clone())
    }
}

struct Setup {
    _scratch: Scratch,
    _socket: UnixListener,
    env: BTreeMap<String, String>,
    tty: FakeTty,
    panes: FakePanes,
    clock: FixedClock,
}

impl Setup {
    fn new() -> Self {
        let scratch = Scratch::new();
        let socket_path = scratch.0.join("mux.sock");
        let socket = UnixListener::bind(&socket_path).expect("bind disposable socket");
        let tty = FakeTty::new();
        let panes = FakePanes(vec![PaneRow {
            pane_id: "42".to_owned(),
            tty_name: Some(tty.path.clone()),
        }]);
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
        }
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
    let mut setup = Setup::new();
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
    setup.env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let result = setup.apply(
        &event("codex", "PreToolUse", "facts", json!({"tool_name":"shell"})),
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "partial");
    assert!(directory.join("activity.json").exists());
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
    let output = Command::new("/opt/homebrew/bin/wezterm")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("WEZTERM_ATTENTION_SMOKE_RESULT", &result)
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env(
            "WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_DIR",
            &setup.env["WEZTERM_ATTENTION_DIR"],
        )
        .env("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE", wire.to_string())
        .args(["--config-file"])
        .arg(root.join("tests/wezterm_protocol_smoke.lua"))
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
    for _ in 0..64 {
        let mut sibling = item.clone();
        sibling.observation_id = Uuid::new_v4().to_string();
        sibling.correlation = None;
        snapshot.reduce(sibling).unwrap();
    }
    assert!(snapshot.pools.general.observations.is_empty());
    assert_eq!(
        snapshot.pools.general.retention_floor_mono_ns,
        Some(item.observed_mono_ns)
    );
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
    future["schema"] = json!(3);
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
            assert_eq!(result.disposition, "applied", "{provider}:{name}");
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
    let mut command = Command::new("/opt/homebrew/bin/node");
    command
        .env_clear()
        .envs(&setup.env)
        .env("PATH", "/usr/bin:/bin")
        .env("WEZTERM_ATTENTION_ROOT", &bridge_root)
        .arg(root.join("tests/pi_lifecycle_runtime.mjs"));
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
fn frozen_reader_accepts_new_manifest_and_ignores_sidecar() {
    let setup = Setup::new();
    setup.claim();
    let start = run_hook(
        &setup,
        &["hooks", "event", "codex", "SessionStart", "--strict"],
        &payload(
            "codex",
            "SessionStart",
            "compat",
            json!({"source":"startup"}),
        ),
    );
    assert!(start.status.success());
    let tool = run_hook(
        &setup,
        &["hooks", "event", "codex", "PreToolUse", "--strict"],
        &payload(
            "codex",
            "PreToolUse",
            "compat",
            json!({"tool_name":"shell","tool_use_id":"compat-call"}),
        ),
    );
    assert!(tool.status.success());
    let sidecar = setup.binding_dir("codex", "compat").join("lifecycle.json");
    let bytes = fs::read(&sidecar).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let contract: Value =
        serde_json::from_str(include_str!("../fixtures/lifecycle/compatibility.json")).unwrap();
    let frozen = setup._scratch.0.join("frozen-reader");
    fs::create_dir_all(frozen.join("plugin")).unwrap();
    fs::create_dir_all(frozen.join("protocol")).unwrap();
    for file in contract["reader_files"].as_array().unwrap() {
        let file = file.as_str().unwrap();
        let output = Command::new("/usr/bin/git")
            .current_dir(&root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args([
                "show",
                &format!("{}:{file}", contract["baseline_commit"].as_str().unwrap()),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "frozen reader source must exist: {file}"
        );
        fs::write(frozen.join(file), output.stdout).unwrap();
    }
    fs::copy(
        root.join("protocol/v2.json"),
        frozen.join("protocol/v2.json"),
    )
    .unwrap();
    let result_path = setup._scratch.0.join("compat-result");
    let (address, _) = pane_address(&setup.env).unwrap();
    let wire =
        json!({"wire":2,"address":address,"launch_id":setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]});
    let output = Command::new("/opt/homebrew/bin/wezterm")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("ATTENTION_FROZEN_READER_ROOT", frozen)
        .env("ATTENTION_COMPAT_RESULT", &result_path)
        .env("WEZTERM_ATTENTION_DIR", &setup.env["WEZTERM_ATTENTION_DIR"])
        .env("ATTENTION_TEST_WIRE", wire.to_string())
        .arg("--config-file")
        .arg(root.join("tests/fixtures/lifecycle/compatibility.lua"))
        .args(["show-keys", "--lua"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result = fs::read_to_string(result_path).unwrap();
    assert!(result.starts_with("ok -"), "{result}");
    assert_eq!(
        fs::read(sidecar).unwrap(),
        bytes,
        "old reader must not modify the sidecar"
    );
}

#[test]
fn native_codex_async_tool_hooks_reach_the_snapshot() {
    let setup = Setup::new();
    setup.claim();
    let bridge = setup._scratch.0.join("native-bridge");
    fs::create_dir_all(bridge.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_attention"),
        bridge.join("bin/attention"),
    )
    .unwrap();
    let output = Command::new("/opt/homebrew/bin/node")
        .env_clear()
        .envs(&setup.env)
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
        .env("WEZTERM_ATTENTION_ROOT", &bridge)
        .env(
            "ATTENTION_CODEX_SOURCE",
            std::env::var("ATTENTION_CODEX_SOURCE")
                .unwrap_or_else(|_| "/Users/provi/Development/_sources/codex".into()),
        )
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lifecycle_contact_probe.mjs"))
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
    let output = Command::new("/opt/homebrew/bin/wezterm")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env("WEZTERM_ATTENTION_SMOKE_RESULT", &result_path)
        .env(
            "WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_DIR",
            &setup.env["WEZTERM_ATTENTION_DIR"],
        )
        .env("WEZTERM_ATTENTION_LIFECYCLE_FIXTURE_WIRE", wire.to_string())
        .env("WEZTERM_ATTENTION_LIFECYCLE_SCENARIO", "publication")
        .arg("--config-file")
        .arg(root.join("tests/wezterm_protocol_smoke.lua"))
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
    let output = Command::new("/opt/homebrew/bin/node")
        .env_clear()
        .envs(&setup.env)
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
        .env("WEZTERM_ATTENTION_ROOT", &bridge)
        .env("ATTENTION_NATIVE_UI", "1")
        .env(
            "ATTENTION_CODEX_SOURCE",
            std::env::var("ATTENTION_CODEX_SOURCE")
                .unwrap_or_else(|_| "/Users/provi/Development/_sources/codex".into()),
        )
        .env("ATTENTION_XTERM_MODULE", module)
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lifecycle_contact_probe.mjs"))
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
            if row["id"] == "H18" {
                assert!(
                    !directory.join("activity.json").exists(),
                    "child work cannot become lead activity"
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
    let projected: Value =
        serde_json::from_slice(&fs::read(&marker).expect("v1 marker")).expect("marker JSON");
    assert_eq!(projected["updated_at"], 12);
    assert_eq!(projected["updated_at_ms"], 12345);
    let sidecar: Value =
        serde_json::from_slice(&fs::read(root.join("42.agents")).expect("agents sidecar"))
            .expect("sidecar JSON");
    assert_eq!(sidecar["agents"]["child-a"]["type"], "Explore");
    assert_eq!(sidecar["agents"]["child-a"]["last_ms"], 12345);

    let cleared = prompt_return(&setup.env, "00000000000000000400").expect("prompt return");
    assert_eq!(cleared.disposition, "applied");
    let binding_dir = setup.binding_dir("claude", "session-a");
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
    assert!(root.join("42.agents").exists());

    assert_eq!(
        setup.apply(&thinking, "00000000000000000500").disposition,
        "applied"
    );
    assert!(marker.exists());
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
        state_root(&setup.env)
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
    assert!(root.join("42").exists());
    let (address, _) = pane_address(&setup.env).expect("address");
    let review = pane_path(&root, &address).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"pi-bus")
    ));
    assert!(review.exists());
}

#[test]
fn covered_activity_never_recreates_the_flat_projection() {
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
        let lock_name = format!(
            "n{}",
            fs::canonicalize(launch.join(".lock"))
                .expect("canonical lock path")
                .display()
        );
        let mut opened = false;
        for _ in 0..60 {
            let output = Command::new("/usr/sbin/lsof")
                .args(["-a", "-p", &child.id().to_string(), "-Fn"])
                .output()
                .expect("inspect delayed clear");
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == lock_name)
            {
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

    let delete_error =
        apply_mark_review(&setup.env, "pi-bus", true).expect_err("future review cannot be deleted");
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
    let status = Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            executable.to_str().expect("test path"),
            "--exact",
            "real_macos_controlling_tty_path_is_rejected_in_a_pty_child",
        ])
        .env(CHILD, "1")
        .status()
        .expect("run pty child");
    assert!(status.success());
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
        apply_mark_review(&setup.env, "pi-bus", false)
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
fn session_start_self_claims_only_when_the_standin_gate_is_enabled() {
    let mut enabled = Setup::new();
    enabled.env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    enabled.env.insert(
        "WEZTERM_ATTENTION_ENABLE_SELF_CLAIM".to_owned(),
        "1".to_owned(),
    );
    let start = event(
        "claude",
        "SessionStart",
        "session-a",
        json!({"source":"startup"}),
    );
    let result = enabled.apply(&start, "00000000000000000200");
    assert_eq!(result.disposition, "applied");
    let root = state_root(&enabled.env).expect("state root");
    let (address, _) = pane_address(&enabled.env).expect("address");
    assert!(pane_path(&root, &address).join("claim.json").exists());
    assert_eq!(read_bindings(&root).expect("bindings").0.len(), 1);

    let mut disabled = Setup::new();
    disabled.env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let disabled_result = disabled.apply(&start, "00000000000000000200");
    assert_eq!(disabled_result.disposition, "ignored");
    assert_eq!(
        disabled_result
            .diagnostic
            .as_ref()
            .map(|item| item.code.as_str()),
        Some("claim_stale")
    );
}

#[test]
fn tty_matching_claim_resolves_without_an_inherited_launch() {
    let mut setup = Setup::new();
    setup.claim();
    setup.env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let start = event(
        "claude",
        "SessionStart",
        "session-a",
        json!({"source":"startup"}),
    );
    assert_eq!(
        setup.apply(&start, "00000000000000000200").disposition,
        "applied"
    );
}

#[test]
fn non_start_without_a_claim_is_stale_for_every_provider() {
    let mut setup = Setup::new();
    setup.env.remove("WEZTERM_ATTENTION_LAUNCH_ID");
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
fn manual_mark_targets_the_launch_and_duplicate_repairs_legacy_projection() {
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
    assert!(marker.exists());
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
    fs::remove_file(&marker).expect("remove projection to test repair");
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
    assert_eq!(duplicate.disposition, "repaired_projection");
    assert!(marker.exists());
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
        apply_mark_review(&setup.env, "manual", false)
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
    apply_mark_review(&setup.env, "manual", true).expect("clear review");
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
    assert_eq!(strict.status.code(), Some(3));
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
