//! Typed invocation identity, progress, failure, and successful results.

use serde::{Deserialize, Serialize};

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

/// The content reference an argument names: a `tilo.content:<reference>`
/// address, or the bare reference a plan that carried the content gives.
pub fn named_reference(value: &str) -> Option<&str> {
    let value = value.trim();
    let reference = content_hash(value).unwrap_or(value);
    tilo_content::valid_reference(reference).then_some(reference)
}

/// The content address (lowercase BLAKE3 hex) a record's value names: a `tilo.content:<reference>`
/// address or a `tilo-blob://<reference>` link, its reference with or without the key that opens
/// it. What a record names this way is what its readers read by the address; `None` for a value
/// that names no content.
pub fn named_content(value: &str) -> Option<String> {
    let value = value.trim();
    let reference = value
        .strip_prefix(tilo_content::BLOB_SCHEME)
        .or_else(|| content_hash(value))?;
    tilo_content::content_address(reference)
}

/// Every content address `text` names anywhere in it ([`named_content`]): a `tilo-blob://` link
/// or a `tilo.content:` address standing alone, or inside a page's words, as a picture placed in
/// a document is (`![plan](tilo-blob://<hash>)`).
pub fn contents_named_in(text: &str) -> impl Iterator<Item = String> + '_ {
    const CONTENT_ADDRESS: &str = "tilo.content:";
    [tilo_content::BLOB_SCHEME, CONTENT_ADDRESS]
        .into_iter()
        .flat_map(move |prefix| {
            text.match_indices(prefix).filter_map(move |(at, _)| {
                let rest = text.get(at.saturating_add(prefix.len())..)?;
                // A reference is its hash and, for sealed content, the key after a `.`.
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
                    .unwrap_or(rest.len());
                let reference = rest.get(..end)?.trim_end_matches('.');
                tilo_content::content_address(reference)
                    .or_else(|| tilo_content::content_address(reference.get(..64)?))
            })
        })
}

pub use tilo_action_types::InvocationId;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct ActionOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Canonical resource addresses produced or revealed by the action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

read_through_json_value!(ActionOutput);

impl ActionOutput {
    /// The characters this action wrote, as its answer names them (`wrote`): the view showing that
    /// text draws them in the writer's colour, with the writer's caret at their end.
    #[must_use]
    pub fn wrote(&self) -> Option<crate::InteractionAnchor> {
        serde_json::from_value(self.value.as_ref()?.get("wrote")?.clone()).ok()
    }

    /// `value`, an answer, naming the characters `wrote` the action wrote too. An answer that is
    /// no object (a summary in words) is kept as it is.
    #[must_use]
    pub fn noting_wrote(
        value: Option<serde_json::Value>,
        wrote: &crate::InteractionAnchor,
    ) -> Option<serde_json::Value> {
        let mut fields = match value {
            None | Some(serde_json::Value::Null) => serde_json::Map::new(),
            Some(serde_json::Value::Object(fields)) => fields,
            other => return other,
        };
        fields.insert("wrote".into(), serde_json::to_value(wrote).ok()?);
        Some(serde_json::Value::Object(fields))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct ActionFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(default)]
    pub retryable: bool,
}

read_through_json_value!(ActionFailure);

impl ActionFailure {
    /// The failure a run answers with when what it reached needs the person's yes: its caller
    /// is refused for want of their answer, showing them `facts`
    /// ([`crate::remote::ActionCallError::ConfirmationRequired`]), and their approve sends the
    /// call again, which the handler reads ([`crate::remote::InvokeRequest::approval`]).
    #[must_use]
    pub fn asks(message: impl Into<String>, facts: &crate::remote::asked::AskedFacts) -> Self {
        Self {
            code: crate::remote::CONFIRMATION_REQUIRED_CODE.to_owned(),
            message: message.into(),
            details: Some(facts.to_details()),
            retryable: false,
        }
    }
}

/// Monotonic lifecycle of one invocation. A transport timeout is not a failed
/// action: callers retain the invocation id and can resume observing it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "state", rename_all = "snake_case")]
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

read_through_json_value!(InvocationUpdate);

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

    /// Whether it ended in a failure the same call may meet again: whoever
    /// follows the invocation learns to ask again, and a request under its id
    /// makes it once more rather than being handed this failure.
    #[must_use]
    pub fn asks_again(&self) -> bool {
        matches!(self, Self::Failed { error, .. } if error.retryable)
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
        an_invocation_id_is_read_back_as_the_caller_wrote_it();
    }

    /// A caller that keeps an invocation id of its own (a page's dialog, a script's file) asks
    /// again under the same one.
    fn an_invocation_id_is_read_back_as_the_caller_wrote_it() {
        let invocation = InvocationId::new();
        let text = invocation.0.to_string();
        assert_eq!(
            text.parse::<InvocationId>().expect("its own id reads back"),
            invocation
        );
        assert_eq!(
            format!(" {text} ")
                .parse::<InvocationId>()
                .expect("spaced text reads"),
            invocation
        );
        assert!("not an invocation".parse::<InvocationId>().is_err());
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
