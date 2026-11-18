//! The hex buffer as a resource: a file's bytes in the document, so they sync,
//! undo and show in history like any document. A keystroke writes the page it
//! lands in; the verbs below offer the same edits by offset, and loading a file
//! is a verb no undo takes back.

use tilo_resource::{self, Atom, Invoke, Model, Refusal, resource};

use crate::buffer::{ByteBuffer, Hex};
use crate::model::BufferEdit;

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct HexEditor {
    /// File name
    #[model(readonly)]
    pub name: Option<Atom>,
    /// Loads
    #[model(readonly)]
    pub opened: u64,
    /// Bytes
    pub bytes: ByteBuffer,
}

impl HexEditor {
    pub fn has_file(&self) -> bool {
        self.name.is_some()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|name| name.0.as_str())
    }
}

impl HexEditor {
    /// What the resource holds: the open buffer's bytes, its name and where it was
    /// opened.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "hex-editor.document",
    read_content = HexEditor::read_document
)]
impl HexEditor {
    /// Load a file's bytes. Loading syncs to everyone and no undo takes it back;
    /// unchanged bytes of the same file change nothing.
    #[action(external)]
    pub fn load(
        &mut self,
        #[param("The file's name")] name: String,
        #[param("The file's bytes")] bytes: Hex,
    ) {
        if self.name() == Some(name.as_str()) && self.bytes == bytes.0 {
            return;
        }
        self.name = Some(Atom(name));
        self.opened = self.opened.saturating_add(1);
        self.bytes = ByteBuffer::of(&bytes.0);
    }

    #[action]
    pub fn insert_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("Hexadecimal bytes, e.g. `de ad be ef`")] bytes: Hex,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Insert { offset, bytes })
    }

    #[action]
    pub fn overwrite_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("Hexadecimal bytes, e.g. `de ad be ef`")] bytes: Hex,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Overwrite { offset, bytes })
    }

    #[action(destructive)]
    pub fn delete_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("How many bytes")] len: usize,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Delete { offset, len })
    }
}

impl HexEditor {
    /// The byte edits at the cursor: insert or overwrite there, delete the selection or
    /// the byte under it.
    pub fn at_cursor(
        cursor: usize,
        len: usize,
        selection: Option<(usize, usize)>,
    ) -> Vec<tilo_resource::ActionDecl<Self>> {
        // The append position is valid for an insert even in an empty buffer.
        let at = cursor.min(len);
        let mut bound = vec![
            Self::INSERT_BYTES
                .bind(hex_editor::InsertBytes::paths().offset, at)
                .declaration(
                    "insert-byte",
                    tilo_resource::word!("hex-editor-document-insert-byte"),
                ),
        ];
        if cursor < len {
            bound.push(
                Self::OVERWRITE_BYTES
                    .bind(hex_editor::OverwriteBytes::paths().offset, cursor)
                    .declaration(
                        "overwrite-byte",
                        tilo_resource::word!("hex-editor-document-overwrite-byte"),
                    ),
            );
        }
        let delete = |offset: usize, len: usize| {
            Self::DELETE_BYTES
                .bind(hex_editor::DeleteBytes::paths().offset, offset)
                .and(hex_editor::DeleteBytes::paths().len, len)
        };
        if let Some((lo, hi)) = selection {
            bound.push(delete(lo, hi - lo + 1).declaration(
                "delete-selection",
                tilo_resource::word!("hex-editor-document-delete-selection"),
            ));
        } else if cursor < len {
            bound.push(delete(cursor, 1).declaration(
                "delete-byte",
                tilo_resource::word!("hex-editor-document-delete-byte"),
            ));
        }
        bound.into_iter().flatten().collect()
    }

    pub fn loading(name: impl Into<String>, bytes: Vec<u8>) -> Invoke<Self> {
        Self::LOAD.with(hex_editor::Load {
            name: name.into(),
            bytes: Hex(bytes),
        })
    }
}
