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

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> DocumentContentKind {
        DocumentContentKind::for_name(&FileNameV1(name.as_bytes().to_vec()))
    }

    tilo_test_env::scenario! {
        fn a_records_content_kind_comes_from_its_name;

        phase serialized_documents_are_known_by_name_not_bytes;
        {
            let document = DocumentContentKind::SerializedDocument {
                schema: SerializedDocumentSchema::TiloDoc,
            };
            assert_eq!(kind("notes.qdoc"), document);
            assert_eq!(kind("Notes.QDOC"), document);
            assert_eq!(kind("notes.tilo-doc"), document);
            assert_eq!(kind("Notes.TILO-DOC"), document);
            assert_eq!(kind("notes.qdoc.bin"), DocumentContentKind::Opaque);
            assert_eq!(kind("notes.tilo-doc.bin"), DocumentContentKind::Opaque);
            assert_eq!(kind("qdoc"), DocumentContentKind::Opaque);
            assert_eq!(kind("tilo-doc"), DocumentContentKind::Opaque);
            assert_eq!(kind("report.docx"), DocumentContentKind::Opaque);
        }

        phase a_file_of_the_namespace_is_known_by_its_name;
        {
            let mut namespace = Namespace::new("content-kind").unwrap();
            let file = namespace
                .create_file(crate::namespace::ROOT_INO, "data.tilo-doc", None, 0, 1)
                .unwrap();
            assert!(
                DocumentContentKind::of(&namespace, file)
                    .unwrap()
                    .is_serialized_document()
            );
            assert_eq!(DocumentContentKind::of(&namespace, u64::MAX), None);
        }
    }
}
