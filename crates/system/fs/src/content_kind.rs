//! Deciding a record's content kind from its record metadata alone (its name), never from the bytes a version holds, so no version is downloaded
//! to classify it.

use crate::namespace::Namespace;
use fs_protocol::FileNameV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerializedDocumentSchema {
    TiloDoc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentContentKind {
    /// Bytes nobody interprets; restoring a version puts them back.
    Opaque,
    /// A CRDT document stored as its serialization; restoring a version is a new
    /// change on the current document.
    SerializedDocument { schema: SerializedDocumentSchema },
}

impl DocumentContentKind {
    /// The kind a record of this name holds.
    pub fn for_name(name: &FileNameV1) -> Self {
        let extension = std::str::from_utf8(name.as_bytes())
            .ok()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase());
        match extension.as_deref() {
            Some("qdoc" | "tilo-doc") => Self::SerializedDocument {
                schema: SerializedDocumentSchema::TiloDoc,
            },
            _ => Self::Opaque,
        }
    }

    pub fn of(namespace: &Namespace, ino: u64) -> Option<Self> {
        namespace
            .file_name_for(ino)
            .map(|name| Self::for_name(&name))
    }

    pub const fn is_serialized_document(&self) -> bool {
        matches!(self, Self::SerializedDocument { .. })
    }
}
