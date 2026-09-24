use super::*;
use wezterm_attention::lifecycle::apply_mark_clear;
use wezterm_attention::query::{PaneScope, RecordAvailability, read_pane_facts_with_ports};
use wezterm_attention::records::FileRecords;

fn mark(setup: &Setup, state: &str, source: &str, observation: &str) {
    apply_mark_activity(
        &setup.env,
        state,
        source,
        None,
        None,
        None,
        observation,
        "00000000012345678900",
    )
    .expect("mark applies");
}

fn review_path(setup: &Setup, source: &str) -> PathBuf {
    let (address, _) = pane_address(&setup.env).unwrap();
    pane_path(&state_root(&setup.env).unwrap(), &address)
        .join("reviews")
        .join(format!(
            "{}.json",
            wezterm_attention::protocol::sha256_hex(source.as_bytes())
        ))
}

fn bound() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "claude",
            "SessionStart",
            "marks",
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}

// `mark clear --source X` withdraws everything X published: its review, and
// its activity through the same activity-clear watermark Pi's bus clear uses.
#[test]
fn mark_clear_withdraws_the_sources_review_and_activity() {
    let setup = bound();
    mark(&setup, "thinking", "build", "00000000000000000300");
    apply_mark_review(&setup.env, "build").unwrap();
    assert!(review_path(&setup, "build").exists());
    let cleared = apply_mark_clear(&setup.env, "build", "00000000000000000400").unwrap();
    assert_eq!(cleared.disposition, "applied");
    assert!(!review_path(&setup, "build").exists());
    let directory = setup.binding_dir("claude", "marks");
    let clear: Value =
        serde_json::from_slice(&fs::read(directory.join("activity-clear.json")).unwrap()).unwrap();
    assert_eq!(clear["observed_mono_ns"], "00000000000000000400");
    let again = apply_mark_clear(&setup.env, "build", "00000000000000000500").unwrap();
    assert_eq!(again.disposition, "skipped", "nothing of build's is left");
}

// One activity slot serves the whole binding. A source may withdraw only what
// it published, so clearing another writer's badge is refused by leaving it.
#[test]
fn mark_clear_leaves_another_writers_activity() {
    let setup = bound();
    setup.apply(
        &event("claude", "UserPromptSubmit", "marks", json!({})),
        "00000000000000000300",
    );
    let result = apply_mark_clear(&setup.env, "build", "00000000000000000400").unwrap();
    assert_eq!(result.disposition, "skipped");
    let directory = setup.binding_dir("claude", "marks");
    assert!(!directory.join("activity-clear.json").exists());
}

// Alt+B in the plugin writes the review owned by "user". The CLI cannot write
// or remove that review, or publish as that owner.
#[test]
fn the_user_source_is_reserved_for_the_plugin() {
    let setup = bound();
    for error in [
        apply_mark_activity(
            &setup.env,
            "notify",
            "user",
            None,
            None,
            None,
            "00000000000000000300",
            "00000000012345678900",
        )
        .unwrap_err(),
        apply_mark_review(&setup.env, "user").unwrap_err(),
        apply_mark_clear(&setup.env, "user", "00000000000000000300").unwrap_err(),
    ] {
        assert_eq!(error.diagnostic.code, "bad_usage");
    }
    apply_mark_review(&setup.env, "pi-bus").expect("pi-bus stays usable");
}

#[test]
fn cli_mark_clear_writes_the_activity_clear() {
    let setup = bound();
    mark(&setup, "notify", "build", "00000000000000000300");
    let output = rust_command(&setup)
        .args(["mark", "clear", "--source", "build", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["disposition"], "applied");
    assert!(
        setup
            .binding_dir("claude", "marks")
            .join("activity-clear.json")
            .exists()
    );
}

/// What the Rust reader reports for the launch's selected activity: its
/// availability and, when one is present, its source.
fn rust_activity(setup: &Setup) -> (RecordAvailability, Option<String>) {
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let root = state_root(&setup.env).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    let pointer: Option<Value> =
        fs::read(launch_path(&root, &address, launch_id).join("current-binding.json"))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap());
    let scope = PaneScope::new(
        address,
        launch_id.clone(),
        pointer.map(|pointer| pointer["binding_id"].as_str().unwrap().to_owned()),
    )
    .unwrap();
    let facts =
        read_pane_facts_with_ports(&root, &scope, &FileRecords, &setup.clock, None, None).unwrap();
    let source = facts
        .activity
        .record
        .as_ref()
        .filter(|_| facts.activity.availability == RecordAvailability::Present)
        .and_then(|record| record["source"].as_str().map(str::to_owned));
    (facts.activity.availability, source)
}

/// The line the plugin reader, run inside a real WezTerm, prints for this pane.
fn lua_activity(setup: &Setup) -> String {
    let (address, _) = pane_address(&setup.env).unwrap();
    let wire =
        json!({"wire":2,"address":address,"launch_id":setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]});
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let result = setup._scratch.0.join(format!("view-{}", Uuid::new_v4()));
    let wezterm = crate::executables::resolve("wezterm");
    let output = Command::new(&wezterm)
        .env_clear()
        .env("PATH", crate::executables::child_path(&[], &[&wezterm]))
        .env("WEZTERM_ATTENTION_DIR", &setup.env["WEZTERM_ATTENTION_DIR"])
        .env("WEZTERM_ATTENTION_TEST_ROOT", &root)
        .env("WEZTERM_ATTENTION_VIEW_RESULT", &result)
        .env("WEZTERM_ATTENTION_VIEW_NOW", setup.clock.unix)
        .env("WEZTERM_ATTENTION_VIEW_WIRE", wire.to_string())
        .arg("--config-file")
        .arg(root.join("tests/lua/support/read_attention_view.lua"))
        .args(["show-keys", "--lua"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let line = fs::read_to_string(result).unwrap();
    line.strip_prefix("ok ")
        .unwrap_or_else(|| panic!("{line}"))
        .trim_end()
        .to_owned()
}

fn launch_activity_path(setup: &Setup) -> PathBuf {
    let root = state_root(&setup.env).unwrap();
    let (address, _) = pane_address(&setup.env).unwrap();
    launch_path(&root, &address, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]).join("activity.json")
}

// Before any provider session binds, `mark` writes the launch's own activity.
// `mark clear` withdraws it from the same place, so neither reader shows it.
#[test]
fn mark_clear_withdraws_a_launch_activity_before_any_binding() {
    let setup = Setup::new();
    setup.claim();
    mark(&setup, "thinking", "build", "00000000000000000300");
    assert_eq!(
        rust_activity(&setup),
        (RecordAvailability::Present, Some("build".to_owned()))
    );
    assert_eq!(lua_activity(&setup), "activity=thinking source=build");
    let cleared = apply_mark_clear(&setup.env, "build", "00000000000000000400").unwrap();
    assert_eq!(cleared.disposition, "applied");
    assert!(!launch_activity_path(&setup).exists());
    assert_eq!(rust_activity(&setup), (RecordAvailability::Absent, None));
    assert_eq!(lua_activity(&setup), "activity=none source=none");
    let again = apply_mark_clear(&setup.env, "build", "00000000000000000500").unwrap();
    assert_eq!(again.disposition, "skipped", "nothing of build's is left");
}

// The launch has one activity slot too. A source clears only what it wrote.
#[test]
fn mark_clear_leaves_another_sources_launch_activity() {
    let setup = Setup::new();
    setup.claim();
    mark(&setup, "thinking", "build", "00000000000000000300");
    mark(&setup, "notify", "deploy", "00000000000000000350");
    let result = apply_mark_clear(&setup.env, "build", "00000000000000000400").unwrap();
    assert_eq!(result.disposition, "skipped");
    assert!(launch_activity_path(&setup).exists());
    assert_eq!(
        rust_activity(&setup),
        (RecordAvailability::Present, Some("deploy".to_owned()))
    );
    assert_eq!(lua_activity(&setup), "activity=notify source=deploy");
}

// Once a session binds, the activity lives with the binding and `mark clear`
// hides it with the binding's activity-clear watermark; both readers agree.
#[test]
fn both_readers_hide_a_bound_activity_its_source_cleared() {
    let setup = bound();
    mark(&setup, "notify", "build", "00000000000000000300");
    assert_eq!(lua_activity(&setup), "activity=notify source=build");
    apply_mark_clear(&setup.env, "build", "00000000000000000400").unwrap();
    assert_eq!(rust_activity(&setup), (RecordAvailability::Cleared, None));
    assert_eq!(lua_activity(&setup), "activity=none source=none");
    mark(&setup, "thinking", "deploy", "00000000000000000500");
    apply_mark_clear(&setup.env, "build", "00000000000000000600").unwrap();
    assert_eq!(
        rust_activity(&setup),
        (RecordAvailability::Present, Some("deploy".to_owned()))
    );
    assert_eq!(lua_activity(&setup), "activity=thinking source=deploy");
}
