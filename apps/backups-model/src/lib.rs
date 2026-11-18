//! The `backups.workspace` resource and the catalog it reads.

pub mod model;

pub use model::*;

use tilo_resource::{self, Model, resource};

/// What the document holds: the retention policy every device follows.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Backups {
    pub policy: model::RetentionPolicy,
}

impl Backups {
    /// What the resource holds: the retention policy every device follows.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "backups.workspace",
    read_content = Backups::read_document
)]
impl Backups {}
