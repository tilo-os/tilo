//! Naming an action as data, and what kind of action it is.

use serde::{Deserialize, Serialize};

/// The stored form of a reference to one published action: a Shortcut step,
/// an agent's plan, a palette entry, a history entry. It names the app, the
/// resource kind that publishes the action and the action's public id.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActionKey {
    pub app: String,
    pub kind: String,
    pub action: String,
}

impl ActionKey {
    pub fn new(app: impl Into<String>, kind: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            kind: kind.into(),
            action: action.into(),
        }
    }
}

impl std::fmt::Display for ActionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.app, self.kind, self.action)
    }
}

pub use tilo_action_policy::ActionKind;
