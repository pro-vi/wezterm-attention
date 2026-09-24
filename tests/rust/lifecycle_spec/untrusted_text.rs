use super::*;
use wezterm_attention::protocol::validate_record;

// U+0085 (NEL), U+009B (CSI) and U+009F (APC) are C1 controls: a terminal acts
// on each of them alone, so none may be stored where a reader prints it.
const C1_SAMPLES: [char; 4] = ['\u{80}', '\u{85}', '\u{9b}', '\u{9f}'];

#[test]
fn c1_controls_are_refused_by_every_writer_text_check() {
    for control in C1_SAMPLES {
        let session = event(
            "claude",
            "SessionStart",
            &format!("session{control}"),
            json!({"source":"startup"}),
        );
        assert_eq!(session.action, ProviderAction::Ignored, "{control:?}");
        assert_eq!(
            session.diagnostic.as_ref().map(|item| item.code.as_str()),
            Some("record_invalid")
        );

        let facts = event(
            "claude",
            "SessionStart",
            "facts",
            json!({
                "source":"startup",
                "cwd": format!("/tmp/project{control}"),
                "transcript_path": format!("/tmp/{control}.jsonl"),
                "model": format!("model{control}"),
            }),
        );
        assert_eq!(facts.cwd, None, "{control:?}");
        assert_eq!(facts.transcript_path, None, "{control:?}");
        assert_eq!(facts.model, None, "{control:?}");

        let child = event(
            "claude",
            "PreToolUse",
            "facts",
            json!({"tool_name":"Bash","agent_id":format!("child{control}")}),
        );
        assert_eq!(child.action, ProviderAction::Ignored, "{control:?}");

        let config = parse_provider_event(
            "codex",
            "SessionStart",
            &payload(
                "codex",
                "SessionStart",
                "facts",
                json!({"source":"startup"}),
            ),
            &BTreeMap::from([("CODEX_HOME".to_owned(), format!("/tmp/codex{control}"))]),
        );
        assert_eq!(config.config_dir, None, "{control:?}");

        let setup = Setup::new();
        setup.claim();
        let label = format!("ready{control}");
        let mark = apply_mark_activity(
            &setup.env,
            "notify",
            "manual",
            None,
            Some(&label),
            None,
            "00000000000000000200",
            "00000000012345678900",
        )
        .expect_err("a C1 label is refused");
        assert_eq!(mark.diagnostic.code, "bad_usage");
        let review = apply_mark_review(&setup.env, &format!("owner{control}"), false)
            .expect_err("a C1 source is refused");
        assert_eq!(review.diagnostic.code, "bad_usage");

        let mut environment = setup.env.clone();
        environment.insert(
            "WEZTERM_ATTENTION_DIR".to_owned(),
            format!("/tmp/state{control}"),
        );
        assert!(state_root(&environment).is_err(), "{control:?}");

        let fixture: Value =
            serde_json::from_str(include_str!("../../fixtures/v2/protocol-cases.json")).unwrap();
        let mut activity = fixture["record_samples"]["activity"].clone();
        activity["label"] = json!(format!("ready{control}"));
        assert!(validate_record(&activity, Some("activity")).is_err());
    }
}

#[test]
fn the_first_character_after_c1_is_ordinary_text() {
    let parsed = event(
        "claude",
        "SessionStart",
        "session\u{a0}a",
        json!({"source":"startup","cwd":"/tmp/caf\u{e9}\u{a0}project","model":"model\u{a0}a"}),
    );
    assert_eq!(parsed.action, ProviderAction::Binding);
    assert_eq!(parsed.cwd.as_deref(), Some("/tmp/caf\u{e9}\u{a0}project"));
    assert_eq!(parsed.model.as_deref(), Some("model\u{a0}a"));
}
