//! `qdoc` — the rich-text and spreadsheet resource types.
//!
//! A rich-text document ([`Document`], edited through [`DocMut`]) and a workbook ([`Workbook`])
//! are versioned resources: every edit is a change on a CRDT-backed history that merges
//! concurrent edits and replays deterministically, read back as a plain snapshot ([`DocSnapshot`],
//! [`WorkbookSnapshot`]) and compared with structural diffs ([`diff`]). Binary inputs such as
//! images are referenced by content address ([`DocMut::attach`], [`DocMut::reference_content`]);
//! the bytes live in the caller's `tilo_content` store, never in the document.
//!
//! File formats are not storage: importing and exporting `.docx`, `.odt`, `.md` and the rest is the
//! job of the `qdoc-codecs` crate, which projects files into and out of these resources.
//!
//! Rich text as a resource, with the typed action vocabulary every editor edits through, is the
//! `qdoc-rich-text` crate. The app framework's resource layer (`tilo_resource`) owns version
//! history, time travel and fractional order keys. What remains here is document-specific:
//! deterministic id minting for replayable actions
//! ([`IdGen`], [`model::seeded_id_gen`]), the shared schema genesis ([`model::init`]) and the
//! bulk change writer ([`DocMut::bulk`]).

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
pub use model::{Containers, DocMut, DocRef, IdGen, NoteKind, Region, ordered_keys_scan_count};
pub use sheet::{CellInput, CellType, CellView, SheetView, Workbook, WorkbookSnapshot};
pub use snapshot::{
    AttrVal, BlockView, BookmarkView, ChangeKind, ChangeView, CommentView, DocSnapshot, NoteView,
};
