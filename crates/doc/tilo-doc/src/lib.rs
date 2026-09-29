//! `tilo-doc` — the private document engine behind the rich-text and workbook
//! resources.
//!
//! Apps do not depend on this crate. The public write path is
//! [`tilo_document::RichTextAction`](tilo_document) (and the workbook resource's
//! action enum). [`DocMut`] exists only so those resources' `Action::apply`
//! can mutate storage; it is not an app API.
//!
//! A rich-text document and a workbook are versioned resources: every edit is a
//! change on a CRDT-backed history that merges concurrent edits and replays
//! deterministically, read back as a plain snapshot ([`DocSnapshot`],
//! [`WorkbookSnapshot`]) and compared with structural diffs ([`diff`]). Binary
//! inputs such as images are referenced by content address; the bytes live in
//! the caller's `tilo_content` store, never in the document.
//!
//! File formats are not storage: importing and exporting `.docx`, `.odt`, `.md`
//! and the rest is the job of the `tilo-doc-codecs` crate, which projects files
//! into and out of these resources.

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

pub mod diff;
pub mod document;
pub mod export;
pub mod history;
pub mod model;
pub mod schema;
pub mod semantic_read;
pub mod sheet;
pub mod snapshot;

pub use diff::{
    AttrChange, BlockDiff, BlockOp, CellChange, CellOp, DiffSummary, DocDiff, PropChange, RunOp,
    SheetChange, SheetDiff, SheetDiffSummary, SheetOp, TextRun, diff_snapshots, diff_workbooks,
};
pub use document::{DocError, Document, MarkSpan, author_runs};
pub use export::{ActiveChange, InlineBoundary, InlinePlan, InlinePoint, InlineRun, RangeBoundary};
/// Private mutation handle for resource `Action::apply` implementations in
/// `tilo-document`. Not an application API — use `tilo_document::RichTextAction`
/// / [`WorkbookAction`].
#[doc(hidden)]
pub use model::DocMut;
pub use model::{Containers, DocRef, IdGen, NoteKind, Region, ordered_keys_scan_count};
pub use sheet::{
    CellInput, CellType, CellView, SheetView, Workbook, WorkbookAction, WorkbookDoc,
    WorkbookSnapshot,
};
pub use snapshot::{
    AttrVal, BlockView, BookmarkView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView,
};
