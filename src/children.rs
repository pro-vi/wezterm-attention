//! The sub-agents of one binding that are running now, and the rules that
//! change that set. No IO, clocks, provider control, or badge policy.
//!
//! A child leaves the set only on evidence that it ended: its own stop, a
//! parent stop that covers its last event, or the end of the binding. Nothing
//! removes a child for being quiet, so a child running one long command stays
//! counted. The set keeps no record of children that have stopped: Claude
//! Code 2.1.283 and Codex at source commit `985cf47a4` run a hook registered
//! without `async: true` to completion before the agent goes on, so one
//! child's events reach the writer in the order they happened, and a stop is
//! never followed by an older event of the same child.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::PaneAddress;
use crate::protocol::{
    AttentionError, Diagnostic, DiagnosticCode, Disposition, Result, vocabulary,
};

vocabulary!(ChildStatus { Running, Waiting });
vocabulary!(ChildEvent {
    Start,
    Tool,
    Permission
});

/// The manifest's `child_presence_statuses`, in the order it lists them.
pub const CHILD_STATUSES: [&str; 2] = ["running", "waiting"];
/// The manifest's `child_presence_events`, in the order it lists them.
pub const CHILD_EVENTS: [&str; 3] = ["start", "tool", "permission"];

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChildPresenceSet {
    pub kind: String,
    pub schema: u64,
    pub address: PaneAddress,
    pub launch_id: String,
    pub binding_id: String,
    pub provider: String,
    pub revision: String,
    pub written_at_unix_ns: String,
    pub live: Vec<LiveChild>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_clear: Option<ParentClear>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_end: Option<LifetimeEnd>,
}

/// A child that is running or waiting on the user. `agent_type` is the
/// provider's name for the kind of sub-agent; nothing decides by its value.
/// `last_event` is the kind of event that last changed the entry.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LiveChild {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    pub last_event: ChildEvent,
    pub status: ChildStatus,
    pub last_mono_ns: String,
}

/// The latest Codex parent stop: children at or before its order ended with
/// it, and `removed` names them, so one that works again afterwards shows that
/// the parent stopped before its children did.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParentClear {
    pub observed_mono_ns: String,
    pub event_id: String,
    pub removed: Vec<String>,
}

/// The binding end this set last applied.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LifetimeEnd {
    pub event_id: String,
    pub observed_mono_ns: String,
}

/// What a writer or reader knows about the binding's end record: its id and
/// order, and whether it ends the binding as the binding now stands.
#[derive(Clone, Copy, Debug)]
pub struct EndMark<'a> {
    pub event_id: &'a str,
    pub observed_mono_ns: &'a str,
    pub ends_binding: bool,
}

impl<'a> EndMark<'a> {
    /// The binding's end record `end`, if it has one, read against the
    /// binding record `binding`.
    pub fn of(end: Option<&'a Value>, binding: &Value) -> Option<Self> {
        end.map(|end| Self {
            event_id: end["event_id"].as_str().unwrap_or(""),
            observed_mono_ns: end["observed_mono_ns"].as_str().unwrap_or(""),
            ends_binding: crate::records::ends_binding(end, binding),
        })
    }
}

/// One event that can change the set. Orders are validated 20-digit
/// monotonic stamps, which compare correctly as text.
#[derive(Clone, Copy, Debug)]
pub enum ChildTransition<'a> {
    Start {
        agent_id: &'a str,
        agent_type: Option<&'a str>,
        order: &'a str,
    },
    Tool {
        agent_id: &'a str,
        agent_type: Option<&'a str>,
        order: &'a str,
    },
    Permission {
        agent_id: &'a str,
        agent_type: Option<&'a str>,
        order: &'a str,
    },
    Stop {
        agent_id: &'a str,
        order: &'a str,
    },
    ParentClear {
        order: &'a str,
        event_id: &'a str,
    },
    /// The ids of the tasks the provider lists as in flight on the lead's
    /// stop, which began at `order`.
    Reconcile {
        order: &'a str,
        in_flight: &'a [String],
    },
}

/// The result of one transition: whether the set changed and must be
/// written, what the hook reports, and at most one diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    pub changed: bool,
    pub disposition: Disposition,
    pub diagnostic: Option<Diagnostic>,
}

impl Reduction {
    fn of(changed: bool, disposition: Disposition) -> Self {
        Self {
            changed,
            disposition,
            diagnostic: None,
        }
    }

    fn diagnosed(
        changed: bool,
        disposition: Disposition,
        code: DiagnosticCode,
        message: &'static str,
    ) -> Self {
        Self {
            changed,
            disposition,
            diagnostic: Some(Diagnostic::new(code, message)),
        }
    }
}

impl ChildPresenceSet {
    /// An empty set for the binding a writer is about to change, with the
    /// provider and schema the writer sets before it writes.
    pub fn empty(address: PaneAddress, launch_id: &str, binding_id: &str) -> Self {
        Self {
            kind: "child_presence_set".to_owned(),
            schema: 0,
            address,
            launch_id: launch_id.to_owned(),
            binding_id: binding_id.to_owned(),
            provider: String::new(),
            revision: String::new(),
            written_at_unix_ns: String::new(),
            live: Vec::new(),
            parent_clear: None,
            lifetime_end: None,
        }
    }

    /// The rules the manifest's shape cannot state: one entry per child in
    /// `live`, and one per child in the parent clear's `removed`.
    pub fn validate_semantics(&self) -> Result<()> {
        fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> bool {
            let mut seen = std::collections::BTreeSet::new();
            ids.into_iter().all(|id| seen.insert(id))
        }
        if !unique(self.live.iter().map(|child| child.agent_id.as_str())) {
            return Err(AttentionError::new(
                DiagnosticCode::RecordInvalid,
                "a child appears twice in the live set",
            ));
        }
        if let Some(clear) = &self.parent_clear
            && !unique(clear.removed.iter().map(String::as_str))
        {
            return Err(AttentionError::new(
                DiagnosticCode::RecordInvalid,
                "a child appears twice among those a parent stop removed",
            ));
        }
        Ok(())
    }

    /// The children a reader counts: none while the binding has an end this
    /// set has not applied, or one that ends the binding. Every write applies
    /// the binding's end before anything else, so a set that has not applied
    /// its end was written before that end, and what it holds belongs to the
    /// lifetime the end closed.
    pub fn counted(&self, end: Option<EndMark<'_>>) -> &[LiveChild] {
        let closed = end.is_some_and(|end| {
            end.ends_binding
                || self
                    .lifetime_end
                    .as_ref()
                    .is_none_or(|last| last.event_id != end.event_id)
        });
        if closed { &[] } else { &self.live }
    }

    /// Applies one transition. The binding's end is applied first whatever
    /// the transition is, so a set written before an end never carries its
    /// children into the binding's next lifetime.
    pub fn apply(
        &mut self,
        end: Option<EndMark<'_>>,
        transition: ChildTransition<'_>,
    ) -> Reduction {
        let mut changed = false;
        if let Some(end) = end
            && self
                .lifetime_end
                .as_ref()
                .is_none_or(|last| last.event_id != end.event_id)
        {
            self.live.clear();
            self.parent_clear = None;
            self.lifetime_end = Some(LifetimeEnd {
                event_id: end.event_id.to_owned(),
                observed_mono_ns: end.observed_mono_ns.to_owned(),
            });
            changed = true;
        }
        let reduction = match transition {
            ChildTransition::ParentClear { order, event_id } => {
                self.clear_for_parent(order, event_id)
            }
            ChildTransition::Reconcile { order, in_flight } => self.reconcile(order, in_flight),
            ChildTransition::Stop { agent_id, order } => self
                .ignored_by_end(end, order)
                .unwrap_or_else(|| self.stop(agent_id, order)),
            ChildTransition::Start {
                agent_id,
                agent_type,
                order,
            } => self
                .ignored_by_end(end, order)
                .unwrap_or_else(|| self.advance(agent_id, agent_type, order, ChildEvent::Start)),
            ChildTransition::Tool {
                agent_id,
                agent_type,
                order,
            } => self
                .ignored_by_end(end, order)
                .unwrap_or_else(|| self.advance(agent_id, agent_type, order, ChildEvent::Tool)),
            ChildTransition::Permission {
                agent_id,
                agent_type,
                order,
            } => self.ignored_by_end(end, order).unwrap_or_else(|| {
                self.advance(agent_id, agent_type, order, ChildEvent::Permission)
            }),
        };
        Reduction {
            changed: changed || reduction.changed,
            ..reduction
        }
    }

    /// Why a child's event at `order` cannot change the set, if it cannot:
    /// the binding has ended, or the event comes from before its last end.
    fn ignored_by_end(&self, end: Option<EndMark<'_>>, order: &str) -> Option<Reduction> {
        if end.is_some_and(|end| end.ends_binding) {
            return Some(Reduction::diagnosed(
                false,
                Disposition::Ignored,
                DiagnosticCode::BindingConflict,
                "child observation arrived after the binding ended",
            ));
        }
        self.lifetime_end
            .as_ref()
            .is_some_and(|last| order <= last.observed_mono_ns.as_str())
            .then(|| {
                Reduction::diagnosed(
                    false,
                    Disposition::Ignored,
                    DiagnosticCode::BindingConflict,
                    "child observation predates the binding's last end",
                )
            })
    }

    fn clear_for_parent(&mut self, order: &str, event_id: &str) -> Reduction {
        if let Some(clear) = &self.parent_clear {
            if clear.observed_mono_ns.as_str() > order {
                return Reduction::of(false, Disposition::Ignored);
            }
            if clear.observed_mono_ns == order {
                return if clear.event_id == event_id {
                    Reduction::of(false, Disposition::Skipped)
                } else {
                    Reduction::diagnosed(
                        false,
                        Disposition::Conflict,
                        DiagnosticCode::RecordInvalid,
                        "equal parent-clear order has different content",
                    )
                };
            }
        }
        let (removed, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.live)
            .into_iter()
            .partition(|child| child.last_mono_ns.as_str() <= order);
        self.live = live;
        self.parent_clear = Some(ParentClear {
            observed_mono_ns: order.to_owned(),
            event_id: event_id.to_owned(),
            removed: removed.into_iter().map(|child| child.agent_id).collect(),
        });
        Reduction::of(true, Disposition::Applied)
    }

    /// Ends the children the provider no longer lists, so one whose stop
    /// never came stops being counted. A child with an event after `order`,
    /// when the stop's hook began, stays: the provider took its list a little
    /// before that, so the list says nothing of the event. A child that
    /// starts in between is ended too, and counted again at its next typed
    /// event. Nothing is recorded of the children it ends.
    fn reconcile(&mut self, order: &str, in_flight: &[String]) -> Reduction {
        let before = self.live.len();
        self.live.retain(|child| {
            child.last_mono_ns.as_str() > order || in_flight.contains(&child.agent_id)
        });
        if self.live.len() == before {
            Reduction::of(false, Disposition::Skipped)
        } else {
            Reduction::of(true, Disposition::Applied)
        }
    }

    fn stop(&mut self, agent_id: &str, order: &str) -> Reduction {
        let Some(index) = self
            .live
            .iter()
            .position(|child| child.agent_id == agent_id)
        else {
            // A child this set does not hold: one that already stopped, one
            // the provider runs for itself, or one that was never admitted.
            // One the latest parent stop removed, stopping after that stop,
            // shows the parent stopped before its child did.
            if self.parent_clear.as_ref().is_some_and(|clear| {
                order > clear.observed_mono_ns.as_str()
                    && clear.removed.iter().any(|removed| removed == agent_id)
            }) {
                return Reduction::diagnosed(
                    false,
                    Disposition::Skipped,
                    DiagnosticCode::ChildActiveAfterParentClear,
                    "a child stopped after the parent stop that ended it",
                );
            }
            return Reduction::of(false, Disposition::Skipped);
        };
        let last = self.live[index].last_mono_ns.as_str();
        if order < last {
            return Reduction::of(false, Disposition::Ignored);
        }
        if order == last {
            return Reduction::diagnosed(
                false,
                Disposition::Conflict,
                DiagnosticCode::RecordInvalid,
                "equal child order has different content",
            );
        }
        self.live.remove(index);
        Reduction::of(true, Disposition::Applied)
    }

    /// Applies an event that shows a child working: a start, a tool call, or
    /// a permission request, named by `event`.
    fn advance(
        &mut self,
        agent_id: &str,
        agent_type: Option<&str>,
        order: &str,
        event: ChildEvent,
    ) -> Reduction {
        if self
            .parent_clear
            .as_ref()
            .is_some_and(|clear| order <= clear.observed_mono_ns.as_str())
        {
            return Reduction::diagnosed(
                false,
                Disposition::Ignored,
                DiagnosticCode::BindingConflict,
                "child observation is covered by parent clear",
            );
        }
        let status_after = |current: Option<ChildStatus>| match event {
            ChildEvent::Start => current.unwrap_or(ChildStatus::Running),
            ChildEvent::Tool => ChildStatus::Running,
            ChildEvent::Permission => ChildStatus::Waiting,
        };
        if let Some(child) = self
            .live
            .iter_mut()
            .find(|child| child.agent_id == agent_id)
        {
            let last = child.last_mono_ns.as_str();
            if order < last {
                return Reduction::of(false, Disposition::Ignored);
            }
            let status = status_after(Some(child.status));
            if order == last {
                return if child.status == status && child.last_event == event {
                    Reduction::of(false, Disposition::Skipped)
                } else {
                    Reduction::diagnosed(
                        false,
                        Disposition::Conflict,
                        DiagnosticCode::RecordInvalid,
                        "equal child order has different content",
                    )
                };
            }
            child.status = status;
            child.last_event = event;
            child.last_mono_ns = order.to_owned();
            if let Some(agent_type) = agent_type {
                child.agent_type = Some(agent_type.to_owned());
            }
            return Reduction::of(true, Disposition::Applied);
        }
        // Claude Code 2.1.283 also runs agents of its own, and one sent a tool
        // event with an agent id and no type; only a start or a typed event
        // shows a sub-agent someone asked for. Such an event is normal, so the
        // hook still succeeds; the diagnostic is there for a sub-agent that
        // arrives untyped.
        if event != ChildEvent::Start && agent_type.is_none() {
            return Reduction::diagnosed(
                false,
                Disposition::Skipped,
                DiagnosticCode::RecordInvalid,
                "child event has no agent type and follows no start",
            );
        }
        let resumed_after_parent_clear = self.parent_clear.as_mut().is_some_and(|clear| {
            let before = clear.removed.len();
            clear.removed.retain(|removed| removed != agent_id);
            clear.removed.len() != before
        });
        self.live.push(LiveChild {
            agent_id: agent_id.to_owned(),
            agent_type: agent_type.map(str::to_owned),
            last_event: event,
            status: status_after(None),
            last_mono_ns: order.to_owned(),
        });
        if resumed_after_parent_clear {
            Reduction::diagnosed(
                true,
                Disposition::Applied,
                DiagnosticCode::ChildActiveAfterParentClear,
                "a child worked after the parent stop that ended it",
            )
        } else {
            Reduction::of(true, Disposition::Applied)
        }
    }

    /// Whether some counted child asked for permission at or after `since`
    /// and has done nothing since: a child that can hold a notify ordered at
    /// `since`.
    pub fn waits_since(&self, end: Option<EndMark<'_>>, since: &str) -> bool {
        self.counted(end).iter().any(|child| {
            child.status == ChildStatus::Waiting && child.last_mono_ns.as_str() >= since
        })
    }
}
