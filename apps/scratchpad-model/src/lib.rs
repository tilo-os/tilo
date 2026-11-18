//! The `scratchpad.note` resource: the one note a workspace keeps, as the app
//! that draws it and an agent that reads it both see it.

use tilo_resource::{self, Model, resource};

/// The note.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    /// Note text
    pub body: String,
}

impl Note {
    /// What the resource holds: the note's text.
    ///
    /// # Errors
    /// When the field cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "scratchpad.note",
    read_content = Note::read_document
)]
impl Note {}
