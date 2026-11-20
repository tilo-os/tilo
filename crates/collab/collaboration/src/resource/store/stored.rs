//! A resource read as this device stored it, without opening it.

use automerge::AutoCommit;

use super::super::transfer;
use super::{DocumentId, ResourceOpenError, ResourceStore};

impl ResourceStore {
    /// The resource `id` as this device last made it durable, read without opening it, taking its
    /// writer or asking the workspace for it: what a conversion reads of a resource this build no
    /// longer opens. `None` when it is not kept here or does not read as that resource.
    #[must_use]
    pub fn stored_resource(&self, id: &DocumentId) -> Option<AutoCommit> {
        let store = self.backend()?;
        let (package, home) = self.app_and_home();
        if home.is_empty() {
            return None;
        }
        match transfer::read_by_id(&store, &package, &home, id) {
            Ok(doc) => Some(doc),
            Err(transfer::TransferError::Open(ResourceOpenError::MissingResource)) => None,
            Err(error) => {
                tracing::warn!(document = %id, workspace = %home, %error, "a stored document does not read");
                None
            }
        }
    }
}
