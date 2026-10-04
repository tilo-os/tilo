//! Stable local operation/resource contract for palette, agents and a11y.
use crate::ClockState;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, OperationKind, Resource, ResourcePresentation};
use uuid::Uuid;

pub const OPERATION_TYPE: &str = "clocks.operation";

/// Every mutating operation names the durable entity it changes. This keeps
/// global commands and history from acting on an arbitrary list item.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClockOperation {
    NewTimer {
        seconds: u64,
        label: String,
    },
    NewAlarm {
        hours: u8,
        minutes: u8,
        label: String,
    },
    AddWorldClock {
        city: String,
        timezone: String,
    },
    ToggleTimer {
        id: Uuid,
    },
    AddMinute {
        id: Uuid,
    },
    SetTimerOverlay {
        id: Uuid,
        pinned: bool,
    },
    RemoveTimer {
        id: Uuid,
    },
    ToggleAlarm {
        id: Uuid,
    },
    RemoveAlarm {
        id: Uuid,
    },
    RemoveWorldClock {
        id: Uuid,
    },
    ToggleStopwatch,
    ResetStopwatch,
    Lap,
    SetStopwatchOverlay {
        pinned: bool,
    },
}

/// Decodes a Clocks operation payload; returns `Ok(None)` for other operation types.
///
/// # Errors
///
/// Returns a message when `type_id` is the Clocks operation type but the
/// payload is not a valid JSON-encoded [`ClockOperation`].
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<ClockOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| format!("invalid Clocks operation: {e}"))
}

fn op(id: &str, label: &str, value: &ClockOperation) -> ActionSpec {
    #[allow(
        clippy::expect_used,
        reason = "ClockOperation's data-only enum is always JSON serializable"
    )]
    ActionSpec::operation(id, label, OperationKind::Effect, OPERATION_TYPE, value)
        .expect("ClockOperation serializes")
}

fn timer_resource(timer: &crate::Timer) -> Resource {
    let remaining = timer.remaining(crate::app::now_utc()).as_secs();
    Resource::new(
        format!("clocks.timer:{}", timer.id),
        "clocks.timer",
        &timer.label,
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "{}, {} remaining",
            if timer.ends_at_utc.is_some() {
                "Running"
            } else {
                "Paused"
            },
            crate::app::duration(remaining.saturating_mul(1_000))
        )),
        badge: timer.pinned_overlay.then_some("Floating".into()),
        preview: None,
    })
    .with_action(op(
        "toggle",
        "Pause or resume",
        &ClockOperation::ToggleTimer { id: timer.id },
    ))
    .with_action(op(
        "add-minute",
        "Add one minute",
        &ClockOperation::AddMinute { id: timer.id },
    ))
    .with_action(op(
        "set-floating",
        if timer.pinned_overlay {
            "Stop floating"
        } else {
            "Keep floating"
        },
        &ClockOperation::SetTimerOverlay {
            id: timer.id,
            pinned: !timer.pinned_overlay,
        },
    ))
    .with_action(op(
        "remove",
        "Remove timer",
        &ClockOperation::RemoveTimer { id: timer.id },
    ))
}

fn alarm_resource(alarm: &crate::Alarm) -> Resource {
    Resource::new(
        format!("clocks.alarm:{}", alarm.id),
        "clocks.alarm",
        &alarm.label,
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "{} at {}",
            crate::app::repeat_summary(&alarm.weekdays),
            alarm.local_time.format("%H:%M")
        )),
        badge: Some(if alarm.enabled { "On" } else { "Off" }.into()),
        preview: None,
    })
    .with_action(op(
        "toggle",
        if alarm.enabled { "Turn off" } else { "Turn on" },
        &ClockOperation::ToggleAlarm { id: alarm.id },
    ))
    .with_action(op(
        "remove",
        "Remove alarm",
        &ClockOperation::RemoveAlarm { id: alarm.id },
    ))
}

fn stopwatch_resource(state: &ClockState) -> Resource {
    Resource::new("clocks.stopwatch", "clocks.stopwatch", "Stopwatch")
        .with_presentation(ResourcePresentation {
            detail: Some(crate::app::duration(
                state
                    .stopwatch
                    .elapsed(crate::app::now_utc())
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            )),
            badge: state.stopwatch.pinned_overlay.then_some("Floating".into()),
            preview: None,
        })
        .with_action(op(
            "toggle",
            "Start or pause",
            &ClockOperation::ToggleStopwatch,
        ))
        .with_action(op("lap", "Record lap", &ClockOperation::Lap))
        .with_action(op("reset", "Reset", &ClockOperation::ResetStopwatch))
        .with_action(op(
            "set-floating",
            if state.stopwatch.pinned_overlay {
                "Stop floating"
            } else {
                "Keep floating"
            },
            &ClockOperation::SetStopwatchOverlay {
                pinned: !state.stopwatch.pinned_overlay,
            },
        ))
}

fn world_clock_resource(clock: &crate::WorldClock) -> Resource {
    Resource::new(
        format!("clocks.world:{}", clock.id),
        "clocks.world-clock",
        &clock.city,
    )
    .with_presentation(ResourcePresentation {
        detail: Some(clock.timezone.clone()),
        badge: None,
        preview: None,
    })
    .with_action(op(
        "remove",
        "Remove city",
        &ClockOperation::RemoveWorldClock { id: clock.id },
    ))
}

/// Create / stopwatch commands that do not name a live timer, alarm, or city.
/// They ride [`crate::resource::Clocks::local_operations`] through
/// `tilo_resource::publish`. Per-item rows and the floating-overlay polarity
/// stay on [`live_clock_extras`].
pub(crate) fn collection_local_operations() -> Vec<ActionSpec> {
    vec![
        op(
            "new-timer",
            "Start a five-minute timer",
            &ClockOperation::NewTimer {
                seconds: 300,
                label: "Timer".into(),
            },
        ),
        op(
            "new-alarm",
            "Create a 07:30 alarm",
            &ClockOperation::NewAlarm {
                hours: 7,
                minutes: 30,
                label: "Alarm".into(),
            },
        ),
        op(
            "add-new-york",
            "Add New York",
            &ClockOperation::AddWorldClock {
                city: "New York".into(),
                timezone: "America/New_York".into(),
            },
        ),
        op("toggle", "Start or pause", &ClockOperation::ToggleStopwatch),
        op("lap", "Record lap", &ClockOperation::Lap),
        op("reset", "Reset", &ClockOperation::ResetStopwatch),
    ]
}

/// Live device rows beside `tilo_resource::publish`: timers, alarms, cities,
/// and the stopwatch floating toggle. Not a second `clocks.collection` root.
pub(crate) fn live_clock_extras(state: &ClockState) -> Vec<Resource> {
    let mut extras: Vec<Resource> = state.timers.iter().map(timer_resource).collect();
    extras.extend(state.alarms.iter().map(alarm_resource));
    extras.extend(state.world_clocks.iter().map(world_clock_resource));
    let mut stopwatch = stopwatch_resource(state);
    stopwatch
        .actions
        .retain(|action| action.id.as_str() == "set-floating");
    extras.push(stopwatch);
    extras
}

/// Empty-state provider. The app publishes through [`crate::resource::Clocks`].
pub struct ClockResources;
// Inherent resource publication (was ResourceProvider)
impl ClockResources {
    pub fn publish_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let state = ClockState::default();
        vec![ui::tilo_resource::publish::<crate::resource::Clocks>(
            "clocks", "Clocks", &state,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn resource_contract_addresses_the_exact_timer_and_rejects_foreign_payloads() {
        // Case: an empty collection publishes no timer rows.
        let empty = ui::tilo_resource::publish::<crate::resource::Clocks>(
            "clocks",
            "Clocks",
            &ClockState::default(),
        );
        assert_eq!(empty.id.0, "clocks", "case empty: root id");
        assert_eq!(empty.kind, "clocks.collection");
        assert!(
            empty.actions.iter().any(|action| action.id == "new-timer"),
            "case empty: start-timer stays on the collection"
        );
        assert!(
            empty
                .children
                .iter()
                .all(|child| child.kind != "clocks.timer"),
            "case empty: no timer rows"
        );

        // Case: timer operations address the exact timer.
        let mut state = ClockState::default();
        let timer = crate::Timer::new("Pasta", Duration::from_secs(600));
        let id = timer.id;
        state.timers.push(timer);
        let mut root =
            ui::tilo_resource::publish::<crate::resource::Clocks>("clocks", "Clocks", &state);
        root.children.extend(live_clock_extras(&state));
        let timer = root
            .children
            .iter()
            .find(|child| child.id.0 == format!("clocks.timer:{id}"))
            .expect("timer resource");
        let toggle = timer
            .actions
            .iter()
            .find(|action| action.id == "toggle")
            .expect("toggle");
        let tilo_actions::ActionInput::Immediate(encoded) = &toggle.input else {
            panic!("timer action should be immediate");
        };
        let operation = decode(&encoded.type_id, &encoded.payload).expect("valid operation");
        assert_eq!(
            operation,
            Some(ClockOperation::ToggleTimer { id }),
            "case exact-timer: operation"
        );

        // Case: foreign payloads are rejected.
        assert_eq!(decode("other", b"{}"), Ok(None), "case foreign-payload");
    }

    /// A clock state with one of every entity, so each resource kind and its
    /// operations are published.
    fn populated_state() -> ClockState {
        let mut state = ClockState::default();
        state
            .timers
            .push(crate::Timer::new("Pasta", Duration::from_secs(600)));
        state.alarms.push(crate::Alarm {
            id: Uuid::new_v4(),
            label: "Wake up".into(),
            local_time: chrono::NaiveTime::from_hms_opt(7, 30, 0).expect("valid time"),
            weekdays: std::collections::BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        });
        state.world_clocks.push(crate::WorldClock {
            id: Uuid::new_v4(),
            city: "Tokyo".into(),
            timezone: "Asia/Tokyo".into(),
        });
        state
    }

    // The published collection plus live timer, alarm, and city rows. The
    // registry is empty; the sweep checks the surface protocol.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: tilo_actions::ActionRegistry::new(),
        resources: |_| {
            let state = populated_state();
            let mut root = ui::tilo_resource::publish::<crate::resource::Clocks>(
                "clocks",
                "Clocks",
                &state,
            );
            root.children.extend(live_clock_extras(&state));
            vec![root]
        },
        palette: [
            "Start a five-minute timer",
            "Create a 07:30 alarm",
            "Add New York",
            "Remove timer",
            "Remove alarm",
            "Remove city",
            "Record lap",
        ],
    }
}
