//! Owner-admitted time ceilings for a task run, never model-supplied requests.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutionBudget {
    pub turn_ms: u32,
    pub total_ms: u32,
}

impl TaskExecutionBudget {
    /// Validate an explicit ceiling. This confers no caller or task authority.
    ///
    /// # Errors
    /// Rejects zero, excessive or contradictory deadlines.
    pub fn validate(self) -> Result<(), &'static str> {
        if self.turn_ms == 0
            || self.turn_ms > 900_000
            || self.total_ms == 0
            || self.total_ms > 7_200_000
            || self.turn_ms > self.total_ms
        {
            return Err(
                "task budget requires 1..900000ms per turn, 1..7200000ms total and turn <= total",
            );
        }
        Ok(())
    }
}
