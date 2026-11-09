//! The app's record: which runs Agents shows. It follows the person to every
//! device and is shared with the workspace like any record, so an agent asked
//! to "show me what Codex did" sets it as the person would.
//!
//! The runs themselves are the workspaces' history, read through the session
//! ([`ui::tilo_actions::remote::workspace_activity`]); this record holds no
//! part of them.

use ui::prelude::*;

/// Which runs Agents shows. Each part is picked whole from a list, so each
/// is written whole.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Console {
    /// Agent
    ///
    /// Only this agent's runs, by the name its runs show; empty for every
    /// agent.
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

    /// Whether a run by `agent` in `workspace` shows.
    pub fn shows(&self, agent: &str, workspace: Option<&str>) -> bool {
        (self.agent.is_empty() || self.agent == agent)
            && (self.workspace.is_empty() || Some(self.workspace.as_str()) == workspace)
    }
}

#[resource(kind = "console.view", label = "Agent runs shown")]
impl Console {
    /// Show every agent's runs in every workspace again.
    #[action]
    pub fn show_everything(&mut self) {
        *self = Self::default();
    }
}
