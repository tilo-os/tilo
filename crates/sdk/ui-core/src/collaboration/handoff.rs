//! Handoff: what the shell and one application instance say to each other
//! while that instance's window moves to another workspace.
//!
//! The shell addresses one instance by its primary route and asks it for
//! one step at a time; the instance answers on the same route. Both ride
//! session-local control frames that are never relayed to peers
//! ([`crate::collaboration::CollabEvent::Handoff`],
//! [`crate::collaboration::CollabRequest::ReportHandoff`]).
//!
//! - [`HandoffRequest::Checkpoint`]: make everything the window shows
//!   durable now, so the shell reads the document as the person left it
//!   rather than as its writer last got round to saving it.
//! - [`HandoffRequest::Retire`]: the window has reopened in its new
//!   workspace. Stamp the document it showed retired, as one change of its
//!   own that travels to everyone holding it over its normal route, make
//!   that durable, and say which heads it had before the stamp — everything
//!   up to there is what still has to come along.
//!
//! An instance also reports on its own when a document it shows arrives
//! retired from somewhere else ([`HandoffReport::Retired`]), so its desktop
//! can tell the person who moved it where.

use automerge::ChangeHash;

pub use crate::record::transfer::Successor;

/// Upper bound on one encoded message; anything larger is not a handoff.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// The shell asking one instance for the next step of a handoff.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffRequest {
    /// Make everything this window shows durable, then answer.
    Checkpoint { token: u64 },
    /// The window moved to `successor`: stamp what it showed retired, make
    /// that durable, then answer with the heads from before the stamp.
    Retire { token: u64, successor: Successor },
}

/// One instance answering a handoff request, or reporting a retirement.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "report", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffReport {
    /// The step is done. `heads` are the shown document's heads before the
    /// step changed it, hex-encoded; empty when it changed nothing.
    Done { token: u64, heads: Vec<String> },
    /// The step could not be done; nothing was retired.
    Failed { token: u64, reason: String },
    /// A document this instance shows arrived retired: moved elsewhere.
    Retired { successor: Successor },
}

impl HandoffRequest {
    pub fn token(&self) -> u64 {
        match self {
            Self::Checkpoint { token } | Self::Retire { token, .. } => *token,
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed request.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() <= MAX_MESSAGE_BYTES)
            .then(|| serde_json::from_slice(bytes).ok())
            .flatten()
    }
}

impl HandoffReport {
    /// Done, with `heads` as they were before the step.
    pub fn done(token: u64, heads: &[ChangeHash]) -> Self {
        Self::Done {
            token,
            heads: heads.iter().map(ToString::to_string).collect(),
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed report.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() <= MAX_MESSAGE_BYTES)
            .then(|| serde_json::from_slice(bytes).ok())
            .flatten()
    }

    /// The heads a [`Self::Done`] carries; `None` when any is malformed.
    pub fn heads(&self) -> Option<Vec<ChangeHash>> {
        match self {
            Self::Done { heads, .. } => heads.iter().map(|head| head.parse().ok()).collect(),
            Self::Failed { .. } | Self::Retired { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_messages_round_trip_and_refuse_anything_else() {
        let successor = Successor {
            room: "3f2b1c00-0000-4000-8000-0000000000e1".into(),
            record: "uuid:5b0e".into(),
            by: "person:ada".into(),
        };
        for request in [
            HandoffRequest::Checkpoint { token: 7 },
            HandoffRequest::Retire {
                token: 8,
                successor: successor.clone(),
            },
        ] {
            assert_eq!(HandoffRequest::decode(&request.encode()), Some(request));
        }
        let mut doc = automerge::AutoCommit::new();
        let _ = automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "a", 1);
        doc.commit();
        let heads = doc.get_heads();
        let done = HandoffReport::done(8, &heads);
        let decoded = HandoffReport::decode(&done.encode()).unwrap();
        assert_eq!(decoded.heads(), Some(heads));
        let retired = HandoffReport::Retired { successor };
        assert_eq!(HandoffReport::decode(&retired.encode()), Some(retired));
        assert_eq!(HandoffRequest::decode(b"{\"step\":\"format_disk\"}"), None);
        assert_eq!(
            HandoffRequest::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
            None
        );
    }
}
