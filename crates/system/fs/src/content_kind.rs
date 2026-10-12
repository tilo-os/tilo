//! Who owns a record's content, decided from record metadata alone.
//!
//! History restore and merge treat a record by what its content is: opaque
//! bytes, a serialized Tilo document, or a document the namespace itself holds
//! as a CRDT. That is a property of the record (its codec, its name), never of
//! the bytes one version happens to hold — a `.bin` whose bytes parse as a
//! document is still opaque, and deciding otherwise would mean downloading
//! whole versions just to classify them.

use crate::namespace::Namespace;
use fs_protocol::FileNameV1;

/// The schema of a document serialized into a record's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerializedDocumentSchema {
    /// A `tilo_doc::Document`: an Automerge document of the rich-text schema.
    Qdoc,
}

/// How a record's content is owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordContentKind {
    /// Bytes nobody interprets. Restoring a version puts its bytes back.
    Opaque,
    /// A CRDT document stored as its own serialization. Restoring a version
    /// is a new change on top of the current document.
    SerializedDocument { schema: SerializedDocumentSchema },
    /// A legacy-format document (`.docx`, `.md`, …) the namespace holds as a
    /// CRDT and exports on read.
    NamespaceDocument { codec: String },
}

/// Record names whose content is a serialized document, by extension.
const SERIALIZED_DOCUMENTS: [(&str, SerializedDocumentSchema); 2] = [
    ("qdoc", SerializedDocumentSchema::Qdoc),
    ("tilo-doc", SerializedDocumentSchema::Qdoc),
];

impl RecordContentKind {
    /// The kind a record of this name holds when the namespace keeps no
    /// codec for it. Signed record bindings carry the name, so every replica
    /// classifies a record the same way.
    pub fn for_name(name: &FileNameV1) -> Self {
        let extension = std::str::from_utf8(name.as_bytes())
            .ok()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase());
        extension
            .and_then(|extension| {
                SERIALIZED_DOCUMENTS
                    .iter()
                    .find(|(known, _)| *known == extension)
                    .map(|(_, schema)| Self::SerializedDocument { schema: *schema })
            })
            .unwrap_or(Self::Opaque)
    }

    /// The kind of `ino`'s content, or `None` when the namespace has no such
    /// file.
    pub fn of(namespace: &Namespace, ino: u64) -> Option<Self> {
        if let Some(codec) = namespace.document_codec(ino) {
            return Some(Self::NamespaceDocument { codec });
        }
        namespace
            .file_name_for(ino)
            .map(|name| Self::for_name(&name))
    }

    pub const fn is_serialized_document(&self) -> bool {
        matches!(self, Self::SerializedDocument { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> RecordContentKind {
        RecordContentKind::for_name(&FileNameV1(name.as_bytes().to_vec()))
    }

    #[test]
    fn serialized_documents_are_known_by_name_not_bytes() {
        let qdoc = RecordContentKind::SerializedDocument {
            schema: SerializedDocumentSchema::Qdoc,
        };
        assert_eq!(kind("notes.qdoc"), qdoc);
        assert_eq!(kind("Notes.QDOC"), qdoc);
        assert_eq!(kind("draft.tilo-doc"), qdoc);
        assert_eq!(kind("notes.qdoc.bin"), RecordContentKind::Opaque);
        assert_eq!(kind("qdoc"), RecordContentKind::Opaque);
        assert_eq!(kind("report.docx"), RecordContentKind::Opaque);
    }

    #[test]
    fn a_namespace_codec_makes_a_namespace_document() {
        let mut namespace = Namespace::new("content-kind").unwrap();
        let document = namespace
            .create_document(crate::namespace::ROOT_INO, "report.md", "md", 1)
            .unwrap();
        let file = namespace
            .create_file(crate::namespace::ROOT_INO, "data.qdoc", None, 0, 1)
            .unwrap();
        assert_eq!(
            RecordContentKind::of(&namespace, document),
            Some(RecordContentKind::NamespaceDocument { codec: "md".into() })
        );
        assert!(
            RecordContentKind::of(&namespace, file)
                .unwrap()
                .is_serialized_document()
        );
        assert_eq!(RecordContentKind::of(&namespace, u64::MAX), None);
    }
}
