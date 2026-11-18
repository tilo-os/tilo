//! The `contacts.book` resource: the contacts and the places their groups and duplicates sit in.

pub mod model;

pub use model::*;

use tilo_resource::{self, Model, resource};

/// The address book's own resource: the page every contact hangs off. The
/// contacts are kept in numbered parts beside it ([`crate::contact_cards::ContactCards`]),
/// so what a change costs is one part whatever the book holds.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Contacts {}

impl Contacts {
    /// What the book's own resource holds, read through
    /// [`fields`](tilo_resource::read::fields): nothing of its own, since the
    /// contacts are kept in the parts beside it and read from those. A caller
    /// with no window open is told that, rather than meeting a resource that
    /// offers no read at all.
    ///
    /// # Errors
    /// A named item is not this read's to answer.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(kind = "contacts.book", read_content = Contacts::read_document)]
impl Contacts {}

/// The id of the place of the group `name`, the same for every spelling
/// of it.
pub fn group_place(name: &str) -> String {
    format!("group:{}", normalize_text(name.trim()))
}

pub fn duplicate_place(survivor: ContactId, source: ContactId) -> String {
    format!("duplicate:{survivor}:{source}")
}
