use super::*;
use wezterm_attention::observations::{AnsweredQuestion, LeadPrompts, PromptReference};
use wezterm_attention::query::{PaneFacts, PaneScope, read_pane_facts_with_ports};
use wezterm_attention::records::FileRecords;

const SESSION: &str = "prompts";

fn started(provider: &str) -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event(
            provider,
            "SessionStart",
            SESSION,
            json!({"source":"startup"}),
        ),
        "00000000000000000200",
    );
    setup
}

fn facts(setup: &Setup, provider: &str) -> PaneFacts {
    let launch_id = &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"];
    let scope = PaneScope::new(
        pane_address(&setup.env).unwrap().0,
        launch_id.clone(),
        Some(binding_id(provider, SESSION, launch_id)),
    )
    .unwrap();
    read_pane_facts_with_ports(
        &state_root(&setup.env).unwrap(),
        &scope,
        &FileRecords,
        &setup.clock,
        Some(&setup.panes),
        None,
    )
    .unwrap()
}

fn lifecycle(setup: &Setup, provider: &str) -> Value {
    serde_json::to_value(facts(setup, provider).lifecycle).unwrap()
}

fn prompt_record(setup: &Setup, provider: &str) -> Value {
    let path = setup.binding_dir(provider, SESSION).join("prompts.json");
    serde_json::from_slice(&fs::read(path).expect("prompts.json")).expect("prompts JSON")
}

fn prompt_ids(lifecycle: &Value) -> Vec<String> {
    lifecycle["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "prompt_submitted")
        .map(|item| item["observation_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_latest_prompt_outlives_its_eviction_from_the_general_pool() {
    let setup = started("claude");
    setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000300",
    );
    let prompt = prompt_ids(&lifecycle(&setup, "claude"));
    assert_eq!(prompt.len(), 1);
    for index in 0..70 {
        setup.apply(
            &event(
                "claude",
                "PreToolUse",
                SESSION,
                json!({"tool_name":"Bash","tool_use_id":format!("tool-{index}")}),
            ),
            &format!("{:020}", 400 + index),
        );
    }
    let view = lifecycle(&setup, "claude");
    assert!(
        prompt_ids(&view).is_empty(),
        "the general pool still holds the prompt"
    );
    assert!(view["retention_floors"]["lead_general"].is_string());
    assert_eq!(view["latest_prompt"]["observation_id"], json!(prompt[0]));
    assert_eq!(
        view["latest_prompt"]["observed_mono_ns"],
        "00000000000000000300"
    );
    let record = prompt_record(&setup, "claude");
    assert_eq!(record["latest"], view["latest_prompt"]);
    assert_eq!(record["answers"], json!([]));
}

#[test]
fn a_prompt_delivered_late_leaves_the_newer_latest_prompt() {
    let setup = started("claude");
    setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000500",
    );
    setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000400",
    );
    let view = lifecycle(&setup, "claude");
    assert_eq!(prompt_ids(&view).len(), 2);
    assert_eq!(
        prompt_record(&setup, "claude")["latest"]["observed_mono_ns"],
        "00000000000000000500"
    );
    assert_eq!(
        view["latest_prompt"]["observed_mono_ns"],
        "00000000000000000500"
    );
}

#[test]
fn an_invalid_prompt_record_is_moved_aside_and_started_again() {
    let setup = started("claude");
    let directory = setup.binding_dir("claude", SESSION);
    fs::write(directory.join("prompts.json"), b"{not json").unwrap();
    let result = setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000300",
    );
    let diagnostic = serde_json::to_value(result.diagnostic.expect("restart reported")).unwrap();
    assert_eq!(diagnostic["code"], "record_invalid");
    assert_eq!(
        diagnostic["message"],
        "an invalid prompt record was moved aside and started again"
    );
    assert_eq!(
        prompt_record(&setup, "claude")["latest"]["observed_mono_ns"],
        "00000000000000000300"
    );
    let aside = fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with(".prompts.json.invalid."))
        .count();
    assert_eq!(aside, 1);
}

#[test]
fn a_prompt_record_that_cannot_be_read_is_reported_and_the_prompt_still_recorded() {
    let setup = started("claude");
    let path = setup.binding_dir("claude", SESSION).join("prompts.json");
    fs::create_dir_all(&path).unwrap();
    let result = setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000300",
    );
    assert!(
        result.diagnostic.is_some(),
        "the unreadable record is reported"
    );
    assert!(path.is_dir(), "the record is left as it is");
    let view = lifecycle(&setup, "claude");
    assert_eq!(prompt_ids(&view).len(), 1);
    assert_eq!(view["availability"], "available");
    assert_eq!(
        view["latest_prompt"]["observed_mono_ns"],
        "00000000000000000300"
    );
}

#[test]
fn inspect_reports_an_unusable_prompt_record_and_keeps_the_pooled_prompt() {
    let setup = started("claude");
    setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000300",
    );
    let path = setup.binding_dir("claude", SESSION).join("prompts.json");
    let valid = prompt_record(&setup, "claude");
    let codes = |setup: &Setup| -> Vec<Value> {
        let view = lifecycle(setup, "claude");
        assert_eq!(view["availability"], "available");
        assert_eq!(
            view["latest_prompt"]["observed_mono_ns"],
            "00000000000000000300"
        );
        view["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic["code"].clone())
            .collect()
    };
    let mut invalid = valid.clone();
    invalid["answers"] = json!("none");
    fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert_eq!(codes(&setup), [json!("record_invalid")]);
    let mut newer = valid;
    newer["schema"] = json!(999);
    fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
    assert_eq!(codes(&setup), [json!("future_schema")]);
    // A record a newer writer wrote is left as it is.
    setup.apply(
        &event("claude", "UserPromptSubmit", SESSION, json!({})),
        "00000000000000000400",
    );
    assert_eq!(prompt_record(&setup, "claude")["schema"], 999);
}

/// Codex publishes question call `call` in turn `turn`.
fn ask(setup: &Setup, call: &str, turn: &str, at: u64) {
    setup.apply(
        &event(
            "codex",
            "PreToolUse",
            SESSION,
            json!({"tool_name":"request_user_input_async","tool_use_id":call,"turn_id":turn}),
        ),
        &format!("{at:020}"),
    );
    setup.apply(
        &event(
            "codex",
            "PostToolUse",
            SESSION,
            json!({"tool_name":"request_user_input_async","tool_use_id":call,"turn_id":turn,
                   "tool_response":"{\"accepted\":true}"}),
        ),
        &format!("{:020}", at + 1),
    );
}

/// The wrapper Codex 0.162.0 submits for an answer to question `index` of
/// call `call`.
fn reply(call: &str, index: u64, answer: &str) -> String {
    let id = json!(["request_user_input_async", call, index]).to_string();
    let items = json!([{"answer":answer,"question":"Which one?","questionItemId":id}]);
    format!("<send_user_message_question_reply>\n{items}\n</send_user_message_question_reply>")
}

/// Submits `prompt` with turn id `turn`.
fn submit_in(setup: &Setup, provider: &str, prompt: &str, turn: &str, at: u64) {
    setup.apply(
        &event(
            provider,
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":prompt,"turn_id":turn}),
        ),
        &format!("{at:020}"),
    );
}

/// Submits `prompt` as the start of turn `turn-<at>` and returns the latest
/// prompt's observation id.
fn submit(setup: &Setup, provider: &str, prompt: &str, at: u64) -> String {
    submit_in(setup, provider, prompt, &format!("turn-{at}"), at);
    let view = lifecycle(setup, provider);
    view["latest_prompt"]["observation_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn answers(setup: &Setup, call: &str) -> Value {
    lifecycle(setup, "codex")["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["correlation"]["tool_call_id"] == call)
        .expect("question group")["answer_observation_ids"]
        .clone()
}

#[test]
fn a_codex_answer_names_its_question_whatever_turn_it_arrives_in() {
    let setup = started("codex");
    ask(&setup, "call_question_a", "turn-a", 300);
    ask(&setup, "call_question_b", "turn-a", 310);
    assert_eq!(answers(&setup, "call_question_a"), json!([]));
    let answer = submit(&setup, "codex", &reply("call_question_a", 0, "yes"), 400);
    assert_eq!(answers(&setup, "call_question_a"), json!([answer]));
    assert_eq!(answers(&setup, "call_question_b"), json!([]));
    assert_eq!(
        prompt_record(&setup, "codex")["answers"],
        json!([{"tool_call_id":"call_question_a","question_index":0,"observation_id":answer,
                "observed_mono_ns":"00000000000000000400"}])
    );
}

// As of Codex 0.162.0 the submit hook for input typed while a turn runs
// carries that turn's id, and the general pool keeps one prompt per id. The
// prompt still becomes the latest prompt, and an answer in it still counts.
#[test]
fn a_prompt_typed_while_the_turn_runs_is_the_latest_prompt() {
    let setup = started("codex");
    let opening = submit(&setup, "codex", "start the work", 300);
    submit_in(&setup, "codex", "and also this", "turn-300", 400);
    let view = lifecycle(&setup, "codex");
    assert_eq!(
        prompt_ids(&view),
        std::slice::from_ref(&opening),
        "the pool keeps one prompt per turn"
    );
    assert_eq!(
        view["latest_prompt"]["observed_mono_ns"],
        "00000000000000000400"
    );
    assert_ne!(view["latest_prompt"]["observation_id"], json!(opening));
}

#[test]
fn an_answer_typed_while_the_turn_runs_still_names_its_question() {
    let setup = started("codex");
    submit(&setup, "codex", "start the work", 300);
    ask(&setup, "call_mid_turn", "turn-300", 310);
    submit_in(
        &setup,
        "codex",
        &reply("call_mid_turn", 0, "yes"),
        "turn-300",
        400,
    );
    let answered = prompt_record(&setup, "codex")["answers"].clone();
    assert_eq!(answered.as_array().unwrap().len(), 1);
    let answer = answered[0]["observation_id"].clone();
    assert_eq!(
        lifecycle(&setup, "codex")["latest_prompt"]["observation_id"],
        answer
    );
    assert_eq!(answers(&setup, "call_mid_turn"), json!([answer]));
    // The same answer delivered again is not a second answer.
    submit_in(
        &setup,
        "codex",
        &reply("call_mid_turn", 0, "yes"),
        "turn-300",
        410,
    );
    assert_eq!(prompt_record(&setup, "codex")["answers"], answered);
}

#[test]
fn each_answered_question_of_a_call_adds_its_prompt() {
    let setup = started("codex");
    ask(&setup, "call_two_questions", "turn-a", 300);
    let first = submit(&setup, "codex", &reply("call_two_questions", 0, "a"), 400);
    let second = submit(&setup, "codex", &reply("call_two_questions", 1, "b"), 500);
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(answers(&setup, "call_two_questions"), json!(expected));
}

#[test]
fn ide_context_before_the_wrapper_still_names_the_call() {
    let setup = started("codex");
    ask(&setup, "call_after_context", "turn-a", 300);
    let with_context = format!(
        "# Context from my IDE setup:\n\n## Open tabs:\n- main.rs\n\n## My request for Codex:\n{}\n",
        reply("call_after_context", 0, "yes")
    );
    let first = submit(&setup, "codex", &with_context, 400);
    assert_eq!(answers(&setup, "call_after_context"), json!([first]));
}

#[test]
fn text_that_is_not_a_codex_answer_answers_nothing() {
    let setup = started("codex");
    ask(&setup, "call_question", "turn-a", 300);
    let wrapped = reply("call_question", 0, "yes");
    let bare = format!(
        "<send_user_message_question_reply>\n{}\n</send_user_message_question_reply>",
        json!([{"answer":"no","question":"Q","questionItemId":"call_question"}])
    );
    for (at, prompt) in [
        // The wrapper is not what the prompt ends with.
        (400, format!("{wrapped}\nand one more thing")),
        // Its body is not a JSON array.
        (
            410,
            "<send_user_message_question_reply>{\"questionItemId\":\"call_question\"}</send_user_message_question_reply>"
                .to_owned(),
        ),
        // An id with a control character is not a label.
        (420, reply("call_question\u{7}", 0, "yes")),
        // A bare call id carries no question index.
        (430, bare),
    ] {
        submit(&setup, "codex", &prompt, at);
    }
    assert_eq!(answers(&setup, "call_question"), json!([]));
    assert_eq!(prompt_record(&setup, "codex")["answers"], json!([]));
    assert_eq!(prompt_ids(&lifecycle(&setup, "codex")).len(), 4);
    // Only a Codex prompt is read for answers.
    let claude = started("claude");
    submit(&claude, "claude", &wrapped, 400);
    assert_eq!(prompt_record(&claude, "claude")["answers"], json!([]));
}

#[test]
fn one_prompt_names_at_most_a_pool_of_questions_and_only_labels() {
    let limit = wezterm_attention::protocol::manifest()
        .unwrap()
        .limits
        .lifecycle_pool_max_count;
    let wrap = |items: Value| {
        format!("<send_user_message_question_reply>\n{items}\n</send_user_message_question_reply>")
    };
    let item = |call: &str, index: usize| {
        json!({"answer":"a","question":"q",
               "questionItemId":json!(["request_user_input_async", call, index]).to_string()})
    };
    let parse = |prompt: String| {
        event(
            "codex",
            "UserPromptSubmit",
            SESSION,
            json!({"prompt":prompt,"turn_id":"turn-a"}),
        )
        .answered_questions
    };
    let many = parse(wrap(Value::Array(
        (0..limit + 1)
            .map(|index| item("call_many", index))
            .collect(),
    )));
    assert_eq!(many.len(), limit);
    let other_tool =
        json!({"questionItemId":json!(["send_user_message_async", "call_x", 0]).to_string()});
    assert!(parse(wrap(json!([other_tool]))).is_empty());
    let beyond = json!({"questionItemId":json!(["request_user_input_async", "call_x", 1_000_001]).to_string()});
    assert!(parse(wrap(json!([beyond]))).is_empty());
    let long = "c".repeat(4096);
    assert!(parse(wrap(json!([item(&long, 0)]))).is_empty());
    let nested = format!("{}{}", "[".repeat(200), "]".repeat(200));
    assert!(parse(wrap(json!([{"questionItemId":nested}]))).is_empty());
    assert_eq!(
        parse(wrap(json!([item("call_one", 2)]))),
        [AnsweredQuestion {
            tool_call_id: "call_one".into(),
            question_index: 2,
        }]
    );
}

#[test]
fn no_answer_or_question_text_reaches_a_record() {
    let setup = started("codex");
    ask(&setup, "call_private", "turn-a", 300);
    let marker = "PRIVATE-ANSWER-SENTINEL-7f3a";
    submit(&setup, "codex", &reply("call_private", 0, marker), 400);
    assert_eq!(answers(&setup, "call_private").as_array().unwrap().len(), 1);
    let mut stack = vec![state_root(&setup.env).unwrap()];
    let mut files = 0;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else {
            files += 1;
            let bytes = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(marker),
                "{} holds the answer text",
                path.display()
            );
        }
    }
    assert!(files > 0);
}

#[test]
fn the_record_keeps_the_newest_answers() {
    let protocol = wezterm_attention::protocol::manifest().unwrap();
    let limit = protocol.limits.lifecycle_pool_max_count;
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/lifecycle/observations.json")).unwrap();
    let snapshot: LifecycleSnapshot = serde_json::from_value(
        fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "prompt_submitted")
            .unwrap()["value"]
            .clone(),
    )
    .unwrap();
    let template = snapshot.pools.general.observations[0].clone();
    let mut record = LeadPrompts::empty(
        snapshot.address.clone(),
        &snapshot.launch_id,
        &snapshot.binding_id,
        protocol.record_schema,
    );
    for index in 0..=limit {
        let mut prompt = template.clone();
        prompt.observation_id = Uuid::new_v4().to_string();
        prompt.observed_mono_ns = format!("{:020}", 1000 + index);
        let reference = PromptReference::of(&prompt);
        assert!(record.record_prompt(&reference));
        let answered = [AnsweredQuestion {
            tool_call_id: format!("call_{index}"),
            question_index: 0,
        }];
        assert!(record.record_answers(&reference, &answered, limit));
        // A second delivery of the same answer is not a second answer.
        assert!(!record.record_answers(&reference, &answered, limit));
    }
    assert_eq!(record.answers.len(), limit);
    assert_eq!(record.answers[0].tool_call_id, "call_1");
    assert_eq!(
        record.latest.as_ref().unwrap().observed_mono_ns,
        format!("{:020}", 1000 + limit)
    );
    assert_eq!(
        wezterm_attention::protocol::parse_record_value(&json!(record), protocol),
        wezterm_attention::protocol::Verdict::Valid
    );
}
