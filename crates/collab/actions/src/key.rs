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

/// What an action is able to do, derived by the framework from its code and
/// never declared by an app. Grants are given by kind.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum ActionKind {
    /// Shows or reads; nothing to undo.
    Read,
    /// Changes the document and nothing outside it; undone by construction.
    #[default]
    Undoable,
    /// Reaches outside the document: sends, writes a device, calls a server.
    Outside,
}

impl ActionKind {
    pub const fn is_undoable(&self) -> bool {
        matches!(self, Self::Undoable)
    }
}
