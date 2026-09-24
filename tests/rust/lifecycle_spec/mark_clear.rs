use super::*;
use wezterm_attention::lifecycle::apply_mark_clear;

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
    apply_mark_review(&setup.env, "build", false).unwrap();
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
        apply_mark_review(&setup.env, "user", false).unwrap_err(),
        apply_mark_review(&setup.env, "user", true).unwrap_err(),
        apply_mark_clear(&setup.env, "user", "00000000000000000300").unwrap_err(),
    ] {
        assert_eq!(error.diagnostic.code, "bad_usage");
    }
    apply_mark_review(&setup.env, "pi-bus", false).expect("pi-bus stays usable");
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
