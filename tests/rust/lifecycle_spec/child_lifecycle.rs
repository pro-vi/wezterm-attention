use super::child_presence::{SESSION, bound, child, lead, live};
use super::*;
use wezterm_attention::observations::LifecycleView;
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
fn more_child_observations_than_a_pool_holds_leave_the_leads_newest_in_place() {
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

/// A generic Codex tool call observed at `at`, by the lead, or by the child
/// `agent`, correlated by the tool call id `call`.
fn tool_call(id: u64, at: u64, agent: Option<&str>, call: &str) -> Value {
    let actor = match agent {
        Some(agent) => json!({
            "kind":"child","agent_id":agent,
            "agent_key":wezterm_attention::protocol::sha256_hex(agent.as_bytes())
        }),
        None => json!({"kind":"lead"}),
    };
    json!({
        "kind":"tool_preflight","tool_name":"shell","tool_class":"generic",
        "observation_id":format!("00000000-0000-4000-8000-{id:012}"),
        "source_event":"PreToolUse","observed_mono_ns":mono(at),
        "written_at_unix_ns":"00000000000000001000","actor":actor,
        "correlation":{"tool_call_id":call}
    })
}

/// A valid snapshot of `kind` holding `general`, with the given pool floors.
fn snapshot_of(
    kind: &str,
    general: Vec<Value>,
    general_floor: Option<u64>,
    requests_floor: Option<u64>,
) -> LifecycleSnapshot {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/lifecycle/observations.json")).unwrap();
    let mut value = fixture["cases"][1]["value"].clone();
    value["kind"] = json!(kind);
    value["pools"]["general"] = json!({"observations":general});
    value["pools"]["requests"] = json!({"observations":[]});
    for (pool, floor) in [("general", general_floor), ("requests", requests_floor)] {
        if let Some(floor) = floor {
            value["pools"][pool]["retention_floor_mono_ns"] = json!(mono(floor));
        }
    }
    let protocol = wezterm_attention::protocol::manifest().unwrap();
    assert_eq!(
        wezterm_attention::protocol::parse_record_value(&value, protocol).as_str(),
        "valid",
        "{value}"
    );
    serde_json::from_value(value).unwrap()
}

fn lead_snapshot(general: Vec<Value>, general_floor: Option<u64>) -> LifecycleSnapshot {
    snapshot_of("lifecycle_snapshot", general, general_floor, None)
}

fn children_snapshot(general: Vec<Value>, general_floor: Option<u64>) -> LifecycleSnapshot {
    snapshot_of("child_lifecycle_snapshot", general, general_floor, None)
}

/// How both readers must assemble one binding's two lifecycle files, by what
/// the case holds: the lead's snapshot, the children's, or both. The parity
/// test runs every one through the installed Lua reader too.
pub(super) fn two_file_cases() -> Vec<(
    &'static str,
    Option<LifecycleSnapshot>,
    Option<LifecycleSnapshot>,
)> {
    let lead = || tool_call(1, 50, None, "m");
    let old_copy = || tool_call(2, 100, Some("child-a"), "k");
    let mut differing = tool_call(5, 100, Some("child-a"), "k");
    differing["source_version"] = json!("another");
    vec![
        (
            "an old child copy under a later one in the children's file",
            Some(lead_snapshot(vec![lead(), old_copy()], None)),
            Some(children_snapshot(
                vec![tool_call(3, 200, Some("child-a"), "k")],
                None,
            )),
        ),
        (
            "an old child copy later than the children's",
            Some(lead_snapshot(
                vec![lead(), tool_call(2, 300, Some("child-a"), "k")],
                None,
            )),
            Some(children_snapshot(
                vec![tool_call(3, 200, Some("child-a"), "k")],
                None,
            )),
        ),
        (
            "one child observation in both files, identical",
            Some(lead_snapshot(
                vec![lead(), tool_call(6, 100, Some("child-a"), "k")],
                None,
            )),
            Some(children_snapshot(
                vec![tool_call(6, 100, Some("child-a"), "k")],
                None,
            )),
        ),
        (
            "an old child copy at or below the children's floor",
            Some(lead_snapshot(vec![lead(), old_copy()], None)),
            Some(children_snapshot(
                vec![tool_call(4, 300, Some("child-b"), "d")],
                Some(150),
            )),
        ),
        (
            "an old child copy exactly at the children's floor",
            Some(lead_snapshot(vec![lead(), old_copy()], None)),
            Some(children_snapshot(
                vec![tool_call(4, 300, Some("child-b"), "d")],
                Some(100),
            )),
        ),
        (
            "one child observation at one instant with different content",
            Some(lead_snapshot(vec![lead(), old_copy()], None)),
            Some(children_snapshot(vec![differing], None)),
        ),
        (
            "one id naming a lead observation and a child's",
            Some(lead_snapshot(vec![lead()], None)),
            Some(children_snapshot(
                vec![tool_call(1, 400, Some("child-a"), "e")],
                None,
            )),
        ),
        (
            "one id naming an old child observation and a later, different one",
            Some(lead_snapshot(vec![lead(), old_copy()], None)),
            Some(children_snapshot(
                vec![tool_call(2, 200, Some("child-a"), "other")],
                None,
            )),
        ),
        (
            "an old lifecycle file holding children's observations and a floor",
            Some(lead_snapshot(vec![lead(), old_copy()], Some(20))),
            None,
        ),
        (
            "only the children's file",
            None,
            Some(children_snapshot(
                vec![tool_call(3, 200, Some("child-a"), "k")],
                None,
            )),
        ),
        (
            "request floors in both files",
            Some(snapshot_of(
                "lifecycle_snapshot",
                vec![lead()],
                None,
                Some(5),
            )),
            Some(snapshot_of(
                "child_lifecycle_snapshot",
                vec![tool_call(3, 200, Some("child-a"), "k")],
                None,
                Some(9),
            )),
        ),
        (
            "more conflicts than the facet reports",
            Some(lead_snapshot(
                (1..=9)
                    .map(|id| tool_call(id, 10 * id, None, &format!("lead-{id}")))
                    .collect(),
                None,
            )),
            Some(children_snapshot(
                (1..=9)
                    .map(|id| tool_call(id, 10 * id + 1, Some("child-a"), &format!("child-{id}")))
                    .collect(),
                None,
            )),
        ),
        (
            "a request floor in the children's file only",
            Some(lead_snapshot(vec![lead()], None)),
            Some(snapshot_of(
                "child_lifecycle_snapshot",
                vec![tool_call(3, 200, Some("child-a"), "k")],
                None,
                Some(9),
            )),
        ),
    ]
}

/// What the tests compare of the view assembled from a named case's files.
struct Assembled {
    shown: Vec<(String, String)>,
    floors: BTreeMap<String, String>,
    diagnostics: Vec<String>,
    snapshot_id: Option<String>,
}

fn assembled(case: &str) -> Assembled {
    let (_, lead, children) = two_file_cases()
        .into_iter()
        .find(|(name, _, _)| *name == case)
        .expect("named case");
    let view = LifecycleView::assemble(
        lead.as_ref(),
        children.as_ref(),
        Some("99999999999999999999"),
        vec![],
    );
    Assembled {
        shown: view
            .observations
            .iter()
            .map(|pooled| {
                (
                    pooled.observation.observation_id[24..].to_owned(),
                    pooled.pool.clone(),
                )
            })
            .collect(),
        floors: view.retention_floors.into_iter().collect(),
        diagnostics: view
            .diagnostics
            .into_iter()
            .map(|d| d.code.to_string())
            .collect(),
        snapshot_id: view.snapshot_id,
    }
}

fn shown_as(pairs: &[(u64, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(id, pool)| (format!("{id:012}"), (*pool).to_owned()))
        .collect()
}

#[test]
fn a_later_copy_in_the_childrens_file_replaces_an_old_one() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("an old child copy under a later one in the children's file");
    assert_eq!(shown, shown_as(&[(1, "general"), (3, "child_general")]));
    assert!(diagnostics.is_empty());
}

#[test]
fn an_old_child_copy_later_than_the_childrens_stands() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("an old child copy later than the children's");
    assert_eq!(shown, shown_as(&[(1, "general"), (2, "general")]));
    assert!(diagnostics.is_empty());
}

#[test]
fn one_observation_in_both_files_is_shown_once_from_the_childrens() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("one child observation in both files, identical");
    assert_eq!(shown, shown_as(&[(1, "general"), (6, "child_general")]));
    assert!(diagnostics.is_empty());
}

#[test]
fn an_old_child_copy_exactly_at_the_childrens_floor_is_dropped() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("an old child copy exactly at the children's floor");
    assert_eq!(shown, shown_as(&[(1, "general"), (4, "child_general")]));
    assert!(diagnostics.is_empty());
}

#[test]
fn an_old_child_copy_the_childrens_floor_fences_is_dropped() {
    let Assembled {
        shown,
        floors,
        diagnostics,
        ..
    } = assembled("an old child copy at or below the children's floor");
    assert_eq!(shown, shown_as(&[(1, "general"), (4, "child_general")]));
    assert_eq!(
        floors.get("child_general").map(String::as_str),
        Some(mono(150).as_str())
    );
    assert_eq!(floors.get("general"), floors.get("child_general"));
    assert!(!floors.contains_key("lead_general"));
    assert!(diagnostics.is_empty());
}

#[test]
fn two_copies_of_one_child_observation_that_differ_are_reported() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("one child observation at one instant with different content");
    assert_eq!(shown, shown_as(&[(1, "general"), (5, "child_general")]));
    assert_eq!(diagnostics, ["record_invalid"]);
}

#[test]
fn a_childs_observation_never_displaces_the_leads_with_the_same_id() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("one id naming a lead observation and a child's");
    assert_eq!(shown, shown_as(&[(1, "general")]));
    assert_eq!(diagnostics, ["record_invalid"]);
}

#[test]
fn an_old_child_observation_keeps_its_id_over_the_childrens_file() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("one id naming an old child observation and a later, different one");
    assert_eq!(shown, shown_as(&[(1, "general"), (2, "general")]));
    assert_eq!(diagnostics, ["record_invalid"]);
}

#[test]
fn an_old_lifecycle_file_reads_as_before_with_its_floor_named_as_the_leads() {
    let Assembled {
        shown,
        floors,
        diagnostics,
        snapshot_id,
        ..
    } = assembled("an old lifecycle file holding children's observations and a floor");
    assert_eq!(shown, shown_as(&[(1, "general"), (2, "general")]));
    assert_eq!(
        floors,
        BTreeMap::from([
            ("general".to_owned(), mono(20)),
            ("lead_general".to_owned(), mono(20)),
        ])
    );
    assert!(diagnostics.is_empty());
    assert!(snapshot_id.is_some());
}

#[test]
fn the_childrens_file_alone_is_shown_and_names_no_snapshot() {
    let Assembled {
        shown,
        diagnostics,
        snapshot_id,
        ..
    } = assembled("only the children's file");
    assert_eq!(shown, shown_as(&[(3, "child_general")]));
    assert!(diagnostics.is_empty());
    assert_eq!(snapshot_id, None, "the snapshot id is the lead's file's");
}

// `requests` still says that some request evidence was evicted, whichever
// file evicted it; a consumer that asks about the lead reads `lead_requests`.
#[test]
fn an_aggregate_floor_is_the_later_of_the_two_files() {
    let Assembled { floors, .. } = assembled("request floors in both files");
    assert_eq!(floors.get("lead_requests"), Some(&mono(5)));
    assert_eq!(floors.get("child_requests"), Some(&mono(9)));
    assert_eq!(floors.get("requests"), Some(&mono(9)));
    let Assembled { floors, .. } = assembled("a request floor in the children's file only");
    assert_eq!(floors.get("requests"), Some(&mono(9)));
    assert!(!floors.contains_key("lead_requests"));
}

#[test]
fn conflicts_are_reported_within_the_facets_eight_diagnostics() {
    let Assembled {
        shown, diagnostics, ..
    } = assembled("more conflicts than the facet reports");
    assert_eq!(shown.len(), 9);
    assert!(shown.iter().all(|(_, pool)| pool == "general"));
    assert_eq!(diagnostics, ["record_invalid"; 8]);
}

// A provider that runs no sub-agents has no children's file to read, and a
// stray one changes nothing, as in the plugin.
#[test]
fn inspect_reads_no_childrens_file_for_a_provider_without_sub_agents() {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            "pi",
            "session_start",
            SESSION,
            json!({"start_source":"startup"}),
        ),
        &mono(200),
    );
    let children = children_path(&setup, "pi");
    fs::create_dir_all(children.parent().unwrap()).unwrap();
    fs::write(&children, b"{not json").unwrap();
    let lifecycle = super::child_presence::facts(&setup, "pi").lifecycle;
    assert!(
        lifecycle.diagnostics.is_empty(),
        "{:?}",
        lifecycle.diagnostics
    );
    assert_ne!(
        lifecycle.availability,
        wezterm_attention::observations::LifecycleAvailability::Invalid
    );
}

// What `attention inspect` shows once a child has recorded more than its
// pool holds.
#[test]
fn inspect_keeps_the_leads_newest_past_more_child_observations_than_a_pool_holds() {
    let setup = bound("claude");
    setup.apply(&lead("claude", "Stop"), &mono(300));
    for index in 0..100 {
        setup.apply(
            &child("claude", "PreToolUse", "child-a", Some("Explore")),
            &mono(1_000 + index),
        );
    }
    let lifecycle = super::child_presence::facts(&setup, "claude").lifecycle;
    assert_eq!(
        lifecycle.availability,
        wezterm_attention::observations::LifecycleAvailability::Available
    );
    assert!(
        lifecycle
            .observations
            .iter()
            .any(|pooled| pooled.pool == "general"
                && pooled.observation.observed_mono_ns == mono(300))
    );
    assert_eq!(
        lifecycle
            .observations
            .iter()
            .filter(|pooled| pooled.pool == "child_general")
            .count(),
        64
    );
    assert!(lifecycle.retention_floors.contains_key("child_general"));
    assert!(lifecycle.retention_floors.contains_key("general"));
    assert!(!lifecycle.retention_floors.contains_key("lead_general"));
}

#[test]
fn inspect_shows_a_binding_whose_only_lifecycle_evidence_is_its_childrens() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(300),
    );
    let _ = fs::remove_file(lead_path(&setup, "claude"));
    let lifecycle = super::child_presence::facts(&setup, "claude").lifecycle;
    assert_eq!(
        lifecycle.availability,
        wezterm_attention::observations::LifecycleAvailability::Available
    );
    assert_eq!(lifecycle.observations.len(), 1);
    assert_eq!(lifecycle.observations[0].pool, "child_general");
}

#[test]
fn inspect_shows_the_leads_evidence_past_a_corrupt_childrens_file_and_says_so() {
    let setup = bound("claude");
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(300),
    );
    fs::write(children_path(&setup, "claude"), b"{not json").unwrap();
    setup.apply(&lead("claude", "PreToolUse"), &mono(400));
    let lifecycle = super::child_presence::facts(&setup, "claude").lifecycle;
    assert_eq!(
        lifecycle.availability,
        wezterm_attention::observations::LifecycleAvailability::Available
    );
    assert_eq!(lifecycle.observations.len(), 1);
    assert_eq!(lifecycle.observations[0].pool, "general");
    assert_eq!(
        lifecycle.diagnostics.len(),
        1,
        "{:?}",
        lifecycle.diagnostics
    );
    assert_eq!(lifecycle.diagnostics[0].code, DiagnosticCode::RecordInvalid);
}

#[test]
fn inspect_shows_no_childrens_evidence_while_the_leads_file_is_corrupt() {
    let setup = bound("claude");
    setup.apply(&lead("claude", "PreToolUse"), &mono(300));
    setup.apply(
        &child("claude", "PreToolUse", "child-a", Some("Explore")),
        &mono(400),
    );
    fs::write(lead_path(&setup, "claude"), b"{not json").unwrap();
    let lifecycle = super::child_presence::facts(&setup, "claude").lifecycle;
    assert_eq!(
        lifecycle.availability,
        wezterm_attention::observations::LifecycleAvailability::Invalid
    );
    assert!(lifecycle.observations.is_empty());
}
