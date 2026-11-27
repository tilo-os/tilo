//! `tilo-doc`: the private document engine behind the rich-text and workbook
//! resources. Apps do not depend on this crate; the public write path is
//! `tilo_document::RichTextAction`, and `DocMut` exists only so that resource's
//! `Action::apply` can mutate storage. Both resources are versioned: every edit is
//! a change on a CRDT history that merges concurrent edits and replays
//! deterministically, read back as a plain snapshot and compared with structural
//! diffs. Binary inputs are referenced by content address and live in the
//! caller's `tilo_content` store; file formats are the job of
//! `tilo-doc-codecs`.
//!
//! The `model` feature (on by default) is what makes the resources: it brings
//! Automerge, `tilo-resource` and the actions that change a document. Setting text
//! into a measure is `tilo-text-layout`'s.

pub mod inline_time;
pub mod table_source;

#[cfg(feature = "model")]
pub mod diff;
#[cfg(feature = "model")]
pub mod document;
#[cfg(feature = "model")]
pub mod export;
#[cfg(feature = "model")]
pub mod model;
#[cfg(feature = "model")]
pub mod reconcile;
#[cfg(feature = "model")]
pub mod restore;
#[cfg(feature = "model")]
pub mod schema;
#[cfg(feature = "model")]
pub mod semantic_read;
#[cfg(feature = "model")]
pub mod sheet;
#[cfg(feature = "model")]
pub mod snapshot;
#[cfg(feature = "model")]
pub mod written;

#[cfg(feature = "model")]
pub use diff::{
    AttrChange, BlockDiff, BlockOp, CellChange, CellOp, DiffSummary, DocDiff, PropChange, RunOp,
    SheetChange, SheetDiff, SheetDiffSummary, SheetOp, TextRun, diff_snapshots, diff_workbooks,
};
#[cfg(feature = "model")]
pub use document::{DocError, Document, MarkSpan, author_runs};
#[cfg(feature = "model")]
pub use export::{ActiveChange, InlineBoundary, InlinePlan, InlinePoint, InlineRun, RangeBoundary};
/// Private mutation handle for resource `Action::apply` implementations in
/// `tilo-document`. Not an application API — use `tilo_document::RichTextAction`.
#[cfg(feature = "model")]
#[doc(hidden)]
pub use model::DocMut;
#[cfg(feature = "model")]
pub use model::{Containers, DocRef, IdGen, NoteKind, Region, ordered_keys_scan_count};
#[cfg(feature = "model")]
pub use reconcile::{
    ReconcileStats, reads_as, reconcile_body, reconcile_body_from, reconcile_body_under,
    reconcile_document, reconcile_into, reconcile_styles_under, reconcile_under,
};
#[cfg(feature = "model")]
pub use sheet::{CellInput, CellType, CellView, SheetView, Workbook, WorkbookSnapshot};
#[cfg(feature = "model")]
pub use snapshot::{
    AttrVal, BlockView, BookmarkView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView,
};
