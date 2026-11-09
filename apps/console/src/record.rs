//! The app's record: which runs Agents shows. It follows the person to every
//! device and is shared with the workspace like any record, so an agent asked
//! to "show me what Codex did" sets it as the person would.
//!
//! The runs themselves are the workspaces' history, read through the session
//! ([`ui::tilo_actions::remote::workspace_activity`]); this record holds no
//! part of them.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;

/// Which runs Agents shows. Each part is picked whole from a list, so each
/// is written whole.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Console {
    /// Agent
    ///
    /// One run, by the name it shows, or one client, by its own name, for
    /// every run it has; empty for every agent.
    pub agent: String,
    /// Workspace
    ///
    /// Only the runs in this workspace, by its name; empty for every
    /// workspace.
    pub workspace: String,
}

impl Console {
    /// Whether it narrows the runs at all.
    pub fn narrows(&self) -> bool {
        self != &Self::default()
    }

    /// Whether `run` shows: its own name picks it, and the name of the
    /// client it comes from picks every run of that client.
    pub fn shows(&self, run: &wire::AgentRun) -> bool {
        let named = self.agent.is_empty()
            || self.agent == run.agent.name
            || self.agent == crate::client::of(&run.agent.name).name();
        named
            && (self.workspace.is_empty()
                || Some(self.workspace.as_str()) == run.workspace.as_deref())
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
