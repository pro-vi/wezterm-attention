use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::PaneAddress;
use crate::identity::socket_identity;
use crate::observations::{LifecycleAvailability, LifecycleSnapshot, LifecycleView};
use crate::protocol::{AttentionError, Diagnostic, Result};
use crate::records::{
    FileRecords, RecordReader, ends_binding, launch_path, pane_path, session_dir,
    session_entry_path, session_index_path,
};
use crate::records::{RecordIdentity, RecordRead, read_record, read_record_typed};
use crate::wezterm::Clock;
use crate::wezterm::{GuiWindowLister, PaneLister, Presence, ProcessListing, ProcessProbe};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PaneScope {
    address: PaneAddress,
    launch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binding_id: Option<String>,
}

impl PaneScope {
    pub fn new(
        address: PaneAddress,
        launch_id: String,
        binding_id: Option<String>,
    ) -> Result<Self> {
        let wire = serde_json::json!({"wire":2,"address":address,"launch_id":launch_id});
        if crate::protocol::parse_wire_value(&wire, crate::protocol::manifest()?)
            != crate::protocol::Verdict::Valid
            || binding_id.as_ref().is_some_and(|id| {
                id.len() != 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(AttentionError::usage(
                "scope requires a canonical address, launch UUID and optional binding ID",
            ));
        }
        Ok(Self {
            address,
            launch_id,
            binding_id,
        })
    }
    pub fn address(&self) -> &PaneAddress {
        &self.address
    }
    pub fn launch_id(&self) -> &str {
        &self.launch_id
    }
    pub fn binding_id(&self) -> Option<&str> {
        self.binding_id.as_deref()
    }
}

impl<'de> Deserialize<'de> for PaneScope {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            address: Value,
            launch_id: String,
            binding_id: Option<String>,
        }
        let input = Input::deserialize(deserializer)?;
        let wire =
            serde_json::json!({"wire":2,"address":input.address,"launch_id":input.launch_id});
        let protocol = crate::protocol::manifest().map_err(serde::de::Error::custom)?;
        if crate::protocol::parse_wire_value(&wire, protocol) != crate::protocol::Verdict::Valid {
            return Err(serde::de::Error::custom("invalid scope address or launch"));
        }
        let address = serde_json::from_value(input.address).map_err(serde::de::Error::custom)?;
        Self::new(address, input.launch_id, input.binding_id).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRelation {
    Matched,
    LaunchChanged,
    BindingChanged,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordAvailability {
    Present,
    Absent,
    Cleared,
    Expired,
    Unavailable,
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordFacet {
    pub availability: RecordAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

impl RecordFacet {
    fn empty(availability: RecordAvailability) -> Self {
        Self {
            availability,
            record: None,
            diagnostics: vec![],
        }
    }
    fn read(
        reader: &dyn RecordReader,
        path: &Path,
        kind: &str,
        identity: &RecordIdentity,
        facet: &str,
    ) -> Self {
        let (availability, record, error) = match reader.read(path, Some(kind), identity) {
            RecordRead::Present(record) => (RecordAvailability::Present, Some(record), None),
            RecordRead::Missing => (RecordAvailability::Absent, None, None),
            RecordRead::Unavailable(error) => (RecordAvailability::Unavailable, None, Some(error)),
            RecordRead::Invalid(error) => (RecordAvailability::Invalid, None, Some(error)),
            RecordRead::Unsupported(error) => (RecordAvailability::Unsupported, None, Some(error)),
        };
        let diagnostics = error
            .into_iter()
            .map(|mut error| {
                error
                    .diagnostic
                    .context
                    .insert("facet".into(), Value::String(facet.into()));
                error.diagnostic
            })
            .collect();
        Self {
            availability,
            record,
            diagnostics,
        }
    }
    fn failed(&self) -> bool {
        matches!(
            self.availability,
            RecordAvailability::Unavailable
                | RecordAvailability::Invalid
                | RecordAvailability::Unsupported
        )
    }
    /// The diagnostic code of a failed read, or None when the read did not fail.
    fn failure_code(&self) -> Option<&str> {
        if !self.failed() {
            return None;
        }
        Some(
            self.diagnostics
                .first()
                .map_or("record_invalid", |diagnostic| diagnostic.code.as_str()),
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceCollection {
    pub availability: RecordAvailability,
    pub count: usize,
    pub evidence: Vec<Value>,
    pub coverage: EvidenceCoverage,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eligibility: Option<ChildEligibility>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChildEligibility {
    pub ttl_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_mono_ns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor_mono_ns: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCoverage {
    EligibleRecords,
}

impl EvidenceCollection {
    fn empty(availability: RecordAvailability) -> Self {
        Self {
            availability,
            count: 0,
            evidence: vec![],
            coverage: EvidenceCoverage::EligibleRecords,
            diagnostics: vec![],
            eligibility: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PaneFacts {
    pub scope: PaneScope,
    pub scope_relation: ScopeRelation,
    pub binding: Option<BindingRow>,
    pub pane_presence: PanePresence,
    pub reader_confidence: ReaderConfidence,
    pub binding_health: BindingHealth,
    pub activity: RecordFacet,
    pub binding_end: RecordFacet,
    pub children: EvidenceCollection,
    pub review: EvidenceCollection,
    pub lifecycle: LifecycleView,
    pub diagnostics: Vec<Diagnostic>,
    /// Where the call spent its time, in the shape a bindings answer uses.
    pub timing_ms: BindingTiming,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PanePresence {
    Present,
    VerifiedAbsent,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReaderConfidence {
    Confirmed,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BindingHealth {
    Valid,
    Invalid,
    FutureSchema,
    Conflicted,
}

impl BindingHealth {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::FutureSchema => "future_schema",
            Self::Conflicted => "conflicted",
        }
    }
}

impl ReaderConfidence {
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// A binding's health, one rule for `bindings` and `inspect` so a row reads
/// the same through both. It rests only on the records that make the row: the
/// binding record, its end record, the pane's claim and the launch's
/// current-binding pointer, the first failed read deciding, and on whether the
/// provider session is live at another pane address, which outranks them. An
/// unreadable review, activity or child record is a diagnostic about that
/// record, not about the binding.
fn binding_health(failed_reads: [Option<&str>; 4], conflicted: bool) -> BindingHealth {
    if conflicted {
        return BindingHealth::Conflicted;
    }
    match failed_reads.into_iter().flatten().next() {
        None => BindingHealth::Valid,
        Some("future_schema") => BindingHealth::FutureSchema,
        Some(_) => BindingHealth::Invalid,
    }
}

/// A reader can act on a row when it is the pane's current binding and the
/// pane was seen. The same rule for `bindings` and `inspect`.
fn reader_confidence(current: bool, presence: &str) -> ReaderConfidence {
    if current && presence == "present" {
        ReaderConfidence::Confirmed
    } else {
        ReaderConfidence::Unconfirmed
    }
}

/// Whether a row takes part in the provider-session conflict check. Only live
/// claims compete: a binding that has ended, whose pane is verified absent, or
/// whose server is gone (a new server owns its socket path, or the path is
/// gone) is history. A session resumed in a new pane, or under a restarted
/// mux, leaves one behind every time, and calling that a conflict hides the
/// pane the session actually runs in.
fn competes(ended: bool, server_gone: bool, presence: &str) -> bool {
    !ended && !server_gone && presence != "verified_absent"
}

impl PaneFacts {
    /// An answer with no row: the scope did not match, or a record that makes
    /// the row could not be read. `failed` is that read's diagnostic code, if
    /// one failed, and decides health by the rule a bindings row follows.
    fn unavailable(
        scope: &PaneScope,
        relation: ScopeRelation,
        diagnostics: Vec<Diagnostic>,
        failed: Option<&str>,
    ) -> Self {
        Self {
            scope: scope.clone(),
            scope_relation: relation,
            binding: None,
            pane_presence: PanePresence::Unavailable,
            reader_confidence: ReaderConfidence::Unconfirmed,
            binding_health: binding_health([failed, None, None, None], false),
            activity: RecordFacet::empty(RecordAvailability::Unavailable),
            binding_end: RecordFacet::empty(RecordAvailability::Unavailable),
            children: EvidenceCollection::empty(RecordAvailability::Unavailable),
            review: EvidenceCollection::empty(RecordAvailability::Unavailable),
            lifecycle: LifecycleView::empty(LifecycleAvailability::Unavailable),
            diagnostics,
            timing_ms: BindingTiming::default(),
        }
    }
    pub fn complete(&self) -> bool {
        self.scope_relation == ScopeRelation::Matched && self.diagnostics.is_empty()
    }
}

fn facet_diagnostic(code: &str, facet: &str, message: &str) -> Diagnostic {
    let mut diagnostic = diagnostic(code, message);
    diagnostic
        .context
        .insert("facet".into(), Value::String(facet.into()));
    diagnostic
}

pub fn read_pane_facts(root: &Path, scope: &PaneScope) -> Result<PaneFacts> {
    read_pane_facts_with_ports(
        root,
        scope,
        &FileRecords,
        &crate::wezterm::SystemClock,
        Some(&crate::wezterm::WeztermPaneLister),
        Some(&crate::wezterm::SystemProcessProbe),
    )
}

pub fn read_pane_facts_with_ports(
    root: &Path,
    scope: &PaneScope,
    reader: &dyn RecordReader,
    clock: &dyn Clock,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<PaneFacts> {
    let started = Instant::now();
    let listed_once = panes.map(ListOncePerSocket::new);
    let probed_once = processes.map(ProbeOncePerAssembly::new);
    let mut facts = read_pane_facts_once(
        root,
        scope,
        reader,
        clock,
        listed_once.as_ref().map(|lister| lister as &dyn PaneLister),
        probed_once.as_ref().map(|probe| probe as &dyn ProcessProbe),
    )?;
    facts.timing_ms = BindingTiming::from_wall(
        started,
        SpawnSpend {
            pane_list: listed_once
                .as_ref()
                .map(ListOncePerSocket::spent)
                .unwrap_or_default(),
            process_list: probed_once
                .as_ref()
                .map(ProbeOncePerAssembly::spent)
                .unwrap_or_default(),
        },
    );
    Ok(facts)
}

/// One inspection, with ports that list each socket and take the process
/// listing at most once, so the rival lookup reuses what presence asked.
fn read_pane_facts_once(
    root: &Path,
    scope: &PaneScope,
    reader: &dyn RecordReader,
    clock: &dyn Clock,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<PaneFacts> {
    use RecordAvailability as A;
    // Scope is constructed/decoded through validation before any record or process I/O.
    let address = &scope.address;
    let realm_root = root.join("v2/realms").join(&address.realm_id);
    let realm = RecordFacet::read(
        reader,
        &realm_root.join("realm.json"),
        "realm",
        &RecordIdentity::realm(&address.realm_id),
        "realm",
    );
    let incarnation = RecordFacet::read(
        reader,
        &realm_root
            .join("incarnations")
            .join(&address.incarnation_id)
            .join("incarnation.json"),
        "incarnation",
        &RecordIdentity::incarnation(&address.realm_id, &address.incarnation_id),
        "incarnation",
    );
    let mut diagnostics = [realm.diagnostics.clone(), incarnation.diagnostics.clone()].concat();
    let socket = realm
        .record
        .as_ref()
        .and_then(|r| r["socket_path"].as_str());
    let Some(socket) = socket.filter(|_| incarnation.record.is_some()) else {
        diagnostics.push(facet_diagnostic(
            "identity_unpublished",
            "scope",
            "requested server identity is not available",
        ));
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            diagnostics,
            None,
        ));
    };
    // Whether the scope's server still owns the socket, by the rule every
    // reader applies. True when it is shown to have exited, so the pane is
    // absent and the records are still the latest word about it.
    let check_socket = || -> std::result::Result<bool, Diagnostic> {
        match recorded_server(socket, &address.realm_id, &address.incarnation_id) {
            RecordedServer::Current => Ok(false),
            RecordedServer::Replaced(_)
                if replaced_server_pane_gone(socket, &address.pane_id, processes) =>
            {
                Ok(true)
            }
            RecordedServer::Replaced(diagnostic) => Err(diagnostic),
            RecordedServer::Unreadable(error) => Err(error.diagnostic),
        }
    };
    let server_exited = match check_socket() {
        Ok(exited) => exited,
        Err(mut diagnostic) => {
            diagnostic
                .context
                .insert("facet".into(), Value::String("scope".into()));
            return Ok(PaneFacts::unavailable(
                scope,
                ScopeRelation::Unavailable,
                vec![diagnostic],
                None,
            ));
        }
    };
    let pane = pane_path(root, address);
    let launch = launch_path(root, address, &scope.launch_id);
    let claim = RecordFacet::read(
        reader,
        &pane.join("claim.json"),
        "claim",
        &RecordIdentity::pane(address),
        "claim",
    );
    if claim.record.is_none() {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            if claim.diagnostics.is_empty() {
                vec![facet_diagnostic(
                    "identity_unpublished",
                    "claim",
                    "claim is absent",
                )]
            } else {
                claim.diagnostics.clone()
            },
            claim.failure_code(),
        ));
    }
    if claim.record.as_ref().and_then(|r| r["launch_id"].as_str()) != Some(&scope.launch_id) {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::LaunchChanged,
            vec![facet_diagnostic(
                "claim_stale",
                "claim",
                "requested launch is no longer current",
            )],
            None,
        ));
    }
    let pointer = RecordFacet::read(
        reader,
        &launch.join("current-binding.json"),
        "current_binding",
        &RecordIdentity::launch(address, &scope.launch_id),
        "binding_selection",
    );
    if pointer.failed() {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            pointer.diagnostics.clone(),
            pointer.failure_code(),
        ));
    }
    let selected = pointer
        .record
        .as_ref()
        .and_then(|r| r["binding_id"].as_str());
    if scope
        .binding_id
        .as_deref()
        .is_some_and(|expected| Some(expected) != selected)
    {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::BindingChanged,
            vec![facet_diagnostic(
                "claim_stale",
                "binding_selection",
                "requested binding is no longer current",
            )],
            None,
        ));
    }
    let selected_root = selected
        .map(|id| launch.join("bindings").join(id))
        .unwrap_or_else(|| launch.clone());
    let identity = selected
        .map(|id| RecordIdentity::binding(address, &scope.launch_id, id))
        .unwrap_or_else(|| RecordIdentity::launch(address, &scope.launch_id));
    let binding = if selected.is_some() {
        RecordFacet::read(
            reader,
            &selected_root.join("binding.json"),
            "binding",
            &identity,
            "binding",
        )
    } else {
        RecordFacet::empty(A::Absent)
    };
    diagnostics.extend(binding.diagnostics.clone());
    if selected.is_some() && binding.record.is_none() {
        if !binding.failed() {
            diagnostics.push(facet_diagnostic(
                "record_invalid",
                "binding",
                "selected binding record is absent",
            ));
        }
        let failed = binding.failure_code().unwrap_or("record_invalid");
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Matched,
            diagnostics,
            Some(failed),
        ));
    }
    let now = match clock.unix_ns20() {
        Ok(value) if value.len() == 20 && value.bytes().all(|b| b.is_ascii_digit()) => Some(value),
        _ => {
            diagnostics.push(facet_diagnostic(
                "probe_unavailable",
                "clock",
                "inspection UTC is unavailable",
            ));
            None
        }
    };
    let raw_activity = RecordFacet::read(
        reader,
        &selected_root.join("activity.json"),
        "activity",
        &identity,
        "activity",
    );
    let target = selected
        .map(|id| serde_json::json!({"kind":"binding","binding_id":id}))
        .unwrap_or_else(|| serde_json::json!({"kind":"launch"}));
    let mut activity = raw_activity.clone();
    if activity
        .record
        .as_ref()
        .is_some_and(|r| r["target"] != target)
    {
        activity = RecordFacet::empty(A::Invalid);
        activity.diagnostics.push(facet_diagnostic(
            "record_invalid",
            "activity",
            "activity target differs from selected scope",
        ));
    }
    let clear = if selected.is_some() {
        RecordFacet::read(
            reader,
            &selected_root.join("activity-clear.json"),
            "activity_clear",
            &identity,
            "activity_clear",
        )
    } else {
        RecordFacet::empty(A::Absent)
    };
    if !activity.failed() {
        if clear.failed() {
            activity = clear.clone();
        } else if clear.record.as_ref().is_some_and(|clear| {
            activity.record.as_ref().is_none_or(|record| {
                record["observed_mono_ns"].as_str() <= clear["observed_mono_ns"].as_str()
            })
        }) {
            activity = clear.clone();
            activity.availability = A::Cleared;
        } else if let Some(record) = &activity.record
            && let Some(ttl) = record["ttl_ms"].as_u64()
        {
            match now.as_deref().zip(record["written_at_unix_ns"].as_str()) {
                Some((now, written)) if now >= written => {
                    if now.parse::<u128>().unwrap()
                        > written.parse::<u128>().unwrap() + u128::from(ttl) * 1_000_000
                    {
                        activity.availability = A::Expired;
                    }
                }
                _ => {
                    activity = RecordFacet::empty(A::Unavailable);
                    activity.diagnostics.push(facet_diagnostic(
                        "clock_skew",
                        "activity",
                        "activity age is unavailable or negative",
                    ));
                }
            }
        }
    }
    let mut end = if selected.is_some() {
        RecordFacet::read(
            reader,
            &selected_root.join("end.json"),
            "binding_end",
            &identity,
            "binding_end",
        )
    } else {
        RecordFacet::empty(A::Absent)
    };
    if end
        .record
        .as_ref()
        .zip(binding.record.as_ref())
        .is_some_and(|(end, binding)| !ends_binding(end, binding))
    {
        end = RecordFacet::empty(A::Absent);
    }
    let mut lifecycle = if selected.is_some() {
        let read = RecordFacet::read(
            reader,
            &selected_root.join("lifecycle.json"),
            "lifecycle_snapshot",
            &identity,
            "lifecycle",
        );
        lifecycle_from_read(
            read,
            binding.record.as_ref().and_then(|r| r["provider"].as_str()),
            now.as_deref(),
        )?
    } else {
        LifecycleView::empty(LifecycleAvailability::Absent)
    };
    let ack = RecordFacet::read(
        reader,
        &selected_root.join("ack.json"),
        "acknowledgement",
        &identity,
        "badge_acknowledgement",
    );
    if let Some(ack_record) = &ack.record
        && let Some(raw) = &raw_activity.record
        && ack_record["target"] == target
        && raw["target"] == target
        && ack_record["activity_event_id"] == raw["event_id"]
    {
        lifecycle.badge_acknowledgement = Some(
            serde_json::json!({"activity_event_id":ack_record["activity_event_id"],"event_id":ack_record["event_id"],"target":ack_record["target"]}),
        );
    }
    let review = read_fact_collection(reader, &pane.join("reviews"), scope, None, now.as_deref());
    let children = if let Some(selected_binding) = selected {
        let child_clear = RecordFacet::read(
            reader,
            &selected_root.join("agents-clear.json"),
            "subagent_clear",
            &identity,
            "children",
        );
        let floor = RecordFacet::read(
            reader,
            &selected_root.join("agents-floor.json"),
            "subagent_retention_floor",
            &identity,
            "children",
        );
        if child_clear.failed() || floor.failed() {
            let mut result = EvidenceCollection::empty(if child_clear.failed() {
                child_clear.availability
            } else {
                floor.availability
            });
            result.diagnostics = [child_clear.diagnostics, floor.diagnostics].concat();
            result
        } else {
            read_fact_collection(
                reader,
                &selected_root.join("agents"),
                scope,
                Some(ChildSelection {
                    binding_id: selected_binding,
                    provider: binding
                        .record
                        .as_ref()
                        .and_then(|r| r["provider"].as_str())
                        .unwrap(),
                    clear: &child_clear,
                    floor: &floor,
                }),
                now.as_deref(),
            )
        }
    } else {
        EvidenceCollection::empty(A::Absent)
    };
    for items in [
        &activity.diagnostics,
        &clear.diagnostics,
        &end.diagnostics,
        &ack.diagnostics,
        &review.diagnostics,
        &children.diagnostics,
    ] {
        diagnostics.extend(items.clone());
    }
    diagnostics.extend(lifecycle.diagnostics.clone());
    let before_presence = diagnostics.len();
    let presence = if server_exited {
        "verified_absent".to_owned()
    } else {
        match presence_at_socket(socket, address, panes, processes, &mut diagnostics) {
            PaneEvidence::Observed(presence) => presence,
            // A socket that refuses is answered as one that is gone or
            // replaced: the scope's server may no longer be the one there.
            PaneEvidence::ServerGone { mut diagnostic } => {
                diagnostic
                    .context
                    .insert("facet".into(), Value::String("scope".into()));
                return Ok(PaneFacts::unavailable(
                    scope,
                    ScopeRelation::Unavailable,
                    vec![diagnostic],
                    None,
                ));
            }
        }
    };
    for item in &mut diagnostics[before_presence..] {
        item.context
            .insert("facet".into(), Value::String("pane_presence".into()));
    }
    // The claim names this launch and the pointer this binding, or the scope
    // would not have matched, so the row is the pane's current one.
    let confidence = reader_confidence(true, &presence);
    let ended = end.availability == A::Present;
    // A scope whose server may be gone was answered above, so the one left
    // here is live or shown exited, as `bindings` would say of this row.
    let server_gone = false;
    let conflicted = binding.record.as_ref().is_some_and(|record| {
        competes(ended, server_gone, &presence)
            && session_live_elsewhere(
                root,
                address,
                &string(record, "provider").unwrap_or_default(),
                &string(record, "provider_session_id").unwrap_or_default(),
                panes,
                processes,
            )
    });
    let health = binding_health([None, end.failure_code(), None, None], conflicted);
    let row = binding.record.as_ref().map(|record| BindingRow {
        address: address.clone(),
        launch_id: scope.launch_id.clone(),
        binding_id: selected.unwrap().into(),
        provider: string(record, "provider").unwrap(),
        provider_session_id: string(record, "provider_session_id").unwrap(),
        binding_phase: if ended { "ended" } else { "active" }.into(),
        pane_presence: presence.clone(),
        reader_confidence: confidence.as_str().into(),
        binding_health: health.as_str().into(),
        current: true,
        expected_session_match: string(record, "expected_session_id")
            .map(|v| Some(v) == string(record, "provider_session_id")),
        expected_session_id: string(record, "expected_session_id"),
        transcript_path: string(record, "transcript_path"),
        cwd: string(record, "cwd"),
        config_dir: string(record, "config_dir"),
        model: string(record, "model"),
        start_source: string(record, "start_source"),
    });
    let after_claim = RecordFacet::read(
        reader,
        &pane.join("claim.json"),
        "claim",
        &RecordIdentity::pane(address),
        "claim",
    );
    let after_pointer = RecordFacet::read(
        reader,
        &launch.join("current-binding.json"),
        "current_binding",
        &RecordIdentity::launch(address, &scope.launch_id),
        "binding_selection",
    );
    if let Err(mut diagnostic) = check_socket() {
        diagnostic
            .context
            .insert("facet".into(), Value::String("scope".into()));
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            vec![diagnostic],
            None,
        ));
    }
    if after_claim.failed() || after_pointer.failed() {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            [
                after_claim.diagnostics.clone(),
                after_pointer.diagnostics.clone(),
            ]
            .concat(),
            after_claim
                .failure_code()
                .or_else(|| after_pointer.failure_code()),
        ));
    }
    if claim.record != after_claim.record || pointer.record != after_pointer.record {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            vec![facet_diagnostic(
                "claim_stale",
                "scope",
                "scope changed or became unavailable during inspection",
            )],
            None,
        ));
    }
    Ok(PaneFacts {
        scope: scope.clone(),
        scope_relation: ScopeRelation::Matched,
        binding: row,
        pane_presence: match presence.as_str() {
            "present" => PanePresence::Present,
            "verified_absent" => PanePresence::VerifiedAbsent,
            _ => PanePresence::Unavailable,
        },
        reader_confidence: confidence,
        binding_health: health,
        activity,
        binding_end: end,
        children,
        review,
        lifecycle,
        diagnostics,
        timing_ms: BindingTiming::default(),
    })
}

fn lifecycle_from_read(
    read: RecordFacet,
    provider: Option<&str>,
    now: Option<&str>,
) -> Result<LifecycleView> {
    let availability = match read.availability {
        RecordAvailability::Present => LifecycleAvailability::Available,
        RecordAvailability::Absent => LifecycleAvailability::Absent,
        RecordAvailability::Unavailable => LifecycleAvailability::Unavailable,
        RecordAvailability::Unsupported => LifecycleAvailability::Unsupported,
        _ => LifecycleAvailability::Invalid,
    };
    let mut view = LifecycleView::empty(availability);
    view.diagnostics = read.diagnostics;
    if let Some(record) = read.record {
        if record["provider"].as_str() != provider {
            view.availability = LifecycleAvailability::Invalid;
            view.diagnostics.push(facet_diagnostic(
                "record_invalid",
                "lifecycle",
                "lifecycle provider differs from selected binding",
            ));
        } else {
            let snapshot: LifecycleSnapshot =
                serde_json::from_value(record).map_err(AttentionError::record_json)?;
            view = LifecycleView::from_snapshot(&snapshot, now)?;
            for diagnostic in &mut view.diagnostics {
                diagnostic
                    .context
                    .insert("facet".into(), Value::String("lifecycle".into()));
            }
        }
    }
    Ok(view)
}

struct ChildSelection<'a> {
    binding_id: &'a str,
    provider: &'a str,
    clear: &'a RecordFacet,
    floor: &'a RecordFacet,
}

fn read_fact_collection(
    reader: &dyn RecordReader,
    directory: &Path,
    scope: &PaneScope,
    child: Option<ChildSelection<'_>>,
    now: Option<&str>,
) -> EvidenceCollection {
    use RecordAvailability as A;
    let facet = if child.is_some() {
        "children"
    } else {
        "review"
    };
    let mut result = EvidenceCollection::empty(A::Present);
    if let Some(child) = &child {
        result.eligibility = Some(ChildEligibility {
            ttl_ms: crate::protocol::manifest()
                .expect("reader validated manifest")
                .limits
                .subagent_ttl_ms,
            clear_mono_ns: child
                .clear
                .record
                .as_ref()
                .and_then(|r| string(r, "observed_mono_ns")),
            floor_mono_ns: child
                .floor
                .record
                .as_ref()
                .and_then(|r| string(r, "floor_mono_ns")),
        });
    }
    let paths = match reader.entries(directory) {
        Ok(paths) => paths,
        Err(mut error) => {
            result.availability = if error.diagnostic.code == "record_invalid" {
                A::Invalid
            } else {
                A::Unavailable
            };
            error
                .diagnostic
                .context
                .insert("facet".into(), Value::String(facet.into()));
            result.diagnostics.push(error.diagnostic);
            return result;
        }
    };
    for path in paths {
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
            result.availability = A::Invalid;
            result.diagnostics.push(facet_diagnostic(
                "record_invalid",
                facet,
                "record filename is invalid",
            ));
            continue;
        };
        let identity = match &child {
            Some(child) => {
                RecordIdentity::agent(&scope.address, &scope.launch_id, child.binding_id, key)
            }
            None => RecordIdentity::review(&scope.address, key),
        };
        let record = RecordFacet::read(
            reader,
            &path,
            if child.is_some() {
                "subagent_presence"
            } else {
                "review"
            },
            &identity,
            facet,
        );
        if record.failed() {
            result.availability = record.availability;
            result.diagnostics.extend(record.diagnostics);
            continue;
        }
        let Some(record) = record.record else {
            result.availability = A::Unavailable;
            result.diagnostics.push(facet_diagnostic(
                "probe_unavailable",
                facet,
                "record disappeared during enumeration",
            ));
            continue;
        };
        if let Some(child) = &child {
            if record["provider"].as_str() != Some(child.provider) {
                result.availability = A::Invalid;
                result.diagnostics.push(facet_diagnostic(
                    "record_invalid",
                    facet,
                    "child provider differs from selected binding",
                ));
                continue;
            }
            let (eligible, problem) = crate::protocol::eligible_subagent_presence(
                &record,
                child
                    .clear
                    .record
                    .as_ref()
                    .and_then(|r| r["observed_mono_ns"].as_str()),
                child
                    .floor
                    .record
                    .as_ref()
                    .and_then(|r| r["floor_mono_ns"].as_str()),
                now,
                crate::protocol::manifest().expect("reader validated manifest"),
            );
            if let Some(code) = problem {
                result.availability = A::Unavailable;
                result.diagnostics.push(facet_diagnostic(
                    code,
                    facet,
                    "child eligibility is unavailable",
                ));
            }
            if !eligible {
                continue;
            }
        }
        result.evidence.push(record);
    }
    result.count = result.evidence.len();
    result
}

#[derive(Clone, Debug, Serialize)]
pub struct BindingRow {
    pub address: PaneAddress,
    pub launch_id: String,
    pub binding_id: String,
    pub provider: String,
    pub provider_session_id: String,
    pub binding_phase: String,
    pub pane_presence: String,
    pub reader_confidence: String,
    pub binding_health: String,
    pub current: bool,
    pub expected_session_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_source: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BindingQueryScope {
    pub realm_id: String,
    pub incarnation_id: String,
}

pub fn validate_socket_selector(socket: &str) -> Result<()> {
    if !Path::new(socket).is_absolute()
        || socket.len() > crate::protocol::manifest()?.limits.path_max_bytes
        || socket.chars().any(char::is_control)
    {
        return Err(AttentionError::usage(
            "--socket must be an absolute path within the path bound",
        ));
    }
    Ok(())
}

pub fn read_bindings_for_socket_with_ports(
    root: &Path,
    socket: &str,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(BindingQueryScope, Vec<BindingRow>, Vec<Diagnostic>)> {
    let (scope, rows, diagnostics, _) =
        read_bindings_for_socket_timed(root, socket, panes, processes)?;
    Ok((scope, rows, diagnostics))
}

/// The socket-scoped query with where its time went. See [`read_bindings_timed`].
pub fn read_bindings_for_socket_timed(
    root: &Path,
    socket: &str,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(
    BindingQueryScope,
    Vec<BindingRow>,
    Vec<Diagnostic>,
    BindingTiming,
)> {
    let started = Instant::now();
    validate_socket_selector(socket)?;
    let (realm_id, incarnation_id, _) = selected_socket_identity(socket)?;
    let scope = BindingQueryScope {
        realm_id,
        incarnation_id,
    };
    let selected = root
        .join("v2/realms")
        .join(&scope.realm_id)
        .join("incarnations")
        .join(&scope.incarnation_id);
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_selected_binding_files(root, &selected, &mut files, &mut diagnostics, true);
    let filter = BindingFilter {
        realm_id: Some(scope.realm_id.clone()),
        incarnation_id: Some(scope.incarnation_id.clone()),
        provider: None,
    };
    // Whether a row's provider session is live at another pane address is a
    // fact about the row, and the other address may be under any server, as
    // inspect finds it. Those rivals are looked up by session: what cannot be
    // read there is not part of this server's answer.
    let rivals = |sessions: &Sessions| {
        let mut elsewhere = session_candidates(root, sessions);
        elsewhere
            .retain(|path| !path.starts_with(&selected) && path_identity(root, path).is_some());
        elsewhere
    };
    let (rows, mut read_diagnostics, spawns) =
        assemble_bindings(root, files, &filter, Some(&rivals), panes, processes, true)?;
    diagnostics.append(&mut read_diagnostics);
    let after = selected_socket_identity(socket)?;
    if after.0 != scope.realm_id || after.1 != scope.incarnation_id {
        let mut error = AttentionError::new(
            "incarnation_changed",
            "selected socket identity changed during discovery",
        );
        error.exit_code = 1;
        return Err(error);
    }
    Ok((
        scope,
        rows,
        diagnostics,
        BindingTiming::from_wall(started, spawns),
    ))
}

/// The identity of the socket a caller named. A path with nothing there is
/// a server whose socket is gone, as readers of a recorded socket say it.
fn selected_socket_identity(
    socket: &str,
) -> Result<(String, String, crate::identity::SocketMetadata)> {
    socket_identity(socket).map_err(|error| {
        if fs::symlink_metadata(socket)
            .is_err_and(|missing| missing.kind() == std::io::ErrorKind::NotFound)
        {
            let mut gone = AttentionError::new("socket_gone", "mux socket no longer exists");
            gone.exit_code = error.exit_code;
            gone
        } else {
            error
        }
    })
}

/// The binding records below one server's incarnation. A directory or entry
/// that cannot be read is reported as the realm-wide walk reports it.
fn collect_selected_binding_files(
    root: &Path,
    path: &Path,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
    missing_ok: bool,
) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            diagnostics.push(unreadable_state(root, path));
            return;
        }
    };
    let symlink = |message: &str, path: &Path| {
        let mut item = diagnostic("record_invalid", message);
        item.context
            .insert("path".into(), Value::String(state_relative(root, path)));
        item
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                diagnostics.push(unreadable_state(root, path));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if entry.file_name() == "binding.json" => {
                if kind.is_symlink() {
                    diagnostics.push(symlink(
                        "selected binding record is a symlink",
                        &entry.path(),
                    ));
                } else {
                    output.push(entry.path());
                }
            }
            Ok(kind) if kind.is_dir() => {
                collect_selected_binding_files(root, &entry.path(), output, diagnostics, false)
            }
            Ok(kind) if kind.is_symlink() => diagnostics.push(symlink(
                "selected binding directory contains a symlink that was not traversed",
                &entry.path(),
            )),
            Err(_) => diagnostics.push(unreadable_state(root, &entry.path())),
            _ => {}
        }
    }
}

fn collect_binding_files(
    root: &Path,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    collect_state_files(
        root,
        &root.join("v2/realms"),
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("binding.json"),
        output,
        diagnostics,
    );
}

/// The binding records of one provider session, from the session index, or
/// None when the index cannot answer and the caller has to walk every
/// binding: it is not marked complete, or a directory or entry of it could
/// not be read. An entry whose binding is gone is still listed, and reads as
/// no record, as a walk would not have found it.
fn session_binding_files(root: &Path, provider: &str, session: &str) -> Option<Vec<PathBuf>> {
    read_record(
        &session_index_path(root),
        Some("session_index"),
        &RecordIdentity::unscoped(),
    )
    .ok()??;
    let entries = match fs::read_dir(session_dir(root, provider, session)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.ok()?.file_name();
        let name = name.to_str()?;
        // A temporary left by an interrupted write, not an entry.
        if name.starts_with('.') {
            continue;
        }
        let path = session_dir(root, provider, session).join(name);
        let record =
            read_record(&path, Some("session_binding"), &RecordIdentity::unscoped()).ok()??;
        let address = record_address(&record)?;
        let launch_id = string(&record, "launch_id")?;
        let binding_id = string(&record, "binding_id")?;
        // An entry names the binding its file name was made from, and no other.
        if session_entry_path(root, provider, session, &address, &launch_id, &binding_id) != path {
            return None;
        }
        files.push(
            launch_path(root, &address, &launch_id)
                .join("bindings")
                .join(binding_id)
                .join("binding.json"),
        );
    }
    Some(files)
}

/// The binding records that may hold one of these provider sessions: the
/// session index's entries for them, or every binding in the store where
/// the index cannot answer for one of them.
fn session_candidates(root: &Path, sessions: &Sessions) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (provider, session) in sessions {
        match session_binding_files(root, provider, session) {
            Some(indexed) => files.extend(indexed),
            None => {
                let mut every = Vec::new();
                collect_binding_files(root, &mut every, &mut Vec::new());
                return every;
            }
        }
    }
    files
}

/// Every file below `path` that `wanted` accepts, without following a symlink.
///
/// A directory or entry that cannot be read is reported, not skipped: whatever
/// is below it is missing from the answer, and an answer that looks complete
/// hides that. The diagnostic names the path relative to the state root. A
/// starting directory that does not exist is an empty store, and one removed
/// mid-walk was removed by its owner.
pub(crate) fn collect_state_files(
    root: &Path,
    path: &Path,
    wanted: &dyn Fn(&Path) -> bool,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            diagnostics.push(unreadable_state(root, path));
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            diagnostics.push(unreadable_state(root, path));
            continue;
        };
        let candidate = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                collect_state_files(root, &candidate, wanted, output, diagnostics)
            }
            Ok(_) if wanted(&candidate) => output.push(candidate),
            Ok(_) => {}
            Err(_) => diagnostics.push(unreadable_state(root, &candidate)),
        }
    }
}

/// The diagnostic for a state path a walk could not read.
pub(crate) fn unreadable_state(root: &Path, path: &Path) -> Diagnostic {
    let mut item = diagnostic("state_permissions", "state directory could not be read");
    item.context
        .insert("path".into(), Value::String(state_relative(root, path)));
    item
}

/// A state path as the diagnostic context names it: relative to the state
/// root, so a diagnostic never carries the local home directory.
pub(crate) fn state_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn string(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn path_identity(root: &Path, path: &Path) -> Option<(String, String, String, String, String)> {
    let parts: Vec<_> = path
        .strip_prefix(root)
        .ok()?
        .iter()
        .map(|part| part.to_str())
        .collect();
    if parts.len() != 12
        || parts[0] != Some("v2")
        || parts[1] != Some("realms")
        || parts[3] != Some("incarnations")
        || parts[5] != Some("panes")
        || parts[7] != Some("launches")
        || parts[9] != Some("bindings")
        || parts[11] != Some("binding.json")
    {
        return None;
    }
    Some((
        parts[2]?.to_owned(),
        parts[4]?.to_owned(),
        parts[6]?.to_owned(),
        parts[8]?.to_owned(),
        parts[10]?.to_owned(),
    ))
}

fn diagnostic(code: &str, message: &str) -> Diagnostic {
    AttentionError::new(code, message).diagnostic
}

pub fn read_bindings(root: &Path) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    read_bindings_with_ports(root, None, None)
}

/// Where a bindings or inspect query spent its wall time.
///
/// A query that took seconds either waited on a subprocess or read a lot of
/// files, and a caller's own clock cannot tell those apart. `pane_list` is the
/// wall time spent waiting on `wezterm cli list`; sockets asked together count
/// once, for as long as the slowest took. `process_list`
/// is the time inside the process listing that answers for panes the mux no
/// longer lists. `records` is everything else: finding and reading the binding
/// records, measured as the query's wall time with the two spawns taken out.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BindingTiming {
    pub pane_list: Duration,
    pub process_list: Duration,
    pub records: Duration,
}

impl BindingTiming {
    fn from_wall(started: Instant, spawns: SpawnSpend) -> Self {
        let total = started.elapsed();
        Self {
            pane_list: spawns.pane_list,
            process_list: spawns.process_list,
            records: total
                .saturating_sub(spawns.pane_list)
                .saturating_sub(spawns.process_list),
        }
    }

    /// The three durations in whole milliseconds, the shape the CLI prints.
    pub fn as_millis(&self) -> Value {
        serde_json::json!({
            "pane_list": u64::try_from(self.pane_list.as_millis()).unwrap_or(u64::MAX),
            "process_list": u64::try_from(self.process_list.as_millis()).unwrap_or(u64::MAX),
            "records": u64::try_from(self.records.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

impl Serialize for BindingTiming {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.as_millis().serialize(serializer)
    }
}

/// Time spent inside the subprocesses one assembly spawned.
#[derive(Clone, Copy, Debug, Default)]
struct SpawnSpend {
    pane_list: Duration,
    process_list: Duration,
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

/// What the socket at a realm's recorded path says of the server that held
/// one of its incarnations.
pub(crate) enum RecordedServer {
    /// The socket still carries the incarnation.
    Current,
    /// It no longer does. The diagnostic says how: `socket_gone` or
    /// `incarnation_changed`.
    Replaced(Diagnostic),
    /// Its identity could not be read.
    Unreadable(AttentionError),
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
        Ok(_) => RecordedServer::Replaced(diagnostic(
            "incarnation_changed",
            "realm socket identity changed",
        )),
        // Only a path that is not there at all. A socket that exists and
        // cannot be read says nothing about the server.
        Err(_)
            if fs::symlink_metadata(socket_path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            RecordedServer::Replaced(diagnostic("socket_gone", "mux socket no longer exists"))
        }
        // Something other than this user's socket now holds the path.
        Err(error) if error.diagnostic.code == "realm_unavailable" => RecordedServer::Replaced(
            diagnostic("incarnation_changed", "realm socket identity changed"),
        ),
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
fn reader_presence(
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
    let realm_path = root
        .join("v2/realms")
        .join(&address.realm_id)
        .join("realm.json");
    let incarnation_path = root
        .join("v2/realms")
        .join(&address.realm_id)
        .join("incarnations")
        .join(&address.incarnation_id)
        .join("incarnation.json");
    let realm = match read_record(
        &realm_path,
        Some("realm"),
        &RecordIdentity::realm(&address.realm_id),
    ) {
        Ok(Some(record)) => record,
        Ok(None) => return unavailable(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return unavailable();
        }
    };
    match read_record(
        &incarnation_path,
        Some("incarnation"),
        &RecordIdentity::incarnation(&address.realm_id, &address.incarnation_id),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => return unavailable(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return unavailable();
        }
    }
    let Some(socket_path) = realm.get("socket_path").and_then(Value::as_str) else {
        return unavailable();
    };
    match recorded_server(socket_path, &address.realm_id, &address.incarnation_id) {
        RecordedServer::Current => {}
        RecordedServer::Replaced(_)
            if replaced_server_pane_gone(socket_path, &address.pane_id, processes) =>
        {
            return PaneEvidence::Observed("verified_absent".to_owned());
        }
        RecordedServer::Replaced(diagnostic) => return PaneEvidence::ServerGone { diagnostic },
        RecordedServer::Unreadable(error) => {
            diagnostics.push(error.diagnostic);
            return unavailable();
        }
    }
    presence_at_socket(socket_path, address, Some(panes), processes, diagnostics)
}

/// Whether another pane address holds a binding of this provider session that
/// competes with the inspected one, by the rule `bindings` applies across its
/// rows. Only same-session bindings have their end read and their pane probed,
/// and the session index names them, so a store with no rival costs one
/// directory read and no subprocess.
fn session_live_elsewhere(
    root: &Path,
    address: &PaneAddress,
    provider: &str,
    session: &str,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> bool {
    let sessions = BTreeSet::from([(provider.to_owned(), session.to_owned())]);
    for path in session_candidates(root, &sessions) {
        let Some((realm_id, incarnation_id, pane_id, launch_id, binding_id)) =
            path_identity(root, &path)
        else {
            continue;
        };
        let other = PaneAddress {
            realm_id,
            incarnation_id,
            pane_id,
        };
        if other == *address {
            continue;
        }
        let identity = RecordIdentity::binding(&other, &launch_id, &binding_id);
        let Ok(Some(binding)) = read_record(&path, Some("binding"), &identity) else {
            continue;
        };
        if string(&binding, "provider").as_deref() != Some(provider)
            || string(&binding, "provider_session_id").as_deref() != Some(session)
        {
            continue;
        }
        let (presence, server_gone) =
            reader_presence(root, &other, panes, processes, &mut Vec::new());
        if competes(
            binding_ended(&path, &binding, &identity),
            server_gone,
            &presence,
        ) {
            return true;
        }
    }
    false
}

fn session_key(binding: &Value) -> (String, String) {
    (
        string(binding, "provider").unwrap_or_default(),
        string(binding, "provider_session_id").unwrap_or_default(),
    )
}

fn record_address(record: &Value) -> Option<PaneAddress> {
    serde_json::from_value(record.get("address")?.clone()).ok()
}

/// The socket `pane_presence` would list for this address: the realm's
/// recorded socket, when it still carries this incarnation. None when it would
/// answer without listing.
fn realm_socket(root: &Path, address: &PaneAddress) -> Option<String> {
    let realm = root.join("v2/realms").join(&address.realm_id);
    let record = read_record(
        &realm.join("realm.json"),
        Some("realm"),
        &RecordIdentity::realm(&address.realm_id),
    )
    .ok()??;
    read_record(
        &realm
            .join("incarnations")
            .join(&address.incarnation_id)
            .join("incarnation.json"),
        Some("incarnation"),
        &RecordIdentity::incarnation(&address.realm_id, &address.incarnation_id),
    )
    .ok()??;
    let socket = string(&record, "socket_path")?;
    let (realm_id, incarnation_id, _) = socket_identity(&socket).ok()?;
    (realm_id == address.realm_id && incarnation_id == address.incarnation_id).then_some(socket)
}

/// Whether the end record beside a binding ends it, by [`ends_binding`]. An
/// unreadable one ends nothing.
fn binding_ended(binding_path: &Path, binding: &Value, identity: &RecordIdentity) -> bool {
    binding_path
        .parent()
        .and_then(|dir| {
            read_record(&dir.join("end.json"), Some("binding_end"), identity)
                .ok()
                .flatten()
        })
        .is_some_and(|end| ends_binding(&end, binding))
}

/// A pane's presence under a socket that carried its incarnation when last
/// looked at.
fn presence_at_socket(
    socket_path: &str,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PaneEvidence {
    let pane_id = address.pane_id.as_str();
    let observed = |presence: &str| PaneEvidence::Observed(presence.to_owned());
    let Some(panes) = panes else {
        diagnostics.push(diagnostic("probe_unavailable", "pane probe is unavailable"));
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
                    diagnostics.push(diagnostic(
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
            diagnostic: diagnostic("socket_refused", "mux socket refuses connections"),
        };
    }
    diagnostics.push(error.diagnostic);
    observed("unavailable")
}

pub fn read_bindings_with_ports(
    root: &Path,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    let answer = read_bindings_timed(root, &BindingFilter::default(), panes, processes)?;
    Ok((answer.rows, answer.diagnostics))
}

/// A realm-wide bindings answer as the CLI reports it.
#[derive(Debug)]
pub struct RealmBindings {
    pub rows: Vec<BindingRow>,
    pub diagnostics: Vec<Diagnostic>,
    pub timing: BindingTiming,
    /// False when a directory in the state tree could not be read, so rows
    /// below it may be missing. An unreadable binding record is not counted:
    /// it is reported, and it is not a row.
    pub walked_every_directory: bool,
}

/// The realm-wide query with where its time went; the CLI prints the timing,
/// maintenance and the other library callers do not want it.
pub fn read_bindings_timed(
    root: &Path,
    filter: &BindingFilter,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<RealmBindings> {
    let started = Instant::now();
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_binding_files(root, &mut files, &mut diagnostics);
    let walked_every_directory = diagnostics.is_empty();
    let (rows, mut read_diagnostics, spawns) =
        assemble_bindings(root, files, filter, None, panes, processes, false)?;
    diagnostics.append(&mut read_diagnostics);
    Ok(RealmBindings {
        rows,
        diagnostics,
        timing: BindingTiming::from_wall(started, spawns),
        walked_every_directory,
    })
}

/// One pane listing per socket, rather than one per bound pane.
///
/// Resolving a binding's presence asks whether its pane id appears in the
/// socket's pane list. Every bound pane on one socket asks that of the same
/// list, and each miss used to spawn a fresh `wezterm cli list` subprocess --
/// about 20 ms per bound pane on top of a 5 ms floor, paid on every call.
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

    fn spent(&self) -> Duration {
        self.spent.lock().map(|spent| *spent).unwrap_or_default()
    }

    /// Lists every socket in `sockets` at once, one thread each, and keeps
    /// the answers for the per-pane asks that follow. A hung socket then costs
    /// one listing deadline for the whole query rather than one per socket.
    /// The time charged is the wall time of the batch, not the sum.
    fn list_together(&self, sockets: BTreeSet<String>) {
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
/// runs, which takes tens of milliseconds; a look per pane made a store with
/// many ended panes slow on every call. The listing is taken on the first
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

    fn spent(&self) -> Duration {
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

/// Which rows a bindings query returns. Applied before any socket is asked,
/// so a realm ruled out costs no `wezterm cli list`. `incarnation_id` is set
/// by the socket-scoped query, which returns one server's rows.
#[derive(Clone, Debug, Default)]
pub struct BindingFilter {
    pub realm_id: Option<String>,
    pub incarnation_id: Option<String>,
    pub provider: Option<String>,
}

impl BindingFilter {
    /// Whether a binding at this realm and incarnation can be returned,
    /// whatever its provider.
    fn admits_path(&self, realm_id: &str, incarnation_id: &str) -> bool {
        self.realm_id
            .as_deref()
            .is_none_or(|realm| realm == realm_id)
            && self
                .incarnation_id
                .as_deref()
                .is_none_or(|incarnation| incarnation == incarnation_id)
    }

    fn admits(&self, binding: &Value) -> bool {
        let address = binding.get("address");
        let field = |name: &str| {
            address
                .and_then(|value| value.get(name))
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        self.admits_path(field("realm_id"), field("incarnation_id"))
            && self.provider.as_deref().is_none_or(|provider| {
                binding.get("provider").and_then(Value::as_str) == Some(provider)
            })
    }
}

/// Provider sessions, each as (provider, provider session id).
type Sessions = BTreeSet<(String, String)>;

/// The binding records outside a query's own that may hold these sessions.
type RivalLookup<'a> = dyn Fn(&Sessions) -> Vec<PathBuf> + 'a;

/// The rows `files` make under `filter`. `rivals`, when given, names the
/// binding records outside `files` that may share a provider session with an
/// admitted row; without it `files` already holds every binding.
fn assemble_bindings(
    root: &Path,
    mut files: Vec<PathBuf>,
    filter: &BindingFilter,
    rivals: Option<&RivalLookup<'_>>,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    typed: bool,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>, SpawnSpend)> {
    let listed_once = panes.map(ListOncePerSocket::new);
    let panes = listed_once.as_ref().map(|lister| lister as &dyn PaneLister);
    let probed_once = processes.map(ProbeOncePerAssembly::new);
    let processes = probed_once.as_ref().map(|probe| probe as &dyn ProcessProbe);
    let read_record = |path: &Path, kind: Option<&str>, identity: &RecordIdentity| {
        let read = if !typed {
            read_record(path, kind, identity)
        } else {
            match read_record_typed(path, kind, identity) {
                RecordRead::Present(value) => Ok(Some(value)),
                RecordRead::Missing => Ok(None),
                RecordRead::Unavailable(mut error) => {
                    error.diagnostic.message = "selected state record I/O is unavailable".into();
                    Err(error)
                }
                RecordRead::Invalid(error) | RecordRead::Unsupported(error) => Err(error),
            }
        };
        read.map_err(|mut error| {
            error
                .diagnostic
                .context
                .insert("path".into(), Value::String(state_relative(root, path)));
            error
        })
    };
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    // Every binding record is read first: its realm and provider decide
    // whether the filter admits it, before any socket is asked.
    let mut read = Vec::new();
    let read_all = |files: Vec<PathBuf>,
                    read: &mut Vec<(PathBuf, Value)>,
                    diagnostics: &mut Vec<Diagnostic>| {
        for path in files {
            let Some((path_realm, path_incarnation, path_pane, path_launch, path_binding)) =
                path_identity(root, &path)
            else {
                let mut item = diagnostic("record_invalid", "binding path has the wrong shape");
                item.context
                    .insert("path".into(), Value::String(state_relative(root, &path)));
                diagnostics.push(item);
                continue;
            };
            let ruled_out = !filter.admits_path(&path_realm, &path_incarnation);
            let path_address = PaneAddress {
                realm_id: path_realm,
                incarnation_id: path_incarnation,
                pane_id: path_pane,
            };
            match read_record(
                &path,
                Some("binding"),
                &RecordIdentity::binding(&path_address, &path_launch, &path_binding),
            ) {
                Ok(Some(binding)) => read.push((path, binding)),
                Ok(None) => {}
                // A realm or server the filter rules out is not part of the
                // answer, and neither are its unreadable records.
                Err(_) if ruled_out => {}
                Err(error) => diagnostics.push(error.diagnostic),
            }
        }
    };
    files.sort();
    read_all(files, &mut read, &mut diagnostics);
    // A row outside the filter is still assessed when it shares a provider
    // session with an admitted row: whether that session is live elsewhere is
    // a fact about the admitted row. It is dropped from the answer afterwards.
    let sessions: Sessions = read
        .iter()
        .filter(|(_, binding)| filter.admits(binding))
        .map(|(_, binding)| session_key(binding))
        .collect();
    if let Some(rivals) = rivals {
        let known: BTreeSet<PathBuf> = read.iter().map(|(path, _)| path.clone()).collect();
        let mut elsewhere = rivals(&sessions);
        elsewhere.sort();
        elsewhere.dedup();
        elsewhere.retain(|path| !known.contains(path));
        read_all(elsewhere, &mut read, &mut diagnostics);
    }
    read.sort_by(|left, right| left.0.cmp(&right.0));
    let assessed: Vec<(PathBuf, Value, bool)> = read
        .into_iter()
        .filter_map(|(path, binding)| {
            let admitted = filter.admits(&binding);
            (admitted || sessions.contains(&session_key(&binding)))
                .then_some((path, binding, admitted))
        })
        .collect();
    if let Some(listed_once) = &listed_once {
        listed_once.list_together(
            assessed
                .iter()
                .filter_map(|(_, binding, _)| record_address(binding))
                .filter_map(|address| realm_socket(root, &address))
                .collect(),
        );
    }
    let mut admitted_rows = Vec::new();
    let mut servers_gone = Vec::new();
    let mut ignored = Vec::new();
    let mut presence_cache: BTreeMap<(String, String, String), (String, bool)> = BTreeMap::new();
    let mut claim_cache: BTreeMap<PathBuf, (Option<Value>, Option<String>)> = BTreeMap::new();
    for (path, binding, admitted) in assessed {
        let diagnostics = if admitted {
            &mut diagnostics
        } else {
            &mut ignored
        };
        let Some(address_value) = binding.get("address") else {
            continue;
        };
        let address: PaneAddress = serde_json::from_value(address_value.clone()).map_err(|_| {
            crate::protocol::AttentionError::new("record_invalid", "binding address is invalid")
        })?;
        let Some(launch_id) = string(&binding, "launch_id") else {
            continue;
        };
        let Some(binding_id) = string(&binding, "binding_id") else {
            continue;
        };
        let binding_dir = path.parent().expect("binding file has parent");
        let launch_dir = binding_dir
            .parent()
            .and_then(Path::parent)
            .expect("binding path has launch parent");
        let pane_dir = launch_dir
            .parent()
            .and_then(Path::parent)
            .expect("launch path has pane parent");
        let identity = RecordIdentity::binding(&address, &launch_id, &binding_id);
        let mut end_health = None;
        let end = match read_record(
            &binding_dir.join("end.json"),
            Some("binding_end"),
            &identity,
        ) {
            Ok(value) => value,
            Err(error) => {
                end_health = Some(if error.diagnostic.code == "future_schema" {
                    "future_schema"
                } else {
                    "invalid"
                });
                diagnostics.push(error.diagnostic);
                None
            }
        };
        let mut pointer_health = None;
        let pointer = match read_record(
            &launch_dir.join("current-binding.json"),
            Some("current_binding"),
            &RecordIdentity::launch(&address, &launch_id),
        ) {
            Ok(value) => value,
            Err(error) => {
                pointer_health = Some(if error.diagnostic.code == "future_schema" {
                    "future_schema"
                } else {
                    "invalid"
                });
                diagnostics.push(error.diagnostic);
                None
            }
        };
        let claim_path = pane_dir.join("claim.json");
        let (claim, claim_health) = if let Some(cached) = claim_cache.get(&claim_path) {
            cached.clone()
        } else {
            let loaded =
                match read_record(&claim_path, Some("claim"), &RecordIdentity::pane(&address)) {
                    Ok(value) => (value, None),
                    Err(error) => {
                        let health = Some(if error.diagnostic.code == "future_schema" {
                            "future_schema".to_owned()
                        } else {
                            "invalid".to_owned()
                        });
                        diagnostics.push(error.diagnostic);
                        (None, health)
                    }
                };
            claim_cache.insert(claim_path.clone(), loaded.clone());
            loaded
        };
        let current = claim
            .as_ref()
            .and_then(|value| string(value, "launch_id"))
            .as_deref()
            == Some(&launch_id)
            && pointer
                .as_ref()
                .and_then(|value| string(value, "binding_id"))
                .as_deref()
                == Some(&binding_id);
        let ended = end.as_ref().is_some_and(|end| ends_binding(end, &binding));
        let expected_session_match = string(&binding, "expected_session_id").map(|expected| {
            string(&binding, "provider_session_id").is_some_and(|actual| actual == expected)
        });
        let presence_key = (
            address.realm_id.clone(),
            address.incarnation_id.clone(),
            address.pane_id.clone(),
        );
        let (presence, server_gone) = if let Some(cached) = presence_cache.get(&presence_key) {
            cached.clone()
        } else {
            let before_presence = diagnostics.len();
            let observed = reader_presence(root, &address, panes, processes, diagnostics);
            if typed && observed.0 == "unavailable" && diagnostics.len() == before_presence {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "selected binding presence is unavailable",
                ));
            }
            // One socket failure is reported once per pane it leaves unknown;
            // the address says which.
            for item in &mut diagnostics[before_presence..] {
                for (field, value) in [
                    ("realm_id", &address.realm_id),
                    ("incarnation_id", &address.incarnation_id),
                    ("pane_id", &address.pane_id),
                ] {
                    item.context
                        .insert(field.into(), Value::String(value.clone()));
                }
            }
            presence_cache.insert(presence_key, observed.clone());
            observed
        };
        let binding_health = binding_health(
            [None, end_health, claim_health.as_deref(), pointer_health],
            false,
        )
        .as_str()
        .to_owned();
        rows.push(BindingRow {
            address,
            launch_id,
            binding_id,
            provider: string(&binding, "provider").unwrap_or_default(),
            provider_session_id: string(&binding, "provider_session_id").unwrap_or_default(),
            binding_phase: if ended { "ended" } else { "active" }.to_owned(),
            pane_presence: presence.clone(),
            reader_confidence: reader_confidence(current, &presence).as_str().to_owned(),
            binding_health,
            current,
            expected_session_match,
            expected_session_id: string(&binding, "expected_session_id"),
            transcript_path: string(&binding, "transcript_path"),
            cwd: string(&binding, "cwd"),
            config_dir: string(&binding, "config_dir"),
            model: string(&binding, "model"),
            start_source: string(&binding, "start_source"),
        });
        admitted_rows.push(admitted);
        servers_gone.push(server_gone);
    }
    let mut duplicates: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        if !competes(
            row.binding_phase == "ended",
            servers_gone[index],
            &row.pane_presence,
        ) {
            continue;
        }
        duplicates
            .entry((row.provider.clone(), row.provider_session_id.clone()))
            .or_default()
            .push(index);
    }
    for indices in duplicates.values() {
        let addresses: BTreeSet<_> = indices
            .iter()
            .map(|index| {
                let address = &rows[*index].address;
                (
                    address.realm_id.clone(),
                    address.incarnation_id.clone(),
                    address.pane_id.clone(),
                )
            })
            .collect();
        if addresses.len() > 1 {
            for index in indices {
                rows[*index].binding_health = BindingHealth::Conflicted.as_str().to_owned();
            }
            // Rows outside the filter that conflict only with each other are
            // not part of this answer.
            if !indices.iter().any(|index| admitted_rows[*index]) {
                continue;
            }
            let mut item = diagnostic(
                "binding_conflict",
                "provider session is bound to multiple pane addresses",
            );
            let first = &rows[indices[0]];
            item.context
                .insert("provider".into(), Value::String(first.provider.clone()));
            item.context.insert(
                "provider_session_id".into(),
                Value::String(first.provider_session_id.clone()),
            );
            item.context.insert(
                "addresses".into(),
                Value::Array(
                    addresses
                        .iter()
                        .map(|(realm_id, incarnation_id, pane_id)| {
                            serde_json::json!({
                                "realm_id": realm_id,
                                "incarnation_id": incarnation_id,
                                "pane_id": pane_id,
                            })
                        })
                        .collect(),
                ),
            );
            diagnostics.push(item);
        }
    }
    let mut admitted_rows = admitted_rows.into_iter();
    rows.retain(|_| admitted_rows.next().unwrap_or(false));
    rows.sort_by(|left, right| {
        (
            &left.address.realm_id,
            &left.address.incarnation_id,
            &left.address.pane_id,
            &left.launch_id,
            &left.binding_id,
        )
            .cmp(&(
                &right.address.realm_id,
                &right.address.incarnation_id,
                &right.address.pane_id,
                &right.launch_id,
                &right.binding_id,
            ))
    });
    let spawns = SpawnSpend {
        pane_list: listed_once
            .as_ref()
            .map(ListOncePerSocket::spent)
            .unwrap_or_default(),
        process_list: probed_once
            .as_ref()
            .map(ProbeOncePerAssembly::spent)
            .unwrap_or_default(),
    };
    Ok((rows, diagnostics, spawns))
}

/// The exact GUI socket incarnation that supplies a publication's window IDs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSource {
    socket_path: String,
    realm_id: String,
    incarnation_id: String,
}

pub fn read_tab_source(socket: &str) -> Result<TabSource> {
    validate_socket_selector(socket)?;
    let (realm_id, incarnation_id, metadata) = socket_identity(socket)?;
    Ok(TabSource {
        socket_path: metadata.socket_path,
        realm_id,
        incarnation_id,
    })
}

/// The tab order one GUI window's tab bar drew, as the bar published it.
///
/// This is not a v2 record. It names no pane address, carries no launch fence and no
/// TTL, and nothing acts on it, so it carries its own `schema` rather than the
/// manifest's record schema. `published_at_ms` says when the bar last drew
/// something different; nothing refreshes it while the bar is idle.
#[derive(Clone, Debug, Serialize)]
pub struct TabPublication {
    pub window_id: u64,
    pub published_at_ms: u64,
    pub tabs: Vec<PublishedTab>,
    pub source: Option<TabSource>,
    #[serde(skip)]
    pub(crate) relative_path: PathBuf,
    /// The file as it stood before it was read, so a deletion decided from
    /// these contents can refuse a file that has since been replaced.
    #[serde(skip)]
    pub(crate) stamp: Option<FileStamp>,
}

/// Enough of a regular file's metadata to tell that it was replaced or
/// rewritten: a rename changes the inode, an in-place write the size or mtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileStamp {
    dev: u64,
    ino: u64,
    nlink: u64,
    size: u64,
    mtime: i128,
}

impl FileStamp {
    pub(crate) fn of(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            nlink: metadata.nlink(),
            size: metadata.size(),
            mtime: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
        }
    }

    /// The stamp of the regular file at `path`, or None when nothing, or
    /// something other than a regular file, is there now.
    pub(crate) fn regular_file(path: &Path) -> std::io::Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(Some(Self::of(&metadata))),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckedTabPublication {
    #[serde(flatten)]
    pub publication: TabPublication,
    pub window_check: WindowCheck,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WindowCheck {
    Present {
        checked_at_ms: u64,
    },
    NotListed {
        checked_at_ms: u64,
    },
    Unavailable {
        checked_at_ms: u64,
        reason: WindowCheckReason,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowCheckReason {
    SourceUnrecorded,
    SourceChanged,
    SocketGone,
    ProbeUnavailable,
    InventoryInvalid,
}

fn observe_tab_source(
    source: &TabSource,
    lister: &dyn GuiWindowLister,
) -> std::result::Result<BTreeSet<u64>, WindowCheckReason> {
    // The same reading of the socket as a pane's reader makes of its realm's.
    let matches = || match read_tab_source(&source.socket_path) {
        Ok(current) if current == *source => Ok(()),
        Ok(_) => Err(WindowCheckReason::SourceChanged),
        Err(_) => match recorded_server(
            &source.socket_path,
            &source.realm_id,
            &source.incarnation_id,
        ) {
            RecordedServer::Replaced(diagnostic) if diagnostic.code == "socket_gone" => {
                Err(WindowCheckReason::SocketGone)
            }
            RecordedServer::Replaced(_) => Err(WindowCheckReason::SourceChanged),
            RecordedServer::Current | RecordedServer::Unreadable(_) => {
                Err(WindowCheckReason::ProbeUnavailable)
            }
        },
    };
    matches()?;
    let inventory = lister.list_windows(&source.socket_path);
    matches()?;
    // A GUI whose own process is gone has exited, as a pane's reader finds
    // it, whether or not it left its socket file behind. A refusal alone
    // shows nothing gone, and is an inventory that did not answer.
    if inventory.is_err() && crate::wezterm::gui_process_exited(&source.socket_path) {
        return Err(WindowCheckReason::SocketGone);
    }
    inventory.map_err(|error| {
        if error.diagnostic.code == "record_invalid" {
            WindowCheckReason::InventoryInvalid
        } else {
            WindowCheckReason::ProbeUnavailable
        }
    })
}

/// Check each publication in the namespace that produced its window ID.
/// A complete read can include unavailable checks; those are facts about one source.
pub fn read_checked_tab_publications(
    root: &Path,
    lister: &dyn GuiWindowLister,
    clock: &dyn Clock,
) -> Result<(Vec<CheckedTabPublication>, Vec<Diagnostic>)> {
    let (publications, diagnostics) = read_tab_publications(root)?;
    let mut inventories = BTreeMap::new();
    let mut checked = Vec::new();
    for publication in publications {
        let key = publication.source.clone();
        if !inventories.contains_key(&key) {
            let inventory = key
                .as_ref()
                .map_or(Err(WindowCheckReason::SourceUnrecorded), |source| {
                    observe_tab_source(source, lister)
                });
            let checked_at_ms = clock
                .unix_ns20()?
                .parse::<u128>()
                .ok()
                .and_then(|ns| u64::try_from(ns / 1_000_000).ok())
                .ok_or_else(|| AttentionError::new("clock_skew", "window check time is invalid"))?;
            inventories.insert(key.clone(), (inventory, checked_at_ms));
        }
        let (inventory, checked_at_ms) = &inventories[&key];
        let checked_at_ms = *checked_at_ms;
        let window_check = match inventory {
            Ok(windows) if windows.contains(&publication.window_id) => {
                WindowCheck::Present { checked_at_ms }
            }
            Ok(_) => WindowCheck::NotListed { checked_at_ms },
            Err(reason) => WindowCheck::Unavailable {
                checked_at_ms,
                reason: *reason,
            },
        };
        checked.push(CheckedTabPublication {
            publication,
            window_check,
        });
    }
    Ok((checked, diagnostics))
}

/// One drawn tab: the number the bar printed, the text it drew, and the ids the
/// plugin already uses for those panes. A v1 pane is a canonical decimal marker
/// id; a v2 pane is `v2:<realm_id>:<incarnation_id>:<pane_id>`. Both are already
/// translated out of the window's local numbering.
#[derive(Clone, Debug, Serialize)]
pub struct PublishedTab {
    pub number: u64,
    pub text: String,
    pub marker_ids: Vec<String>,
}

const TAB_PUBLICATION_SCHEMA: u64 = 2;

/// Read every tab order published under `<root>/tabs`.
///
/// A file that cannot be read or does not hold a tab publication is diagnosed
/// and skipped. The other windows drew independently of it, and withholding
/// them would answer a different question than the one asked.
pub fn read_tab_publications(root: &Path) -> Result<(Vec<TabPublication>, Vec<Diagnostic>)> {
    let limits = &crate::protocol::manifest()?.limits;
    let mut windows: Vec<TabPublication> = Vec::new();
    let mut diagnostics = Vec::new();
    let directory = root.join("tabs");
    // `read_dir` follows a symlink, and sweep deletes by the paths read here,
    // so a linked directory would aim the collection outside the state root.
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(AttentionError::new(
            "record_invalid",
            "tab publication directory is a symlink",
        ));
    }
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((windows, diagnostics));
        }
        Err(_) => {
            return Err(AttentionError::new(
                "probe_unavailable",
                "tab publication directory could not be enumerated",
            ));
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            diagnostics.push(diagnostic(
                "probe_unavailable",
                "tab publication entry is unavailable",
            ));
            continue;
        };
        let path = entry.path();
        let before = diagnostics.len();
        read_tab_publication(&path, &entry, limits, &mut windows, &mut diagnostics);
        for item in &mut diagnostics[before..] {
            item.context
                .insert("path".into(), Value::String(state_relative(root, &path)));
        }
    }
    windows.sort_by(|a, b| {
        a.window_id
            .cmp(&b.window_id)
            .then_with(|| a.source.cmp(&b.source))
    });
    Ok((windows, diagnostics))
}

/// One entry of `tabs/`: a publication, a diagnostic, or nothing for a file
/// that is not a tab order.
fn read_tab_publication(
    path: &Path,
    entry: &fs::DirEntry,
    limits: &crate::protocol::Limits,
    windows: &mut Vec<TabPublication>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if path.extension().and_then(|value| value.to_str()) != Some("json") {
        return;
    }
    match entry.file_type() {
        Ok(kind) if kind.is_symlink() => {
            diagnostics.push(diagnostic("record_invalid", "tab publication is a symlink"));
            return;
        }
        Ok(kind) if !kind.is_file() => return,
        Ok(_) => {}
        Err(_) => {
            diagnostics.push(diagnostic(
                "probe_unavailable",
                "tab publication entry type is unavailable",
            ));
            return;
        }
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let (incarnation, id) = match stem.split_once('-') {
        Some((incarnation, id)) if hex64(incarnation) => (Some(incarnation), id),
        None => (None, stem),
        _ => (None, ""),
    };
    let window_id = canonical_decimal(id, limits.canonical_decimal_max_digits)
        .then(|| id.parse::<u64>().ok())
        .flatten();
    let Some(window_id) = window_id else {
        diagnostics.push(diagnostic(
            "record_invalid",
            "tab publication is not named by a window ID",
        ));
        return;
    };
    let stamp = FileStamp::regular_file(path).ok().flatten();
    match read_record_typed(path, None, &RecordIdentity::unscoped()) {
        RecordRead::Present(value) => {
            match tab_publication(&value, window_id, incarnation, limits) {
                Ok(mut window) => {
                    window.relative_path = Path::new("tabs").join(format!("{stem}.json"));
                    window.stamp = stamp;
                    windows.push(window);
                }
                Err(error) => diagnostics.push(error.diagnostic),
            }
        }
        // Published and removed between the listing and the read. The window
        // it described is gone or is about to publish again.
        RecordRead::Missing => {}
        RecordRead::Unavailable(error)
        | RecordRead::Invalid(error)
        | RecordRead::Unsupported(error) => diagnostics.push(error.diagnostic),
    }
}

/// Digits with no leading zero, the way every ID this project writes is spelled.
fn canonical_decimal(text: &str, max_digits: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_digits
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text.len() == 1 || !text.starts_with('0'))
}

fn hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// What `gui_tab_pane_ids` publishes: a v1 marker id, or the v2 cache key
/// `address_cache_key` builds after a poll has identified the pane.
fn published_marker_id(text: &str, pane_id_max_digits: usize) -> bool {
    if canonical_decimal(text, pane_id_max_digits) {
        return true;
    }
    const HEX: usize = 64;
    let Some(rest) = text.strip_prefix("v2:") else {
        return false;
    };
    let realm_end = HEX;
    let incarnation_start = HEX + 1;
    let incarnation_end = incarnation_start + HEX;
    let pane_start = incarnation_end + 1;
    rest.len() >= pane_start
        && rest.as_bytes()[realm_end] == b':'
        && rest.as_bytes()[incarnation_end] == b':'
        && hex64(&rest[..realm_end])
        && hex64(&rest[incarnation_start..incarnation_end])
        && canonical_decimal(&rest[pane_start..], pane_id_max_digits)
}

fn tab_publication(
    value: &Value,
    window_id: u64,
    incarnation: Option<&str>,
    limits: &crate::protocol::Limits,
) -> Result<TabPublication> {
    let invalid = || AttentionError::new("record_invalid", "tab publication is invalid");
    let object = value.as_object().ok_or_else(invalid)?;
    let schema = object
        .get("schema")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    if schema > TAB_PUBLICATION_SCHEMA {
        return Err(AttentionError::new(
            "future_schema",
            "tab publication schema is unsupported",
        ));
    }
    if !matches!(schema, 1 | 2)
        || !object.keys().all(|field| {
            matches!(
                field.as_str(),
                "schema" | "window_id" | "published_at_ms" | "tabs"
            ) || (schema == 2 && field == "source")
        })
        // A file that names a window other than the one it is filed under
        // describes neither of them.
        || object.get("window_id").and_then(Value::as_u64) != Some(window_id)
    {
        return Err(invalid());
    }
    let source = if schema == 2 {
        let source: TabSource =
            serde_json::from_value(object.get("source").ok_or_else(invalid)?.clone())
                .map_err(|_| invalid())?;
        if !Path::new(&source.socket_path).is_absolute()
            || source.socket_path.contains('\0')
            || !hex64(&source.realm_id)
            || !hex64(&source.incarnation_id)
            || crate::protocol::sha256_hex(source.socket_path.as_bytes()) != source.realm_id
            || incarnation != Some(source.incarnation_id.as_str())
        {
            return Err(invalid());
        }
        Some(source)
    } else {
        if incarnation.is_some() {
            return Err(invalid());
        }
        None
    };
    let published_at_ms = object
        .get("published_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    let mut tabs = Vec::new();
    for item in object
        .get("tabs")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
    {
        let entry = item.as_object().ok_or_else(invalid)?;
        if !entry
            .keys()
            .all(|field| matches!(field.as_str(), "number" | "text" | "marker_ids"))
        {
            return Err(invalid());
        }
        let number = entry
            .get("number")
            .and_then(Value::as_u64)
            .filter(|number| *number > 0)
            .ok_or_else(invalid)?;
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            // C1 controls count too: U+009B alone starts an escape sequence
            // in a terminal that draws this text back.
            .filter(|text| {
                text.len() <= limits.safe_label_max_bytes && !text.chars().any(char::is_control)
            })
            .ok_or_else(invalid)?
            .to_owned();
        let mut marker_ids = Vec::new();
        for id in entry
            .get("marker_ids")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?
        {
            let id = id
                .as_str()
                .filter(|id| published_marker_id(id, limits.pane_id_max_digits))
                .ok_or_else(invalid)?;
            marker_ids.push(id.to_owned());
        }
        tabs.push(PublishedTab {
            number,
            text,
            marker_ids,
        });
    }
    Ok(TabPublication {
        window_id,
        published_at_ms,
        tabs,
        source,
        relative_path: PathBuf::new(),
        stamp: None,
    })
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

#[cfg(test)]
mod published_marker_id_tests {
    use super::published_marker_id;

    const V2: &str = concat!(
        "v2:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ":",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ":16"
    );

    #[test]
    fn accepts_the_two_forms_the_plugin_writes() {
        assert!(published_marker_id("0", 20));
        assert!(published_marker_id("16", 20));
        assert!(published_marker_id(V2, 20));
    }

    #[test]
    fn refuses_a_key_the_plugin_would_not_write() {
        assert!(!published_marker_id("", 20));
        assert!(!published_marker_id("016", 20));
        assert!(!published_marker_id("not-an-id", 20));
        assert!(!published_marker_id("v2:short:short:16", 20));
        assert!(!published_marker_id(
            "V2:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:16",
            20
        ));
        assert!(!published_marker_id(
            "v2:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:16",
            20
        ));
        assert!(!published_marker_id(&format!("{V2}x"), 20));
        assert!(!published_marker_id(
            "v2:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:016",
            20
        ));
    }
}

#[cfg(test)]
mod socket_selector_tests {
    use super::validate_socket_selector;

    /// A socket path is echoed back in answers and diagnostics, so a C1
    /// control in it is refused like any C0 one.
    #[test]
    fn refuses_a_socket_path_with_any_control_character() {
        assert!(validate_socket_selector("/tmp/mux.sock").is_ok());
        for socket in [
            "/tmp/a\u{1b}b",
            "/tmp/a\u{7f}b",
            "/tmp/a\u{85}b",
            "/tmp/a\u{9b}b",
        ] {
            assert!(validate_socket_selector(socket).is_err(), "{socket:?}");
        }
    }
}
