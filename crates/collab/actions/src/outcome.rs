//! Typed invocation identity, progress, failure, and successful results.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Resource kind for bytes addressed by their content hash.
///
/// Producers return `qos.content:<hash>` in [`ActionOutput::resources`]; a
/// content-collecting action may then consume the hash without asking a model
/// to copy or invent it.
pub const CONTENT_RESOURCE_KIND: &str = "qos.content";

/// Build the canonical resource address for content-addressed bytes.
///
/// Empty hashes and hashes containing the address separator are refused so a
/// malformed producer cannot create an ambiguous resource address.
pub fn content_resource(hash: &str) -> Option<String> {
    (!hash.is_empty() && !hash.contains(':')).then(|| format!("{CONTENT_RESOURCE_KIND}:{hash}"))
}

/// Read a content hash from its canonical resource address.
pub fn content_hash(address: &str) -> Option<&str> {
    address
        .strip_prefix(CONTENT_RESOURCE_KIND)?
        .strip_prefix(':')
        .filter(|hash| !hash.is_empty() && !hash.contains(':'))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_resource_addresses_round_trip_without_guessing() {
        let address = content_resource("abc123").expect("a content hash has an address");
        assert_eq!(address, "qos.content:abc123");
        assert_eq!(content_hash(&address), Some("abc123"));
        assert_eq!(content_resource(""), None);
        assert_eq!(content_resource("two:parts"), None);
        assert_eq!(content_hash("files.entry:/tmp/a"), None);
    }
}
