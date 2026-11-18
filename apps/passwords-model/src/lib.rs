//! The `passwords.vault` resource: an empty document, as the app that publishes it
//! and an agent that reaches it both see it.
//!
//! The vault is this device's own encrypted store and nothing in it syncs, so
//! the document holds no secret material: no login, key or code is in it, in the
//! clear or otherwise.

use tilo_resource::{self, Model, resource};

/// The vault's published document: empty, because every secret stays in this device's
/// store, sealed by the vault's own key.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Passwords {}

impl Passwords {
    /// What the resource holds: nothing — the vault is this device's encrypted store, and no
    /// secret syncs.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "passwords.vault",
    read_content = Passwords::read_document
)]
impl Passwords {}
