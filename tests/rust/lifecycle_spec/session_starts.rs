use super::host_facet::{facts_with, launch, second_agent};
use super::self_claim::{apply_as, setup_launch_dir, start, start_from};
use super::*;
use wezterm_attention::query::ScopeRelation;

// Both providers start a forked session with source "fork": Claude for
// `--fork-session`, Codex when a thread is forked from another. It is a new
// session in the same process, so it takes over the binding the way a resume
// does.
#[test]
fn forked_session_replaces_the_active_binding() {
    for provider in ["claude", "codex"] {
        let setup = Setup::new();
        setup.claim();
        let parent = event(
            provider,
            "SessionStart",
            "parent",
            json!({"source":"startup"}),
        );
        assert_eq!(
            setup.apply(&parent, "00000000000000000200").disposition,
            "applied"
        );
        let fork = event(provider, "SessionStart", "fork", json!({"source":"fork"}));
        assert_eq!(fork.action, ProviderAction::Binding, "{provider}");
        assert_eq!(fork.start_source.as_deref(), Some("fork"));
        assert_eq!(
            setup.apply(&fork, "00000000000000000300").disposition,
            "replaced",
            "{provider}"
        );
        let (rows, _) = read_bindings(&state_root(&setup.env).unwrap()).unwrap();
        assert!(
            rows.iter()
                .any(|row| row.provider_session_id == "fork" && row.current),
            "{provider}"
        );
        let stop = event(provider, "Stop", "fork", json!({"stop_hook_active":false}));
        assert_eq!(
            setup.apply(&stop, "00000000000000000400").disposition,
            "applied",
            "{provider}"
        );
    }
}

// Pi starts a session as "new", "resume" or "fork" and takes over a running
// binding with each; a "startup" or "reload" start does not.
#[test]
fn a_pi_session_replaces_the_active_binding_on_new_resume_and_fork_only() {
    for (source, disposition) in [
        ("new", "replaced"),
        ("resume", "replaced"),
        ("fork", "replaced"),
        ("startup", "conflict"),
        ("reload", "conflict"),
    ] {
        let setup = Setup::new();
        setup.claim();
        assert_eq!(
            setup
                .apply(&start("pi", "first"), "00000000000000000200")
                .disposition,
            "applied"
        );
        let next = event(
            "pi",
            "session_start",
            "next",
            json!({"start_source":source}),
        );
        assert_eq!(
            setup.apply(&next, "00000000000000000300").disposition,
            disposition,
            "{source}"
        );
    }
}

/// The session Codex 0.162.0's `/new` leaves running, and the one it starts.
const OLD: &str = "old";
const NEW: &str = "new";

fn binding_file(setup: &Setup, provider: &str, session: &str, name: &str) -> PathBuf {
    let launch = launch(setup);
    setup_launch_dir(setup, &launch)
        .join("bindings")
        .join(binding_id(provider, session, &launch))
        .join(name)
}

fn host_of(setup: &Setup, provider: &str, session: &str) -> Option<Value> {
    fs::read(binding_file(setup, provider, session, "host.json"))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("host JSON"))
}

fn current_session(setup: &Setup) -> Option<String> {
    let (rows, _) = read_bindings(&state_root(&setup.env).expect("state root")).expect("bindings");
    rows.into_iter()
        .find(|row| row.current)
        .map(|row| row.provider_session_id)
}

/// What scoped `inspect` answers for `session`'s binding, checking its host.
fn inspected(setup: &Setup, provider: &str, session: &str) -> (ScopeRelation, Value) {
    let facts = facts_with(
        setup,
        &binding_id(provider, session, &launch(setup)),
        &setup.tty,
    );
    (
        facts.scope_relation,
        serde_json::to_value(facts.host).expect("host facet JSON"),
    )
}

/// Start `provider`'s session OLD from a hook that names the agent, in a pane
/// its shell claimed or one the agent claims itself, and give the environment
/// its hooks run in.
fn old_session(setup: &Setup, provider: &str, self_owned: bool) -> BTreeMap<String, String> {
    let env = if self_owned {
        setup.agent_env()
    } else {
        setup.claim();
        setup.naming(AGENT_PID)
    };
    let result = apply_as(setup, &env, &start(provider, OLD), "00000000000000000200");
    assert_eq!(result.disposition, "applied", "{provider}: {result:?}");
    assert!(
        host_of(setup, provider, OLD).is_some(),
        "{provider}: no host"
    );
    env
}

// As of Codex 0.162.0, `/new` starts its new session in the same process with
// source "startup", and runs no SessionEnd for the session before. A start
// proved to come from the host the current binding recorded replaces the
// binding, in a pane its shell claimed and in one the agent claimed.
#[test]
fn a_codex_start_from_the_recorded_host_replaces_the_active_binding() {
    for self_owned in [false, true] {
        let setup = Setup::new();
        let env = old_session(&setup, "codex", self_owned);
        let result = apply_as(
            &setup,
            &env,
            &start_from("codex", NEW, "startup"),
            "00000000000000000300",
        );
        assert_eq!(result.disposition, "replaced", "{self_owned}: {result:?}");
        assert_eq!(current_session(&setup).as_deref(), Some(NEW));
        let host = host_of(&setup, "codex", NEW).expect("the new binding's host");
        assert_eq!(host["source_event"], "SessionStart");
        assert_eq!(host["owner_pid"], AGENT_PID.to_string());

        let (relation, facet) = inspected(&setup, "codex", NEW);
        assert_eq!(relation, ScopeRelation::Matched);
        assert_eq!(facet["availability"], "available", "{facet}");
        assert_eq!(facet["record"]["pid"], AGENT_PID);
        let (relation, facet) = inspected(&setup, "codex", OLD);
        assert_eq!(relation, ScopeRelation::BindingChanged);
        assert_eq!(facet["availability"], "unavailable", "{facet}");
        assert_eq!(facet["reason"], "scope_not_matched", "{facet}");

        let prompt = event("codex", "UserPromptSubmit", NEW, json!({"prompt":"go"}));
        assert_eq!(
            apply_as(&setup, &env, &prompt, "00000000000000000400").disposition,
            "applied"
        );
    }
}

/// Run the start `arrange` gives, on the pane it set up, and record in
/// `failed` how it left the binding if it did not leave OLD current with
/// nothing written for NEW.
fn leaves_the_binding(
    failed: &mut Vec<String>,
    name: &str,
    provider: &str,
    disposition: &str,
    arrange: impl FnOnce(&Setup) -> (BTreeMap<String, String>, ProviderEvent),
) {
    let setup = Setup::new();
    let (env, start) = arrange(&setup);
    let result = apply_as(&setup, &env, &start, "00000000000000000300");
    let code = result.diagnostic.as_ref().map(|found| found.code);
    if result.disposition != disposition
        || (disposition == "conflict" && code != Some(DiagnosticCode::BindingConflict))
        || current_session(&setup).as_deref() != Some(OLD)
        || host_of(&setup, provider, NEW).is_some()
        || binding_file(&setup, provider, NEW, "binding.json").exists()
    {
        failed.push(format!("{name}: {result:?}"));
    }
}

// Only a lead Codex "startup" that names its transcript and is proved by the
// host the current binding recorded, while no other process contests that
// record, replaces the binding. A second agent sharing the pane's launch, a
// start that names no transcript, or one the hook does not prove, leaves it as
// it is. Every case runs, so one failure does not hide another's.
#[test]
fn a_start_that_does_not_qualify_leaves_the_binding_as_it_is() {
    let startup = || start_from("codex", NEW, "startup");
    let mut failed = Vec::new();
    leaves_the_binding(
        &mut failed,
        "no host was recorded for the current binding",
        "codex",
        "conflict",
        |setup| {
            setup.claim();
            setup.apply(&start("codex", OLD), "00000000000000000200");
            assert!(host_of(setup, "codex", OLD).is_none());
            (setup.naming(AGENT_PID), startup())
        },
    );
    leaves_the_binding(
        &mut failed,
        "another agent process",
        "codex",
        "conflict",
        |setup| {
            old_session(setup, "codex", false);
            (second_agent(setup), startup())
        },
    );
    leaves_the_binding(
        &mut failed,
        "the recorded pid, started at another time",
        "codex",
        "conflict",
        |setup| {
            let env = old_session(setup, "codex", false);
            setup
                .processes
                .change(AGENT_PID, |agent| agent.start.microseconds += 1);
            (env, startup())
        },
    );
    leaves_the_binding(
        &mut failed,
        "a host recorded on another terminal",
        "codex",
        "conflict",
        |setup| {
            let env = old_session(setup, "codex", false);
            let mut host = host_of(setup, "codex", OLD).expect("host");
            host["tty_fingerprint"] = json!("e".repeat(64));
            atomic_replace(&binding_file(setup, "codex", OLD, "host.json"), &host)
                .expect("rewrite host");
            (env, startup())
        },
    );
    leaves_the_binding(
        &mut failed,
        "a contested host record",
        "codex",
        "conflict",
        |setup| {
            let env = old_session(setup, "codex", false);
            let tool = event("codex", "PreToolUse", OLD, json!({"tool_name":"shell"}));
            apply_as(setup, &second_agent(setup), &tool, "00000000000000000250");
            *setup.processes.own.lock().unwrap() = HOOK_PID;
            let host = host_of(setup, "codex", OLD).expect("host");
            assert!(host.get("contested_at_unix_ns").is_some(), "{host}");
            (env, startup())
        },
    );
    leaves_the_binding(
        &mut failed,
        "an inherited launch whose hook names no agent",
        "codex",
        "conflict",
        |setup| {
            old_session(setup, "codex", false);
            (setup.env.clone(), startup())
        },
    );
    // As of Codex 0.162.0 the memory consolidation worker runs as an ephemeral
    // session in the same process, and its start names no transcript.
    leaves_the_binding(
        &mut failed,
        "a start that names no transcript",
        "codex",
        "conflict",
        |setup| {
            let worker = event(
                "codex",
                "SessionStart",
                NEW,
                json!({"source":"startup","transcript_path":null}),
            );
            (old_session(setup, "codex", false), worker)
        },
    );
    for provider in ["claude", "pi"] {
        leaves_the_binding(
            &mut failed,
            &format!("a {provider} startup from the recorded host"),
            provider,
            "conflict",
            |setup| {
                (
                    old_session(setup, provider, false),
                    start_from(provider, NEW, "startup"),
                )
            },
        );
    }
    leaves_the_binding(
        &mut failed,
        "a Codex compact start from the recorded host",
        "codex",
        "conflict",
        |setup| {
            (
                old_session(setup, "codex", false),
                start_from("codex", NEW, "compact"),
            )
        },
    );
    leaves_the_binding(
        &mut failed,
        "a sub-agent's start from the recorded host",
        "codex",
        "ignored",
        |setup| {
            let child = event(
                "codex",
                "SessionStart",
                NEW,
                json!({"source":"startup","agent_id":"child-a"}),
            );
            (old_session(setup, "codex", false), child)
        },
    );
    assert!(failed.is_empty(), "{failed:#?}");
}

// The proof is read again under the locks of the commit that would replace
// the binding; a host that changed since replaces nothing.
#[test]
fn a_start_whose_host_changes_before_the_commit_replaces_nothing() {
    let setup = Setup::new();
    let env = old_session(&setup, "codex", false);
    let mut changed = setup.processes.facts(AGENT_PID);
    changed.start.seconds += 1;
    // Proving the hook reads its agent twice; the commit's check reads it next.
    let reads = std::sync::atomic::AtomicUsize::new(0);
    *setup.processes.on_read.lock().unwrap() = Some(std::sync::Arc::new(move |pid| {
        if pid != AGENT_PID {
            return None;
        }
        let read = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        (read > 2).then(|| ProcessRead::Found(changed.clone()))
    }));
    let result = apply_as(
        &setup,
        &env,
        &start_from("codex", NEW, "startup"),
        "00000000000000000300",
    );
    assert_eq!(result.disposition, "ignored", "{result:?}");
    assert_eq!(
        result.diagnostic.as_ref().map(|found| found.code),
        Some(DiagnosticCode::SelfClaimParentUnverified),
        "{result:?}"
    );
    *setup.processes.on_read.lock().unwrap() = None;
    assert_eq!(current_session(&setup).as_deref(), Some(OLD));
    assert!(host_of(&setup, "codex", NEW).is_none());
}

// As of Codex 0.162.0 the same process can go on running turns in the session
// `/new` left: input queued to it by thread id runs there. Once the new
// session holds the binding, those turns' hooks are ignored, so the tab does
// not show them, and the earlier binding's activity stays as it was. That
// holds for a turn that began before the replacement and ends after it too.
#[test]
fn the_old_session_s_turns_after_the_replacement_are_refused() {
    let setup = Setup::new();
    let env = old_session(&setup, "codex", false);
    let prompt = event("codex", "UserPromptSubmit", OLD, json!({"prompt":"queued"}));
    let stop = event("codex", "Stop", OLD, json!({"stop_hook_active":false}));
    assert_eq!(
        apply_as(&setup, &env, &prompt, "00000000000000000250").disposition,
        "applied"
    );
    let activity = binding_file(&setup, "codex", OLD, "activity.json");
    let thinking = fs::read_to_string(&activity).expect("the old turn's activity");
    assert_eq!(
        apply_as(
            &setup,
            &env,
            &start_from("codex", NEW, "startup"),
            "00000000000000000300"
        )
        .disposition,
        "replaced"
    );
    for (event, observation) in [
        (&stop, "00000000000000000400"),
        (&prompt, "00000000000000000500"),
    ] {
        let result = apply_as(&setup, &env, event, observation);
        assert_eq!(result.disposition, "ignored", "{result:?}");
        assert_eq!(
            result.diagnostic.as_ref().map(|found| found.code),
            Some(DiagnosticCode::ClaimStale),
            "{result:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(&activity).expect("activity").as_str(),
        thinking
    );
    assert_eq!(current_session(&setup).as_deref(), Some(NEW));
}
