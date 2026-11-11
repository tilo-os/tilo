//! The record: which runs Agents shows. It follows the person to every device and is shared with
//! the workspace like any record, so an agent asked to "show me what Codex did" sets it as the
//! person would.

use tilo_resource::{self, Model, resource};

/// Which runs Agents shows. Each part is picked whole from a list, so each is written whole.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Console {
    /// Agent
    ///
    /// One run, by the name it shows, or one client, by its own name, for every run it has; empty
    /// for every agent.
    pub agent: String,
    /// Workspace
    ///
    /// Only the runs in this workspace, by its name; empty for every workspace.
    pub workspace: String,
}

impl Console {
    /// Whether it narrows the runs at all.
    #[must_use]
    pub fn narrows(&self) -> bool {
        self != &Self::default()
    }
}

#[resource(
    kind = "console.view",
    label = "Agent runs shown",
    effects = crate::ConsoleEffect
)]
impl Console {
    /// Show every agent's runs in every workspace again.
    #[action]
    pub fn show_everything(&mut self) {
        *self = Self::default();
    }
}
