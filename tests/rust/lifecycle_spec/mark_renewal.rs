//! A mark with a time limit, sent again with the same content while it is
//! still shown, renews it: the stored event stays and the limit starts again.
//! Sent again after the limit ran out, it is a new event.

use super::*;
use crate::mark_clear::{launch_activity_path, rust_activity};
use wezterm_attention::query::RecordAvailability;

const FIRST_WRITTEN: &str = "00000000012345678900";
const TEN_SECONDS_LATER: &str = "00000000022345678900";
const TWENTY_SECONDS_LATER: &str = "00000000032345678900";
const THIRTY_ONE_SECONDS_LATER: &str = "00000000043345678900";
const FORTY_SECONDS_LATER: &str = "00000000052345678900";

fn claimed() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup
}

fn mark(
    setup: &Setup,
    state: &str,
    ttl_ms: Option<u64>,
    observation: &str,
    written_at: &str,
) -> wezterm_attention::lifecycle::LifecycleResult {
    apply_mark_activity(
        &setup.env,
        state,
        "manual",
        None,
        None,
        ttl_ms,
        observation,
        written_at,
    )
    .expect("mark applies")
}

fn activity(setup: &Setup) -> Value {
    serde_json::from_slice(&fs::read(launch_activity_path(setup)).expect("activity record"))
        .expect("activity json")
}

#[test]
fn a_thinking_mark_with_a_time_limit_renews_before_it_expires() {
    let setup = claimed();
    let first = mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    assert_eq!(first.disposition, "applied");
    let second = mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000300",
        TEN_SECONDS_LATER,
    );
    assert_eq!(second.disposition, "renewed");
    assert_eq!(second.event_id, first.event_id);
    let stored = activity(&setup);
    assert_eq!(stored["event_id"], first.event_id.as_deref().unwrap());
    assert_eq!(stored["observed_mono_ns"], "00000000000000000200");
    assert_eq!(stored["written_at_unix_ns"], TEN_SECONDS_LATER);
}

#[test]
fn a_thinking_mark_sent_after_its_time_limit_is_a_new_event() {
    let setup = claimed();
    let first = mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    let second = mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000300",
        THIRTY_ONE_SECONDS_LATER,
    );
    assert_eq!(second.disposition, "applied");
    assert_ne!(second.event_id, first.event_id);
    let stored = activity(&setup);
    assert_eq!(stored["event_id"], second.event_id.as_deref().unwrap());
    assert_eq!(stored["observed_mono_ns"], "00000000000000000300");
    assert_eq!(stored["written_at_unix_ns"], THIRTY_ONE_SECONDS_LATER);
}

#[test]
fn a_thinking_mark_without_a_time_limit_stays_skipped() {
    let setup = claimed();
    mark(
        &setup,
        "thinking",
        None,
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    let before = activity(&setup);
    let second = mark(
        &setup,
        "thinking",
        None,
        "00000000000000000300",
        TEN_SECONDS_LATER,
    );
    assert_eq!(second.disposition, "skipped");
    assert_eq!(activity(&setup), before);
}

#[test]
fn a_stop_with_a_time_limit_renews_before_it_expires() {
    let setup = claimed();
    let first = mark(
        &setup,
        "stop",
        Some(30_000),
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    let second = mark(
        &setup,
        "stop",
        Some(30_000),
        "00000000000000000300",
        TEN_SECONDS_LATER,
    );
    assert_eq!(second.disposition, "renewed");
    assert_eq!(second.event_id, first.event_id);
    assert_eq!(activity(&setup)["written_at_unix_ns"], TEN_SECONDS_LATER);
}

#[test]
fn a_stop_sent_after_its_time_limit_is_a_new_event() {
    let setup = claimed();
    let first = mark(
        &setup,
        "stop",
        Some(30_000),
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    let second = mark(
        &setup,
        "stop",
        Some(30_000),
        "00000000000000000300",
        THIRTY_ONE_SECONDS_LATER,
    );
    assert_eq!(second.disposition, "applied");
    assert_ne!(second.event_id, first.event_id);
}

#[test]
fn a_renewed_mark_is_still_present_after_the_first_limit() {
    let mut setup = claimed();
    mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000200",
        FIRST_WRITTEN,
    );
    let renewed = mark(
        &setup,
        "thinking",
        Some(30_000),
        "00000000000000000300",
        TWENTY_SECONDS_LATER,
    );
    assert_eq!(renewed.disposition, "renewed");
    setup.clock = FixedClock {
        monotonic: "00000000000000000400",
        unix: FORTY_SECONDS_LATER,
    };
    assert_eq!(rust_activity(&setup).0, RecordAvailability::Present);
}
