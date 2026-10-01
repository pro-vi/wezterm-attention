use super::self_claim::start;
use super::*;

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
