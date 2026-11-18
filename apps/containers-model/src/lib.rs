//! The `containers.engine` resource: the new-container draft, and the values the window reads.

pub mod draft;
pub mod model;

pub use draft::{CreateDraft, CreateField};
pub use model::*;

use tilo_resource::{self, Model, resource};

#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Containers {
    /// New container
    pub draft: CreateDraft,
}

impl Containers {
    /// What the resource holds: the new-container draft.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "containers.engine",
    read_content = Containers::read_document
)]
impl Containers {
    /// Clear new container
    #[action]
    pub fn clear_create(&mut self) {
        self.draft = CreateDraft::default();
    }
}
