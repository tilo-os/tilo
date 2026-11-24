//! Handoff: what the shell and one application instance say to each other
//! while that instance's window moves to another workspace.
//!
//! The shell addresses one instance by its primary route and asks it to save;
//! the instance answers on the same route. Both ride session-local control
//! frames that are never relayed to peers (the toolkit's `CollabEvent::Handoff`
//! and `CollabRequest::ReportHandoff`).
//!
//! - [`HandoffRequest::Checkpoint`]: make everything the window shows
//!   durable now, so the shell reads the document as the person left it
//!   rather than as its writer last got round to saving it.
//!
//! The record itself moves by the workspace store's own `move` action, which
//! the shell invokes once what the window shows is durable.

/// Upper bound on one encoded message; anything larger is not a handoff.
const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// The shell asking one instance to save what its window shows.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffRequest {
    /// Make everything this window shows durable, then answer.
    Checkpoint { token: u64 },
}

/// A window's app answering a handoff request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "report", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffReport {
    /// The window's document is durable.
    Done { token: u64 },
    /// The step could not be done.
    Failed { token: u64, reason: String },
}

impl HandoffRequest {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed request.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bounded(bytes)
    }
}

impl HandoffReport {
    /// The number of the request this answers.
    pub const fn token(&self) -> u64 {
        match self {
            Self::Done { token } | Self::Failed { token, .. } => *token,
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed report.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bounded(bytes)
    }
}

/// `bytes` read as a `T`, when they are no more than [`MAX_MESSAGE_BYTES`].
fn bounded<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    tilo_failure::ignored!(
        "bytes that do not read as a handoff message are no handoff",
        serde_json::from_slice(bytes).ok()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// The save messages on the wire: what round-trips, what is refused and what is bounded.
        fn save_messages_are_bounded_and_exact;

        phase save_messages_round_trip_and_refuse_anything_else;
        {
            let request = HandoffRequest::Checkpoint { token: 7 };
            assert_eq!(HandoffRequest::decode(&request.encode()), Some(request));
            let done = HandoffReport::Done { token: 8 };
            assert_eq!(HandoffReport::decode(&done.encode()), Some(done));
            let failed = HandoffReport::Failed {
                token: 8,
                reason: "the record is not here".into(),
            };
            assert_eq!(HandoffReport::decode(&failed.encode()), Some(failed));
            assert_eq!(HandoffRequest::decode(b"{\"step\":\"format_disk\"}"), None);
            assert_eq!(
                HandoffRequest::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
                None
            );
            let oversized = HandoffReport::Failed {
                token: 1,
                reason: "x".repeat(MAX_MESSAGE_BYTES),
            };
            assert!(HandoffReport::decode(&oversized.encode()).is_none());
        }
    }
}
