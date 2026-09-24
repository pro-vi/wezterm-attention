//! What doctor asks of the machine, and what it says when it could not ask.

use super::*;
use std::sync::atomic::AtomicUsize;
use wezterm_attention::wezterm::{PaneProcessSet, ProcessListing};

/// Offers one listing of every process, and counts how it is asked.
struct CountingListing {
    listings: AtomicUsize,
    single_looks: AtomicUsize,
}

impl ProcessProbe for CountingListing {
    fn available(&self) -> bool {
        true
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
    let probe = CountingListing {
        listings: AtomicUsize::new(0),
        single_looks: AtomicUsize::new(0),
    };
    doctor(&setup.root(), Some(&setup.panes), Some(&probe)).expect("doctor");
    assert_eq!(probe.single_looks.load(Ordering::SeqCst), 0);
    assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
}
