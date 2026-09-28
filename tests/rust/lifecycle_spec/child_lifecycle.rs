use super::child_presence::{SESSION, bound, child, lead, live};
use super::*;
use wezterm_attention::protocol::Disposition;

fn lead_path(setup: &Setup, provider: &str) -> PathBuf {
    setup.binding_dir(provider, SESSION).join("lifecycle.json")
}

fn children_path(setup: &Setup, provider: &str) -> PathBuf {
    setup
        .binding_dir(provider, SESSION)
        .join("children-lifecycle.json")
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("snapshot")).expect("snapshot JSON")
}

fn mono(value: u64) -> String {
    format!("{value:020}")
}

#[test]
fn a_leads_observation_goes_to_lifecycle_json_and_a_childs_to_its_own_file() {
    for provider in ["claude", "codex"] {
        let setup = bound(provider);
        setup.apply(&lead(provider, "PreToolUse"), &mono(300));
        let lead_bytes = fs::read(lead_path(&setup, provider)).expect("lead snapshot");
        assert!(!children_path(&setup, provider).exists(), "{provider}");
        setup.apply(
            &child(provider, "PreToolUse", "child-a", Some("Explore")),
            &mono(400),
        );
        assert_eq!(
            fs::read(lead_path(&setup, provider)).expect("lead snapshot"),
            lead_bytes,
            "{provider}: a child's observation changed the lead's snapshot"
        );
        let children = read_json(&children_path(&setup, provider));
        assert_eq!(children["kind"], "child_lifecycle_snapshot");
        let observations = children["pools"]["general"]["observations"]
            .as_array()
            .expect("general pool");
        assert_eq!(observations.len(), 1, "{provider}");
        assert_eq!(observations[0]["actor"]["agent_id"], "child-a");
    }
}

// However much its children record, the lead's newest observation stays, and
// its pool never gets a floor from their eviction.
#[test]
fn a_flood_of_child_observations_leaves_the_leads_newest_in_place() {
    let setup = bound("claude");
    let finished = setup.apply(&lead("claude", "Stop"), &mono(300));
    assert_eq!(finished.disposition, Disposition::Applied);
    for index in 0..200 {
        for (offset, name) in [(0, "PreToolUse"), (1, "PostToolUse")] {
            setup.apply(
                &child("claude", name, "child-a", Some("Explore")),
                &mono(1_000 + 2 * index + offset),
            );
        }
    }
    let lead = read_json(&lead_path(&setup, "claude"));
    let general = &lead["pools"]["general"];
    assert!(
        general.get("retention_floor_mono_ns").is_none(),
        "{general}"
    );
    assert!(
        general["observations"]
            .as_array()
            .expect("general pool")
            .iter()
            .any(
                |item| item["kind"] == "response_finished" && item["observed_mono_ns"] == mono(300)
            ),
        "{general}"
    );
    let children = read_json(&children_path(&setup, "claude"));
    assert_eq!(
        children["pools"]["general"]["observations"]
            .as_array()
            .expect("children's general pool")
            .len(),
        64
    );
    assert!(children["pools"]["general"]["retention_floor_mono_ns"].is_string());
}

// A `lifecycle.json` written before children had their own file holds both
// actors' observations. A child's next observation goes to the children's file
// and leaves the old one as it is: nothing is moved.
#[test]
fn a_childs_observation_leaves_an_older_lifecycle_json_as_it_is() {
    let setup = bound("claude");
    setup.apply(&lead("claude", "PreToolUse"), &mono(300));
    let path = lead_path(&setup, "claude");
    let mut snapshot = read_json(&path);
    let mut member = snapshot["pools"]["general"]["observations"][0].clone();
    member["observation_id"] = json!(Uuid::new_v4().to_string());
    member["observed_mono_ns"] = json!(mono(350));
    member["actor"] = json!({
        "kind":"child","agent_id":"child-a",
        "agent_key":wezterm_attention::protocol::sha256_hex(b"child-a")
    });
    snapshot["pools"]["general"]["observations"]
        .as_array_mut()
        .expect("general pool")
        .push(member);
    snapshot["pools"]["general"]["retention_floor_mono_ns"] = json!(mono(100));
    atomic_replace(&path, &snapshot).expect("older lifecycle.json");
    let before = fs::read(&path).expect("older lifecycle.json");
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(400),
    );
    assert_eq!(fs::read(&path).expect("lifecycle.json"), before);
    assert!(children_path(&setup, "claude").exists());
}

// The children's file is the only one a child's observation reads, and the
// lead's observations never read it.
#[test]
fn a_corrupt_childrens_file_rejects_only_the_childrens_observations() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(300),
    );
    let children = children_path(&setup, "claude");
    fs::write(&children, b"{not json").expect("corrupt the children's file");
    let led = setup.apply(&lead("claude", "PreToolUse"), &mono(400));
    assert_eq!(
        led.disposition,
        Disposition::Applied,
        "{:?}",
        led.diagnostic
    );
    let lead_bytes = fs::read(lead_path(&setup, "claude")).expect("lead snapshot");
    let rejected = setup.apply(
        &child("claude", "PreToolUse", "child-b", Some("Explore")),
        &mono(500),
    );
    assert_eq!(rejected.disposition, Disposition::Partial);
    assert_eq!(
        rejected.diagnostic.as_ref().map(|d| d.code.as_str()),
        Some("record_invalid")
    );
    assert_eq!(fs::read(&children).expect("children's file"), b"{not json");
    assert_eq!(
        fs::read(lead_path(&setup, "claude")).expect("lead snapshot"),
        lead_bytes
    );
    // The child is still counted: presence is its own record.
    assert!(live(&setup, "claude").iter().any(|(id, _)| id == "child-b"));
}

// A children's file is as large as a lead's may be, and no larger, whatever
// the general record bound allows.
#[test]
fn the_childrens_file_is_read_under_the_lifecycle_bound() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(300),
    );
    let path = children_path(&setup, "claude");
    let limits = &wezterm_attention::protocol::manifest().unwrap().limits;
    assert!(limits.lifecycle_max_json_bytes < limits.max_json_bytes);
    let mut padded = fs::read(&path).expect("children's file");
    padded.resize(limits.lifecycle_max_json_bytes + 1, b' ');
    fs::write(&path, &padded).expect("pad the children's file");
    let rejected = setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(400),
    );
    assert_eq!(rejected.disposition, Disposition::Partial);
    assert_eq!(fs::read(&path).expect("children's file"), padded);
}
