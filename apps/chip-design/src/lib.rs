//! Native engineering evidence viewer for model-specific chip development.

mod model;
mod resource;
mod view;

pub use model::{Assumptions, AttentionCache, StorageReport};
pub use resource::{ChipProject, ProjectAction, ProjectState};
pub use view::ChipDesignShell;

/// The same resource application can be mounted by native and browser hosts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<ChipDesignShell>()
}

#[cfg(test)]
mod tests;
