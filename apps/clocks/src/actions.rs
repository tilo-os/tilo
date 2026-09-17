//! Stable local operation/resource contract for palette, agents and a11y.
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, OperationKind, Resource, ResourceProvider, RetryPolicy};

pub const OPERATION_TYPE: &str = "clocks.operation";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClockOperation {
    NewTimer { seconds: u64, label: String },
    ToggleTimer,
    AddMinute,
    ToggleOverlay,
    ResetStopwatch,
    Lap,
    OpenWorldClock { id: uuid::Uuid },
    OpenAlarm { id: uuid::Uuid },
    OpenTimer { id: uuid::Uuid },
}

pub fn item_address(kind: &str, id: uuid::Uuid) -> tilo_actions::ActionAddress {
    tilo_actions::ActionAddress::new(format!("clocks.{kind}:{id}"), "open")
}

#[allow(clippy::expect_used, reason = "ClockOperation always serializes")]
pub fn item_resource(
    kind: &str,
    id: uuid::Uuid,
    label: &str,
    operation: ClockOperation,
) -> Resource {
    Resource::new(
        format!("clocks.{kind}:{id}"),
        format!("clocks.{kind}"),
        label,
    )
    .with_action(
        ActionSpec::operation(
            "open",
            "Open in Clocks",
            OperationKind::View,
            OPERATION_TYPE,
            &operation,
        )
        .expect("ClockOperation serializes")
        .retry_policy(RetryPolicy::Idempotent),
    )
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
        .retry_policy(RetryPolicy::Idempotent)
}

pub struct ClockResources;
impl ResourceProvider for ClockResources {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![
            Resource::new("clocks", "clocks.collection", "Clocks")
                .with_action(op(
                    "new-timer",
                    "Start a timer",
                    &ClockOperation::NewTimer {
                        seconds: 300,
                        label: "Timer".into(),
                    },
                ))
                .with_action(op(
                    "toggle",
                    "Pause or resume timer",
                    &ClockOperation::ToggleTimer,
                ))
                .with_action(op(
                    "add-minute",
                    "Add one minute",
                    &ClockOperation::AddMinute,
                ))
                .with_action(op(
                    "overlay",
                    "Show or hide floating timer",
                    &ClockOperation::ToggleOverlay,
                ))
                .with_action(op("lap", "Record stopwatch lap", &ClockOperation::Lap)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ResourceProvider;
    #[test]
    fn contract_is_addressable() {
        assert_eq!(
            ClockResources.resources(&AutoCommit::new())[0].id.0,
            "clocks"
        );
    }
    #[test]
    fn rejects_foreign_payloads() {
        assert_eq!(decode("other", b"{}"), Ok(None));
    }
}
