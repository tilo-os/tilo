//! Owner-admitted ceilings for a task run, never model-supplied requests.
use serde::{Deserialize, Serialize};

/// The most model turns a budget may allow one run.
pub const MAX_RUN_TURNS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutionBudget {
    pub turn_ms: u32,
    pub total_ms: u32,
    /// The most model turns the run may take; `None` leaves the run bounded
    /// by its time alone. Older senders omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

impl TaskExecutionBudget {
    /// Validate an explicit ceiling. This confers no caller or task authority.
    ///
    /// # Errors
    /// Rejects zero, excessive or contradictory ceilings.
    pub fn validate(self) -> Result<(), &'static str> {
        if self.turn_ms == 0
            || self.turn_ms > 900_000
            || self.total_ms == 0
            || self.total_ms > 7_200_000
            || self.turn_ms > self.total_ms
            || self
                .max_turns
                .is_some_and(|turns| turns == 0 || turns > MAX_RUN_TURNS)
        {
            return Err(
                "task budget requires 1..900000ms per turn, 1..7200000ms total, turn <= total \
                 and 1..10000 turns",
            );
        }
        Ok(())
    }
}
