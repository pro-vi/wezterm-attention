//! What the state and the mux say about whether a pane is still there.
//!
//! This is the evidence the absence rule reads: `sweep` ends a binding and
//! removes an old pane's tree on it, and the readers report it as a pane's
//! presence. It lives apart from both so that they read one rule, and so do
//! the listings each of them takes at most once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::identity::{PaneAddress, socket_identity};
use crate::protocol::{AttentionError, Diagnostic, Result};
use crate::records::{RecordIdentity, read_record_at};
use crate::wezterm::{PaneLister, Presence, ProcessListing, ProcessProbe};

/// What the state and the mux say about one pane, before a caller decides
/// what to make of it.
pub(crate) enum PaneEvidence {
    /// `present`, `verified_absent` or `unavailable`, as a reader reports it.
    /// A pane whose server is shown to have exited reads `verified_absent`.
    Observed(String),
    /// The realm's socket path no longer serves this incarnation: the file
    /// is gone (`socket_gone`), holds another identity
    /// (`incarnation_changed`), or refuses connections (`socket_refused`),
    /// and nothing shows the server gone with it. It may still run with its
    /// socket removed, replaced or not accepting, so its records are kept. A
    /// reader reports the pane as unavailable, with the diagnostic, which
    /// says what became of the socket: no probe failed to answer.
    ServerGone { diagnostic: Diagnostic },
}

/// Whether a diagnostic says a pane's server may be gone, which
/// [`PaneEvidence::ServerGone`] carries: kept history, not a probe that did
/// not answer.
pub(crate) fn kept_history_code(code: &str) -> bool {
    matches!(
        code,
        "socket_gone" | "socket_refused" | "incarnation_changed"
    )
}

/// What the socket at a realm's recorded path says of the server that held
/// one of its incarnations.
pub(crate) enum RecordedServer {
    /// The socket still carries the incarnation.
    Current,
    /// It no longer does, for the reason given.
    Replaced(SocketChange),
    /// Its identity could not be read.
    Unreadable(AttentionError),
}

/// How a recorded socket stopped carrying its incarnation.
pub(crate) enum SocketChange {
    /// The socket file is gone.
    Gone,
    /// The path holds another identity.
    IdentityChanged,
}

impl SocketChange {
    /// What a reader reports of it when nothing shows the server gone.
    fn diagnostic(&self) -> Diagnostic {
        match self {
            Self::Gone => Diagnostic::new("socket_gone", "mux socket no longer exists"),
            Self::IdentityChanged => {
                Diagnostic::new("incarnation_changed", "realm socket identity changed")
            }
        }
    }
}

/// The server behind a pane's recorded incarnation, by the rule every reader
/// applies.
pub(crate) enum ServerState {
    /// Its socket still carries the incarnation.
    Current,
    /// It no longer does, and the pane is shown gone with it.
    Exited,
    /// It no longer does and nothing shows the server gone, so its records
    /// are kept history; the diagnostic says what became of the socket.
    Kept(Diagnostic),
    /// The socket's identity could not be read.
    Unreadable(AttentionError),
}

pub(crate) fn server_state(
    socket_path: &str,
    address: &PaneAddress,
    processes: Option<&dyn ProcessProbe>,
) -> ServerState {
    match recorded_server(socket_path, &address.realm_id, &address.incarnation_id) {
        RecordedServer::Current => ServerState::Current,
        RecordedServer::Replaced(_)
            if replaced_server_pane_gone(socket_path, &address.pane_id, processes) =>
        {
            ServerState::Exited
        }
        RecordedServer::Replaced(change) => ServerState::Kept(change.diagnostic()),
        RecordedServer::Unreadable(error) => ServerState::Unreadable(error),
    }
}

pub(crate) fn recorded_server(
    socket_path: &str,
    realm_id: &str,
    incarnation_id: &str,
) -> RecordedServer {
    match socket_identity(socket_path) {
        Ok((realm, incarnation, _)) if realm == realm_id && incarnation == incarnation_id => {
            RecordedServer::Current
        }
        Ok(_) => RecordedServer::Replaced(SocketChange::IdentityChanged),
        // Only a path that is not there at all. A socket that exists and
        // cannot be read says nothing about the server.
        Err(_)
            if fs::symlink_metadata(socket_path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            RecordedServer::Replaced(SocketChange::Gone)
        }
        // Something other than this user's socket now holds the path.
        Err(error) if error.diagnostic.code == "realm_unavailable" => {
            RecordedServer::Replaced(SocketChange::IdentityChanged)
        }
        Err(error) => RecordedServer::Unreadable(error),
    }
}

/// Whether a pane whose server's socket no longer carries its incarnation is
/// shown gone: the socket was a GUI's own and that GUI has exited, or the
/// process probe read every process and none carries the socket and pane id.
/// A process that carries it, one the probe could not read, or a probe that
/// did not answer shows nothing.
pub(crate) fn replaced_server_pane_gone(
    socket_path: &str,
    pane_id: &str,
    processes: Option<&dyn ProcessProbe>,
) -> bool {
    crate::wezterm::gui_process_exited(socket_path)
        || processes.is_some_and(|probe| probe.presence(socket_path, pane_id) == Presence::Absent)
}

/// A pane's presence as a reader reports it, and whether the server that
/// held its incarnation may be gone, which the report alone does not say.
pub(crate) fn reader_presence(
    root: &Path,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (String, bool) {
    match pane_evidence(root, address, panes, processes, diagnostics) {
        PaneEvidence::Observed(presence) => (presence, false),
        PaneEvidence::ServerGone { diagnostic } => {
            diagnostics.push(diagnostic);
            ("unavailable".to_owned(), true)
        }
    }
}

pub(crate) fn pane_evidence(
    root: &Path,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PaneEvidence {
    let unavailable = || PaneEvidence::Observed("unavailable".to_owned());
    let Some(panes) = panes else {
        return unavailable();
    };
    let socket_path = match recorded_socket(root, &address.realm_id, &address.incarnation_id) {
        Ok(Some(socket_path)) => socket_path,
        Ok(None) => return unavailable(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return unavailable();
        }
    };
    let socket_path = socket_path.as_str();
    match server_state(socket_path, address, processes) {
        ServerState::Current => {}
        ServerState::Exited => return PaneEvidence::Observed("verified_absent".to_owned()),
        ServerState::Kept(diagnostic) => return PaneEvidence::ServerGone { diagnostic },
        ServerState::Unreadable(error) => {
            diagnostics.push(error.diagnostic);
            return unavailable();
        }
    }
    presence_at_socket(socket_path, address, Some(panes), processes, diagnostics)
}

/// The socket a realm's record names, when both the realm and its
/// incarnation `incarnation_id` are recorded: the server identity a pane's
/// hooks publish, and every reader starts from.
pub(crate) fn recorded_socket(
    root: &Path,
    realm_id: &str,
    incarnation_id: &str,
) -> Result<Option<String>> {
    let Some(realm) = read_record_at(root, "realm", &RecordIdentity::realm(realm_id))? else {
        return Ok(None);
    };
    let incarnation = read_record_at(
        root,
        "incarnation",
        &RecordIdentity::incarnation(realm_id, incarnation_id),
    )?;
    Ok(incarnation.and(realm["socket_path"].as_str().map(str::to_owned)))
}

/// The socket `pane_evidence` would list for this address: the realm's
/// recorded socket, when it still carries this incarnation. None when it would
/// answer without listing.
pub(crate) fn realm_socket(root: &Path, address: &PaneAddress) -> Option<String> {
    let socket = recorded_socket(root, &address.realm_id, &address.incarnation_id).ok()??;
    matches!(
        recorded_server(&socket, &address.realm_id, &address.incarnation_id),
        RecordedServer::Current
    )
    .then_some(socket)
}

/// A pane's presence under a socket that carried its incarnation when last
/// looked at.
pub(crate) fn presence_at_socket(
    socket_path: &str,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PaneEvidence {
    let pane_id = address.pane_id.as_str();
    let observed = |presence: &str| PaneEvidence::Observed(presence.to_owned());
    let Some(panes) = panes else {
        diagnostics.push(Diagnostic::new(
            "probe_unavailable",
            "pane probe is unavailable",
        ));
        return observed("unavailable");
    };
    let listing = panes.list(socket_path);
    let error = match listing {
        Ok(rows) if rows.iter().any(|row| row.pane_id == pane_id) => return observed("present"),
        // The server answered and does not list the pane, which is what shows
        // it gone; the process listing is asked only whether a process still
        // carries it. One that could not read every process has still read
        // all it could, so a pair it did not see counts here as it does not
        // where the process listing is the only evidence.
        Ok(_) => {
            return match processes.map(|probe| probe.presence(socket_path, pane_id)) {
                Some(Presence::Present) => observed("present"),
                Some(Presence::Absent | Presence::Unseen) => observed("verified_absent"),
                _ => {
                    diagnostics.push(Diagnostic::new(
                        "probe_unavailable",
                        "identity-scoped process probe is unavailable",
                    ));
                    observed("unavailable")
                }
            };
        }
        Err(error) => error,
    };
    // The identity is read again after each look below: a file put in the
    // socket's place meanwhile would answer for its own server.
    let still_current = || {
        matches!(
            recorded_server(socket_path, &address.realm_id, &address.incarnation_id),
            RecordedServer::Current
        )
    };
    // A GUI that quit leaves its socket file behind, and its local panes
    // ended with it.
    if crate::wezterm::gui_process_exited(socket_path) && still_current() {
        return observed("verified_absent");
    }
    // A refusal says nothing listens now, not that the server exited: a
    // live server whose accept queue is full refuses, and so does one whose
    // listener stopped accepting while its panes run on. It is read as a
    // socket that is gone is: absent only on the same proof, and otherwise
    // kept, with no probe failed.
    if crate::wezterm::listener_refuses(socket_path) && still_current() {
        if replaced_server_pane_gone(socket_path, pane_id, processes) {
            return observed("verified_absent");
        }
        return PaneEvidence::ServerGone {
            diagnostic: Diagnostic::new("socket_refused", "mux socket refuses connections"),
        };
    }
    diagnostics.push(error.diagnostic);
    observed("unavailable")
}

/// Time spent inside the subprocesses one assembly spawned.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpawnSpend {
    pub(crate) pane_list: Duration,
    pub(crate) process_list: Duration,
}

/// Adds the time a call took to a shared total; a poisoned lock loses the
/// number rather than the answer.
fn record_spent<T>(spent: &Mutex<Duration>, call: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let answer = call();
    if let Ok(mut total) = spent.lock() {
        *total += started.elapsed();
    }
    answer
}

/// One pane listing per socket, rather than one per bound pane.
///
/// Resolving a binding's presence asks whether its pane id appears in the
/// socket's pane list. Every bound pane on one socket asks that of the same
/// list, and asking it afresh for each miss would spawn a `wezterm cli list`
/// subprocess per bound pane -- about 20 ms each on top of a 5 ms floor, paid
/// on every call.
/// The answers are memoised for the lifetime of one assembly and no longer, so
/// a later call still observes panes that opened or closed in between. A sweep
/// preview shares one across its steps; an apply does not, for the reason
/// given at [`ProbeOncePerAssembly`].
pub(crate) struct ListOncePerSocket<'a> {
    inner: &'a dyn PaneLister,
    listed: Mutex<BTreeMap<String, Result<Vec<crate::wezterm::PaneRow>>>>,
    spent: Mutex<Duration>,
}

impl<'a> ListOncePerSocket<'a> {
    pub(crate) fn new(inner: &'a dyn PaneLister) -> Self {
        Self {
            inner,
            listed: Mutex::new(BTreeMap::new()),
            spent: Mutex::new(Duration::ZERO),
        }
    }

    pub(crate) fn spent(&self) -> Duration {
        self.spent.lock().map(|spent| *spent).unwrap_or_default()
    }

    /// Lists every socket in `sockets` at once, one thread each, and keeps
    /// the answers for the per-pane asks that follow. A hung socket then costs
    /// one listing deadline for the whole query rather than one per socket.
    /// The time charged is the wall time of the batch, not the sum.
    pub(crate) fn list_together(&self, sockets: BTreeSet<String>) {
        let wanted: Vec<String> = match self.listed.lock() {
            Ok(listed) => sockets
                .into_iter()
                .filter(|socket| !listed.contains_key(socket))
                .collect(),
            Err(_) => return,
        };
        if wanted.len() < 2 {
            return;
        }
        let answers = record_spent(&self.spent, || {
            std::thread::scope(|scope| {
                let asks: Vec<_> = wanted
                    .iter()
                    .map(|socket| (socket, scope.spawn(|| self.inner.list(socket))))
                    .collect();
                asks.into_iter()
                    .filter_map(|(socket, ask)| Some((socket.clone(), ask.join().ok()?)))
                    .collect::<Vec<_>>()
            })
        });
        if let Ok(mut listed) = self.listed.lock() {
            listed.extend(answers);
        }
    }
}

impl PaneLister for ListOncePerSocket<'_> {
    fn list(&self, socket_path: &str) -> Result<Vec<crate::wezterm::PaneRow>> {
        // A poisoned lock would mean a panic inside `list`; fall back to the
        // uncached path rather than propagating a panic through a read command.
        let Ok(mut listed) = self.listed.lock() else {
            return record_spent(&self.spent, || self.inner.list(socket_path));
        };
        if let Some(cached) = listed.get(socket_path) {
            return cached.clone();
        }
        let answer = record_spent(&self.spent, || self.inner.list(socket_path));
        listed.insert(socket_path.to_owned(), answer.clone());
        answer
    }
}

/// One process listing per assembly, rather than one per absent pane.
///
/// A bound pane missing from the mux listing is looked for among live
/// processes, and one look reads the environment of every process this user
/// runs, which takes tens of milliseconds; a look per pane would make a store
/// with many ended panes slow on every call. The listing is taken on the first
/// miss, or when asked whether the probe is available, and kept for the
/// lifetime of one assembly and no longer. A listing that failed is kept the
/// same way, and answers every later miss as unavailable: asking the probe
/// pane by pane would run the failed listing once per pane. Doctor shares one
/// across its checks, and a sweep preview across its steps. A sweep apply does
/// not use this: it acts on the answer, so it keeps a fresh look per decision.
pub(crate) struct ProbeOncePerAssembly<'a> {
    inner: &'a dyn ProcessProbe,
    listed: Mutex<Option<ProcessListing>>,
    spent: Mutex<Duration>,
}

impl<'a> ProbeOncePerAssembly<'a> {
    pub(crate) fn new(inner: &'a dyn ProcessProbe) -> Self {
        Self {
            inner,
            listed: Mutex::new(None),
            spent: Mutex::new(Duration::ZERO),
        }
    }

    pub(crate) fn spent(&self) -> Duration {
        self.spent.lock().map(|spent| *spent).unwrap_or_default()
    }
}

impl ProcessProbe for ProbeOncePerAssembly<'_> {
    /// Answered from the kept listing when the probe offers one, so asking
    /// costs no second listing.
    fn available(&self) -> bool {
        let Ok(mut listed) = self.listed.lock() else {
            return self.inner.available();
        };
        match listed
            .get_or_insert_with(|| record_spent(&self.spent, || self.inner.pane_processes()))
        {
            ProcessListing::Listed(_) => true,
            ProcessListing::Failed => false,
            ProcessListing::NotOffered => self.inner.available(),
        }
    }

    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        // A poisoned lock would mean a panic inside the listing; fall back to
        // the uncached path rather than propagating it through a read command.
        let Ok(mut listed) = self.listed.lock() else {
            return record_spent(&self.spent, || self.inner.presence(socket_path, pane_id));
        };
        match listed
            .get_or_insert_with(|| record_spent(&self.spent, || self.inner.pane_processes()))
        {
            ProcessListing::Listed(processes) => processes.presence(socket_path, pane_id),
            ProcessListing::Failed => Presence::Unavailable,
            ProcessListing::NotOffered => {
                record_spent(&self.spent, || self.inner.presence(socket_path, pane_id))
            }
        }
    }
}

/// An apply's pane lister: a listing that failed answers every later ask
/// about that socket for the rest of the run, and one that answered is taken
/// fresh each time. A failed listing leaves a pane undecided and so removes
/// nothing, while each ask against a mux that accepts and never answers
/// waits out the listing deadline; asking once per pane would make an apply's
/// wait grow with the panes on that socket.
pub(crate) struct FailedListingOncePerSocket<'a> {
    inner: &'a dyn PaneLister,
    failed: Mutex<BTreeMap<String, AttentionError>>,
}

impl<'a> FailedListingOncePerSocket<'a> {
    pub(crate) fn new(inner: &'a dyn PaneLister) -> Self {
        Self {
            inner,
            failed: Mutex::new(BTreeMap::new()),
        }
    }
}

impl PaneLister for FailedListingOncePerSocket<'_> {
    fn list(&self, socket_path: &str) -> Result<Vec<crate::wezterm::PaneRow>> {
        if let Some(error) = self
            .failed
            .lock()
            .ok()
            .and_then(|failed| failed.get(socket_path).cloned())
        {
            return Err(error);
        }
        let answer = self.inner.list(socket_path);
        if let (Err(error), Ok(mut failed)) = (&answer, self.failed.lock()) {
            failed.insert(socket_path.to_owned(), error.clone());
        }
        answer
    }
}

/// The pane and process listings one assembly asks, each taken at most once
/// for that assembly and no longer, with the time spent taking them.
pub(crate) struct AssemblyListings<'a> {
    panes: Option<ListOncePerSocket<'a>>,
    processes: Option<ProbeOncePerAssembly<'a>>,
}

impl<'a> AssemblyListings<'a> {
    pub(crate) fn new(
        panes: Option<&'a dyn PaneLister>,
        processes: Option<&'a dyn ProcessProbe>,
    ) -> Self {
        Self {
            panes: panes.map(ListOncePerSocket::new),
            processes: processes.map(ProbeOncePerAssembly::new),
        }
    }

    pub(crate) fn panes(&self) -> Option<&dyn PaneLister> {
        self.panes.as_ref().map(|lister| lister as &dyn PaneLister)
    }

    pub(crate) fn processes(&self) -> Option<&dyn ProcessProbe> {
        self.processes
            .as_ref()
            .map(|probe| probe as &dyn ProcessProbe)
    }

    /// See [`ListOncePerSocket::list_together`].
    pub(crate) fn list_together(&self, sockets: BTreeSet<String>) {
        if let Some(panes) = &self.panes {
            panes.list_together(sockets);
        }
    }

    pub(crate) fn spent(&self) -> SpawnSpend {
        SpawnSpend {
            pane_list: self
                .panes
                .as_ref()
                .map(ListOncePerSocket::spent)
                .unwrap_or_default(),
            process_list: self
                .processes
                .as_ref()
                .map(ProbeOncePerAssembly::spent)
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod process_probe_tests {
    use super::*;
    use crate::wezterm::PaneProcessSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProbe {
        listing: Listing,
        listings: AtomicUsize,
        single_looks: AtomicUsize,
    }

    impl CountingProbe {
        fn new(listing: Listing) -> Self {
            Self {
                listing,
                listings: AtomicUsize::new(0),
                single_looks: AtomicUsize::new(0),
            }
        }
    }

    impl ProcessProbe for CountingProbe {
        fn available(&self) -> bool {
            true
        }

        fn presence(&self, _socket_path: &str, _pane_id: &str) -> Presence {
            self.single_looks.fetch_add(1, Ordering::SeqCst);
            Presence::Absent
        }

        fn pane_processes(&self) -> ProcessListing {
            self.listings.fetch_add(1, Ordering::SeqCst);
            match self.listing {
                Listing::Lists(listing) => {
                    ProcessListing::Listed(PaneProcessSet::from_process_listing(listing))
                }
                Listing::Fails => ProcessListing::Failed,
                Listing::Never => ProcessListing::NotOffered,
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Listing {
        Lists(&'static str),
        Fails,
        Never,
    }

    #[test]
    fn many_absent_panes_cost_one_process_listing() {
        let probe = CountingProbe::new(Listing::Lists(
            "zsh WEZTERM_UNIX_SOCKET=/mux.sock WEZTERM_PANE=9",
        ));
        let once = ProbeOncePerAssembly::new(&probe);
        for pane in ["1", "2", "3"] {
            assert_eq!(once.presence("/mux.sock", pane), Presence::Absent);
        }
        assert_eq!(once.presence("/mux.sock", "9"), Presence::Present);
        assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
        assert_eq!(probe.single_looks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_probe_with_no_listing_is_asked_once_for_one_and_then_pane_by_pane() {
        let probe = CountingProbe::new(Listing::Never);
        let once = ProbeOncePerAssembly::new(&probe);
        for pane in ["1", "2", "3"] {
            assert_eq!(once.presence("/mux.sock", pane), Presence::Absent);
        }
        assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
        assert_eq!(probe.single_looks.load(Ordering::SeqCst), 3);
    }

    /// A listing that failed is not retried pane by pane: the system probe
    /// answers a per-pane question by taking the same listing again, so with
    /// a hundred absent panes one failed listing would be taken a hundred
    /// times. Every pane is unavailable instead, and the listing is taken
    /// once.
    #[test]
    fn a_failed_listing_answers_every_pane_as_unavailable_without_asking_again() {
        let probe = CountingProbe::new(Listing::Fails);
        let once = ProbeOncePerAssembly::new(&probe);
        for pane in ["1", "2", "3"] {
            assert_eq!(once.presence("/mux.sock", pane), Presence::Unavailable);
        }
        assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
        assert_eq!(probe.single_looks.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod pane_listing_tests {
    use super::*;
    use crate::wezterm::PaneRow;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingLister {
        calls: AtomicUsize,
    }

    impl PaneLister for CountingLister {
        fn list(&self, socket_path: &str) -> Result<Vec<PaneRow>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![PaneRow {
                pane_id: socket_path.to_owned(),
                tty_name: None,
            }])
        }
    }

    #[test]
    fn one_socket_is_listed_once_however_many_panes_ask() {
        let counting = CountingLister {
            calls: AtomicUsize::new(0),
        };
        let once = ListOncePerSocket::new(&counting);
        // Three bound panes on one socket ask the same question of the same
        // list. Before this wrapper each ask spawned its own `wezterm cli list`.
        for _ in 0..3 {
            assert_eq!(once.list("/s/one").unwrap()[0].pane_id, "/s/one");
        }
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);

        // A second socket is a different question and is asked once more.
        assert_eq!(once.list("/s/two").unwrap()[0].pane_id, "/s/two");
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_listing_is_remembered_rather_than_retried_per_pane() {
        struct AlwaysFails {
            calls: AtomicUsize,
        }
        impl PaneLister for AlwaysFails {
            fn list(&self, _socket_path: &str) -> Result<Vec<PaneRow>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(AttentionError::new("probe_unavailable", "socket is gone"))
            }
        }
        let failing = AlwaysFails {
            calls: AtomicUsize::new(0),
        };
        let once = ListOncePerSocket::new(&failing);
        for _ in 0..3 {
            assert_eq!(
                once.list("/s/one").unwrap_err().diagnostic.code,
                "probe_unavailable"
            );
        }
        // Every pane on an unreachable socket reports the same failure, and one
        // failed subprocess is enough to establish it.
        assert_eq!(failing.calls.load(Ordering::SeqCst), 1);
    }
}
