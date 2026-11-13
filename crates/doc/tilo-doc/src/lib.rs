//! `tilo-doc`: the private document engine behind the rich-text and workbook
//! resources. Apps do not depend on this crate; the public write path is
//! `tilo_document::RichTextAction`, and [`DocMut`] exists only so that resource's
//! `Action::apply` can mutate storage. Both resources are versioned: every edit is
//! a change on a CRDT history that merges concurrent edits and replays
//! deterministically, read back as a plain snapshot and compared with structural
//! diffs ([`diff`]). Binary inputs are referenced by content address and live in
//! the caller's `tilo_content` store; file formats are the job of
//! `tilo-doc-codecs`.

pub mod diff;
pub mod document;
pub mod export;
pub mod inline_time;
pub mod model;
pub mod paging;
pub mod reconcile;
pub mod restore;
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
/// `tilo-document`. Not an application API — use `tilo_document::RichTextAction`.
#[doc(hidden)]
pub use model::DocMut;
pub use model::{Containers, DocRef, IdGen, NoteKind, Region, ordered_keys_scan_count};
pub use reconcile::{
    ReconcileStats, reads_as, reconcile_body, reconcile_body_from, reconcile_body_under,
    reconcile_document, reconcile_into, reconcile_under,
};
pub use sheet::{CellInput, CellType, CellView, SheetView, Workbook, WorkbookSnapshot};
pub use snapshot::{
    AttrVal, BlockView, BookmarkView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView,
};
