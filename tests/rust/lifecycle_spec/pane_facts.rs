use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use wezterm_attention::observations::{
    Actor, LifecycleAvailability, LifecycleView, NativeCorrelation,
};
use wezterm_attention::query::{
    PaneFacts, PanePresence, PaneScope, RecordAvailability as A, ScopeRelation,
    read_pane_facts_with_ports,
};
use wezterm_attention::records::{FileRecords, RecordIdentity, RecordRead, RecordReader};

fn setup() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "facts",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}
fn scope(setup: &Setup) -> PaneScope {
    PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        Some(binding_id(
            "claude",
            "facts",
            &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"],
        )),
    )
    .unwrap()
}
fn read(setup: &Setup) -> PaneFacts {
    read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope(setup),
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap()
}
fn bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.extend(bytes(&path));
        } else {
            result.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    result
}

#[test]
fn inspect_is_scoped_read_only_and_keeps_raw_activity_after_acknowledgement() {
    let setup = setup();
    setup.apply(&event("claude","PreToolUse","facts",json!({"tool_name":"Read","agent_id":"child-a","agent_type":"Explore","tool_use_id":"child-tool"})),"00000000000000000300");
    setup.apply(
        &event("claude", "Stop", "facts", json!({})),
        "00000000000000000400",
    );
    apply_mark_review(&setup.env, "fixture-owner", false).unwrap();
    setup.apply(
        &event("claude", "SessionEnd", "facts", json!({"reason":"other"})),
        "00000000000000000500",
    );
    let dir = setup.binding_dir("claude", "facts");
    let activity: Value =
        serde_json::from_slice(&fs::read(dir.join("activity.json")).unwrap()).unwrap();
    atomic_replace(&dir.join("ack.json"),&json!({"kind":"acknowledgement","schema":3,"address":activity["address"],"launch_id":activity["launch_id"],"target":activity["target"],"activity_event_id":activity["event_id"],"event_id":Uuid::new_v4().to_string()})).unwrap();
    let before = bytes(&state_root(&setup.env).unwrap());
    let facts = read(&setup);
    assert!(facts.complete(), "{:?}", facts.diagnostics);
    assert_eq!(facts.activity.availability, A::Present);
    assert_eq!(facts.activity.record.as_ref().unwrap(), &activity);
    assert_eq!(facts.children.count, 1);
    assert_eq!(
        facts.children.eligibility.as_ref().unwrap().ttl_ms,
        wezterm_attention::protocol::manifest()
            .unwrap()
            .limits
            .subagent_ttl_ms
    );
    assert!(facts.review.eligibility.is_none());
    assert_eq!(facts.review.count, 1);
    assert_eq!(facts.binding_end.availability, A::Present);
    assert_eq!(facts.pane_presence, PanePresence::Present);
    assert_eq!(facts.binding.as_ref().unwrap().binding_phase, "ended");
    assert!(facts.lifecycle.badge_acknowledgement.is_some());
    assert_eq!(
        facts.lifecycle.availability,
        LifecycleAvailability::Available
    );
    assert_eq!(before, bytes(&state_root(&setup.env).unwrap()));
    let value = serde_json::to_value(facts).unwrap();
    assert!(
        value["activity"]["record"]
            .get("observed_mono_ns")
            .is_some()
    );
    assert!(
        value["binding_end"]["record"]
            .get("written_at_unix_ns")
            .is_some()
    );
    assert!(value["binding_end"]["record"].get("operation_id").is_none());
    let mut end: Value = serde_json::from_slice(&fs::read(dir.join("end.json")).unwrap()).unwrap();
    let operation = Uuid::new_v4().to_string();
    end["operation_id"] = json!(operation);
    end["reason"] = json!("sweep_absent");
    atomic_replace(&dir.join("end.json"), &end).unwrap();
    assert_eq!(
        read(&setup).binding_end.record.unwrap()["operation_id"],
        operation
    );
    end["observed_mono_ns"] = json!("00000000000000000001");
    atomic_replace(&dir.join("end.json"), &end).unwrap();
    assert_eq!(
        read(&setup).binding_end.availability,
        A::Absent,
        "old end evidence cannot override a newer binding"
    );
}

#[test]
fn inspector_rechecks_socket_and_preserves_unbound_optionality() {
    let setup = Setup::new();
    setup.claim();
    let scope = PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone(),
        None,
    )
    .unwrap();
    let mut wire = serde_json::to_value(&scope).unwrap();
    assert!(wire.get("binding_id").is_none());
    wire["binding_id"] = Value::Null;
    assert_eq!(serde_json::from_value::<PaneScope>(wire).unwrap(), scope);
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap();
    assert!(facts.complete());
    assert!(facts.binding.is_none());
    assert_eq!(facts.activity.availability, A::Absent);
    struct Rebirth {
        socket: PathBuf,
        replacement: Mutex<Option<UnixListener>>,
    }
    impl PaneLister for Rebirth {
        fn list(&self, _: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
            fs::remove_file(&self.socket).unwrap();
            *self.replacement.lock().unwrap() = Some(UnixListener::bind(&self.socket).unwrap());
            Ok(vec![PaneRow {
                pane_id: "42".into(),
                tty_name: None,
            }])
        }
    }
    let rebirth = Rebirth {
        socket: PathBuf::from(&setup.env["WEZTERM_UNIX_SOCKET"]),
        replacement: Mutex::new(None),
    };
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&rebirth),
        None,
    )
    .unwrap();
    assert_eq!(facts.scope_relation, ScopeRelation::Unavailable);
    assert_eq!(facts.diagnostics[0].code, "incarnation_changed");
    assert_eq!(
        facts.activity.availability,
        A::Unavailable,
        "mixed snapshot must be discarded"
    );
}

#[test]
fn activity_absent_cleared_expired_and_bad_fences_are_distinct() {
    let setup = setup();
    let dir = setup.binding_dir("claude", "facts");
    assert_eq!(read(&setup).activity.availability, A::Absent);
    assert_eq!(
        read(&setup).lifecycle.availability,
        LifecycleAvailability::Absent
    );
    setup.apply(
        &event("claude", "Stop", "facts", json!({})),
        "00000000000000000300",
    );
    let mut activity: Value =
        serde_json::from_slice(&fs::read(dir.join("activity.json")).unwrap()).unwrap();
    activity["ttl_ms"] = json!(1);
    activity["written_at_unix_ns"] = json!("00000000000000000001");
    atomic_replace(&dir.join("activity.json"), &activity).unwrap();
    assert_eq!(read(&setup).activity.availability, A::Expired);
    atomic_replace(&dir.join("activity-clear.json"),&json!({"kind":"activity_clear","schema":3,"address":activity["address"],"launch_id":activity["launch_id"],"binding_id":scope(&setup).binding_id(),"event_id":Uuid::new_v4().to_string(),"observed_mono_ns":"00000000000000000400"})).unwrap();
    assert_eq!(read(&setup).activity.availability, A::Cleared);
    fs::remove_file(dir.join("activity.json")).unwrap();
    assert_eq!(read(&setup).activity.availability, A::Cleared);
    fs::write(dir.join("activity-clear.json"), "invalid").unwrap();
    let facts = read(&setup);
    assert_eq!(facts.activity.availability, A::Invalid);
    assert!(!facts.complete());
}

#[test]
fn headless_reads_distinguish_invalid_unsupported_and_io_unavailable() {
    let setup = setup();
    let dir = setup.binding_dir("claude", "facts");
    fs::write(dir.join("lifecycle.json"), "invalid").unwrap();
    let independent = read(&setup);
    assert_eq!(
        independent.binding_health,
        wezterm_attention::query::BindingHealth::Valid
    );
    assert_eq!(
        independent.reader_confidence,
        wezterm_attention::query::ReaderConfidence::Confirmed
    );
    assert_eq!(
        independent.lifecycle.availability,
        LifecycleAvailability::Invalid
    );
    assert!(
        !independent.complete(),
        "lifecycle validity is separate from base identity/read confidence"
    );
    for (content, expected, lifecycle) in [
        ("invalid", A::Invalid, LifecycleAvailability::Invalid),
        (
            r#"{"schema":99}"#,
            A::Unsupported,
            LifecycleAvailability::Unsupported,
        ),
    ] {
        fs::write(dir.join("activity.json"), content).unwrap();
        fs::write(dir.join("lifecycle.json"), content).unwrap();
        let facts = read(&setup);
        assert_eq!(facts.activity.availability, expected);
        assert_eq!(facts.lifecycle.availability, lifecycle);
        assert!(!facts.complete());
        assert!(facts.activity.record.is_none());
    }
    for name in ["activity.json", "lifecycle.json"] {
        fs::remove_file(dir.join(name)).unwrap();
        fs::create_dir(dir.join(name)).unwrap();
    }
    let facts = read(&setup);
    assert_eq!(facts.activity.availability, A::Unavailable);
    assert_eq!(
        facts.lifecycle.availability,
        LifecycleAvailability::Unavailable
    );
    assert!(!facts.complete());
}

#[test]
fn inspect_refuses_changed_scope_and_a_mid_read_revision() {
    let setup = setup();
    let current = scope(&setup);
    let wrong =
        PaneScope::new(current.address().clone(), Uuid::new_v4().to_string(), None).unwrap();
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &wrong,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap();
    assert_eq!(facts.scope_relation, ScopeRelation::LaunchChanged);
    assert!(facts.binding.is_none());
    struct Rotate {
        pointer: PathBuf,
        done: AtomicBool,
    }
    impl RecordReader for Rotate {
        fn read(&self, path: &Path, kind: Option<&str>, identity: &RecordIdentity) -> RecordRead {
            let result = FileRecords.read(path, kind, identity);
            if kind == Some("lifecycle_snapshot") && !self.done.swap(true, Ordering::SeqCst) {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&self.pointer).unwrap()).unwrap();
                value["binding_id"] = json!("d".repeat(64));
                atomic_replace(&self.pointer, &value).unwrap();
            }
            result
        }
        fn entries(&self, path: &Path) -> wezterm_attention::protocol::Result<Vec<PathBuf>> {
            FileRecords.entries(path)
        }
    }
    let reader = Rotate {
        pointer: setup
            .binding_dir("claude", "facts")
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("current-binding.json"),
        done: AtomicBool::new(false),
    };
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &current,
        &reader,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap();
    assert_eq!(facts.scope_relation, ScopeRelation::Unavailable);
    assert!(facts.binding.is_none());
    assert_eq!(facts.activity.availability, A::Unavailable);
    assert!(facts.lifecycle.observations.is_empty());
}

#[test]
fn inspect_unavailable_probe_and_collection_do_not_report_absence() {
    let setup = setup();
    let facts = read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope(&setup),
        &FileRecords,
        &setup.clock,
        None,
        None,
    )
    .unwrap();
    assert_eq!(facts.pane_presence, PanePresence::Unavailable);
    assert!(!facts.complete());
    fs::write(
        setup.binding_dir("claude", "facts").join("agents"),
        "not a directory",
    )
    .unwrap();
    let facts = read(&setup);
    assert_eq!(facts.children.availability, A::Unavailable);
    assert!(!facts.complete());
}

#[test]
fn inspector_cli_validates_scope_before_io_and_uses_public_shapes() {
    let setup = setup();
    let scope = serde_json::to_value(scope(&setup)).unwrap();
    let fake = setup._scratch.0.join("wezterm");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' '[{\"pane_id\":\"42\"}]'\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |scope: &Value| {
        let mut child = rust_command(&setup)
            .env("WEZTERM_EXECUTABLE", &fake)
            .args(["inspect", "--scope", "-", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(scope.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let output = run(&scope);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["schema"], 1);
    assert_eq!(response["result"]["scope"], scope);
    assert_eq!(
        response["result"]["activity"],
        json!({"availability":"absent","diagnostics":[]})
    );
    let mut bad = scope.clone();
    bad["address"]["pane_id"] = json!("../42");
    assert_eq!(run(&bad).status.code(), Some(2));
    bad = scope.clone();
    bad["address"]["extra"] = json!(true);
    assert_eq!(run(&bad).status.code(), Some(2));
    bad = scope;
    bad["binding_id"] = json!("d".repeat(64));
    let output = run(&bad);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"]["scope_relation"],
        "binding_changed"
    );
}

#[test]
fn rust_and_installed_lua_share_relation_cases_and_retention_floors() {
    let setup = Setup::new();
    let fixtures: Value =
        serde_json::from_str(include_str!("../../fixtures/lifecycle/observations.json")).unwrap();
    let relations: Value =
        serde_json::from_str(include_str!("../../fixtures/lifecycle/relations.json")).unwrap();
    let mut cases = Vec::new();
    for case in fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["expected"] == "valid")
    {
        let snapshot: LifecycleSnapshot = serde_json::from_value(case["value"].clone()).unwrap();
        cases.push(json!({"id":case["id"],"snapshot":snapshot,"now":"99999999999999999999","expected":LifecycleView::from_snapshot(&snapshot,Some("99999999999999999999")).unwrap()}));
    }
    for case in relations["cases"].as_array().unwrap() {
        let find = |key: &str| {
            fixtures["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == case[key])
                .unwrap()["value"]
                .clone()
        };
        let mut snapshot: LifecycleSnapshot = serde_json::from_value(find("request")).unwrap();
        let result_snapshot: LifecycleSnapshot = serde_json::from_value(find("result")).unwrap();
        let mut pre = snapshot.pools.requests.observations.remove(0);
        let mut post = result_snapshot.pools.requests.observations[0].clone();
        pre.observation_id = Uuid::new_v4().to_string();
        post.observation_id = Uuid::new_v4().to_string();
        pre.observed_mono_ns = "00000000000000000500".into();
        post.observed_mono_ns = "00000000000000000600".into();
        let elicitation = case["request"] == "elicitation_requested";
        let correlation = if elicitation {
            NativeCorrelation {
                elicitation_id: Some("native-q".into()),
                mcp_server_name: Some("server-a".into()),
                ..Default::default()
            }
        } else {
            NativeCorrelation {
                tool_call_id: Some("native-q".into()),
                ..Default::default()
            }
        };
        pre.correlation = Some(correlation.clone());
        post.correlation = Some(correlation);
        match case["variant"].as_str().unwrap() {
            "reversed" => post.observed_mono_ns = "00000000000000000400".into(),
            "idless" => {
                pre.correlation = None;
                post.correlation = None;
            }
            "turn" => post.correlation.as_mut().unwrap().turn_id = Some("another-turn".into()),
            "actor" => {
                post.actor = Actor::Child {
                    agent_id: "child".into(),
                    agent_key: wezterm_attention::protocol::sha256_hex(b"child"),
                }
            }
            "mcp" => post.correlation.as_mut().unwrap().mcp_server_name = Some("server-b".into()),
            _ => {}
        }
        snapshot.pools = Default::default();
        snapshot.reduce(pre).unwrap();
        snapshot.reduce(post).unwrap();
        snapshot.pools.requests.retention_floor_mono_ns = Some("00000000000000000001".into());
        snapshot.pools.general.retention_floor_mono_ns = Some("00000000000000000002".into());
        let view = LifecycleView::from_snapshot(&snapshot, Some("99999999999999999999")).unwrap();
        assert_eq!(view.retention_floors.len(), 2);
        assert_eq!(view.requests.len() as u64, case["groups"].as_u64().unwrap());
        assert_eq!(
            view.requests
                .iter()
                .map(|r| r.relations.len())
                .sum::<usize>() as u64,
            case["relations"].as_u64().unwrap()
        );
        cases.push(json!({"id":case["id"],"snapshot":snapshot,"now":"99999999999999999999","expected":view}));
    }
    let input = setup._scratch.0.join("parity.json");
    fs::write(&input, serde_json::to_vec(&cases).unwrap()).unwrap();
    let result = setup._scratch.0.join("parity-result");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wezterm = crate::executables::resolve("wezterm");
    let output = Command::new(&wezterm)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("WEZTERM_ATTENTION_SMOKE_RESULT", &result)
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env("WEZTERM_ATTENTION_FACTS_PARITY", &input)
        .arg("--config-file")
        .arg(root.join("tests/lua/wezterm_protocol_smoke.lua"))
        .args(["show-keys", "--lua"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result = fs::read_to_string(result).unwrap();
    assert!(result.starts_with("ok -"), "{result}");
}
