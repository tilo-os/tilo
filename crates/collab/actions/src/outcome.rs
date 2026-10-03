//! Typed invocation identity, progress, failure, and successful results.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Resource kind for bytes addressed by their content hash.
///
/// Producers return `tilo.content:<hash>` in [`ActionOutput::resources`]; a
/// content-collecting action may then consume the hash without asking a model
/// to copy or invent it.
pub const CONTENT_RESOURCE_KIND: &str = "tilo.content";

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

    /// The same update, reported under `id`. A host that tracks a job under
    /// its own handle uses this to answer in the caller's invocation id.
    #[must_use]
    pub fn with_invocation(mut self, id: InvocationId) -> Self {
        match &mut self {
            Self::Accepted { invocation }
            | Self::Progress { invocation, .. }
            | Self::Item { invocation, .. }
            | Self::Succeeded { invocation, .. }
            | Self::Failed { invocation, .. }
            | Self::Cancelled { invocation, .. } => *invocation = id,
        }
        self
    }
}

/// Durable, deliberately redacted outcome of a consequential effect.
///
/// Receipts are suitable for shared workspace activity. They never retain
/// invocation arguments, streamed items, returned values/resource addresses,
/// failure messages, or structured failure details. Those values may contain
/// credentials or document content and remain available only through the
/// private invocation lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EffectReceiptStatus {
    Succeeded,
    Failed { code: String },
    Cancelled,
}

impl EffectReceiptStatus {
    /// Reduce a terminal lifecycle update to its safe durable representation.
    /// Non-terminal updates are intentionally not activity records.
    pub fn from_terminal(update: &InvocationUpdate) -> Option<Self> {
        match update {
            InvocationUpdate::Succeeded { .. } => Some(Self::Succeeded),
            InvocationUpdate::Failed { error, .. } => Some(Self::Failed {
                code: bounded_identifier(&error.code, "action_failed"),
            }),
            InvocationUpdate::Cancelled { .. } => Some(Self::Cancelled),
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => None,
        }
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Succeeded => "Completed",
            Self::Failed { .. } => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

fn bounded_identifier(value: &str, fallback: &str) -> String {
    let safe = value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        .take(96)
        .collect::<String>();
    if safe.is_empty() {
        fallback.to_string()
    } else {
        safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content resource addresses round-trip without guessing and effect receipts redact terminal
    /// payloads.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn outcomes_address_content_exactly_and_redact_terminal_payloads() {
        content_resource_addresses_round_trip_without_guessing();
        effect_receipt_redacts_terminal_payloads();
    }

    fn content_resource_addresses_round_trip_without_guessing() {
        let address = content_resource("abc123").expect("a content hash has an address");
        assert_eq!(address, "tilo.content:abc123");
        assert_eq!(content_hash(&address), Some("abc123"));
        assert_eq!(content_resource(""), None);
        assert_eq!(content_resource("two:parts"), None);
        assert_eq!(content_hash("files.entry:/tmp/a"), None);
    }

    fn effect_receipt_redacts_terminal_payloads() {
        let invocation = InvocationId::new();
        let update = InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "smtp.secret\ncode".into(),
                message: "password=hunter2".into(),
                details: Some(serde_json::json!({"token": "private"})),
                retryable: true,
            },
        };
        let receipt = EffectReceiptStatus::from_terminal(&update).expect("terminal receipt");
        assert_eq!(
            receipt,
            EffectReceiptStatus::Failed {
                code: "smtp.secretcode".into()
            }
        );
        let json = serde_json::to_string(&receipt).expect("encode receipt");
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("private"));
    }
}
