use super::*;
use wezterm_attention::identity::PaneAddress;
use wezterm_attention::lifecycle::{clear_user_review, set_user_review};

const SESSION: &str = "resumed-review";

fn start(provider: &str, source: &str) -> ProviderEvent {
    if provider == "pi" {
        event(
            provider,
            "session_start",
            SESSION,
            json!({"start_source": source}),
        )
    } else {
        event(provider, "SessionStart", SESSION, json!({"source": source}))
    }
}

fn user_review(root: &std::path::Path, address: &PaneAddress) -> PathBuf {
    pane_dir(root, address).join("reviews").join(format!(
        "{}.json",
        wezterm_attention::protocol::sha256_hex(b"user")
    ))
}

fn bound_source(provider: &str) -> (Setup, PaneAddress) {
    let source = Setup::new();
    source.claim();
    source.apply(&start(provider, "startup"), "00000000000000000200");
    let address = pane_address(&source.env).unwrap().0;
    set_user_review(
        &state_root(&source.env).unwrap(),
        &address,
        &source.env["WEZTERM_ATTENTION_LAUNCH_ID"],
    )
    .unwrap();
    (source, address)
}

fn destination(source: &Setup) -> Setup {
    let mut destination = Setup::new();
    destination.env.insert(
        "WEZTERM_ATTENTION_DIR".into(),
        source.env["WEZTERM_ATTENTION_DIR"].clone(),
    );
    destination.clock.unix = "00000000022345678900";
    destination.claim();
    destination
}

fn end_server(source: &mut Setup) {
    source._socket.take();
    fs::remove_file(&source.env["WEZTERM_UNIX_SOCKET"]).unwrap();
}

#[test]
fn a_resumed_session_carries_only_the_user_review_after_server_loss() {
    for provider in ["claude", "codex", "pi"] {
        let (mut source, old_address) = bound_source(provider);
        apply_mark_review(&source.env, "another-owner").unwrap();
        let destination = destination(&source);
        let root = state_root(&source.env).unwrap();
        end_server(&mut source);
        assert_eq!(
            destination
                .apply(&start(provider, "resume"), "00000000000000000300")
                .disposition,
            "applied"
        );
        let new_address = pane_address(&destination.env).unwrap().0;
        assert!(!user_review(&root, &old_address).exists());
        assert!(user_review(&root, &new_address).is_file());
        assert!(
            pane_dir(&root, &old_address)
                .join("reviews")
                .join(format!(
                    "{}.json",
                    wezterm_attention::protocol::sha256_hex(b"another-owner")
                ))
                .is_file()
        );
        assert_eq!(
            mark_clear::plugin_reader_answer(&destination, "user_review"),
            "user_review=true"
        );
        clear_user_review(&root, &new_address).unwrap();
        destination.apply(&start(provider, "resume"), "00000000000000000400");
        assert!(
            !user_review(&root, &new_address).exists(),
            "clear must survive retry"
        );
    }
}

#[test]
fn a_resume_on_a_current_server_or_the_same_address_keeps_the_flag_in_place() {
    let (source, old_address) = bound_source("claude");
    let destination = destination(&source);
    let root = state_root(&source.env).unwrap();
    destination.apply(&start("claude", "resume"), "00000000000000000300");
    assert!(user_review(&root, &old_address).is_file());
    assert!(!user_review(&root, &pane_address(&destination.env).unwrap().0).exists());
    let original = fs::read(user_review(&root, &old_address)).unwrap();
    source.apply(&start("claude", "resume"), "00000000000000000400");
    assert_eq!(
        fs::read(user_review(&root, &old_address)).unwrap(),
        original
    );
}

#[test]
fn non_resume_registration_and_missing_history_do_not_carry_review() {
    for missing in [false, true] {
        let (mut source, old_address) = bound_source("claude");
        let destination = destination(&source);
        let root = state_root(&source.env).unwrap();
        if missing {
            fs::remove_dir_all(pane_dir(&root, &old_address)).unwrap();
        }
        end_server(&mut source);
        destination.apply(
            &start("claude", if missing { "resume" } else { "startup" }),
            "00000000000000000300",
        );
        assert!(!user_review(&root, &pane_address(&destination.env).unwrap().0).exists());
    }
}

#[test]
fn competing_resumes_receive_one_user_flag() {
    let (mut source, old_address) = bound_source("claude");
    let first = destination(&source);
    let second = destination(&source);
    let root = state_root(&source.env).unwrap();
    end_server(&mut source);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.apply(&start("claude", "resume"), "00000000000000000300");
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.apply(&start("claude", "resume"), "00000000000000000300");
        });
        a.join().unwrap();
        b.join().unwrap();
    });
    let count = [&first, &second]
        .iter()
        .filter(|setup| user_review(&root, &pane_address(&setup.env).unwrap().0).exists())
        .count();
    assert_eq!(count, 1);
    assert!(!user_review(&root, &old_address).exists());
}

#[test]
fn a_changed_source_selection_and_future_review_are_preserved() {
    for future in [false, true] {
        let (mut source, old_address) = bound_source("claude");
        let destination = destination(&source);
        let root = state_root(&source.env).unwrap();
        let path = user_review(&root, &old_address);
        if future {
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["schema"] = json!(999);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        } else {
            source.apply(
                &event(
                    "claude",
                    "SessionStart",
                    "replacement-session",
                    json!({"source":"clear"}),
                ),
                "00000000000000000300",
            );
        }
        let before = fs::read(&path).unwrap();
        end_server(&mut source);
        destination.apply(&start("claude", "resume"), "00000000000000000400");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!user_review(&root, &pane_address(&destination.env).unwrap().0).exists());
    }
}

#[test]
fn replacing_a_server_at_the_same_socket_path_carries_review() {
    let (mut source, old_address) = bound_source("claude");
    let old_listener = source._socket.take().unwrap();
    let socket = source.env["WEZTERM_UNIX_SOCKET"].clone();
    fs::remove_file(&socket).unwrap();
    let mut destination = Setup::new();
    destination._socket = Some(UnixListener::bind(&socket).unwrap());
    destination.env.insert("WEZTERM_UNIX_SOCKET".into(), socket);
    destination.env.insert(
        "WEZTERM_ATTENTION_DIR".into(),
        source.env["WEZTERM_ATTENTION_DIR"].clone(),
    );
    destination.clock.unix = "00000000022345678900";
    destination.claim();
    let new_address = pane_address(&destination.env).unwrap().0;
    assert_eq!(old_address.realm_id, new_address.realm_id);
    assert_ne!(old_address.incarnation_id, new_address.incarnation_id);
    drop(old_listener);
    destination.apply(&start("claude", "resume"), "00000000000000000300");
    let root = state_root(&source.env).unwrap();
    assert!(!user_review(&root, &old_address).exists());
    assert!(user_review(&root, &new_address).is_file());
}

#[test]
fn changing_only_live_socket_metadata_does_not_carry_review() {
    use std::os::unix::fs::PermissionsExt;
    let (source, old_address) = bound_source("claude");
    let destination = destination(&source);
    let socket = &source.env["WEZTERM_UNIX_SOCKET"];
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).unwrap();
    destination.apply(&start("claude", "resume"), "00000000000000000300");
    let root = state_root(&source.env).unwrap();
    assert!(user_review(&root, &old_address).is_file());
    assert!(!user_review(&root, &pane_address(&destination.env).unwrap().0).exists());
}

#[test]
fn a_newer_unflagged_binding_prevents_fallback_to_an_older_review() {
    let (mut source, old_address) = bound_source("claude");
    let mut newer = destination(&source);
    newer.apply(&start("claude", "startup"), "00000000000000000300");
    let mut last = destination(&source);
    last.clock.unix = "00000000032345678900";
    end_server(&mut source);
    end_server(&mut newer);
    last.apply(&start("claude", "resume"), "00000000000000000400");
    let root = state_root(&source.env).unwrap();
    assert!(user_review(&root, &old_address).is_file());
    assert!(!user_review(&root, &pane_address(&last.env).unwrap().0).exists());
}

#[test]
fn clearing_source_before_resume_does_not_create_a_destination_flag() {
    let (mut source, old_address) = bound_source("claude");
    let destination = destination(&source);
    let root = state_root(&source.env).unwrap();
    clear_user_review(&root, &old_address).unwrap();
    end_server(&mut source);
    destination.apply(&start("claude", "resume"), "00000000000000000300");
    assert!(!user_review(&root, &pane_address(&destination.env).unwrap().0).exists());
}
