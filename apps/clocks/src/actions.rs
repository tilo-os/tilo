//! Stable local operation/resource contract for palette, agents and a11y.
use crate::ClockState;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, OperationKind, Resource, ResourcePresentation, ResourceProvider};
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
    let remaining = timer.remaining(chrono::Utc::now()).as_secs();
    Resource::new(
        format!("clocks.timer:{}", timer.id),
        "clocks.timer",
        &timer.label,
    )
    .with_presentation(ResourcePresentation {
        detail: Some(format!(
            "{} · {} remaining",
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
            "{} · {}",
            alarm.local_time.format("%H:%M"),
            crate::app::repeat_summary(&alarm.weekdays)
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

/// Recomputed from the durable state each time global search asks. The resource
/// tree is consequently a truthful snapshot, rather than presentation rows.
pub fn provide_resources(state: &ClockState) -> Vec<Resource> {
    let timers = Resource::new("clocks.timers", "clocks.timer-list", "Timers")
        .with_action(op(
            "new-timer",
            "Start a five-minute timer",
            &ClockOperation::NewTimer {
                seconds: 300,
                label: "Timer".into(),
            },
        ))
        .with_children(state.timers.iter().map(timer_resource));
    let alarms = Resource::new("clocks.alarms", "clocks.alarm-list", "Alarms")
        .with_action(op(
            "new-alarm",
            "Create a 07:30 alarm",
            &ClockOperation::NewAlarm {
                hours: 7,
                minutes: 30,
                label: "Alarm".into(),
            },
        ))
        .with_children(state.alarms.iter().map(alarm_resource));
    let world = Resource::new("clocks.world", "clocks.world-list", "World clocks")
        .with_action(op(
            "add-new-york",
            "Add New York",
            &ClockOperation::AddWorldClock {
                city: "New York".into(),
                timezone: "America/New_York".into(),
            },
        ))
        .with_children(state.world_clocks.iter().map(world_clock_resource));
    let stopwatch = Resource::new("clocks.stopwatch", "clocks.stopwatch", "Stopwatch")
        .with_presentation(ResourcePresentation {
            detail: Some(crate::app::duration(
                state
                    .stopwatch
                    .elapsed(chrono::Utc::now())
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
        ));
    vec![
        Resource::new("clocks", "clocks.collection", "Clocks")
            .with_children([world, alarms, timers, stopwatch]),
    ]
}

/// Empty-state provider retained for lightweight contract consumers. The app
/// itself uses [`provide_resources`] with its current state.
pub struct ClockResources;
impl ResourceProvider for ClockResources {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&ClockState::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn contract_is_addressable_and_empty_state_is_truthful() {
        let root = provide_resources(&ClockState::default()).remove(0);
        assert_eq!(root.id.0, "clocks");
        assert_eq!(root.children.len(), 4);
        assert!(root.children.iter().any(|r| r.id.0 == "clocks.timers"));
    }

    #[test]
    fn timer_operations_address_the_exact_timer() {
        let mut state = ClockState::default();
        let timer = crate::Timer::new("Pasta", Duration::from_secs(600));
        let id = timer.id;
        state.timers.push(timer);
        let root = provide_resources(&state).remove(0);
        let timer = root.children[2].children.first().expect("timer resource");
        assert_eq!(timer.id.0, format!("clocks.timer:{id}"));
        let encoded = &timer.actions[0];
        let tilo_actions::ActionInput::Immediate(encoded) = &encoded.input else {
            panic!("timer action should be immediate");
        };
        let operation = decode(&encoded.type_id, &encoded.payload).expect("valid operation");
        assert_eq!(operation, Some(ClockOperation::ToggleTimer { id }));
    }

    #[test]
    fn rejects_foreign_payloads() {
        assert_eq!(decode("other", b"{}"), Ok(None));
    }
}
