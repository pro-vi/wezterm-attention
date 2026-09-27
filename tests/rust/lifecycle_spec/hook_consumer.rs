use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;
use wezterm_attention::consumer::{self, DeliveryEffect, DeliveryStage};
use wezterm_attention::hook_content::HookContent;
use wezterm_attention::lifecycle::apply_provider_event_with_outcome;
use wezterm_attention::lifecycle::outcome::Persistence;

fn bind(setup: &Setup, provider: &str) {
    setup.claim();
    setup.apply(
        &event(
            provider,
            if provider == "pi" {
                "session_start"
            } else {
                "SessionStart"
            },
            "consumer-session",
            if provider == "pi" {
                json!({"start_source":"startup"})
            } else {
                json!({"source":"startup"})
            },
        ),
        "00000000000000000200",
    );
}

fn executable(setup: &Setup, name: &str, body: &str) -> PathBuf {
    let path = setup._scratch.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn hook(
    setup: &Setup,
    provider: &str,
    payload: &Value,
    consumers: &[PathBuf],
    extra: &[&str],
) -> std::process::Output {
    let mut command = rust_command(setup);
    command.args([
        "hooks",
        "event",
        provider,
        payload["hook_event_name"].as_str().unwrap(),
        "--debug",
    ]);
    for consumer in consumers {
        command.arg("--consumer").arg(consumer);
    }
    command
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(payload).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn assert_no_content(root: &Path, text: &str) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_content(&path, text);
        } else {
            assert!(!String::from_utf8_lossy(&fs::read(path).unwrap()).contains(text));
        }
    }
}

#[test]
fn admitted_content_is_transient_scoped_and_runs_after_locks_release() {
    for (provider, name, field, content, flag) in [
        (
            "claude",
            "Stop",
            "last_assistant_message",
            "reply",
            "--include-reply",
        ),
        (
            "codex",
            "Stop",
            "last_assistant_message",
            "reply",
            "--include-reply",
        ),
        (
            "claude",
            "UserPromptSubmit",
            "prompt",
            "prompt",
            "--include-prompt",
        ),
        (
            "codex",
            "UserPromptSubmit",
            "prompt",
            "prompt",
            "--include-prompt",
        ),
    ] {
        let setup = Setup::new();
        bind(&setup, provider);
        let sink = setup._scratch.0.join("delivery.json");
        let consumer = executable(
            &setup,
            "consumer",
            &format!(
                "set -e\n/bin/cat > '{}'\n'{}' mark thinking --json >/dev/null\nprintf 'CHILD-OUTPUT-MUST-NOT-LEAK'\nprintf 'CHILD-ERROR-MUST-NOT-LEAK' >&2",
                sink.display(),
                env!("CARGO_BIN_EXE_attention")
            ),
        );
        let text = "SYNTHETIC-CONTENT-ONLY\nUnicode 中文 🧪\n";
        let payload = json!({"hook_event_name":name, "session_id":"consumer-session", field:text});
        let output = hook(
            &setup,
            provider,
            &payload,
            &[consumer],
            &[flag, "--consumer-timeout-ms", "10000", "--strict"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let diagnostics = String::from_utf8(output.stderr).unwrap();
        assert!(
            !diagnostics.contains("SYNTHETIC-CONTENT-ONLY")
                && !diagnostics.contains("CHILD-OUTPUT")
                && !diagnostics.contains("CHILD-ERROR")
        );
        let delivery: Value = serde_json::from_slice(&fs::read(&sink).unwrap()).unwrap();
        assert_eq!(
            delivery[content],
            json!({"availability":"available", "text":text})
        );
        assert_eq!(
            delivery["scope"]["launch_id"],
            setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]
        );
        assert_eq!(
            delivery["scope"]["target"]["binding_id"],
            binding_id(
                provider,
                "consumer-session",
                &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]
            )
        );
        assert_eq!(
            delivery["persistence"],
            json!({"native_state":"confirmed", "activity":"confirmed", "compatibility":"not_requested", "lifecycle":"confirmed"})
        );
        assert!(delivery.get("observation_id").is_some());
        assert!(delivery.get("correlation").is_none());
        let other = if content == "reply" {
            "prompt"
        } else {
            "reply"
        };
        assert_eq!(delivery[other], json!({"availability":"not_requested"}));
        assert_no_content(&state_root(&setup.env).unwrap(), "SYNTHETIC-CONTENT-ONLY");
        let diagnostics: Value = serde_json::from_str(&diagnostics).unwrap();
        assert_eq!(diagnostics["result"]["consumers"][0]["stage"], "completed");
        assert_eq!(diagnostics["result"]["consumers"][0]["effect"], "possible");
    }
}

#[test]
fn consumer_startup_and_exit_failures_preserve_facts_and_continue() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let missing = setup._scratch.0.join("missing");
    let denied = executable(&setup, "denied", "exit 0");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o600)).unwrap();
    let invalid = executable(&setup, "invalid", "");
    fs::write(&invalid, "invalid executable bytes").unwrap();
    let interpreter = executable(&setup, "interpreter", "");
    fs::write(&interpreter, "#!/nonexistent-synthetic-interpreter\n").unwrap();
    let failed = executable(&setup, "failed", "/bin/cat >/dev/null\nexit 7");
    let sink = setup._scratch.0.join("sink");
    let successful = executable(
        &setup,
        "successful",
        &format!("/bin/cat > '{}'", sink.display()),
    );
    let output = hook(
        &setup,
        "claude",
        &json!({"hook_event_name":"Stop","session_id":"consumer-session"}),
        &[missing, denied, invalid, interpreter, failed, successful],
        &["--consumer-timeout-ms", "10000", "--strict"],
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stderr).unwrap();
    let outcomes = report["result"]["consumers"].as_array().unwrap();
    for outcome in &outcomes[..4] {
        assert_eq!(outcome["stage"], "not_started");
        assert_eq!(outcome["effect"], "none");
        assert!(outcome.get("exit_code").is_none());
    }
    assert_eq!(outcomes[4]["stage"], "failed");
    assert_eq!(outcomes[4]["exit_code"], 7);
    assert_eq!(outcomes[5]["stage"], "completed");
    assert_eq!(report["result"]["persistence"]["lifecycle"], "confirmed");
    assert!(
        sink.exists()
            && setup
                .binding_dir("claude", "consumer-session")
                .join("lifecycle.json")
                .exists()
    );
}

#[test]
fn consumer_deadline_covers_blocked_stdin_and_child_exit() {
    let setup = Setup::new();
    let never_reads = executable(&setup, "never-reads", "exec /bin/sleep 5");
    let started = Instant::now();
    let outcome = consumer::dispatch(
        never_reads.to_str().unwrap(),
        &vec![b'x'; 1024 * 1024],
        Duration::from_millis(60),
    );
    assert_eq!(outcome.stage, DeliveryStage::TimedOut);
    assert_eq!(outcome.effect, DeliveryEffect::Possible);
    assert!(outcome.exit_code.is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
    let closes_pipe = executable(&setup, "closed-stdin", "exec 0<&-\nexec /bin/sleep 60");
    let outcome = consumer::dispatch(
        closes_pipe.to_str().unwrap(),
        &vec![b'x'; 1024 * 1024],
        Duration::from_secs(10),
    );
    assert_eq!(outcome.stage, DeliveryStage::StdinFailed);
    assert_eq!(outcome.effect, DeliveryEffect::Possible);
}

#[test]
fn reply_availability_and_optionality_are_exact() {
    let setup = Setup::new();
    bind(&setup, "codex");
    let event = event("codex", "Stop", "consumer-session", json!({}));
    let outcome = apply_provider_event_with_outcome(
        &event,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    for (payload, requested, expected) in [
        (
            json!({"last_assistant_message":"hidden"}),
            false,
            json!({"availability":"not_requested"}),
        ),
        (json!({}), true, json!({"availability":"absent"})),
        // Codex declares the field nullable and sends null for a turn that
        // ended without final text, which is a normal turn, not bad input.
        (
            json!({"last_assistant_message":null}),
            true,
            json!({"availability":"absent"}),
        ),
        (
            json!({"last_assistant_message":3}),
            true,
            json!({"availability":"invalid"}),
        ),
        (
            json!({"last_assistant_message":""}),
            true,
            json!({"availability":"available","text":""}),
        ),
    ] {
        let bytes = consumer::delivery_bytes(
            &outcome,
            wezterm_attention::providers::reply_content(&event, &payload, requested),
            HookContent::NotRequested,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["reply"],
            expected
        );
    }
    let maximum = wezterm_attention::protocol::manifest()
        .unwrap()
        .limits
        .max_json_bytes;
    let bytes = consumer::delivery_bytes(
        &outcome,
        HookContent::Available {
            text: "x".repeat(maximum),
        },
        HookContent::NotRequested,
    )
    .unwrap();
    assert!(bytes.len() <= maximum);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["reply"],
        json!({"availability":"too_large"})
    );
    let pi = super::event("pi", "agent_settled", "p", json!({}));
    assert_eq!(
        serde_json::to_value(wezterm_attention::providers::reply_content(
            &pi,
            &json!({"last_assistant_message":"not supported"}),
            true
        ))
        .unwrap(),
        json!({"availability":"unsupported"})
    );
}

#[test]
fn prompt_availability_and_independent_requests_are_exact() {
    use wezterm_attention::providers::{prompt_content, reply_content};
    let setup = Setup::new();
    bind(&setup, "codex");
    let submit = event("codex", "UserPromptSubmit", "consumer-session", json!({}));
    let outcome = apply_provider_event_with_outcome(
        &submit,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    for (payload, requested, expected) in [
        (
            json!({"prompt":"hidden"}),
            false,
            json!({"availability":"not_requested"}),
        ),
        (json!({}), true, json!({"availability":"absent"})),
        (
            json!({"prompt":null}),
            true,
            json!({"availability":"invalid"}),
        ),
        (json!({"prompt":3}), true, json!({"availability":"invalid"})),
        (
            json!({"prompt":false}),
            true,
            json!({"availability":"invalid"}),
        ),
        (
            json!({"prompt":[]}),
            true,
            json!({"availability":"invalid"}),
        ),
        (
            json!({"prompt":{}}),
            true,
            json!({"availability":"invalid"}),
        ),
        (
            json!({"prompt":""}),
            true,
            json!({"availability":"available","text":""}),
        ),
    ] {
        let bytes = consumer::delivery_bytes(
            &outcome,
            reply_content(&submit, &payload, true),
            prompt_content(&submit, &payload, requested),
        )
        .unwrap();
        let delivery: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(delivery["prompt"], expected);
        assert_eq!(delivery["reply"], json!({"availability":"unsupported"}));
    }
    for (provider, name, patch) in [
        ("claude", "Stop", json!({})),
        ("codex", "PreToolUse", json!({"tool_name":"Read"})),
        ("claude", "UserPromptSubmit", json!({"agent_id":"child"})),
        ("codex", "UserPromptSubmit", json!({"agent_id":"child"})),
        ("pi", "input", json!({"source":"interactive"})),
    ] {
        let event = event(provider, name, "consumer-session", patch);
        assert_eq!(
            serde_json::to_value(prompt_content(
                &event,
                &json!({"prompt":"not supported"}),
                true
            ))
            .unwrap(),
            json!({"availability":"unsupported"})
        );
        assert_eq!(
            serde_json::to_value(prompt_content(&event, &json!({}), false)).unwrap(),
            json!({"availability":"not_requested"})
        );
    }
    let maximum = wezterm_attention::protocol::manifest()
        .unwrap()
        .limits
        .max_json_bytes;
    assert_eq!(
        serde_json::to_value(prompt_content(
            &submit,
            &json!({"prompt":"x".repeat(maximum + 1)}),
            true
        ))
        .unwrap(),
        json!({"availability":"too_large"})
    );
    for prompt_large in [true, false] {
        let large = HookContent::Available {
            text: "x".repeat(maximum),
        };
        let (reply, prompt) = if prompt_large {
            (HookContent::Unsupported, large)
        } else {
            (large, HookContent::NotRequested)
        };
        let bytes = consumer::delivery_bytes(&outcome, reply, prompt).unwrap();
        assert!(bytes.len() <= maximum);
        let delivery: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            delivery[if prompt_large { "prompt" } else { "reply" }],
            json!({"availability":"too_large"})
        );
        assert_eq!(
            delivery[if prompt_large { "reply" } else { "prompt" }],
            json!({"availability":if prompt_large { "unsupported" } else { "not_requested" }})
        );
    }
    let bytes = consumer::delivery_bytes(
        &outcome,
        HookContent::Available {
            text: "x".repeat(maximum),
        },
        HookContent::Available {
            text: "y".repeat(maximum),
        },
    )
    .unwrap();
    let delivery: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(delivery["reply"], json!({"availability":"too_large"}));
    assert_eq!(delivery["prompt"], json!({"availability":"too_large"}));
    let mut oversized = outcome;
    let persistence = oversized.persistence.clone();
    for field in ["native_state", "activity", "lifecycle"] {
        for value in [Persistence::Rejected, Persistence::Unconfirmed] {
            oversized.persistence = persistence.clone();
            match field {
                "native_state" => oversized.persistence.native_state = value,
                "activity" => oversized.persistence.activity = value,
                "lifecycle" => oversized.persistence.lifecycle = value,
                _ => unreachable!(),
            }
            assert!(
                consumer::delivery_bytes(
                    &oversized,
                    HookContent::NotRequested,
                    HookContent::Available {
                        text: "SYNTHETIC-BLOCKED".into()
                    }
                )
                .is_err()
            );
        }
    }
    oversized.persistence = persistence;
    oversized.admission.as_mut().unwrap().source_event = "x".repeat(maximum);
    assert!(matches!(
        consumer::delivery_bytes(&oversized, HookContent::NotRequested, HookContent::Absent),
        Err(consumer::NotDispatchedReason::EnvelopeTooLarge)
    ));
}

#[test]
fn prompt_cli_opt_in_limits_and_rejection_do_not_leak_content() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let sink = setup._scratch.0.join("prompt.json");
    let consumer = executable(
        &setup,
        "prompt-sink",
        &format!("/bin/cat > '{}'", sink.display()),
    );
    let payload = json!({"hook_event_name":"UserPromptSubmit", "session_id":"consumer-session", "prompt":"SYNTHETIC-PROMPT-SENTINEL"});
    for (flags, expected) in [
        (vec![], json!({"availability":"not_requested"})),
        (
            vec!["--include-reply"],
            json!({"availability":"not_requested"}),
        ),
        (
            vec!["--include-prompt", "--include-reply"],
            json!({"availability":"available", "text":"SYNTHETIC-PROMPT-SENTINEL"}),
        ),
    ] {
        let mut args = vec!["--consumer-timeout-ms", "10000", "--strict"];
        args.extend(flags);
        let output = hook(
            &setup,
            "claude",
            &payload,
            std::slice::from_ref(&consumer),
            &args,
        );
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC-PROMPT-SENTINEL"));
        let delivery: Value = serde_json::from_slice(&fs::read(&sink).unwrap()).unwrap();
        assert_eq!(delivery["prompt"], expected);
        fs::remove_file(&sink).unwrap();
    }
    let maximum = wezterm_attention::protocol::manifest()
        .unwrap()
        .limits
        .max_json_bytes;
    let mut large = payload.clone();
    // Native JSON fits, but the admitted scope/persistence envelope does not.
    large["prompt"] = json!("x".repeat(maximum - 200));
    assert!(serde_json::to_vec(&large).unwrap().len() < maximum);
    let args = [
        "--consumer-timeout-ms",
        "10000",
        "--include-prompt",
        "--include-reply",
        "--strict",
    ];
    let output = hook(
        &setup,
        "claude",
        &large,
        std::slice::from_ref(&consumer),
        &args,
    );
    assert!(output.status.success());
    let delivery: Value = serde_json::from_slice(&fs::read(&sink).unwrap()).unwrap();
    assert_eq!(delivery["prompt"], json!({"availability":"too_large"}));
    assert_eq!(delivery["reply"], json!({"availability":"unsupported"}));
    fs::remove_file(&sink).unwrap();
    large["prompt"] = json!("x".repeat(maximum));
    assert!(
        !hook(
            &setup,
            "claude",
            &large,
            std::slice::from_ref(&consumer),
            &args
        )
        .status
        .success()
    );
    assert!(!sink.exists());
    fs::write(
        setup
            .binding_dir("claude", "consumer-session")
            .join("lifecycle.json"),
        "invalid",
    )
    .unwrap();
    let output = hook(&setup, "claude", &payload, &[consumer], &args);
    assert!(!output.status.success());
    assert!(!sink.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC-PROMPT-SENTINEL"));
    assert_no_content(
        &state_root(&setup.env).unwrap(),
        "SYNTHETIC-PROMPT-SENTINEL",
    );
}

#[test]
fn rejected_and_partial_native_application_never_dispatch() {
    let setup = Setup::new();
    bind(&setup, "claude");
    fs::write(
        setup
            .binding_dir("claude", "consumer-session")
            .join("lifecycle.json"),
        "invalid",
    )
    .unwrap();
    let event = event("claude", "Stop", "consumer-session", json!({}));
    let outcome = apply_provider_event_with_outcome(
        &event,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    assert_eq!(outcome.persistence.native_state, Persistence::Confirmed);
    assert_eq!(outcome.persistence.activity, Persistence::Confirmed);
    assert_eq!(outcome.persistence.lifecycle, Persistence::Rejected);
    assert!(
        consumer::delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::NotRequested
        )
        .is_err()
    );
    let mut stale = setup.env.clone();
    stale.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        Uuid::new_v4().to_string(),
    );
    let outcome =
        apply_provider_event_with_outcome(&event, &stale, "00000000000000000400", &setup.ports());
    assert!(outcome.admission.is_none());
    let mut legacy = setup.env.clone();
    legacy.remove("WEZTERM_ATTENTION_LAUNCH_ID");
    let outcome =
        apply_provider_event_with_outcome(&event, &legacy, "00000000000000000500", &setup.ports());
    assert!(outcome.admission.is_none());
}

#[test]
fn non_lifecycle_mutations_have_explicit_native_persistence() {
    for (provider, name, patch) in [
        ("claude", "SessionStart", json!({"source":"startup"})),
        ("claude", "SessionEnd", json!({"reason":"other"})),
        ("pi", "bus", json!({"state":"review"})),
        ("pi", "bus", json!({"state":"clear"})),
    ] {
        let setup = Setup::new();
        bind(&setup, provider);
        let event = event(provider, name, "consumer-session", patch);
        let outcome = apply_provider_event_with_outcome(
            &event,
            &setup.env,
            "00000000000000000300",
            &setup.ports(),
        );
        assert_eq!(
            outcome.persistence.native_state,
            Persistence::Confirmed,
            "{provider}/{name}"
        );
        assert_eq!(outcome.persistence.lifecycle, Persistence::NotRequested);
        let bytes = consumer::delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::NotRequested,
        )
        .unwrap();
        assert!(
            serde_json::from_slice::<Value>(&bytes)
                .unwrap()
                .get("observation_id")
                .is_none()
        );
    }
}

#[test]
fn delivery_retains_admitted_identity_after_rotation_and_duplicates_are_distinct() {
    let mut setup = Setup::new();
    bind(&setup, "claude");
    let event = event("claude", "UserPromptSubmit", "consumer-session", json!({}));
    let outcome = apply_provider_event_with_outcome(
        &event,
        &setup.env,
        "00000000000000000300",
        &setup.ports(),
    );
    let old = setup.env["WEZTERM_ATTENTION_LAUNCH_ID"].clone();
    setup.env.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        Uuid::new_v4().to_string(),
    );
    setup.clock.monotonic = "00000000000000000400";
    setup.claim();
    let a: Value = serde_json::from_slice(
        &consumer::delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::Available {
                text: "SYNTHETIC-ROTATED".into(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    let b: Value = serde_json::from_slice(
        &consumer::delivery_bytes(
            &outcome,
            HookContent::NotRequested,
            HookContent::NotRequested,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(a["scope"]["launch_id"], old);
    assert_eq!(a["prompt"]["text"], "SYNTHETIC-ROTATED");
    assert_ne!(a["delivery_id"], b["delivery_id"]);
}

#[test]
fn malformed_consumer_arguments_fail_before_native_application() {
    let setup = Setup::new();
    bind(&setup, "claude");
    let payload = json!({"hook_event_name":"Stop","session_id":"consumer-session"});
    for (path, extra) in [
        ("relative", vec!["--consumer-timeout-ms", "100"]),
        ("/missing", vec![]),
        ("/missing", vec!["--consumer-timeout-ms", "0"]),
    ] {
        let output = hook(&setup, "claude", &payload, &[PathBuf::from(path)], &extra);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
        assert!(
            !setup
                .binding_dir("claude", "consumer-session")
                .join("activity.json")
                .exists()
        );
    }
}
