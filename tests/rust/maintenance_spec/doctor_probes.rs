//! What doctor asks of the machine, and what it says when it could not ask.

use super::*;
use std::sync::atomic::AtomicUsize;
use wezterm_attention::wezterm::{PaneProcessSet, ProcessListing};

/// Offers one listing of every process, and counts how it is asked. Like the
/// system probe, it answers whether it is available by taking a listing.
pub(super) struct CountingListing {
    pub(super) listings: AtomicUsize,
    pub(super) single_looks: AtomicUsize,
}

impl CountingListing {
    pub(super) fn new() -> Self {
        Self {
            listings: AtomicUsize::new(0),
            single_looks: AtomicUsize::new(0),
        }
    }
}

impl ProcessProbe for CountingListing {
    fn available(&self) -> bool {
        !matches!(self.pane_processes(), ProcessListing::Failed)
    }

    fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
        self.single_looks.fetch_add(1, Ordering::SeqCst);
        Presence::Absent
    }

    fn pane_processes(&self) -> ProcessListing {
        self.listings.fetch_add(1, Ordering::SeqCst);
        ProcessListing::Listed(PaneProcessSet::from_process_listing(""))
    }
}

/// Copies pane 42's claim to other pane ids, as other claimed panes.
fn claim_more_panes(setup: &Setup, panes: &[&str]) {
    let root = setup.root();
    let (address, _) = pane_address(&setup.env).expect("address");
    let claim: Value = serde_json::from_slice(
        &fs::read(pane_path(&root, &address).join("claim.json")).expect("claim"),
    )
    .expect("claim JSON");
    for pane in panes {
        let mut other = address.clone();
        other.pane_id = (*pane).to_owned();
        let mut copy = claim.clone();
        copy["address"] = json!(other);
        atomic_replace(&pane_path(&root, &other).join("claim.json"), &copy).expect("claim copy");
    }
}

/// One process listing answers for every claim. Each listing reads every
/// process's environment and takes tens of milliseconds, so one per claim
/// made doctor take seconds on a machine with a few hundred claims.
#[test]
fn doctor_takes_one_process_listing_however_many_claims() {
    let setup = Setup::new();
    setup.claim_and_bind();
    claim_more_panes(&setup, &["43", "44", "45"]);
    setup.panes.set(Vec::new());
    let probe = CountingListing::new();
    wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&probe),
    )
    .expect("doctor");
    assert_eq!(probe.single_looks.load(Ordering::SeqCst), 0);
    assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
}

fn probe_status(result: &Value, name: &str) -> Value {
    result["probes"]
        .as_array()
        .expect("probes")
        .iter()
        .find(|probe| probe["name"] == name)
        .unwrap_or_else(|| panic!("no {name} probe in {result}"))["status"]
        .clone()
}

/// With no state, no claims and no pane around it, doctor checked nothing,
/// and "healthy" would say it had. Each probe that found nothing to check
/// says unobserved, and the result lists it among the unobserved scopes.
#[test]
fn doctor_on_an_empty_setup_says_what_it_could_not_observe() {
    let setup = Setup::new();
    let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &BTreeMap::new(),
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("doctor");
    for name in [
        "permissions",
        "state_files",
        "socket",
        "processes",
        "environment",
    ] {
        assert_eq!(
            probe_status(&result, name),
            "unobserved",
            "{name}: {result}"
        );
        assert!(
            result["unobserved"]
                .as_array()
                .expect("unobserved")
                .contains(&json!(name)),
            "{name}: {result}"
        );
    }
    assert_eq!(probe_status(&result, "versions"), "healthy");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Run inside a pane whose server identity nothing has published, doctor
/// says so: that is the setup where hooks run and nothing ever shows.
#[test]
fn doctor_in_a_pane_nothing_has_published_reports_it() {
    let setup = Setup::new();
    let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("doctor");
    assert_eq!(probe_status(&result, "environment"), "finding", "{result}");
    assert!(
        diagnostics.iter().any(|d| d.code == "identity_unpublished"),
        "{diagnostics:?}"
    );

    setup.claim_and_bind();
    let (result, diagnostics) = wezterm_attention::maintenance::doctor_with_environment(
        &setup.root(),
        &setup.env,
        Some(&setup.panes),
        Some(&setup.processes),
    )
    .expect("doctor");
    assert_eq!(probe_status(&result, "environment"), "healthy", "{result}");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
