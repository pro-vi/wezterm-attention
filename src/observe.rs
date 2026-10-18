//! One binding's lifecycle events since a cursor, read from its record
//! files alone. This is the read behind `attention observe`: no pane
//! listing, process probe, host check or write, so a caller may run it on
//! its own cadence or let one invocation poll until its own deadline.
//!
//! The cursor is the caller's only state. It names what one read saw —
//! the observation ids in each pool, each pool's retention floor and
//! newest stamp, the latest prompt and the known answers — so the next
//! read can diff by identity rather than by timestamp: an observation a
//! follower stamped just after its turn's last record, or one a hook
//! delivered late below the cursor's newest stamp, is still reported
//! while its pool retains it. What retention took between two reads is
//! not reconstructed: when a pool's floor moves past everything the
//! cursor saw in that pool, the answer is a gap, not the survivors.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::observations::{
    Actor, LeadPrompts, LifecycleSnapshot, ObservationBody, ObservationPool, PromptReference,
};
use crate::protocol::{AttentionError, Diagnostic, DiagnosticCode, Provider, Result};
use crate::query::PaneScope;
use crate::records::{RecordIdentity, RecordRead, read_record_typed_at};

/// How often a blocking observe re-reads the record files.
const POLL: Duration = Duration::from_millis(100);

/// Which lead happenings a caller waits for. The names are the `--for`
/// values, and a cursor is tied to the set it was taken under.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObserveFilters {
    pub prompt: bool,
    pub answer: bool,
    pub turn_end: bool,
    /// Only answers to this native tool call, when answering is waited for.
    pub call: Option<String>,
}

impl ObserveFilters {
    /// The waited-for names, sorted, as a cursor records them.
    fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.answer {
            names.push("answer".to_owned());
        }
        if self.prompt {
            names.push("prompt".to_owned());
        }
        if self.turn_end {
            names.push("turn_end".to_owned());
        }
        names
    }
}

/// One happening an observe reports. It names the stored observation and
/// nothing it carried: no prompt or reply text is ever in an event.
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct ObserveEvent {
    /// `prompt`, `answer` or `turn_end`.
    pub event: String,
    pub observation_id: String,
    /// The observation's own kind, such as `response_finished`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_event: Option<String>,
    pub observed_mono_ns: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// For an answer: the native call it answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_index: Option<u64>,
    /// For an `attempt_outcome` turn end: the provider's error code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// How an observe ended, as `result.outcome` spells it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveOutcome {
    /// No `--since`: the position a caller starts from, with no events.
    Baseline,
    /// At least one waited-for event since the cursor.
    Events,
    /// The deadline passed with no waited-for event.
    Timeout,
    /// Retention moved past the cursor; what it took is not reported.
    Gap,
    /// The scope no longer names the pane's current launch or binding.
    BindingReplaced,
    /// A record the read needs could not be used.
    Unavailable,
}

impl ObserveOutcome {
    /// A gap, a replacement and an unavailable read are incomplete
    /// answers; the rest are complete, a timeout included.
    pub fn complete(self) -> bool {
        matches!(self, Self::Baseline | Self::Events | Self::Timeout)
    }
}

/// An observe answer before the CLI wraps it in its envelope.
#[derive(Clone, Debug, Serialize)]
pub struct ObserveAnswer {
    pub outcome: ObserveOutcome,
    /// The pinned scope: the scope as asked, with the binding the
    /// baseline selected, so a caller can compare it with its own.
    pub scope: PaneScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// On `binding_replaced`: how the scope stopped matching, and the
    /// binding now selected, when one is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_relation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_binding_id: Option<String>,
    pub events: Vec<ObserveEvent>,
    /// The position to resume from; `None` when the chain is over (a gap
    /// or a replacement) or no position could be taken.
    pub cursor: Option<String>,
    #[serde(skip)]
    pub diagnostics: Vec<Diagnostic>,
}

/// What one pool held at a cursor's read.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PoolCursor {
    /// The ids of the observations the pool then held, sorted.
    seen: Vec<String>,
    floor: Option<String>,
    /// The newest `observed_mono_ns` the pool then held.
    high: Option<String>,
}

/// The decoded cursor. Callers treat the string as opaque; this shape is
/// versioned (`v` and the `v1.` prefix) so a later shape can refuse an
/// older one by name rather than misread it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveCursor {
    v: u8,
    address: crate::identity::PaneAddress,
    launch_id: String,
    binding_id: String,
    provider: String,
    filters: Vec<String>,
    call: Option<String>,
    pools: BTreeMap<String, PoolCursor>,
    lead_present: bool,
    /// Whether a prompt record existed at the cursor's read, so its later
    /// absence is told from its never having existed.
    prompts_present: bool,
    latest_prompt: Option<PromptReference>,
    /// The answer observation ids then known, sorted.
    answers: Vec<String>,
    /// The newest stamp among those answers, for the full-record gap rule.
    answers_high: Option<String>,
}

impl ObserveCursor {
    /// An encoded cursor to resume from, checked against the scope and
    /// filters of the call that would use it. A cursor used under a
    /// different scope or filter set is a usage error: observations the
    /// first set passed over are already inside it, so the second set
    /// would silently never see them.
    pub fn parse(text: &str, scope: &PaneScope, filters: &ObserveFilters) -> Result<Self> {
        let usage = |message: &str| AttentionError::usage(format!("--since {message}"));
        if text.len() > 128 * 1024 {
            return Err(usage("cursor is too large"));
        }
        let Some(encoded) = text.strip_prefix("v1.") else {
            return Err(usage("cursor is not recognized"));
        };
        let bytes = decode_base64url(encoded).ok_or_else(|| usage("cursor is not recognized"))?;
        let cursor: Self =
            serde_json::from_slice(&bytes).map_err(|_| usage("cursor is not recognized"))?;
        if cursor.v != 1
            || &cursor.address != scope.address()
            || cursor.launch_id != scope.launch_id()
            || scope
                .binding_id()
                .is_some_and(|binding| binding != cursor.binding_id)
            || cursor.filters != filters.names()
            || cursor.call != filters.call
        {
            return Err(usage(
                "cursor belongs to a different scope or filter set; take a new baseline",
            ));
        }
        // The shape inside is the encoder's own: an unknown pool, a stamp
        // that is not a 20-digit count, or an id no observation could
        // carry means this text did not come from an observe answer, and
        // trusting it could suppress events or the gap check.
        let stamp = |text: &str| crate::protocol::ns20_text(text);
        let id = |text: &str| !text.is_empty() && text.len() <= 64 && text.is_ascii();
        let valid = Provider::parse(&cursor.provider).is_some()
            && cursor
                .pools
                .keys()
                .map(String::as_str)
                .eq(["lead_general", "lead_requests"])
            && cursor.pools.values().all(|pool| {
                pool.seen.iter().all(|seen| id(seen))
                    && pool.floor.as_deref().is_none_or(stamp)
                    && pool.high.as_deref().is_none_or(stamp)
            })
            && cursor.answers.iter().all(|answer| id(answer))
            && cursor.answers_high.as_deref().is_none_or(stamp)
            && cursor
                .latest_prompt
                .as_ref()
                .is_none_or(|prompt| id(&prompt.observation_id) && stamp(&prompt.observed_mono_ns));
        if !valid {
            return Err(usage("cursor is not recognized"));
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(AttentionError::record_json)?;
        Ok(format!("v1.{}", encode_base64url(&bytes)))
    }
}

/// One binding's records, read for observe. A read that cannot be used is
/// an outcome, not an error: only a broken manifest fails the call.
struct State {
    /// The binding the launch's pointer selects.
    binding_id: String,
    provider: Provider,
    lead: Option<LifecycleSnapshot>,
    prompts: Option<LeadPrompts>,
}

enum Read {
    State(Box<State>),
    Replaced {
        relation: &'static str,
        current_binding_id: Option<String>,
        diagnostic: Diagnostic,
    },
    Unavailable(Vec<Diagnostic>),
}

fn read_failed(read: &RecordRead, facet: &str) -> Option<Diagnostic> {
    match read {
        RecordRead::Present(_) | RecordRead::Missing => None,
        RecordRead::Unavailable(error)
        | RecordRead::Invalid(error)
        | RecordRead::Unsupported(error) => {
            Some(error.diagnostic.clone().with("facet", facet.to_owned()))
        }
    }
}

fn read_state(root: &Path, scope: &PaneScope) -> Read {
    let address = scope.address();
    let claim = read_record_typed_at(root, "claim", &RecordIdentity::pane(address));
    if let Some(diagnostic) = read_failed(&claim, "claim") {
        return Read::Unavailable(vec![diagnostic]);
    }
    if claim.record().and_then(|r| r["launch_id"].as_str()) != Some(scope.launch_id()) {
        return Read::Replaced {
            relation: "launch_changed",
            current_binding_id: None,
            diagnostic: Diagnostic::new(
                DiagnosticCode::ClaimStale,
                "requested launch is no longer current",
            )
            .with("facet", "claim"),
        };
    }
    let pointer = read_record_typed_at(
        root,
        "current_binding",
        &RecordIdentity::launch(address, scope.launch_id()),
    );
    if let Some(diagnostic) = read_failed(&pointer, "binding_selection") {
        return Read::Unavailable(vec![diagnostic]);
    }
    let Some(selected) = pointer
        .record()
        .and_then(|r| r["binding_id"].as_str())
        .map(str::to_owned)
    else {
        return Read::Unavailable(vec![
            Diagnostic::new(
                DiagnosticCode::IdentityUnpublished,
                "no binding is selected for the requested launch",
            )
            .with("facet", "binding_selection"),
        ]);
    };
    if scope
        .binding_id()
        .is_some_and(|expected| expected != selected)
    {
        return Read::Replaced {
            relation: "binding_changed",
            current_binding_id: Some(selected),
            diagnostic: Diagnostic::new(
                DiagnosticCode::ClaimStale,
                "requested binding is no longer current",
            )
            .with("facet", "binding_selection"),
        };
    }
    let identity = RecordIdentity::binding(address, scope.launch_id(), &selected);
    let binding = read_record_typed_at(root, "binding", &identity);
    if let Some(diagnostic) = read_failed(&binding, "binding") {
        return Read::Unavailable(vec![diagnostic]);
    }
    let Some(binding_record) = binding.record() else {
        return Read::Unavailable(vec![
            Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                "selected binding record is absent",
            )
            .with("facet", "binding"),
        ]);
    };
    let Some(provider) = binding_record
        .get("provider")
        .and_then(|value| value.as_str())
        .and_then(Provider::parse)
    else {
        return Read::Unavailable(vec![
            Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                "selected binding names no supported provider",
            )
            .with("facet", "binding"),
        ]);
    };
    let snapshot = |kind: &str| -> std::result::Result<Option<LifecycleSnapshot>, Diagnostic> {
        let read = read_record_typed_at(root, kind, &identity);
        if let Some(diagnostic) = read_failed(&read, "lifecycle") {
            return Err(diagnostic);
        }
        let Some(record) = read.record() else {
            return Ok(None);
        };
        if record["provider"].as_str() != Some(provider.as_str()) {
            return Err(Diagnostic::new(
                DiagnosticCode::RecordInvalid,
                "lifecycle provider differs from selected binding",
            )
            .with("facet", "lifecycle"));
        }
        serde_json::from_value(record.clone())
            .map(Some)
            .map_err(|error| {
                AttentionError::record_json(error)
                    .diagnostic
                    .with("facet", "lifecycle")
            })
    };
    let lead = match snapshot("lifecycle_snapshot") {
        Ok(snapshot) => snapshot,
        Err(diagnostic) => return Read::Unavailable(vec![diagnostic]),
    };
    let prompts_read = read_record_typed_at(root, "lead_prompts", &identity);
    if let Some(diagnostic) = read_failed(&prompts_read, "lifecycle") {
        return Read::Unavailable(vec![diagnostic]);
    }
    let prompts = match prompts_read.record() {
        Some(record) => match serde_json::from_value::<LeadPrompts>(record.clone()) {
            Ok(prompts) => Some(prompts),
            Err(error) => {
                return Read::Unavailable(vec![
                    AttentionError::record_json(error)
                        .diagnostic
                        .with("facet", "lifecycle"),
                ]);
            }
        },
        None => None,
    };
    // The records are separate files, not a transaction: re-read the two
    // that select this binding, so a replacement between the first reads
    // and these cannot be assembled into one answer.
    let claim_again = read_record_typed_at(root, "claim", &RecordIdentity::pane(address));
    let pointer_again = read_record_typed_at(
        root,
        "current_binding",
        &RecordIdentity::launch(address, scope.launch_id()),
    );
    if read_failed(&claim_again, "claim").is_some()
        || read_failed(&pointer_again, "binding_selection").is_some()
        || claim_again.record() != claim.record()
        || pointer_again.record() != pointer.record()
    {
        return Read::Unavailable(vec![
            Diagnostic::new(
                DiagnosticCode::ClaimStale,
                "scope changed or became unavailable during observation",
            )
            .with("facet", "scope"),
        ]);
    }
    Read::State(Box::new(State {
        binding_id: selected,
        provider,
        lead,
        prompts,
    }))
}

/// The lead's two pools, under their view names. Only the lead's
/// happenings are events, so the children's file is not read: its
/// traffic can neither produce nor hide a waited-for event.
fn pools(state: &State) -> [(&'static str, Option<&ObservationPool>); 2] {
    let lead = state.lead.as_ref();
    [
        ("lead_requests", lead.map(|s| &s.pools.requests)),
        ("lead_general", lead.map(|s| &s.pools.general)),
    ]
}

/// The newest lead prompt the evidence names: the later of the prompt
/// record's and the newest the pools still hold, as the lifecycle view
/// takes it.
fn latest_prompt(state: &State) -> Option<PromptReference> {
    let pooled = state.lead.as_ref().and_then(|snapshot| {
        snapshot
            .lead_observations()
            .filter(|item| item.is_lead_prompt())
            .max_by(|a, b| {
                (&a.observed_mono_ns, &a.observation_id)
                    .cmp(&(&b.observed_mono_ns, &b.observation_id))
            })
            .map(PromptReference::of)
    });
    PromptReference::later_of(
        state.prompts.as_ref().and_then(|p| p.latest.clone()),
        pooled,
    )
}

/// The cursor a state would hand a caller.
fn cursor_of(state: &State, scope: &PaneScope, filters: &ObserveFilters) -> ObserveCursor {
    let mut pool_map = BTreeMap::new();
    for (name, pool) in pools(state) {
        let (seen, floor, high) = match pool {
            Some(pool) => {
                let mut seen: Vec<String> = pool
                    .observations
                    .iter()
                    .map(|item| item.observation_id.clone())
                    .collect();
                seen.sort();
                (
                    seen,
                    pool.retention_floor_mono_ns.clone(),
                    pool.observations
                        .iter()
                        .map(|item| item.observed_mono_ns.clone())
                        .max(),
                )
            }
            None => (vec![], None, None),
        };
        pool_map.insert(name.to_owned(), PoolCursor { seen, floor, high });
    }
    let (answers, answers_high) = match &state.prompts {
        Some(prompts) => {
            let mut ids: Vec<String> = prompts
                .answers
                .iter()
                .map(|answer| answer.observation_id.clone())
                .collect();
            ids.sort();
            (
                ids,
                prompts
                    .answers
                    .iter()
                    .map(|answer| answer.observed_mono_ns.clone())
                    .max(),
            )
        }
        None => (vec![], None),
    };
    ObserveCursor {
        v: 1,
        address: scope.address().clone(),
        launch_id: scope.launch_id().to_owned(),
        binding_id: state.binding_id.clone(),
        provider: state.provider.as_str().to_owned(),
        filters: filters.names(),
        call: filters.call.clone(),
        pools: pool_map,
        lead_present: state.lead.is_some(),
        prompts_present: state.prompts.is_some(),
        latest_prompt: latest_prompt(state),
        answers,
        answers_high,
    }
}

/// Whether retention moved past what `cursor` can account for in `state`.
///
/// Any movement of a pool's floor ends the chain, between two reads of
/// one blocking call as much as across calls: an observation that
/// arrived after the cursor, was stamped below its newest stamp, and
/// was evicted moved that floor, and no stamp comparison can tell it
/// from an eviction of what the cursor saw — so a wait that crosses an
/// eviction answers `gap`, and the caller re-baselines and waits again,
/// rather than the survivors being read as the whole story. The prompt
/// record keeps no floor; a record that was there at the cursor and is
/// gone, or one that no longer holds an answer the cursor knew — at any
/// size, since answers only leave a record by being drained at its cap
/// or by the record being replaced — is the same shape.
fn gap_between(cursor: &ObserveCursor, state: &State) -> bool {
    if cursor.lead_present && state.lead.is_none() {
        return true;
    }
    if cursor.prompts_present && state.prompts.is_none() {
        return true;
    }
    for (name, pool) in pools(state) {
        let Some(pool) = pool else { continue };
        let Some(floor) = &pool.retention_floor_mono_ns else {
            continue;
        };
        let Some(known) = cursor.pools.get(name) else {
            continue;
        };
        if known.floor.as_ref() != Some(floor) {
            return true;
        }
    }
    if let Some(prompts) = &state.prompts
        && (!cursor.answers.is_empty()
            && cursor.answers.iter().any(|id| {
                !prompts
                    .answers
                    .iter()
                    .any(|answer| &answer.observation_id == id)
            })
            || (prompts.answers.len() >= 64
                && prompts
                    .answers
                    .iter()
                    .map(|answer| answer.observed_mono_ns.as_str())
                    .min()
                    .is_some_and(|oldest| oldest > cursor.answers_high.as_deref().unwrap_or(""))))
    {
        return true;
    }
    false
}

/// The waited-for events in `state` that `cursor` has not seen, in
/// observation order. Diffing is by observation id, so the order an
/// observation carries never decides whether it is reported.
fn events_between(
    cursor: &ObserveCursor,
    state: &State,
    filters: &ObserveFilters,
) -> Vec<ObserveEvent> {
    let seen: BTreeSet<&str> = cursor
        .pools
        .values()
        .flat_map(|pool| pool.seen.iter().map(String::as_str))
        .collect();
    let mut events: Vec<(String, String, u8, ObserveEvent)> = Vec::new();
    let mut prompt_ids: BTreeSet<String> = BTreeSet::new();
    if let Some(snapshot) = &state.lead {
        for item in snapshot.lead_observations() {
            if seen.contains(item.observation_id.as_str()) {
                continue;
            }
            let base = |event: &str, rank: u8| {
                (
                    item.observed_mono_ns.clone(),
                    item.observation_id.clone(),
                    rank,
                    ObserveEvent {
                        event: event.to_owned(),
                        observation_id: item.observation_id.clone(),
                        kind: item.body.kind().to_owned(),
                        source_event: Some(item.source_event.clone()),
                        observed_mono_ns: item.observed_mono_ns.clone(),
                        actor: item.actor.clone(),
                        turn_id: item.turn_id().map(str::to_owned),
                        tool_call_id: None,
                        question_index: None,
                        error_code: match &item.body {
                            ObservationBody::AttemptOutcome { error_code, .. } => {
                                error_code.clone()
                            }
                            ObservationBody::PromptSubmitted { .. }
                            | ObservationBody::ToolPreflight { .. }
                            | ObservationBody::ToolResult { .. }
                            | ObservationBody::ApprovalRequested { .. }
                            | ObservationBody::AutomaticDenial { .. }
                            | ObservationBody::ResponseFinished { .. }
                            | ObservationBody::RunSettled
                            | ObservationBody::UserInterrupt
                            | ObservationBody::ElicitationRequested { .. }
                            | ObservationBody::ElicitationActionSelected { .. }
                            | ObservationBody::Notice { .. }
                            | ObservationBody::CompactionAttempted { .. }
                            | ObservationBody::CompactionSucceeded { .. } => None,
                        },
                    },
                )
            };
            // A prompt the cursor already names as its latest — reported
            // through the prompt record in an earlier, torn read — is not
            // reported again when a later read finds it in a pool.
            if filters.prompt
                && item.is_lead_prompt()
                && cursor
                    .latest_prompt
                    .as_ref()
                    .is_none_or(|latest| latest.observation_id != item.observation_id)
            {
                prompt_ids.insert(item.observation_id.clone());
                events.push(base("prompt", 0));
            }
            if filters.turn_end && item.ends_turn(state.provider) {
                events.push(base("turn_end", 2));
            }
        }
    }
    if filters.prompt
        && let Some(latest) = latest_prompt(state)
        && cursor
            .latest_prompt
            .as_ref()
            .is_none_or(|known| known.observation_id != latest.observation_id)
        && !prompt_ids.contains(&latest.observation_id)
        && !seen.contains(latest.observation_id.as_str())
    {
        events.push((
            latest.observed_mono_ns.clone(),
            latest.observation_id.clone(),
            0,
            ObserveEvent {
                event: "prompt".to_owned(),
                observation_id: latest.observation_id.clone(),
                kind: "prompt_submitted".to_owned(),
                source_event: None,
                observed_mono_ns: latest.observed_mono_ns.clone(),
                actor: Actor::Lead,
                turn_id: None,
                tool_call_id: None,
                question_index: None,
                error_code: None,
            },
        ));
    }
    if filters.answer
        && let Some(prompts) = &state.prompts
    {
        let known: BTreeSet<&str> = cursor.answers.iter().map(String::as_str).collect();
        for answer in &prompts.answers {
            if known.contains(answer.observation_id.as_str()) {
                continue;
            }
            if filters
                .call
                .as_deref()
                .is_some_and(|call| call != answer.tool_call_id)
            {
                continue;
            }
            events.push((
                answer.observed_mono_ns.clone(),
                answer.observation_id.clone(),
                1,
                ObserveEvent {
                    event: "answer".to_owned(),
                    observation_id: answer.observation_id.clone(),
                    kind: "prompt_submitted".to_owned(),
                    source_event: None,
                    observed_mono_ns: answer.observed_mono_ns.clone(),
                    actor: Actor::Lead,
                    turn_id: None,
                    tool_call_id: Some(answer.tool_call_id.clone()),
                    question_index: Some(answer.question_index),
                    error_code: None,
                },
            ));
        }
    }
    events.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
    events.into_iter().map(|(_, _, _, event)| event).collect()
}

/// Observe one scope: a baseline without `since`, otherwise the events
/// since it, polling until `timeout` when one is given. A zero timeout is
/// one read, whose `timeout` outcome simply says nothing waited-for was
/// new at that read.
pub fn observe(
    root: &Path,
    scope: &PaneScope,
    filters: &ObserveFilters,
    since: Option<&ObserveCursor>,
    timeout: Duration,
) -> Result<ObserveAnswer> {
    let pinned = |state: &State| {
        PaneScope::new(
            scope.address().clone(),
            scope.launch_id().to_owned(),
            Some(state.binding_id.clone()),
        )
    };
    let answer = |outcome: ObserveOutcome,
                  state_scope: PaneScope,
                  provider: Option<String>,
                  events: Vec<ObserveEvent>,
                  cursor: Option<String>,
                  diagnostics: Vec<Diagnostic>| ObserveAnswer {
        outcome,
        scope: state_scope,
        provider,
        scope_relation: None,
        current_binding_id: None,
        events,
        cursor,
        diagnostics,
    };
    let started = Instant::now();
    let mut working: Option<ObserveCursor> = since.cloned();
    // The reads name the cursor's binding, not whatever the launch now
    // selects: a replacement is then reported from the pointer alone,
    // before the replacement's own records are read and can mask it as
    // an unavailable read. A baseline pins this from its first state.
    let mut read_scope = match since {
        Some(cursor) => PaneScope::new(
            scope.address().clone(),
            scope.launch_id().to_owned(),
            Some(cursor.binding_id.clone()),
        )?,
        None => scope.clone(),
    };
    loop {
        match read_state(root, &read_scope) {
            Read::Replaced {
                relation,
                current_binding_id,
                diagnostic,
            } => {
                let state_scope = match &working {
                    Some(cursor) => PaneScope::new(
                        scope.address().clone(),
                        scope.launch_id().to_owned(),
                        Some(cursor.binding_id.clone()),
                    )?,
                    None => scope.clone(),
                };
                let mut result = answer(
                    ObserveOutcome::BindingReplaced,
                    state_scope,
                    working.as_ref().map(|cursor| cursor.provider.clone()),
                    vec![],
                    None,
                    vec![diagnostic],
                );
                result.scope_relation = Some(relation.to_owned());
                result.current_binding_id = current_binding_id;
                return Ok(result);
            }
            Read::Unavailable(diagnostics) => {
                let cursor = match &working {
                    Some(cursor) => Some(cursor.encode()?),
                    None => None,
                };
                return Ok(answer(
                    ObserveOutcome::Unavailable,
                    scope.clone(),
                    working.as_ref().map(|cursor| cursor.provider.clone()),
                    vec![],
                    cursor,
                    diagnostics,
                ));
            }
            Read::State(state) => {
                if let Some(cursor) = &working
                    && cursor.binding_id != state.binding_id
                {
                    // The pointer moved between the cursor and this read:
                    // the scope argument may not name the cursor's binding,
                    // so the replacement is named from the state itself.
                    let mut result = answer(
                        ObserveOutcome::BindingReplaced,
                        pinned(&state).unwrap_or_else(|_| scope.clone()),
                        Some(state.provider.as_str().to_owned()),
                        vec![],
                        None,
                        vec![
                            Diagnostic::new(
                                DiagnosticCode::ClaimStale,
                                "requested binding is no longer current",
                            )
                            .with("facet", "binding_selection"),
                        ],
                    );
                    result.scope_relation = Some("binding_changed".to_owned());
                    result.current_binding_id = Some(state.binding_id.clone());
                    return Ok(result);
                }
                read_scope = pinned(&state)?;
                let current = cursor_of(&state, scope, filters);
                match &working {
                    None => {
                        if timeout.is_zero() {
                            return Ok(answer(
                                ObserveOutcome::Baseline,
                                pinned(&state)?,
                                Some(state.provider.as_str().to_owned()),
                                vec![],
                                Some(current.encode()?),
                                vec![],
                            ));
                        }
                        working = Some(current);
                    }
                    Some(cursor) => {
                        if gap_between(cursor, &state) {
                            return Ok(answer(
                                ObserveOutcome::Gap,
                                pinned(&state)?,
                                Some(state.provider.as_str().to_owned()),
                                vec![],
                                None,
                                vec![
                                    Diagnostic::new(
                                        DiagnosticCode::RecordInvalid,
                                        "lifecycle retention moved past the cursor; take a new baseline",
                                    )
                                    .with("facet", "lifecycle"),
                                ],
                            ));
                        }
                        let events = events_between(cursor, &state, filters);
                        if !events.is_empty() {
                            return Ok(answer(
                                ObserveOutcome::Events,
                                pinned(&state)?,
                                Some(state.provider.as_str().to_owned()),
                                events,
                                Some(current.encode()?),
                                vec![],
                            ));
                        }
                        working = Some(current);
                    }
                }
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let (state_scope, provider, cursor) = match &working {
                Some(cursor) => (
                    PaneScope::new(
                        scope.address().clone(),
                        scope.launch_id().to_owned(),
                        Some(cursor.binding_id.clone()),
                    )?,
                    Some(cursor.provider.clone()),
                    Some(cursor.encode()?),
                ),
                None => (scope.clone(), None, None),
            };
            return Ok(answer(
                ObserveOutcome::Timeout,
                state_scope,
                provider,
                vec![],
                cursor,
                vec![],
            ));
        }
        std::thread::sleep(POLL.min(timeout - elapsed));
    }
}

/// The `v1.` cursor payload encoding: base64url of the cursor JSON, no
/// padding. It is a transport spelling, nothing more; the JSON is the
/// contract a later version can refuse.
fn encode_base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .fold(0u32, |value, byte| (value << 8) | u32::from(*byte))
            << (8 * (3 - chunk.len()));
        for index in 0..=chunk.len() {
            encoded.push(ALPHABET[((value >> (18 - 6 * index)) & 0x3f) as usize] as char);
        }
    }
    encoded
}

fn decode_base64url(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let value_of = |byte: u8| -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    };
    let mut decoded = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut value = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            value |= value_of(*byte)? << (18 - 6 * index);
        }
        decoded.push((value >> 16) as u8);
        if chunk.len() > 2 {
            decoded.push((value >> 8) as u8);
        }
        if chunk.len() > 3 {
            decoded.push(value as u8);
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_every_length() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0xff, 0xfe, 0xfd]] {
            assert_eq!(decode_base64url(&encode_base64url(bytes)).unwrap(), bytes);
        }
        assert_eq!(decode_base64url("a"), None);
        assert_eq!(decode_base64url("a==="), None);
    }
}
