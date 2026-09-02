//! Typed invocation identity, progress, failure, and successful results.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable key carried from admission through completion and used to
/// deduplicate retry-safe calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvocationId(pub Uuid);

impl InvocationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for InvocationId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Canonical resource addresses produced or revealed by the action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(default)]
    pub retryable: bool,
}

/// Monotonic lifecycle of one invocation. A transport timeout is not a failed
/// action: callers retain the invocation id and can resume observing it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InvocationUpdate {
    Accepted {
        invocation: InvocationId,
    },
    Progress {
        invocation: InvocationId,
        sequence: u64,
        phase: String,
        completed: Option<u64>,
        total: Option<u64>,
        message: Option<String>,
    },
    /// One ordered value from a streaming operation.
    Item {
        invocation: InvocationId,
        sequence: u64,
        value: serde_json::Value,
    },
    Succeeded {
        invocation: InvocationId,
        output: ActionOutput,
    },
    Failed {
        invocation: InvocationId,
        error: ActionFailure,
    },
    Cancelled {
        invocation: InvocationId,
        message: Option<String>,
    },
}

impl InvocationUpdate {
    pub fn invocation(&self) -> InvocationId {
        match self {
            Self::Accepted { invocation }
            | Self::Progress { invocation, .. }
            | Self::Item { invocation, .. }
            | Self::Succeeded { invocation, .. }
            | Self::Failed { invocation, .. }
            | Self::Cancelled { invocation, .. } => *invocation,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}
