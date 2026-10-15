//! What every turn follower shares, whatever its provider: the turn ends it
//! records come from no hook, and each is a turn end the manifest declares.

use super::*;
use wezterm_attention::protocol::manifest;
use wezterm_attention::turn_follower::FOLLOWER_SOURCES;

// The manifest's `native_hooks` do not declare a follower's source, and a hook
// event they do not declare is ignored, so only the follower records a turn
// end from it.
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
