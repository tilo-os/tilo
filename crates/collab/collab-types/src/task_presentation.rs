//! Provider-neutral, coalesced model-turn presentation. Not execution authority.

use serde::{Deserialize, Serialize};

/// Maximum retained turns per task execution. Excess turns fail explicitly.
pub const MAX_TASK_PRESENTATIONS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPresentationFinish {
    Complete,
    Truncated,
    Refused,
    Cancelled,
    Failed,
}

/// A full replacement snapshot, not a token delta. `run` is minted by the agent
/// from the task run id and model-turn index; `sequence` increases within it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPresentation {
    pub run: String,
    pub sequence: u64,
    /// Host-authored first receipt time. Sender values are never trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<i64>,
    pub text: String,
    pub reasoning: String,
    pub finish: Option<TaskPresentationFinish>,
    pub detail: String,
}

impl TaskPresentation {
    pub const MAX_RUN_BYTES: usize = 192;
    pub const MAX_TEXT_BYTES: usize = 32 * 1024;
    pub const MAX_REASONING_BYTES: usize = 16 * 1024;
    pub const MAX_DETAIL_BYTES: usize = 2 * 1024;

    /// Enforce retention bounds on UTF-8 boundaries and make any loss explicit.
    ///
    /// # Errors
    /// Rejects an empty, oversized, or control-character-containing run identifier.
    pub fn bounded(mut self) -> Result<Self, String> {
        if self.run.is_empty()
            || self.run.len() > Self::MAX_RUN_BYTES
            || self.run.chars().any(char::is_control)
        {
            return Err("invalid task presentation run".into());
        }
        let mut truncated = false;
        for (value, max) in [
            (&mut self.text, Self::MAX_TEXT_BYTES),
            (&mut self.reasoning, Self::MAX_REASONING_BYTES),
            (&mut self.detail, Self::MAX_DETAIL_BYTES),
        ] {
            if value.len() > max {
                value.truncate(value.floor_char_boundary(max));
                truncated = true;
            }
        }
        if truncated {
            self.finish = Some(TaskPresentationFinish::Truncated);
            self.detail = "Task presentation exceeded retention bounds".into();
        }
        Ok(self)
    }
}
