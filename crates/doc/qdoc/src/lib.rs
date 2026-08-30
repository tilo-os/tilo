//! `qdoc` — the CRDT-native canonical document model and format-provider API.
//!
//! Layer 1 (generic): a file is an automerge document; this crate provides the
//! `FormatProvider` contract + registry. Layer 2 (standard schema): the rich-text
//! `Document` model that md/odt/docx map onto. See
//! `docs/superpowers/specs/2026-06-24-crdt-native-file-formats-design.md`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod blobstore;
pub mod codecs;
pub mod diff;
pub mod document;
pub mod export;
pub mod format;
pub mod history;
pub mod model;
pub mod order;
pub mod schema;
pub mod sheet;
pub mod snapshot;

pub use codecs::safety::ZipLimits;
pub use codecs::{
    DocxCodec, HtmlCodec, MarkdownCodec, OdsCodec, OdtCodec, RtfCodec, SheetProvider, XlsxCodec,
    builtin_registry, is_supported_ext, is_supported_sheet_ext, sheet_provider_for_ext,
    supported_extensions,
};
pub use diff::{
    AttrChange, BlockDiff, BlockOp, CellChange, CellOp, DiffSummary, DocDiff, PropChange, RunOp,
    SheetChange, SheetDiff, SheetDiffSummary, SheetOp, TextRun, diff_snapshots, diff_workbooks,
};
pub use document::{DocError, Document, MarkSpan, author_runs};
pub use export::{ActiveChange, InlineBoundary, InlinePlan, InlinePoint, InlineRun, RangeBoundary};
pub use format::{CodecError, Confidence, FormatProvider, ProviderTier, Registry};
pub use history::VersionMeta;
pub use model::{Containers, DocMut, DocRef, IdGen, NoteKind, Region, ordered_keys_scan_count};
pub use sheet::{CellInput, CellType, CellView, SheetView, Workbook, WorkbookSnapshot};
pub use snapshot::{
    AttrVal, BlockView, BookmarkView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView,
};

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
