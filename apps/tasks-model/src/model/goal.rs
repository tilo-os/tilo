//! What an issue works towards: the project or area above it.

use serde::{Deserialize, Serialize};
use tilo_resource::{JsonSchema, Model};

/// What an issue's goal is: an ordinary resource of the workspace, above the
/// tasks written for it.
#[derive(Model, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Area {
    /// The resource's address in the app that keeps it.
    pub resource: String,
    /// The app that keeps it (`so.tilo.notes`).
    pub app: String,
    /// The resource's kind (`notes.note`).
    pub kind: String,
    /// How it reads where it is named, such as its title.
    pub label: String,
}

impl Area {
    /// Whether this names a resource: an address and the app keeping it.
    #[must_use]
    pub fn names_a_resource(&self) -> bool {
        !self.resource.trim().is_empty() && !self.app.trim().is_empty()
    }
}
