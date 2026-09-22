use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::PaneAddress;
use crate::identity::socket_identity;
use crate::observations::{LifecycleAvailability, LifecycleSnapshot, LifecycleView};
use crate::protocol::{AttentionError, Diagnostic, Result};
use crate::records::{FileRecords, RecordReader, launch_path, pane_path};
use crate::records::{RecordIdentity, RecordRead, read_record, read_record_typed};
use crate::wezterm::Clock;
use crate::wezterm::{GuiWindowLister, PaneLister, Presence, ProcessProbe};

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

impl PaneFacts {
    fn unavailable(
        scope: &PaneScope,
        relation: ScopeRelation,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            scope: scope.clone(),
            scope_relation: relation,
            binding: None,
            pane_presence: PanePresence::Unavailable,
            reader_confidence: ReaderConfidence::Unconfirmed,
            binding_health: if diagnostics.iter().any(|d| d.code == "future_schema") {
                BindingHealth::FutureSchema
            } else {
                BindingHealth::Invalid
            },
            activity: RecordFacet::empty(RecordAvailability::Unavailable),
            binding_end: RecordFacet::empty(RecordAvailability::Unavailable),
            children: EvidenceCollection::empty(RecordAvailability::Unavailable),
            review: EvidenceCollection::empty(RecordAvailability::Unavailable),
            lifecycle: LifecycleView::empty(LifecycleAvailability::Unavailable),
            diagnostics,
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
        Some(&crate::wezterm::ExistingWeztermPaneLister),
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
        ));
    };
    let check_socket = || -> Result<()> {
        let (realm, incarnation, _) = socket_identity(socket)?;
        if realm != address.realm_id || incarnation != address.incarnation_id {
            return Err(AttentionError::new(
                "incarnation_changed",
                "requested socket identity changed",
            ));
        }
        Ok(())
    };
    if let Err(mut error) = check_socket() {
        error
            .diagnostic
            .context
            .insert("facet".into(), Value::String("scope".into()));
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            vec![error.diagnostic],
        ));
    }
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
                claim.diagnostics
            },
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
            pointer.diagnostics,
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
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Matched,
            diagnostics,
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
        .is_some_and(|(end, binding)| {
            end["observed_mono_ns"].as_str() < binding["observed_mono_ns"].as_str()
        })
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
    let base_diagnostics = diagnostics.len();
    diagnostics.extend(lifecycle.diagnostics.clone());
    let before_presence = diagnostics.len();
    let presence = presence_at_socket(socket, &address.pane_id, panes, processes, &mut diagnostics);
    for item in &mut diagnostics[before_presence..] {
        item.context
            .insert("facet".into(), Value::String("pane_presence".into()));
    }
    let base_unavailable = now.is_none()
        || [&binding, &activity, &clear, &end, &ack]
            .iter()
            .any(|facet| facet.availability == A::Unavailable)
        || children.availability == A::Unavailable
        || review.availability == A::Unavailable;
    let confidence = if presence == "present" && !base_unavailable {
        "confirmed"
    } else {
        "unconfirmed"
    };
    let health = if diagnostics[..base_diagnostics]
        .iter()
        .any(|d| d.code == "future_schema")
    {
        "future_schema"
    } else if base_diagnostics == 0 {
        "valid"
    } else {
        "invalid"
    };
    let row = binding.record.as_ref().map(|record| BindingRow {
        address: address.clone(),
        launch_id: scope.launch_id.clone(),
        binding_id: selected.unwrap().into(),
        provider: string(record, "provider").unwrap(),
        provider_session_id: string(record, "provider_session_id").unwrap(),
        binding_phase: if end.availability == A::Present {
            "ended"
        } else {
            "active"
        }
        .into(),
        pane_presence: presence.clone(),
        reader_confidence: confidence.into(),
        binding_health: health.into(),
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
    if let Err(mut error) = check_socket() {
        error
            .diagnostic
            .context
            .insert("facet".into(), Value::String("scope".into()));
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            vec![error.diagnostic],
        ));
    }
    if after_claim.failed() || after_pointer.failed() {
        return Ok(PaneFacts::unavailable(
            scope,
            ScopeRelation::Unavailable,
            [after_claim.diagnostics, after_pointer.diagnostics].concat(),
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
        reader_confidence: if confidence == "confirmed" {
            ReaderConfidence::Confirmed
        } else {
            ReaderConfidence::Unconfirmed
        },
        binding_health: match health {
            "valid" => BindingHealth::Valid,
            "future_schema" => BindingHealth::FutureSchema,
            _ => BindingHealth::Invalid,
        },
        activity,
        binding_end: end,
        children,
        review,
        lifecycle,
        diagnostics,
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
        || socket.chars().any(|c| c < ' ' || c == '\u{7f}')
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
    validate_socket_selector(socket)?;
    let (realm_id, incarnation_id, _) = socket_identity(socket)?;
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
    collect_selected_binding_files(&selected, &mut files, &mut diagnostics, true);
    let (rows, mut read_diagnostics) = assemble_bindings(root, files, panes, processes, true)?;
    diagnostics.append(&mut read_diagnostics);
    let after = socket_identity(socket)?;
    if after.0 != scope.realm_id || after.1 != scope.incarnation_id {
        let mut error = AttentionError::new(
            "incarnation_changed",
            "selected socket identity changed during discovery",
        );
        error.exit_code = 1;
        return Err(error);
    }
    Ok((scope, rows, diagnostics))
}

fn collect_selected_binding_files(
    path: &Path,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
    missing_ok: bool,
) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding directory could not be enumerated",
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                diagnostics.push(diagnostic(
                    "record_invalid",
                    "selected binding directory entry could not be read",
                ));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if entry.file_name() == "binding.json" => {
                if kind.is_symlink() {
                    diagnostics.push(diagnostic(
                        "record_invalid",
                        "selected binding record is a symlink",
                    ));
                } else {
                    output.push(entry.path());
                }
            }
            Ok(kind) if kind.is_dir() => {
                collect_selected_binding_files(&entry.path(), output, diagnostics, false)
            }
            Ok(kind) if kind.is_symlink() => diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding directory contains a symlink that was not traversed",
            )),
            Err(_) => diagnostics.push(diagnostic(
                "record_invalid",
                "selected binding entry type is unavailable",
            )),
            _ => {}
        }
    }
}

fn collect_binding_files(path: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            collect_binding_files(&candidate, output);
        } else if candidate.file_name().and_then(|name| name.to_str()) == Some("binding.json") {
            output.push(candidate);
        }
    }
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

pub(crate) fn pane_presence(
    root: &Path,
    address: &PaneAddress,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> String {
    let Some(panes) = panes else {
        return "unavailable".to_owned();
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
        Ok(None) => return "unavailable".to_owned(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    };
    match read_record(
        &incarnation_path,
        Some("incarnation"),
        &RecordIdentity::incarnation(&address.realm_id, &address.incarnation_id),
    ) {
        Ok(Some(_)) => {}
        Ok(None) => return "unavailable".to_owned(),
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    }
    let Some(socket_path) = realm.get("socket_path").and_then(Value::as_str) else {
        return "unavailable".to_owned();
    };
    match socket_identity(socket_path) {
        Ok((realm_id, incarnation_id, _))
            if realm_id == address.realm_id && incarnation_id == address.incarnation_id => {}
        Ok(_) => {
            diagnostics.push(diagnostic(
                "incarnation_changed",
                "realm socket identity changed",
            ));
            return "unavailable".to_owned();
        }
        Err(error) => {
            diagnostics.push(error.diagnostic);
            return "unavailable".to_owned();
        }
    }
    presence_at_socket(
        socket_path,
        &address.pane_id,
        Some(panes),
        processes,
        diagnostics,
    )
}

fn presence_at_socket(
    socket_path: &str,
    pane_id: &str,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    diagnostics: &mut Vec<Diagnostic>,
) -> String {
    let Some(panes) = panes else {
        diagnostics.push(diagnostic("probe_unavailable", "pane probe is unavailable"));
        return "unavailable".into();
    };
    match panes.list(socket_path) {
        Ok(rows) if rows.iter().any(|row| row.pane_id == pane_id) => "present".to_owned(),
        Ok(_) => match processes.map(|probe| probe.presence(socket_path, pane_id)) {
            Some(Presence::Present) => "present".to_owned(),
            Some(Presence::Absent) => "verified_absent".to_owned(),
            _ => {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "identity-scoped process probe is unavailable",
                ));
                "unavailable".to_owned()
            }
        },
        Err(error) => {
            diagnostics.push(error.diagnostic);
            "unavailable".to_owned()
        }
    }
}

pub fn read_bindings_with_ports(
    root: &Path,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    let mut files = Vec::new();
    collect_binding_files(&root.join("v2/realms"), &mut files);
    assemble_bindings(root, files, panes, processes, false)
}

/// One pane listing per socket, rather than one per bound pane.
///
/// Resolving a binding's presence asks whether its pane id appears in the
/// socket's pane list. Every bound pane on one socket asks that of the same
/// list, and each miss used to spawn a fresh `wezterm cli list` subprocess --
/// about 20 ms per bound pane on top of a 5 ms floor, paid on every call.
/// The answers are memoised for the lifetime of one assembly and no longer, so
/// a later call still observes panes that opened or closed in between.
struct ListOncePerSocket<'a> {
    inner: &'a dyn PaneLister,
    listed: Mutex<BTreeMap<String, Result<Vec<crate::wezterm::PaneRow>>>>,
}

impl<'a> ListOncePerSocket<'a> {
    fn new(inner: &'a dyn PaneLister) -> Self {
        Self {
            inner,
            listed: Mutex::new(BTreeMap::new()),
        }
    }
}

impl PaneLister for ListOncePerSocket<'_> {
    fn list(&self, socket_path: &str) -> Result<Vec<crate::wezterm::PaneRow>> {
        // A poisoned lock would mean a panic inside `list`; fall back to the
        // uncached path rather than propagating a panic through a read command.
        let Ok(mut listed) = self.listed.lock() else {
            return self.inner.list(socket_path);
        };
        if let Some(cached) = listed.get(socket_path) {
            return cached.clone();
        }
        let answer = self.inner.list(socket_path);
        listed.insert(socket_path.to_owned(), answer.clone());
        answer
    }
}

/// One process listing per assembly, rather than one per absent pane.
///
/// A bound pane missing from the mux listing is looked for among live
/// processes, and each look used to spawn its own `ps` over every process on
/// the machine -- about 70 ms each, so a store holding forty ended panes cost
/// three seconds on every call. The listing is taken on the first miss and
/// kept for the lifetime of one assembly and no longer. Maintenance does not
/// use this: it deletes on the answer, so it keeps a fresh look per decision.
struct ProbeOncePerAssembly<'a> {
    inner: &'a dyn ProcessProbe,
    listed: Mutex<Option<Option<crate::wezterm::PaneProcessSet>>>,
}

impl<'a> ProbeOncePerAssembly<'a> {
    fn new(inner: &'a dyn ProcessProbe) -> Self {
        Self {
            inner,
            listed: Mutex::new(None),
        }
    }
}

impl ProcessProbe for ProbeOncePerAssembly<'_> {
    fn available(&self) -> bool {
        self.inner.available()
    }

    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        // A poisoned lock would mean a panic inside the listing; fall back to
        // the uncached path rather than propagating it through a read command.
        let Ok(mut listed) = self.listed.lock() else {
            return self.inner.presence(socket_path, pane_id);
        };
        // A probe that offers no listing is remembered too, and asked one pane
        // at a time as before.
        match listed.get_or_insert_with(|| self.inner.pane_processes()) {
            Some(processes) => processes.presence(socket_path, pane_id),
            None => self.inner.presence(socket_path, pane_id),
        }
    }
}

fn assemble_bindings(
    root: &Path,
    mut files: Vec<PathBuf>,
    panes: Option<&dyn PaneLister>,
    processes: Option<&dyn ProcessProbe>,
    typed: bool,
) -> Result<(Vec<BindingRow>, Vec<Diagnostic>)> {
    let listed_once = panes.map(ListOncePerSocket::new);
    let panes = listed_once.as_ref().map(|lister| lister as &dyn PaneLister);
    let probed_once = processes.map(ProbeOncePerAssembly::new);
    let processes = probed_once.as_ref().map(|probe| probe as &dyn ProcessProbe);
    let read_record = |path: &Path, kind: Option<&str>, identity: &RecordIdentity| {
        if !typed {
            return read_record(path, kind, identity);
        }
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
    files.sort();
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    let mut presence_cache: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut claim_cache: BTreeMap<PathBuf, (Option<Value>, Option<String>)> = BTreeMap::new();
    for path in files {
        let Some((path_realm, path_incarnation, path_pane, path_launch, path_binding)) =
            path_identity(root, &path)
        else {
            diagnostics.push(diagnostic(
                "record_invalid",
                "binding path has the wrong shape",
            ));
            continue;
        };
        let path_address = PaneAddress {
            realm_id: path_realm,
            incarnation_id: path_incarnation,
            pane_id: path_pane,
        };
        let binding = match read_record(
            &path,
            Some("binding"),
            &RecordIdentity::binding(&path_address, &path_launch, &path_binding),
        ) {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(error) => {
                diagnostics.push(error.diagnostic);
                continue;
            }
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
        let binding_order = string(&binding, "observed_mono_ns").unwrap_or_default();
        let ended = end
            .as_ref()
            .and_then(|value| string(value, "observed_mono_ns"))
            .is_some_and(|order| order >= binding_order);
        let expected_session_match = string(&binding, "expected_session_id").map(|expected| {
            string(&binding, "provider_session_id").is_some_and(|actual| actual == expected)
        });
        let presence_key = (
            address.realm_id.clone(),
            address.incarnation_id.clone(),
            address.pane_id.clone(),
        );
        let presence = if let Some(cached) = presence_cache.get(&presence_key) {
            cached.clone()
        } else {
            let before_presence = diagnostics.len();
            let observed = pane_presence(root, &address, panes, processes, &mut diagnostics);
            if typed && observed == "unavailable" && diagnostics.len() == before_presence {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "selected binding presence is unavailable",
                ));
            }
            presence_cache.insert(presence_key, observed.clone());
            observed
        };
        let binding_health = end_health
            .map(str::to_owned)
            .or(claim_health)
            .or_else(|| pointer_health.map(str::to_owned))
            .unwrap_or_else(|| "valid".to_owned());
        rows.push(BindingRow {
            address,
            launch_id,
            binding_id,
            provider: string(&binding, "provider").unwrap_or_default(),
            provider_session_id: string(&binding, "provider_session_id").unwrap_or_default(),
            binding_phase: if ended { "ended" } else { "active" }.to_owned(),
            pane_presence: presence.clone(),
            reader_confidence: if current && presence == "present" {
                "confirmed"
            } else {
                "unconfirmed"
            }
            .to_owned(),
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
    }
    // Only live claims compete. A binding that has ended, or whose pane is
    // verified absent, is history: a session resumed in a new pane leaves one
    // behind every time, and calling that a conflict hides the pane the session
    // actually runs in.
    let mut duplicates: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        if row.binding_phase == "ended" || row.pane_presence == "verified_absent" {
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
                (&address.realm_id, &address.incarnation_id, &address.pane_id)
            })
            .collect();
        if addresses.len() > 1 {
            for index in indices {
                rows[*index].binding_health = "conflicted".to_owned();
            }
            diagnostics.push(diagnostic(
                "binding_conflict",
                "provider session is bound to multiple pane addresses",
            ));
        }
    }
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
    Ok((rows, diagnostics))
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
    ProbeUnavailable,
    InventoryInvalid,
}

fn observe_tab_source(
    source: &TabSource,
    lister: &dyn GuiWindowLister,
) -> std::result::Result<BTreeSet<u64>, WindowCheckReason> {
    let matches = || match read_tab_source(&source.socket_path) {
        Ok(current) if current == *source => Ok(()),
        Ok(_) => Err(WindowCheckReason::SourceChanged),
        Err(_) => Err(WindowCheckReason::ProbeUnavailable),
    };
    matches()?;
    let inventory = lister.list_windows(&source.socket_path);
    matches()?;
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
    let entries = match fs::read_dir(root.join("tabs")) {
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
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => {
                diagnostics.push(diagnostic("record_invalid", "tab publication is a symlink"));
                continue;
            }
            Ok(kind) if !kind.is_file() => continue,
            Ok(_) => {}
            Err(_) => {
                diagnostics.push(diagnostic(
                    "probe_unavailable",
                    "tab publication entry type is unavailable",
                ));
                continue;
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
            continue;
        };
        match read_record_typed(&path, None, &RecordIdentity::unscoped()) {
            RecordRead::Present(value) => {
                match tab_publication(&value, window_id, incarnation, limits) {
                    Ok(mut window) => {
                        window.relative_path = Path::new("tabs").join(format!("{stem}.json"));
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
    windows.sort_by(|a, b| {
        a.window_id
            .cmp(&b.window_id)
            .then_with(|| a.source.cmp(&b.source))
    });
    Ok((windows, diagnostics))
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
            .filter(|text| {
                text.len() <= limits.safe_label_max_bytes
                    && !text
                        .chars()
                        .any(|character| character < ' ' || character == '\u{7f}')
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
    })
}

#[cfg(test)]
mod process_probe_tests {
    use super::*;
    use crate::wezterm::PaneProcessSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProbe {
        listing: Option<&'static str>,
        listings: AtomicUsize,
        single_looks: AtomicUsize,
    }

    impl CountingProbe {
        fn new(listing: Option<&'static str>) -> Self {
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

        fn pane_processes(&self) -> Option<PaneProcessSet> {
            self.listings.fetch_add(1, Ordering::SeqCst);
            self.listing.map(PaneProcessSet::from_process_listing)
        }
    }

    #[test]
    fn many_absent_panes_cost_one_process_listing() {
        let probe = CountingProbe::new(Some("zsh WEZTERM_UNIX_SOCKET=/mux.sock WEZTERM_PANE=9"));
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
        let probe = CountingProbe::new(None);
        let once = ProbeOncePerAssembly::new(&probe);
        for pane in ["1", "2", "3"] {
            assert_eq!(once.presence("/mux.sock", pane), Presence::Absent);
        }
        assert_eq!(probe.listings.load(Ordering::SeqCst), 1);
        assert_eq!(probe.single_looks.load(Ordering::SeqCst), 3);
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
