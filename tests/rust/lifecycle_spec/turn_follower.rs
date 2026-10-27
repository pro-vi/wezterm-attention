//! What every turn follower shares, whatever its provider: the turn ends it
//! records come from no hook, and each is a turn end the manifest declares.

use super::*;
use wezterm_attention::protocol::manifest;
use wezterm_attention::turn_follower::{FOLLOWER_SOURCES, FollowRequest, Followed, follow_turn};

/// The follower request the latest admitted hook made.
pub(super) fn last_request(setup: &Setup) -> FollowRequest {
    setup
        .followers
        .started
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the hook asked for a follower")
}

/// Run the follower to its end. One that would wait for ever, as a follower
/// that misses its turn's end does, is stopped by its host exiting, and the
/// test fails.
pub(super) fn follow_every(setup: &Setup, request: &FollowRequest, poll: Duration) -> Followed {
    let (sent, received) = std::sync::mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let _ = sent.send(follow_turn(request, &setup.env, &setup.ports(), poll));
        });
        match received.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result.expect("the follower runs"),
            Err(_) => {
                setup.processes.set(AGENT_PID, ProcessRead::Gone);
                let _ = received.recv();
                panic!("the follower did not finish within 5 s");
            }
        }
    })
}

pub(super) fn recorded(followed: &Followed) -> &str {
    let Followed::Recorded(result) = followed else {
        panic!("the follower recorded nothing: {followed:?}");
    };
    result.disposition.as_str()
}

#[test]
fn follower_sources_are_not_native_hooks() {
    let protocol = manifest().expect("manifest");
    for (provider, source) in FOLLOWER_SOURCES {
        let name = provider.as_str();
        assert!(
            !protocol.native_hooks[name].contains_key(source),
            "{name} declares {source} as a hook"
        );
        let parsed = parse_provider_event(
            name,
            source,
            &payload(name, source, "s", json!({})),
            &BTreeMap::new(),
        );
        assert_eq!(parsed.action, ProviderAction::Ignored, "{name}: {parsed:?}");
        assert!(parsed.observation.is_none(), "{name}");
    }
}

#[test]
fn follower_sources_are_turn_end_sources() {
    let protocol = manifest().expect("manifest");
    for (provider, source) in FOLLOWER_SOURCES {
        let name = provider.as_str();
        for (table, sources) in [
            ("turn_end_sources", &protocol.turn_end_sources),
            ("lifecycle_sources", &protocol.lifecycle_sources),
        ] {
            assert!(
                sources[name].values().any(|events| events.contains(source)),
                "{table} has no {name} kind from {source}"
            );
        }
    }
}
