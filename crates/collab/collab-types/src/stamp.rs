//! What a resource store's change names in its message: the action that made it, which every
//! holder judges the change against, and which history and undo group it by.

use serde::{Deserialize, Serialize};

/// The action a change was made by, and the membership its author saw: what a holder judges
/// the change against. It travels in the change itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The verb invoked.
    pub verb: String,
    /// Who invoked it: a person, or an agent acting for one.
    pub invoker: String,
    /// The invocation: every change it made, in every document, names it, so history and undo
    /// take them as one step.
    pub invocation: String,
    /// The heads of the membership the author saw when it wrote the change, as the membership
    /// names them: who may do what is judged as of there, not as of the receiver's now.
    pub membership: Vec<String>,
    /// The invocation this one takes back, when it is an undo: it names that invocation's verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    /// When it was invoked, ISO-8601 UTC, recorded like an argument so every holder reads the
    /// same; empty where the invoker named no time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
}

impl Stamp {
    /// The message a change carries it as.
    #[must_use]
    pub fn message(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The stamp a change's message `message` names; `None` for one that names none.
    #[must_use]
    pub fn of_message(message: &str) -> Option<Self> {
        serde_json::from_str(message).ok()
    }

    /// The stamp `change` carries; `None` for a change that carries none.
    #[must_use]
    pub fn of(change: &automerge::Change) -> Option<Self> {
        Self::of_message(change.message()?)
    }
}
