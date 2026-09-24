use super::*;

// Optional metadata describes an event; it is not the event. A field that
// fails its check is dropped and named in a diagnostic, and the event keeps
// its action, so a start, a stop or a badge is not lost to a long model name.
#[test]
fn a_bad_optional_field_is_dropped_and_the_event_kept() {
    let setup = Setup::new();
    setup.claim();
    let payload = payload(
        "claude",
        "SessionStart",
        "metadata",
        json!({"source":"startup","cwd":"relative/project","model":"m".repeat(300)}),
    );
    let mut environment = setup.env.clone();
    environment.insert("CLAUDE_CONFIG_DIR".to_owned(), "relative/claude".to_owned());
    let start = parse_provider_event("claude", "SessionStart", &payload, &environment);
    assert_eq!(start.action, ProviderAction::Binding);
    assert_eq!(start.cwd, None);
    assert_eq!(start.model, None);
    assert_eq!(start.config_dir, None);
    assert_eq!(start.transcript_path.as_deref(), Some("/tmp/session.jsonl"));
    let diagnostic = start
        .diagnostic
        .clone()
        .expect("the dropped fields are named");
    assert_eq!(diagnostic.code, "record_invalid");
    assert_eq!(
        diagnostic.context["dropped_fields"],
        json!(["cwd", "CLAUDE_CONFIG_DIR", "model"])
    );
    let applied = apply_provider_event(&start, &setup.env, "00000000000000000200", &setup.ports())
        .expect("binding applies");
    assert_eq!(applied.disposition, "applied");
    assert_eq!(
        applied
            .diagnostic
            .map(|item| item.context["dropped_fields"].clone()),
        Some(json!(["cwd", "CLAUDE_CONFIG_DIR", "model"]))
    );
    let binding: Value = serde_json::from_slice(
        &fs::read(setup.binding_dir("claude", "metadata").join("binding.json")).unwrap(),
    )
    .unwrap();
    assert!(binding.get("cwd").is_none());
    assert!(binding.get("model").is_none());

    let stop = event(
        "codex",
        "Stop",
        "metadata",
        json!({"transcript_path":"relative.jsonl","stop_hook_active":false}),
    );
    assert_eq!(stop.action, ProviderAction::ParentStop);
    assert_eq!(stop.transcript_path, None);

    let badge = event(
        "pi",
        "bus",
        "metadata",
        json!({"state":"notify","label":"line\nbreak"}),
    );
    assert_eq!(badge.action, ProviderAction::Activity);
    assert_eq!(badge.activity_type.as_deref(), Some("notify"));
    assert_eq!(badge.label, None);
    assert_eq!(
        badge
            .diagnostic
            .map(|item| item.context["dropped_fields"].clone()),
        Some(json!(["label"]))
    );

    let clean = event("claude", "Stop", "metadata", json!({}));
    assert!(clean.diagnostic.is_none());
}

// Providers add enum values faster than the manifest does (Claude 2.1.280
// already sends error "verification_required"). A value this writer does not
// know keeps the observation: it reads as "unknown" where the vocabulary has
// that word, and is left out where it does not. A known value that another
// provider owns is a contradiction, not news, and is still refused.
#[test]
fn an_unknown_native_enum_value_keeps_the_observation() {
    let snapshot_json = |value: &ProviderEvent| {
        serde_json::to_value(value.observation.as_ref().expect("observation kept")).unwrap()
    };
    let failure = event(
        "claude",
        "StopFailure",
        "enums",
        json!({"error":"verification_required"}),
    );
    assert!(failure.observation_diagnostic.is_none());
    assert_eq!(snapshot_json(&failure)["error_category"], "unknown");

    let known = event(
        "claude",
        "StopFailure",
        "enums",
        json!({"error":"rate_limit"}),
    );
    assert_eq!(snapshot_json(&known)["error_category"], "rate_limit");

    let input = event("pi", "input", "enums", json!({"source":"voice"}));
    assert!(input.observation_diagnostic.is_none());
    assert!(snapshot_json(&input).get("input_source").is_none());

    for (provider, name, field, value) in [
        ("claude", "PreCompact", "trigger", "scheduled"),
        ("pi", "session_compact", "reason", "idle"),
    ] {
        let compaction = event(provider, name, "enums", json!({ field: value }));
        assert!(
            compaction.observation_diagnostic.is_none(),
            "{provider} {value}"
        );
        assert!(snapshot_json(&compaction).get("trigger").is_none());
    }
    let foreign = event(
        "claude",
        "PreCompact",
        "enums",
        json!({"trigger":"threshold"}),
    );
    assert!(foreign.observation_diagnostic.is_some());
}
