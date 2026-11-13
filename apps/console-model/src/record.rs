//! The record: which runs Agents shows.

use tilo_resource::{self, Model, resource};

/// Which runs Agents shows.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Console {
    /// Agent
    pub agent: String,
    /// Workspace
    pub workspace: String,
}

impl Console {
    /// Whether it narrows the runs at all.
    #[must_use]
    pub fn narrows(&self) -> bool {
        self != &Self::default()
    }
}

impl Console {
    /// What the record holds: which runs Agents shows: the agent and the workspace.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "console.view",
    effects = crate::ConsoleEffect,
    read_content = Console::read_document
)]
impl Console {
    /// Show every agent's runs in every workspace again.
    #[action]
    pub fn show_everything(&mut self) {
        *self = Self::default();
    }
}
