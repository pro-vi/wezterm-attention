//! Hook writers wait at most 2 s for a pane's locks. Sweep must not hold them
//! while it waits on a subprocess that can take 5 s.

use super::*;

/// Lists no panes, and notes every time it was asked while either of the
/// pane's locks was held.
pub(super) struct LockCheckingPanes {
    pub(super) locks: Vec<PathBuf>,
    pub(super) asked_under_lock: AtomicU64,
    pub(super) asked: AtomicU64,
}

impl LockCheckingPanes {
    pub(super) fn for_setup(setup: &Setup) -> Self {
        let root = setup.root();
        let (address, _) = pane_address(&setup.env).expect("address");
        let launch = launch_path(&root, &address, &setup.env["WEZTERM_ATTENTION_LAUNCH_ID"]);
        Self {
            locks: vec![
                launch.join(".lock"),
                pane_path(&root, &address).join(".claim.lock"),
            ],
            asked_under_lock: AtomicU64::new(0),
            asked: AtomicU64::new(0),
        }
    }
}

impl PaneLister for LockCheckingPanes {
    fn list(&self, _socket_path: &str) -> wezterm_attention::protocol::Result<Vec<PaneRow>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let held = self
            .locks
            .iter()
            .any(|lock| with_lock(lock, std::time::Duration::ZERO, || Ok(())).is_err());
        if held {
            self.asked_under_lock.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Vec::new())
    }
}

#[test]
fn sweep_apply_never_lists_panes_while_holding_a_pane_lock() {
    let setup = Setup::new();
    setup.claim_and_bind();
    setup.processes.set(Presence::Absent);
    let panes = LockCheckingPanes::for_setup(&setup);
    let (result, _) = sweep(
        &setup.root(),
        None,
        true,
        Some("00000000-0000-4000-8000-000000000903"),
        &setup.clock,
        &panes,
        Some(&setup.processes),
    )
    .expect("sweep");
    assert!(
        result
            .details
            .iter()
            .any(|detail| detail["action"] == "first_absence"),
        "{:?}",
        result.details
    );
    assert!(panes.asked.load(Ordering::SeqCst) > 0);
    assert_eq!(panes.asked_under_lock.load(Ordering::SeqCst), 0);
}
