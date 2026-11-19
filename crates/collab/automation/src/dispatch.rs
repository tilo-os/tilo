//! Turning events into invocations: which armed program an event fires, with
//! what inputs, under which invocation id, in which order.
//!
//! Pure over what it is shown. The shell feeds it a person's record
//! changes; a workspace's host feeds it record changes and connector events
//! for bots. Both get the same answers, so a person's Shortcut and a bot
//! fire on the same change in the same way.
//!
//! The rules: no program fires on its own
//! change; a chain of invocations stops past [`MAX_CHAIN_DEPTH`]; an invocation's id is
//! derived from what fired it, so firing twice reuses every invocation id;
//! one installation's firings run one at a time in event order.

mod event;
mod matching;
pub mod queue;

#[cfg(test)]
mod tests;

pub use event::{
    AppEvent, ConnectorEvent, DocumentEvent, Event, ItemRef, ScheduleEvent, SourceEvent, TimeEvent,
    app_event_object, event_of,
};
pub use matching::{
    Armed, Dispatched, Firer, Firing, Skipped, dispatch, edited_paths, field_matches,
    invocation_id, verbs_of,
};
pub use tilo_actions::context::Cause;

/// An invocation fired at a greater depth than this fires nothing.
pub const MAX_CHAIN_DEPTH: u8 = 4;
/// Firings one installation may have waiting; the oldest beyond are
/// dead-lettered as "fell behind".
pub const MAX_QUEUED_FIRINGS: usize = 64;

/// Whether a dispatched firing represents a deterministic person commitment
/// that must bypass discretionary relevance judging and discretionary spend
/// caps, or discretionary proactive work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchClass {
    /// A due reminder, one-shot moment, or explicit person-set calendar or
    /// due-time commitment. Already authorized by the person: executes through
    /// the deterministic scheduler even when the discretionary judge is down
    /// or discretionary spend is exhausted.
    DeterministicDue,
    /// Proactive, periodic-polling, or reactive work governed by
    /// discretionary relevance judging and discretionary daily spend caps.
    Discretionary,
}

impl DispatchClass {
    /// Whether this firing is a deterministic due reminder or commitment.
    #[must_use]
    pub const fn is_deterministic_due(self) -> bool {
        matches!(self, Self::DeterministicDue)
    }

    /// Whether this firing bypasses discretionary judge and spend limits.
    #[must_use]
    pub const fn bypasses_discretionary_limits(self) -> bool {
        self.is_deterministic_due()
    }
}

/// Availability of discretionary relevance judging and discretionary spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscretionaryAvailability {
    /// Whether the relevance judge is reachable and healthy.
    pub judge_available: bool,
    /// Whether discretionary daily spend remains below its cap.
    pub budget_available: bool,
}

impl DiscretionaryAvailability {
    /// Both relevance judging and discretionary spend are available.
    #[must_use]
    pub const fn all_available() -> Self {
        Self {
            judge_available: true,
            budget_available: true,
        }
    }

    /// Relevance judge is unavailable while discretionary spend remains.
    #[must_use]
    pub const fn judge_down() -> Self {
        Self {
            judge_available: false,
            budget_available: true,
        }
    }

    /// Discretionary spend cap is exhausted while relevance judge is up.
    #[must_use]
    pub const fn budget_exhausted() -> Self {
        Self {
            judge_available: true,
            budget_available: false,
        }
    }

    /// Both relevance judging and discretionary spend are unavailable.
    #[must_use]
    pub const fn both_unavailable() -> Self {
        Self {
            judge_available: false,
            budget_available: false,
        }
    }
}

/// Scheduling admission decision for a dispatched [`Firing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchAdmission {
    /// Execute immediately via the deterministic scheduler, bypassing
    /// discretionary judge and discretionary spend limits.
    ProceedDeterministic,
    /// Execute under normal discretionary limits.
    ProceedDiscretionary,
    /// Defer because discretionary relevance judging is unavailable.
    DeferredJudgeUnavailable,
    /// Defer because discretionary spend cap is exhausted.
    DeferredBudgetExhausted,
}

impl DispatchAdmission {
    /// Whether the firing is admitted for execution now.
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(
            self,
            Self::ProceedDeterministic | Self::ProceedDiscretionary
        )
    }
}

/// Classify `trigger` into [`DispatchClass::DeterministicDue`] or
/// [`DispatchClass::Discretionary`].
///
/// One-shot moments (`Once`), due timestamps held by resources
/// (`ResourceTime`), and explicit wall-clock calendar schedules
/// (`Schedule::Calendar`) represent explicit person commitments and always
/// classify as [`DispatchClass::DeterministicDue`]. Periodic polling intervals
/// (`Schedule::Interval`) and reactive triggers classify as
/// [`DispatchClass::Discretionary`].
#[must_use]
pub const fn classify_trigger(trigger: &crate::shortcut::TriggerSpec) -> DispatchClass {
    use crate::shortcut::{Schedule, TriggerSpec};
    match trigger {
        TriggerSpec::Once { .. }
        | TriggerSpec::ResourceTime { .. }
        | TriggerSpec::Schedule {
            schedule: Schedule::Calendar { .. },
            ..
        } => DispatchClass::DeterministicDue,
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { .. },
            ..
        }
        | TriggerSpec::Keyboard { .. }
        | TriggerSpec::AppOpened { .. }
        | TriggerSpec::WorkspaceJoined { .. }
        | TriggerSpec::FileChanged { .. }
        | TriggerSpec::FocusMode { .. }
        | TriggerSpec::NetworkChanged { .. }
        | TriggerSpec::DeviceConnected { .. }
        | TriggerSpec::ResourceChanged { .. }
        | TriggerSpec::Connector { .. }
        | TriggerSpec::AppEvent { .. }
        | TriggerSpec::IncomingMessage { .. }
        | TriggerSpec::NearbyDevice { .. }
        | TriggerSpec::SourceChanged { .. } => DispatchClass::Discretionary,
    }
}

/// Decide whether `firing` executes under `availability`.
#[must_use]
pub const fn admit_firing(
    firing: &Firing,
    availability: DiscretionaryAvailability,
) -> DispatchAdmission {
    match classify_trigger(&firing.trigger) {
        DispatchClass::DeterministicDue => DispatchAdmission::ProceedDeterministic,
        DispatchClass::Discretionary => {
            if !availability.budget_available {
                DispatchAdmission::DeferredBudgetExhausted
            } else if !availability.judge_available {
                DispatchAdmission::DeferredJudgeUnavailable
            } else {
                DispatchAdmission::ProceedDiscretionary
            }
        }
    }
}

/// Split `firings` into those admitted for execution now and those deferred by
/// discretionary limits, preserving relative order in both groups.
#[must_use]
pub fn partition_admitted(
    firings: Vec<Firing>,
    availability: DiscretionaryAvailability,
) -> (Vec<Firing>, Vec<(Firing, DispatchAdmission)>) {
    let mut admitted = Vec::with_capacity(firings.len());
    let mut deferred = Vec::new();
    for firing in firings {
        let decision = admit_firing(&firing, availability);
        if decision.is_admitted() {
            admitted.push(firing);
        } else {
            deferred.push((firing, decision));
        }
    }
    (admitted, deferred)
}

impl Firing {
    /// Whether this firing is [`DispatchClass::DeterministicDue`] or
    /// [`DispatchClass::Discretionary`].
    #[must_use]
    pub const fn dispatch_class(&self) -> DispatchClass {
        classify_trigger(&self.trigger)
    }

    /// Whether this firing bypasses discretionary judge and spend limits.
    #[must_use]
    pub const fn is_deterministic_due(&self) -> bool {
        self.dispatch_class().is_deterministic_due()
    }

    /// Evaluate scheduling admission for this firing under `availability`.
    #[must_use]
    pub const fn admit(&self, availability: DiscretionaryAvailability) -> DispatchAdmission {
        admit_firing(self, availability)
    }
}

#[cfg(test)]
mod deterministic_due_tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::shortcut::{CatchUp, Schedule, Shortcut, TimeOfDay, TriggerSpec};

    fn program(triggers: Vec<TriggerSpec>) -> Shortcut {
        let mut shortcut: Shortcut = serde_json::from_value(json!({
            "id": "due-check",
            "name": "Due check",
            "steps": [],
            "saved_by": "ada"
        }))
        .unwrap();
        shortcut.triggers = triggers;
        shortcut
    }

    #[test]
    fn deterministic_due_commitments_bypass_discretionary_judge_and_budget_limits() {
        let triggers = vec![
            TriggerSpec::Once { at_ms: 14_000 },
            TriggerSpec::ResourceTime {
                kind: "tasks.issues".to_owned(),
                field: "issues.*.due_ms".to_owned(),
                offset_secs: 0,
                filter: Map::new(),
            },
            TriggerSpec::Schedule {
                schedule: Schedule::Calendar {
                    at: TimeOfDay {
                        hour: 14,
                        minute: 0,
                    },
                    weekdays: vec![],
                },
                catch_up: CatchUp::default(),
            },
            TriggerSpec::Schedule {
                schedule: Schedule::Interval { seconds: 1800 },
                catch_up: CatchUp::default(),
            },
            TriggerSpec::SourceChanged {
                kind: crate::source_change::PERSON_MAIL.to_owned(),
                at: String::new(),
                every_secs: None,
                filter: Map::new(),
            },
        ];

        assert_eq!(
            classify_trigger(&triggers[0]),
            DispatchClass::DeterministicDue
        );
        assert_eq!(
            classify_trigger(&triggers[1]),
            DispatchClass::DeterministicDue
        );
        assert_eq!(
            classify_trigger(&triggers[2]),
            DispatchClass::DeterministicDue
        );
        assert_eq!(classify_trigger(&triggers[3]), DispatchClass::Discretionary);
        assert_eq!(classify_trigger(&triggers[4]), DispatchClass::Discretionary);

        let shortcut = program(triggers);
        let armed = [Armed {
            firer: Firer::Bot { installation: "b1" },
            shortcut: &shortcut,
        }];

        let once_event = ScheduleEvent {
            workspace: "ws-1".to_owned(),
            trigger: 0,
            due_ms: 14_000,
        };
        let time_event = TimeEvent {
            workspace: "ws-1".to_owned(),
            trigger: 1,
            key: "issue-1".to_owned(),
            at_ms: 14_000,
            payload: Map::new(),
        };
        let calendar_event = ScheduleEvent {
            workspace: "ws-1".to_owned(),
            trigger: 2,
            due_ms: 14_000,
        };
        let interval_event = ScheduleEvent {
            workspace: "ws-1".to_owned(),
            trigger: 3,
            due_ms: 14_000,
        };
        let source_event = SourceEvent {
            workspace: "ws-1".to_owned(),
            trigger: 4,
            key: "msg-1".to_owned(),
            payload: Map::new(),
        };

        let mut firings = Vec::new();
        firings.extend(dispatch(&armed, Event::Schedule(&once_event)).firings);
        firings.extend(dispatch(&armed, Event::Time(&time_event)).firings);
        firings.extend(dispatch(&armed, Event::Schedule(&calendar_event)).firings);
        firings.extend(dispatch(&armed, Event::Schedule(&interval_event)).firings);
        firings.extend(dispatch(&armed, Event::Source(&source_event)).firings);
        assert_eq!(firings.len(), 5);

        // When both judge and discretionary spend are unavailable, all 3
        // DeterministicDue firings proceed while both Discretionary firings defer.
        let (admitted, deferred) = partition_admitted(
            firings.clone(),
            DiscretionaryAvailability::both_unavailable(),
        );
        assert_eq!(admitted.len(), 3);
        assert!(admitted.iter().all(Firing::is_deterministic_due));
        assert_eq!(deferred.len(), 2);
        assert!(
            deferred
                .iter()
                .all(|(_, reason)| *reason == DispatchAdmission::DeferredBudgetExhausted)
        );

        // When judge alone is down, DeterministicDue still proceeds and Discretionary defers.
        let (admitted_judge_down, deferred_judge_down) =
            partition_admitted(firings.clone(), DiscretionaryAvailability::judge_down());
        assert_eq!(admitted_judge_down.len(), 3);
        assert_eq!(deferred_judge_down.len(), 2);
        assert!(
            deferred_judge_down
                .iter()
                .all(|(_, reason)| *reason == DispatchAdmission::DeferredJudgeUnavailable)
        );

        // When both judge and budget are available, all 5 firings proceed.
        let (admitted_all, deferred_all) =
            partition_admitted(firings, DiscretionaryAvailability::all_available());
        assert_eq!(admitted_all.len(), 5);
        assert!(deferred_all.is_empty());
    }
}
