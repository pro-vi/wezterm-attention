//! Retention of a binding the pane has moved on from: what the preview says
//! is what apply does, and an interrupted write does not pin a binding.

use super::*;

/// Ends session-a long ago and starts session-b, so session-a's binding is
/// old history. Returns session-a's binding directory.
fn old_history(setup: &Setup) -> PathBuf {
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
    setup.clock.set_unix(RETENTION_AGE_NS as u64 + 2);
    old_dir
}

fn retention_actions(details: &[Value]) -> Vec<Value> {
    details
        .iter()
        .filter(|detail| detail["kind"] == "binding_retention")
        .map(|detail| detail["action"].clone())
        .collect()
}

#[test]
fn the_retention_preview_says_keep_where_apply_would_keep() {
    let setup = Setup::new();
    let old_dir = old_history(&setup);
    fs::write(old_dir.join("notes.txt"), "keep me").expect("unknown file");
    let (preview, diagnostics) = setup.run_sweep(false, None);
    assert_eq!(retention_actions(&preview.details), [json!("keep")]);
    assert!(diagnostics.iter().any(|d| d.code == "record_invalid"));
    let (applied, _) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000921"));
    assert!(retention_actions(&applied.details).is_empty());
    assert!(old_dir.exists());
}

/// A crash between writing a record's temporary file and renaming it leaves
/// the temporary beside the record. It is part of the binding it sits in, and
/// goes with it.
#[test]
fn an_interrupted_write_does_not_block_retention() {
    let setup = Setup::new();
    let old_dir = old_history(&setup);
    fs::write(
        old_dir.join(".activity.json.00000000-0000-4000-8000-00000000abcd"),
        "{",
    )
    .expect("rust leftover");
    fs::write(old_dir.join("ack.json.session.tmp"), "{").expect("plugin leftover");
    let (preview, _) = setup.run_sweep(false, None);
    assert_eq!(retention_actions(&preview.details), [json!("prune")]);
    let (applied, _) = setup.run_sweep(true, Some("00000000-0000-4000-8000-000000000922"));
    assert_eq!(retention_actions(&applied.details), [json!("prune")]);
    assert!(!old_dir.exists());
}
