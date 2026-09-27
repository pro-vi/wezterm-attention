//! Every write an event makes happens while the pane's claim is the one the
//! event was resolved against: under the launch lock and then the claim lock,
//! compared again there, and held until the last record is written.

use super::self_claim::{
    AGENT_LAUNCH, apply_as, every_action, install, launch_of, records, self_owned_claim, start,
    stored_claim,
};
use super::*;

use std::sync::mpsc;

/// A pane whose shell claim has bound a Codex and a Pi session "s", so every
/// action has something to act on.
fn bound() -> Setup {
    let setup = Setup::new();
    setup.claim();
    setup.apply(
        &event("codex", "SessionStart", "s", json!({"source":"startup"})),
        "00000000000000000200",
    );
    setup.apply(
        &event(
            "pi",
            "session_start",
            "s",
            json!({"start_source":"startup"}),
        ),
        "00000000000000000210",
    );
    setup
}

fn claim_lock(setup: &Setup) -> PathBuf {
    let (address, _) = pane_address(&setup.env).expect("address");
    pane_dir(&state_root(&setup.env).expect("state root"), &address).join(".claim.lock")
}

/// One writer an event or a mark can reach, run against a pane.
type Writer = Box<dyn Fn(&Setup) -> wezterm_attention::protocol::Result<()> + Send + Sync>;

/// Every writer an event or a mark can reach.
fn every_writer() -> Vec<(String, Writer)> {
    let mut writers: Vec<(String, Writer)> = every_action()
        .into_iter()
        .map(|event| {
            let label = format!("{} {:?}", event.source_event, event.action);
            let writer: Writer = Box::new(move |setup: &Setup| {
                apply_provider_event(&event, &setup.env, "00000000000000000900", &setup.ports())
                    .map(|_| ())
            });
            (label, writer)
        })
        .collect();
    writers.push((
        "mark activity".into(),
        Box::new(|setup: &Setup| {
            apply_mark_activity(
                &setup.env,
                "notify",
                "tester",
                None,
                None,
                None,
                "00000000000000000900",
                "00000000012345678900",
            )
            .map(|_| ())
        }),
    ));
    writers.push((
        "mark review".into(),
        Box::new(|setup: &Setup| apply_mark_review(&setup.env, "tester").map(|_| ())),
    ));
    writers.push((
        "mark clear".into(),
        Box::new(|setup: &Setup| {
            apply_mark_clear(&setup.env, "tester", "00000000000000000900").map(|_| ())
        }),
    ));
    writers.push((
        "prompt return".into(),
        Box::new(|setup: &Setup| prompt_return(&setup.env, "00000000000000000900").map(|_| ())),
    ));
    writers
}

#[test]
fn every_writer_waits_for_the_pane_claim_lock() {
    // Each case holds its own pane's claim lock past the writer's two-second
    // wait, all at once, so the whole table costs one wait.
    let failures: Vec<String> = thread::scope(|scope| {
        let cases: Vec<_> = every_writer()
            .into_iter()
            .map(|(label, writer)| {
                scope.spawn(move || {
                    let setup = bound();
                    let before = records(&setup);
                    let outcome = with_lock(&claim_lock(&setup), Duration::from_secs(1), || {
                        Ok(writer(&setup))
                    })
                    .expect("the test holds the claim lock");
                    match outcome {
                        Err(error) if error.diagnostic.code == "probe_unavailable" => {
                            (records(&setup) != before)
                                .then(|| format!("{label}: wrote while waiting"))
                        }
                        other => Some(format!("{label}: did not wait: {other:?}")),
                    }
                })
            })
            .collect();
        cases
            .into_iter()
            .filter_map(|case| case.join().expect("case thread"))
            .collect()
    });
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A clock whose wall reading, taken after an event is resolved and before
/// it takes any lock, waits until the test lets it go on.
struct PausingClock {
    entered: std::sync::Barrier,
    released: std::sync::Barrier,
}

impl Clock for PausingClock {
    fn monotonic_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        Ok("00000000000000000900".into())
    }

    fn unix_ns20(&self) -> wezterm_attention::protocol::Result<String> {
        self.entered.wait();
        self.released.wait();
        Ok("00000000012345678900".into())
    }
}

/// Resolve `event` in `env`, then let `replace` change the pane's claim
/// before the event reaches its locks.
fn resolve_then_replace(
    setup: &Setup,
    env: &BTreeMap<String, String>,
    event: &ProviderEvent,
    replace: impl FnOnce(),
) -> wezterm_attention::lifecycle::LifecycleResult {
    let clock = PausingClock {
        entered: std::sync::Barrier::new(2),
        released: std::sync::Barrier::new(2),
    };
    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            let ports = RuntimePorts {
                clock: &clock,
                tty: &setup.tty,
                panes: &setup.panes,
                processes: &setup.processes,
            };
            apply_provider_event(event, env, "00000000000000000900", &ports)
        });
        clock.entered.wait();
        replace();
        clock.released.wait();
        pending
            .join()
            .expect("event thread")
            .expect("a refusal is a result")
    })
}

/// The records an event can write: everything but the claim and the realm
/// and incarnation manifests a claim writes beside it.
fn event_records(mut records: BTreeMap<PathBuf, String>) -> BTreeMap<PathBuf, String> {
    records.retain(|path, _| {
        path.file_name().is_none_or(|name| {
            !["claim.json", "realm.json", "incarnation.json"].contains(&name.to_str().unwrap_or(""))
        })
    });
    records
}

#[test]
fn a_claim_replaced_after_resolution_refuses_every_event_before_it_writes() {
    for event in every_action() {
        let setup = bound();
        let before = records(&setup);
        let mut newer = setup.env.clone();
        newer.insert(
            "WEZTERM_ATTENTION_LAUNCH_ID".into(),
            "00000000-0000-4000-8000-0000000009aa".into(),
        );
        let result = resolve_then_replace(&setup, &setup.env, &event, || {
            let ports = RuntimePorts {
                clock: &FixedClock {
                    monotonic: "00000000000000000500",
                    unix: "00000000012345678900",
                },
                tty: &setup.tty,
                panes: &setup.panes,
                processes: &setup.processes,
            };
            wezterm_attention::claim_launch(&newer, &ports).expect("newer claim");
        });
        let label = format!("{} {:?}", event.source_event, event.action);
        assert_eq!(result.disposition, "ignored", "{label}: {result:?}");
        assert_eq!(
            result.diagnostic.as_ref().map(|item| item.code.as_str()),
            Some("claim_stale"),
            "{label}"
        );
        assert_eq!(
            event_records(records(&setup)),
            event_records(before),
            "{label}: nothing is written, in the old launch or the new one"
        );
    }
}

#[test]
fn an_agent_s_claim_taken_over_after_resolution_refuses_its_event() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let env = setup.agent_env();
    apply_as(&setup, &env, &start("codex", "s"), "00000000000000000200");
    let before = records(&setup);
    let mut shell = setup.env.clone();
    shell.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        "00000000-0000-4000-8000-0000000009ab".into(),
    );
    let tool = event("codex", "PreToolUse", "s", json!({"tool_name":"shell"}));
    let result = resolve_then_replace(&setup, &env, &tool, || {
        wezterm_attention::claim_launch(&shell, &setup.ports()).expect("shell claim");
    });
    assert_eq!(result.disposition, "ignored", "{result:?}");
    assert_eq!(event_records(records(&setup)), event_records(before));
}

#[test]
fn a_claim_change_waits_while_an_admitted_event_writes() {
    let setup = Setup::new();
    install(&setup, &self_owned_claim(&setup, AGENT_LAUNCH, AGENT_PID));
    let env = setup.agent_env();
    apply_as(&setup, &env, &start("codex", "s"), "00000000000000000200");
    let claim = stored_claim(&setup).expect("agent claim");
    // Resolving reads the agent twice; the third reading is the one the event
    // takes under its locks, just before it writes.
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let entered_tx = Mutex::new(entered_tx);
    *setup.processes.on_read.lock().unwrap() = Some(std::sync::Arc::new(move |pid| {
        if pid == AGENT_PID && reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        None
    }));
    let mut shell = setup.env.clone();
    shell.insert(
        "WEZTERM_ATTENTION_LAUNCH_ID".into(),
        "00000000-0000-4000-8000-0000000009ac".into(),
    );
    let replaced = std::sync::atomic::AtomicBool::new(false);
    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            apply_as(
                &setup,
                &env,
                &event("codex", "PreToolUse", "s", json!({"tool_name":"shell"})),
                "00000000000000000300",
            )
        });
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the event reads its agent again under its locks");
        let competitor = scope.spawn(|| {
            let result = wezterm_attention::claim_launch(&shell, &setup.ports());
            replaced.store(true, std::sync::atomic::Ordering::SeqCst);
            result
        });
        thread::sleep(Duration::from_millis(300));
        assert!(
            !replaced.load(std::sync::atomic::Ordering::SeqCst),
            "the claim changed while the event held its locks"
        );
        assert_eq!(stored_claim(&setup).as_ref(), Some(&claim));
        release.send(()).unwrap();
        assert_eq!(pending.join().unwrap().disposition, "applied");
        competitor
            .join()
            .unwrap()
            .expect("the claim changes afterwards");
    });
    *setup.processes.on_read.lock().unwrap() = None;
    let binding = launch_dir(
        &state_root(&setup.env).unwrap(),
        &pane_address(&setup.env).unwrap().0,
        &launch_of(&claim),
    )
    .join("bindings")
    .join(binding_id("codex", "s", AGENT_LAUNCH));
    assert!(binding.join("activity.json").exists());
    assert_eq!(
        launch_of(&stored_claim(&setup).unwrap()),
        "00000000-0000-4000-8000-0000000009ac"
    );
}
