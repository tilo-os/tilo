//! `qdoc` — the CRDT-native canonical document model and format-provider API.
//!
//! Layer 1 (generic): a file is an automerge document; this crate provides the
//! `FormatProvider` contract + registry. Layer 2 (standard schema): the rich-text
//! `Document` model that md/odt/docx map onto. See
//! `docs/superpowers/specs/2026-06-24-crdt-native-file-formats-design.md`.

pub mod codecs;
pub mod document;
pub mod format;
pub mod history;
pub mod model;
pub mod order;
pub mod schema;
pub mod snapshot;

pub use codecs::{builtin_registry, is_supported_ext, supported_extensions, DocxCodec, MarkdownCodec, OdtCodec, RtfCodec};
pub use document::{DocError, Document, MarkSpan};
pub use format::{CodecError, Confidence, FormatProvider, ProviderTier, Registry};
pub use history::VersionMeta;
pub use model::{Containers, DocMut, DocRef, IdGen, NoteKind, Region};
pub use snapshot::{AttrVal, BlockView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView};

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
