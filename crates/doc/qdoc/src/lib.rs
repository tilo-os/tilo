//! `qdoc` — the CRDT-native document model and format-provider API.
//!
//! A file is an Automerge document. This crate provides the `FormatProvider`
//! contract and registry, plus the rich-text `Document` model used by the
//! Markdown, ODT, and DOCX codecs.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod blobstore;
pub mod codecs;
pub mod diff;
pub mod document;
pub mod export;
pub mod export_epub;
pub mod export_html;
pub mod export_rtf;
pub mod format;
pub mod history;
pub mod model;
pub mod order;
pub mod schema;
pub mod semantic_read;
pub mod sheet;
pub mod snapshot;

pub use codecs::safety::ZipLimits;
pub use codecs::{
    DocxCodec, HtmlCodec, MarkdownCodec, OdsCodec, OdtCodec, ParsedSheet, ParsedWorkbook, RtfCodec,
    SheetProvider, XlsxCodec, builtin_registry, is_supported_ext, is_supported_sheet_ext,
    sheet_provider_for_ext, supported_extensions,
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
