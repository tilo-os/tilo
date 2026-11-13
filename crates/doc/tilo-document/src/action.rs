//! The rich-text **action vocabulary**: every collaborative, document-mutating
//! edit of a rich-text document — a text splice, a formatting mark, a
//! paragraph attribute, a structural insert or delete, a review decision — as
//! a small serializable value that deterministically mutates the document.
//!
//! Editors, the palette, agents and scripts all submit edits as
//! [`RichTextAction`]s, so one vocabulary is one replay path. Presentation
//! (caret, selection, scroll, open panels) stays out of the document and never
//! becomes an action.
//!
//! **Determinism contract**: [`Action::apply`] never reads the wall clock or
//! an RNG. New block ids are minted inside `apply` from the action's own
//! deterministic context ([`IdGen::for_action`](tilo_doc::IdGen::for_action)),
//! and sets of blocks and spans are sorted before iterating, so a replay
//! emits the identical op sequence. A decoded action with stale or hostile
//! arguments is refused before it writes anything, so callers can surface a
//! useful error without leaving a partial edit.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_resource::JsonSchema;

use tilo_doc::model::Region;
use tilo_doc::{Containers, DocMut};

use tilo_actions::{Action, ActionError, Doc, ExecutionLocus};

use crate::ClipBlock;

/// Registry type id every rich-text action is encoded under: the resource
/// kind.
pub const RICH_TEXT_ACTION_TYPE: &str = "tilo.rich-text";

pub use crate::validate::{action_is_valid, refusal, splice_target_len};

/// Saturating integer conversions for offsets and counts.
pub(crate) mod num {
    pub fn i64_of_usize(value: usize) -> i64 {
        i64::try_from(value).unwrap_or(i64::MAX)
    }
    pub fn isize_of_usize(value: usize) -> isize {
        isize::try_from(value).unwrap_or(isize::MAX)
    }
    pub fn usize_of_i64(value: i64) -> usize {
        usize::try_from(value).unwrap_or_default()
    }
}

/// Which page region a text/formatting op addresses. Mirrors [`tilo_doc::model::Region`]
/// (which is not itself serializable) so the region travels in the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RegionKind {
    #[default]
    Body,
    Header,
    Footer,
}

impl RegionKind {
    pub fn to_region(self) -> Region {
        match self {
            RegionKind::Body => Region::Body,
            RegionKind::Header => Region::Header,
            RegionKind::Footer => Region::Footer,
        }
    }

    pub fn from_region(r: Region) -> Self {
        match r {
            Region::Body => RegionKind::Body,
            Region::Header => RegionKind::Header,
            Region::Footer => RegionKind::Footer,
        }
    }
}

/// A serializable inline-mark value (the character-format payload). `Bool` is a
/// toggle mark (bold/italic/underline/strike/super/sub); `Int` is a size; `Str`
/// is a font family / colour / highlight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MarkVal {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// A serializable block-attribute value. Maps onto the scalar an attr stores
/// (`align`/`style`/`tab_stops` are `Str`, `level`/image dims are `Int`,
/// `indent`/`line_spacing` are `Float`, `page_break_before`/flips are `Bool`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum AttrVal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// One non-empty selection expressed as stable block ids and character offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionTarget {
    pub start_block: String,
    pub start: usize,
    pub end_block: String,
    pub end: usize,
}

/// One `(block, [start, end))` range a formatting mark covers. A selection that
/// spans several blocks is carried as one span per touched block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub block: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TocTarget {
    pub heading: String,
    pub level: i64,
    pub text: String,
    pub page: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TableEdit {
    InsertRow { at: usize },
    InsertColumn { at: usize },
    DeleteRow { row: usize },
    DeleteColumn { column: usize },
    Delete,
}

/// A file format [`RichTextAction::ReplaceFormatted`] can import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportFormat {
    Markdown,
    Text,
    Html,
    Rtf,
    Docx,
    Odt,
}

/// Every collaborative, document-mutating operation the word processor exports.
///
/// Deliberately positional: each variant names the exact block ids and offsets
/// it touches, so [`Action::apply`] is a pure, replayable write. Ordinary typing,
/// deletion, paragraph splitting, Find & Replace, and agent-authored edits all
/// enter through these same envelopes; caret and selection updates remain local.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum RichTextAction {
    /// Replace `[at, at+del)` of `block` with `ins` (insert = `del` 0; delete =
    /// `ins` empty). The deterministic text primitive.
    SpliceText {
        region: RegionKind,
        block: String,
        at: usize,
        del: usize,
        ins: String,
    },
    /// Replace disjoint character ranges together without recreating blocks.
    /// Offsets refer to the pre-action document; application runs back to front.
    ReplaceMatches {
        region: RegionKind,
        spans: Vec<Span>,
        ins: String,
    },
    /// Replace one selection in document order.  Unlike a sequence of splices
    /// and deletes, this preserves the old editor's cross-paragraph merge as a
    /// single deterministic, atomic action/undo boundary.
    ReplaceSelection {
        region: RegionKind,
        start_block: String,
        start: usize,
        end_block: String,
        end: usize,
        ins: String,
    },
    /// Apply or clear an inline mark over `spans`. `value` `None` removes the
    /// mark; `Some` replaces it (any prior value of the same mark over each span
    /// is removed first, so a re-colour/re-size overwrites rather than stacks).
    /// `also_clear` names sibling marks to drop over the same spans first (so a
    /// super/subscript pair stays mutually exclusive).
    SetMark {
        region: RegionKind,
        spans: Vec<Span>,
        name: String,
        value: Option<MarkVal>,
        #[serde(default)]
        also_clear: Vec<String>,
    },
    /// Set a paragraph's structural kind (`heading`/`paragraph`/`list-item`/…)
    /// on each of `blocks`, plus its `level` / `ordered` attrs when given.
    SetParagraphStyle {
        #[serde(default)]
        region: RegionKind,
        blocks: Vec<String>,
        kind: String,
        level: Option<i64>,
        ordered: Option<bool>,
    },
    /// Apply a named paragraph style to `blocks`: set the `style` attr, and — for
    /// a built-in style — the structural `kind`/`level`/`ordered` too, as one
    /// action/undo boundary (not a kind write racing a separate style write).
    ApplyNamedStyle {
        #[serde(default)]
        region: RegionKind,
        blocks: Vec<String>,
        style: String,
        kind: Option<String>,
        level: Option<i64>,
        ordered: Option<bool>,
    },
    /// Set one block attribute (`align`, `page_break_before`, `indent`,
    /// `first_line_indent`, `line_spacing`, `tab_stops`, `style`, image
    /// `width`/`height`/`flip_*`/`alt`) to `value` on each of `blocks`. An
    /// image's `alt` is its accessible description and is accepted on image
    /// blocks only.
    SetBlockAttr {
        region: RegionKind,
        blocks: Vec<String>,
        key: String,
        value: AttrVal,
    },
    /// Remove one block attribute from each of `blocks`, restoring its
    /// default: a list item that stops being a task (`checked`), a code line
    /// that becomes prose (`code`, `lang`, `code_group`), a block back at the
    /// outline's top (`nesting`), an image whose description falls back to its
    /// caption (`alt`).
    ClearBlockAttr {
        #[serde(default)]
        region: RegionKind,
        blocks: Vec<String>,
        key: String,
    },
    /// Replace all four page margins as one absolute page-setup operation.
    /// Keeping the rectangle atomic avoids transient invalid page geometry while
    /// still giving history and collaborators a single authored mutation.
    SetPageMargins {
        top: f64,
        right: f64,
        bottom: f64,
        left: f64,
    },
    /// Set one scalar property of one stable named style. Keeping each field in
    /// its own envelope lets replicas edit e.g. font and spacing concurrently
    /// without replaying an entire stale stylesheet over either change.
    SetStyleProperty {
        style: String,
        key: String,
        value: AttrVal,
    },
    /// Delete one custom style and atomically move its addressed instances back
    /// to the body default. The style id receives a grow-only tombstone.
    DeleteStyle {
        style: String,
        fallback_blocks: Vec<String>,
    },
    /// Set an image crop in ten-thousandths of its source rectangle.  Keeping
    /// all four edges in one action is important: a crop must never briefly
    /// become invalid while a collaborator/replay observes it.
    SetImageCrop {
        block: String,
        left: i64,
        top: i64,
        right: i64,
        bottom: i64,
    },
    /// Resize an image frame while preserving a single replay/undo boundary.
    SetImageSize {
        block: String,
        width: i64,
        height: i64,
    },
    /// Move an image between normal flow and a free page-positioned object.
    /// Offsets are document pixels relative to the page content origin; `page`
    /// is zero-based.  This is deliberately a single canonical operation so a
    /// drag cannot publish a half-updated anchor/coordinate pair.
    SetImagePlacement {
        block: String,
        anchor: String,
        wrap: String,
        x: i64,
        y: i64,
        page: i64,
    },
    /// Insert a `rows`×`cols` table after `after`. Cells are consecutive `cell`
    /// blocks sharing a freshly-minted `table` id, tagged with `row`/`col`.
    InsertTable {
        after: String,
        rows: usize,
        cols: usize,
    },
    /// Insert a table after `after` already holding its contents: `cells` is
    /// its rows top to bottom, each row's cell texts left to right, and every
    /// row has the same number of cells. The first row is the header row
    /// (tilo-doc has no separate header marker: the editor shades row 0 and the
    /// exporters write it as the table's head). Laid out exactly as
    /// [`InsertTable`](Self::InsertTable) lays out an empty table, with the
    /// table and cell ids minted inside `apply`, so the table and its text
    /// are one edit and one undo step.
    InsertFilledTable {
        after: String,
        cells: Vec<Vec<String>>,
    },
    /// Mutate one stable table id using an absolute row/column boundary.
    EditTable { table: String, edit: TableEdit },
    /// Insert an image at `(block, at)`: the caret block is split so the tail follows the image in
    /// a fresh paragraph beneath it. The image is a binary input referenced by its content address
    /// (already in the caller's content store), described by its media type, length and optional
    /// intrinsic `width`/`height`; the action never carries the bytes.
    InsertImage {
        block: String,
        at: usize,
        content: String,
        #[serde(default)]
        mime: String,
        #[serde(default)]
        len: u64,
        width: Option<i64>,
        height: Option<i64>,
    },
    /// Insert a manual page break at `(block, at)`: split the block like Enter and
    /// mark the new block to start on a fresh page.
    InsertPageBreak {
        #[serde(default)]
        region: RegionKind,
        block: String,
        at: usize,
    },
    /// Split one paragraph at an absolute character offset. The new block id is
    /// minted by the pinned action actor, so origin and replay produce the same
    /// stable child entity. Used by Enter in body/header/footer alike.
    SplitBlock {
        region: RegionKind,
        block: String,
        at: usize,
        kind: String,
        ordered: Option<bool>,
        /// The block that followed `block` where the split was made, so the
        /// new block's place is found without reading the region's order.
        /// Stale (a block inserted in between meanwhile) it still places the
        /// new block between the two, as a concurrent insert would.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// Insert one canonical field marker at an absolute text position.
    InsertField {
        region: RegionKind,
        block: String,
        at: usize,
        field: String,
    },
    /// Replace the generated TOC using stable heading ids and authored page
    /// labels captured from one pagination frame.
    GenerateToc { entries: Vec<TocTarget> },
    /// Paste a rich or plain multi-paragraph fragment at an absolute selection.
    /// Paragraphs and marks are authored payload, while newly-created block ids
    /// are minted under the pinned action actor.
    PasteFragment {
        region: RegionKind,
        start_block: String,
        start: usize,
        end_block: String,
        end: usize,
        blocks: Vec<ClipBlock>,
        adopt_first_style: bool,
    },
    /// Insert a fresh paragraph carrying `text` directly after block `after`.
    /// Enter itself is [`Self::SplitBlock`] (published as `new-paragraph`);
    /// this inserts a whole paragraph after a named block in one envelope.
    InsertParagraph { after: String, text: String },
    /// Append `text` as a new paragraph at the end of the body. The target is
    /// resolved from the document at apply time (not captured in the payload),
    /// so a catalogue captured before other appends still lands at the end.
    /// An empty trailing `paragraph` (a fresh document's only line) is filled
    /// instead of leaving a blank line above the new text.
    AppendParagraph { text: String },
    /// Append `text` at the end of the last body block, which must be a text
    /// block (paragraph, heading or list item). Resolved at apply time, so
    /// repeated invocations append in order.
    ///
    /// A caret-free writer (an agent streaming text) uses it; an editor with
    /// a caret uses `move-caret {to: document-end}` then `type-text`.
    ContinueParagraph { text: String },
    /// Remove `blocks` (an image, or every cell of a table). Ids are sorted
    /// before removal so replicas emit the identical op sequence.
    RemoveBlocks { blocks: Vec<String> },
    /// Tombstone `blocks` in a header or footer. Body removal stays
    /// [`Self::RemoveBlocks`].
    RemoveRegionBlocks {
        region: RegionKind,
        blocks: Vec<String>,
    },
    /// Insert one empty paragraph when `region` has no blocks. A no-op when
    /// the region already has a write target, so a replay does not add another.
    EnsureRegionBlock { region: RegionKind },
    /// Move body paragraphs `blocks`, in the order given, to directly after
    /// `after`, or to the start of the body when `after` is `None`: a
    /// dragged paragraph, or a run of them. Each keeps its id, text and
    /// style.
    MoveBlocks {
        blocks: Vec<String>,
        after: Option<String>,
    },
    /// Replace the whole document body with `markdown` (re-import a template /
    /// restore a version): every existing body block is removed, then the
    /// Markdown is imported as fresh blocks.
    ReplaceBody { markdown: String },
    /// Insert `blocks` of any kind, in order, at `at`. A new body's single
    /// empty paragraph is replaced when the blocks go at its end. Block ids
    /// are minted under the pinned action actor.
    InsertBlocks {
        at: crate::block_edit::Placement,
        blocks: Vec<crate::block_edit::NewBlock>,
    },
    /// Insert the blocks `markdown` says (a block per line, see
    /// [`block_markdown`](crate::block_markdown)) at `at`.
    InsertMarkdown {
        at: crate::block_edit::Placement,
        markdown: String,
    },
    /// Make the text of `block` `text`, changing only the characters that
    /// differ. With `markdown`, `text` is inline Markdown and the block's
    /// dialect marks follow it; otherwise its marks stay as they are.
    SetBlockText {
        block: String,
        text: String,
        #[serde(default)]
        markdown: bool,
    },
    /// Turn `block` into the kind `shape` names, keeping its text.
    SetBlockShape {
        block: String,
        shape: crate::block_edit::BlockShape,
    },
    /// Set a checklist item's state, or toggle it when `done` is absent. A
    /// bullet or numbered item becomes a checklist item.
    SetBlockDone { block: String, done: Option<bool> },
    /// Indent a list item by `by` levels (negative outdents), within the
    /// depth the editor draws.
    ShiftIndent { block: String, by: i64 },
    /// Move `block` to `at`. Resolved at apply time, so `End` is the end
    /// the body has when it applies.
    MoveBlock {
        block: String,
        at: crate::block_edit::Placement,
    },
    /// Remove `blocks` from the body, leaving it a paragraph if none is left.
    DeleteBlocks { blocks: Vec<String> },
    /// Replace the body with `markdown` as the fewest edits that make it so:
    /// blocks named by their `^id`, or whose line is unchanged, stay the same
    /// blocks (their ids, marks and authorship hold, and no tombstone is
    /// made); edited blocks are edited in place, the rest inserted or
    /// removed. Unlike [`Self::ReplaceBody`] it does not recreate the body.
    SyncBody { markdown: String },
    /// Replace the first occurrence of `find` in the body's text (every
    /// occurrence with `all`) by `replace`, in place: other text, marks,
    /// images and blocks stay. Resolved as it applies; refused when `find`
    /// is not in the body.
    ReplaceText {
        find: String,
        replace: String,
        #[serde(default)]
        all: bool,
    },
    /// Replace the whole body with `blocks`, formatted as they are: text an
    /// app brings with marks Markdown cannot carry (a note's links to other
    /// notes, a slide's runs, a copied fragment).
    ReplaceClips { blocks: Vec<ClipBlock> },
    /// Make the body read as `blocks`, formatted as they are, as the fewest
    /// edits: a paragraph that already reads so stays, a changed one is
    /// spliced and re-marked in place, the rest inserted or removed. A whole
    /// value written over a field (a tilolang assignment, an agent's rewrite)
    /// merges with a concurrent edit this way, never as a replacement.
    ///
    /// With `replace`, `blocks` stand for those blocks alone, by id, where
    /// they are: one edit of an editor, the paragraphs it changed and nothing
    /// else, so a keystroke in a long note is its paragraph, never the note.
    /// Where the body does not name each of them so (blocks the edit's writer
    /// made, which the body names otherwise), they are as many blocks from the
    /// place `at` they were at.
    SyncClips {
        blocks: Vec<ClipBlock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replace: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "is_zero")]
        at: usize,
    },
    // Decoded again on every replica, the images the bytes embed named by
    // address and kept nowhere: what documents recorded before
    // `ReplaceImported`, and a body written inside another action.
    /// Replace the body with `bytes` decoded as `format`. A decode failure
    /// refuses the action so the open document stays as it was. The
    /// document remembers the bytes' content address as the file it was
    /// last read from ([`FILE_DIGEST`]).
    ReplaceFormatted {
        format: ImportFormat,
        bytes: Vec<u8>,
    },
    /// The document was written to its file, whose bytes now have content
    /// address `digest` ([`FILE_DIGEST`]): what a later open compares the
    /// file with to tell whether it changed elsewhere. Content from outside:
    /// it syncs, but no undo takes it back.
    NoteFileWritten { digest: String },
    /// Accept one tracked change, or every change when `id` is absent.
    AcceptChanges { id: Option<String> },
    /// Reject one tracked change, or every change when `id` is absent.
    RejectChanges { id: Option<String> },
    /// Resolve a comment by removing both its anchor mark and record.
    ResolveComment { id: String },
    /// Insert `text` at `at` and mark it as a tracked insertion by `author`.
    TrackInsert {
        block: String,
        at: usize,
        text: String,
        author: String,
        date: String,
    },
    /// Add a durable root comment anchored to one exact body range. The comment
    /// and its first body block are minted inside `apply`, so replay gets the
    /// same stable CRDT ids as the originating action.
    AddComment {
        block: String,
        start: usize,
        end: usize,
        author: String,
        date: String,
        text: String,
    },
    /// Append a durable reply below an existing comment thread. Parent ids are
    /// stable resources; stale/deleted parents refuse atomically. `body`, when
    /// given, is the reply as written (paragraphs, their kinds and marks);
    /// `text` is its plain text, and the whole reply when `body` is empty.
    ReplyToComment {
        comment: String,
        author: String,
        date: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body: Vec<ClipBlock>,
    },
    /// Replace characters `start..end` of `block` with `text` (insert when
    /// they are equal), leaving the rest of the block, its kind and its
    /// marks as they are. With `markdown`, `text` is inline Markdown whose
    /// marks (**bold**, _italic_, `code`, [link](url)) cover what it writes.
    /// The edit stays inside the block, so the body keeps its blocks: what a
    /// person selected and asked an agent to change.
    ReplaceBlockRange {
        block: String,
        start: usize,
        end: usize,
        text: String,
        #[serde(default)]
        markdown: bool,
    },
    // Read once where it is invoked (`stage_import`): every replica writes
    // `document` as it is, never reading the file again. Its images are
    // named by address; `content` maps each to the (sealed) reference the
    // content store keeps it under. `file_digest` is the file's address.
    /// Replace the body with an imported file.
    ReplaceImported {
        document: Vec<u8>,
        #[serde(default)]
        content: BTreeMap<String, String>,
        file_digest: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionDomain {
    Text,
    Formatting,
    Media,
    Table,
    Structure,
    Review,
    /// Edits by block id: [`crate::block_edit`].
    Blocks,
}

impl RichTextAction {
    pub(crate) fn domain(&self) -> ActionDomain {
        match self {
            Self::SpliceText { .. }
            | Self::ReplaceMatches { .. }
            | Self::ReplaceSelection { .. }
            | Self::SetMark { .. }
            | Self::PasteFragment { .. }
            | Self::InsertParagraph { .. }
            | Self::AppendParagraph { .. }
            | Self::ContinueParagraph { .. }
            | Self::RemoveBlocks { .. }
            | Self::RemoveRegionBlocks { .. }
            | Self::EnsureRegionBlock { .. }
            | Self::ReplaceBody { .. }
            | Self::ReplaceClips { .. }
            | Self::SyncClips { .. }
            | Self::ReplaceFormatted { .. }
            | Self::ReplaceImported { .. } => ActionDomain::Text,
            Self::SetParagraphStyle { .. }
            | Self::ApplyNamedStyle { .. }
            | Self::SetBlockAttr { .. }
            | Self::ClearBlockAttr { .. }
            | Self::SetPageMargins { .. }
            | Self::NoteFileWritten { .. }
            | Self::SetStyleProperty { .. }
            | Self::DeleteStyle { .. } => ActionDomain::Formatting,
            Self::SetImageCrop { .. }
            | Self::SetImageSize { .. }
            | Self::SetImagePlacement { .. }
            | Self::InsertImage { .. } => ActionDomain::Media,
            Self::InsertTable { .. } | Self::InsertFilledTable { .. } | Self::EditTable { .. } => {
                ActionDomain::Table
            }
            Self::InsertPageBreak { .. }
            | Self::SplitBlock { .. }
            | Self::InsertField { .. }
            | Self::GenerateToc { .. }
            | Self::MoveBlocks { .. } => ActionDomain::Structure,
            Self::AcceptChanges { .. }
            | Self::RejectChanges { .. }
            | Self::ResolveComment { .. }
            | Self::AddComment { .. }
            | Self::ReplyToComment { .. }
            | Self::TrackInsert { .. } => ActionDomain::Review,
            Self::InsertBlocks { .. }
            | Self::InsertMarkdown { .. }
            | Self::SetBlockText { .. }
            | Self::ReplaceBlockRange { .. }
            | Self::SetBlockShape { .. }
            | Self::SetBlockDone { .. }
            | Self::ShiftIndent { .. }
            | Self::MoveBlock { .. }
            | Self::DeleteBlocks { .. }
            | Self::SyncBody { .. }
            | Self::ReplaceText { .. } => ActionDomain::Blocks,
        }
    }
}

// Action payloads may arrive after the document has changed (or from an
// untrusted automation).  Keep the limits deliberately generous for real
// documents while making the action boundary a useful resource boundary too.
/// The most bytes one text an action carries may hold: a paste of a long document into one
/// paragraph is one such text, and it is as long as a window carries in one string
/// ([`tilo_actions::MAX_TEXT_BYTES`]), so what an edit keeps is what the window can show.
pub const MAX_ACTION_TEXT_BYTES: usize = tilo_actions::MAX_TEXT_BYTES;
pub const MAX_ACTION_BLOB_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TABLE_DIMENSION: usize = 100;
pub const MAX_TABLE_CELLS: usize = 10_000;
/// The longest image description an action may set, in characters.
pub const MAX_IMAGE_ALT_CHARS: usize = 2_048;

// ---------------------------------------------------------------------------
// Region-aware mark helpers (mirroring `WordProcessor::region_*`), so `apply`
// writes marks the same way the live editor does across body / header / footer.
// ---------------------------------------------------------------------------

fn add_mark_region(dm: &mut DocMut, region: Region, span: &Span, name: &str, value: &MarkVal) {
    match region {
        Region::Body => match value {
            MarkVal::Bool(v) => dm.add_mark(&span.block, span.start, span.end, name, *v),
            MarkVal::Int(n) => dm.add_mark(&span.block, span.start, span.end, name, *n),
            MarkVal::Str(s) => dm.add_mark(&span.block, span.start, span.end, name, s.as_str()),
        },
        r => match value {
            MarkVal::Bool(v) => dm.add_mark_in(r, &span.block, span.start, span.end, name, *v),
            MarkVal::Int(n) => dm.add_mark_in(r, &span.block, span.start, span.end, name, *n),
            MarkVal::Str(s) => {
                dm.add_mark_in(r, &span.block, span.start, span.end, name, s.as_str());
            }
        },
    }
}

fn remove_mark_region(dm: &mut DocMut, region: Region, span: &Span, name: &str) {
    dm.remove_mark_in(region, &span.block, span.start, span.end, name);
}

fn set_attr_region(dm: &mut DocMut, region: Region, block: &str, key: &str, value: &AttrVal) {
    match region {
        Region::Body => match value {
            AttrVal::Bool(v) => dm.set_attr(block, key, *v),
            AttrVal::Int(n) => dm.set_attr(block, key, *n),
            AttrVal::Float(f) => dm.set_attr(block, key, *f),
            AttrVal::Str(s) => dm.set_attr(block, key, s.as_str()),
        },
        r => match value {
            AttrVal::Bool(v) => dm.set_attr_in(r, block, key, *v),
            AttrVal::Int(n) => dm.set_attr_in(r, block, key, *n),
            AttrVal::Float(f) => dm.set_attr_in(r, block, key, *f),
            AttrVal::Str(s) => dm.set_attr_in(r, block, key, s.as_str()),
        },
    }
}

fn set_kind_region(dm: &mut DocMut, region: Region, block: &str, kind: &str) {
    dm.set_kind_in(region, block, kind);
}

fn remove_attr_region(dm: &mut DocMut, region: Region, block: &str, key: &str) {
    dm.remove_attr_in(region, block, key);
}

// ---------------------------------------------------------------------------
// Per-variant `apply` bodies. Split out of `Action::apply` below (one match
// arm each) so that single trait method stays short; kept as free functions
// (not methods on `RichTextAction`) because they live outside the trait
// impl. Each takes the `dm: &mut DocMut` the caller builds once (`doc`/`c`/
// `ids` bundled), matching the region helpers above; `dm.c` stands in for a
// separate `c` parameter where one is needed.
// ---------------------------------------------------------------------------

fn apply_splice_text(
    dm: &mut DocMut,
    region: Region,
    block: &str,
    at: usize,
    del: usize,
    ins: &str,
) {
    dm.splice_text_in(region, block, at, num::isize_of_usize(del), ins);
}

fn remove_block_in_region(dm: &mut DocMut, region: Region, block: &str) {
    dm.tombstone_block_in(region, block);
}

fn apply_replace_selection(
    dm: &mut DocMut,
    region: Region,
    start_block: &str,
    start: usize,
    end_block: &str,
    end: usize,
    ins: &str,
) {
    if start_block == end_block {
        dm.splice_text_in(
            region,
            start_block,
            start,
            num::isize_of_usize(end.saturating_sub(start)),
            ins,
        );
        return;
    }
    // Validation has already established both block positions/offsets against
    // one snapshot, so this body cannot partly merge one valid end-point and
    // then discover a stale second end-point.
    let blocks = match region {
        Region::Body => tilo_doc::model::blocks_in_order(dm.doc, dm.c),
        region => tilo_doc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .map(|block| block.id)
            .collect(),
    };
    let Some(start_index) = blocks.iter().position(|block| block == start_block) else {
        return;
    };
    let Some(end_index) = blocks.iter().position(|block| block == end_block) else {
        return;
    };
    let end_text = match region {
        Region::Body => tilo_doc::model::block_text(dm.doc, dm.c, end_block),
        region => tilo_doc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .find(|block| block.id == end_block)
            .map(|block| block.text),
    }
    .unwrap_or_default();
    let tail: String = end_text.chars().skip(end).collect();
    let start_len = match region {
        Region::Body => tilo_doc::model::block_text(dm.doc, dm.c, start_block),
        region => tilo_doc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .find(|block| block.id == start_block)
            .map(|block| block.text),
    }
    .map_or(0, |text| text.chars().count());
    dm.splice_text_in(
        region,
        start_block,
        start,
        num::isize_of_usize(start_len.saturating_sub(start)),
        ins,
    );
    if !tail.is_empty() {
        dm.splice_text_in(
            region,
            start_block,
            start.saturating_add(ins.chars().count()),
            0,
            &tail,
        );
    }
    if let Some(to_remove) = blocks.get(start_index.saturating_add(1)..=end_index) {
        for block in to_remove {
            remove_block_in_region(dm, region, block);
        }
    }
}

fn apply_set_mark(
    dm: &mut DocMut,
    region: Region,
    spans: &[Span],
    name: &str,
    value: Option<&MarkVal>,
    also_clear: &[String],
) {
    let mut sorted = spans.to_vec();
    sorted.sort_by(|a, b| {
        (a.block.as_str(), a.start, a.end).cmp(&(b.block.as_str(), b.start, b.end))
    });
    for span in &sorted {
        if span.start >= span.end {
            continue;
        }
        for sib in also_clear {
            remove_mark_region(dm, region, span, sib);
        }
        match value {
            None => remove_mark_region(dm, region, span, name),
            Some(v) => {
                // Replace: drop any prior value of the same mark before
                // re-adding so values overwrite, not stack.
                remove_mark_region(dm, region, span, name);
                add_mark_region(dm, region, span, name, v);
            }
        }
    }
}

/// Write the kind `kind` onto each of `blocks`, with `level` and `ordered` when they are given,
/// and without the ones that kind does not carry: a line that stops being a heading forgets its
/// level, and a list item that becomes prose its order and depth.
fn set_kind_and_attrs(
    dm: &mut DocMut,
    region: Region,
    block: &str,
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) {
    set_kind_region(dm, region, block, kind);
    let carries_level = matches!(kind, "heading" | "list_item");
    let carries_ordered = kind == "list_item";
    if carries_level {
        if let Some(l) = level {
            set_attr_region(dm, region, block, "level", &AttrVal::Int(l));
        }
    } else {
        remove_attr_region(dm, region, block, "level");
    }
    if carries_ordered {
        if let Some(o) = ordered {
            set_attr_region(dm, region, block, "ordered", &AttrVal::Bool(o));
        }
    } else {
        remove_attr_region(dm, region, block, "ordered");
    }
}

fn apply_set_paragraph_style(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        set_kind_and_attrs(dm, region, block, kind, level, ordered);
    }
}

fn apply_named_style(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    style: &str,
    kind: Option<&String>,
    level: Option<i64>,
    ordered: Option<bool>,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        if let Some(k) = kind {
            set_kind_and_attrs(dm, region, block, k, level, ordered);
        }
        set_attr_region(dm, region, block, "style", &AttrVal::Str(style.to_string()));
    }
}

fn apply_set_block_attr(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    key: &str,
    value: &AttrVal,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        set_attr_region(dm, region, block, key, value);
    }
}

/// Lay out a `rows`×`cols` table after `after`, row by row, writing
/// `text(row, col)` into each cell that has any. An empty table writes no
/// text, so it emits exactly the ops it always has.
fn apply_insert_table<'t>(
    dm: &mut DocMut,
    after: &str,
    rows: usize,
    cols: usize,
    text: impl Fn(usize, usize) -> Option<&'t str>,
) {
    let table_id = dm.ids.mint(); // one id shared by every cell
    let mut prev = Some(after.to_string());
    for row in 0..rows {
        for col in 0..cols {
            let id = dm.insert_block(prev.as_deref(), "cell");
            dm.set_attr(&id, "table", table_id.as_str());
            dm.set_attr(&id, "row", num::i64_of_usize(row));
            dm.set_attr(&id, "col", num::i64_of_usize(col));
            if let Some(text) = text(row, col).filter(|text| !text.is_empty()) {
                dm.splice_text(&id, 0, 0, text);
            }
            prev = Some(id);
        }
    }
}

type TableCells = Vec<(String, usize, usize)>;

fn table_cells(dm: &DocMut<'_>, table: &str) -> TableCells {
    tilo_doc::model::materialize(dm.doc, dm.c)
        .blocks
        .into_iter()
        .filter(|block| {
            block.kind == "cell"
                && matches!(block.attrs.get("table"), Some(tilo_doc::AttrVal::Str(id)) if id == table)
        })
        .map(|block| {
            let coordinate = |key: &str| match block.attrs.get(key) {
                Some(tilo_doc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
                _ => 0,
            };
            (block.id, coordinate("row"), coordinate("col"))
        })
        .collect()
}

fn table_head_anchor(dm: &DocMut<'_>, cells: &TableCells) -> Option<String> {
    let order = tilo_doc::model::blocks_in_order(dm.doc, dm.c);
    let first = cells
        .iter()
        .filter_map(|(id, _, _)| order.iter().position(|candidate| candidate == id))
        .min()?;
    first
        .checked_sub(1)
        .and_then(|index| order.get(index).cloned())
}

fn apply_insert_table_row(dm: &mut DocMut<'_>, table: &str, at: usize, cells: &TableCells) {
    let columns = cells
        .iter()
        .map(|(_, _, column)| *column)
        .max()
        .map_or(0, |column| column.saturating_add(1));
    let anchor = if at == 0 {
        table_head_anchor(dm, cells)
    } else {
        cells
            .iter()
            .filter(|(_, row, _)| *row == at.saturating_sub(1))
            .max_by_key(|(_, _, column)| *column)
            .map(|(id, _, _)| id.clone())
    };
    let mut shifted: Vec<_> = cells.iter().filter(|(_, row, _)| *row >= at).collect();
    shifted.sort_by_key(|(id, _, _)| id.as_str());
    for (id, row, _) in shifted {
        dm.set_attr(id, "row", num::i64_of_usize(row.saturating_add(1)));
    }
    let mut previous = anchor;
    for column in 0..columns {
        let id = dm.insert_block(previous.as_deref(), "cell");
        dm.set_attr(&id, "table", table);
        dm.set_attr(&id, "row", num::i64_of_usize(at));
        dm.set_attr(&id, "col", num::i64_of_usize(column));
        previous = Some(id);
    }
}

fn apply_insert_table_column(dm: &mut DocMut<'_>, table: &str, at: usize, cells: &TableCells) {
    let rows = cells
        .iter()
        .map(|(_, row, _)| *row)
        .max()
        .map_or(0, |row| row.saturating_add(1));
    let mut shifted: Vec<_> = cells
        .iter()
        .filter(|(_, _, column)| *column >= at)
        .collect();
    shifted.sort_by_key(|(id, _, _)| id.as_str());
    for (id, _, column) in shifted {
        dm.set_attr(id, "col", num::i64_of_usize(column.saturating_add(1)));
    }
    for row in 0..rows {
        let anchor = if at == 0 {
            if row == 0 {
                table_head_anchor(dm, cells)
            } else {
                cells
                    .iter()
                    .filter(|(_, candidate_row, _)| *candidate_row == row.saturating_sub(1))
                    .max_by_key(|(_, _, column)| *column)
                    .map(|(id, _, _)| id.clone())
            }
        } else {
            cells
                .iter()
                .find(|(_, candidate_row, column)| {
                    *candidate_row == row && *column == at.saturating_sub(1)
                })
                .map(|(id, _, _)| id.clone())
        };
        let id = dm.insert_block(anchor.as_deref(), "cell");
        dm.set_attr(&id, "table", table);
        dm.set_attr(&id, "row", num::i64_of_usize(row));
        dm.set_attr(&id, "col", num::i64_of_usize(at));
    }
}

fn apply_delete_table_axis(dm: &mut DocMut<'_>, cells: &TableCells, row_axis: bool, index: usize) {
    let axis_value = |cell: &&(String, usize, usize)| if row_axis { cell.1 } else { cell.2 };
    let extent = cells
        .iter()
        .map(|cell| axis_value(&cell))
        .max()
        .map_or(0, |value| value.saturating_add(1));
    if extent <= 1 {
        for (id, _, _) in cells {
            dm.tombstone_block_in(Region::Body, id);
        }
        return;
    }
    let mut ordered = cells.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(id, _, _)| id.as_str());
    for cell @ (id, row, column) in ordered {
        let value = axis_value(&cell);
        if value == index {
            dm.tombstone_block_in(Region::Body, id);
        } else if value > index {
            dm.set_attr(
                id,
                if row_axis { "row" } else { "col" },
                num::i64_of_usize(if row_axis {
                    row.saturating_sub(1)
                } else {
                    column.saturating_sub(1)
                }),
            );
        }
    }
}

fn apply_table_edit(dm: &mut DocMut<'_>, table: &str, edit: TableEdit) {
    let cells = table_cells(dm, table);
    match edit {
        TableEdit::InsertRow { at } => apply_insert_table_row(dm, table, at, &cells),
        TableEdit::InsertColumn { at } => apply_insert_table_column(dm, table, at, &cells),
        TableEdit::DeleteRow { row } => apply_delete_table_axis(dm, &cells, true, row),
        TableEdit::DeleteColumn { column } => {
            apply_delete_table_axis(dm, &cells, false, column);
        }
        TableEdit::Delete => {
            for (id, _, _) in &cells {
                dm.tombstone_block_in(Region::Body, id);
            }
        }
    }
}

fn apply_insert_image(dm: &mut DocMut, block: &str, at: usize, image: &ImageInput) {
    let ImageInput {
        content: hash,
        mime,
        len,
        width,
        height,
    } = image;
    let (width, height) = (*width, *height);
    let text = tilo_doc::model::block_text(dm.doc, dm.c, block).unwrap_or_default();
    let char_count = text.chars().count();
    let tail: String = text.chars().skip(at).collect();
    let dimension = |v: Option<i64>| v.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    dm.reference_content(
        hash,
        &tilo_content::ContentMeta {
            mime: mime.clone(),
            len: *len,
            w: dimension(width),
            h: dimension(height),
        },
    );
    if let Some(tail) = char_count.checked_sub(at).filter(|n| *n > 0) {
        dm.splice_text(block, at, num::isize_of_usize(tail), "");
    }
    let img = dm.insert_block(Some(block), "image");
    dm.set_attr(&img, "blob", hash.as_str());
    if let Some(w) = width {
        dm.set_attr(&img, "width", w);
    }
    if let Some(h) = height {
        dm.set_attr(&img, "height", h);
    }
    let after = dm.insert_block(Some(&img), "paragraph");
    if !tail.is_empty() {
        dm.splice_text(&after, 0, 0, &tail);
    }
}

fn apply_insert_page_break(dm: &mut DocMut, region: Region, block: &str, at: usize) {
    // Split the block at the caret (like Enter): the tail moves into a fresh
    // paragraph that starts on a new page.
    let text = match region {
        Region::Body => tilo_doc::model::block_text(dm.doc, dm.c, block),
        other => tilo_doc::model::materialize_region(dm.doc, dm.c, other)
            .into_iter()
            .find(|entry| entry.id == block)
            .map(|entry| entry.text),
    }
    .unwrap_or_default();
    let char_count = text.chars().count();
    let tail: String = text.chars().skip(at).collect();
    if let Some(tail) = char_count.checked_sub(at).filter(|n| *n > 0) {
        dm.splice_text_in(region, block, at, num::isize_of_usize(tail), "");
    }
    let new = dm.insert_block_in(region, Some(block), "paragraph");
    if !tail.is_empty() {
        dm.splice_text_in(region, &new, 0, 0, &tail);
    }
    dm.set_attr_in(region, &new, "page_break_before", true);
}

fn apply_split_block(
    dm: &mut DocMut,
    region: Region,
    block: &str,
    at: usize,
    kind: &str,
    ordered: Option<bool>,
    next: Option<&str>,
) {
    let text = match region {
        Region::Body => tilo_doc::model::block_text(dm.doc, dm.c, block),
        other => tilo_doc::model::materialize_region(dm.doc, dm.c, other)
            .into_iter()
            .find(|entry| entry.id == block)
            .map(|entry| entry.text),
    }
    .unwrap_or_default();
    let tail: String = text.chars().skip(at).collect();
    let remove = text.chars().count().saturating_sub(at);
    if remove > 0 {
        dm.splice_text_in(region, block, at, num::isize_of_usize(remove), "");
    }
    let new = dm.insert_block_before_in(region, block, next, kind);
    if let Some(value) = ordered {
        dm.set_attr_in(region, &new, "ordered", value);
    }
    if !tail.is_empty() {
        dm.splice_text_in(region, &new, 0, 0, &tail);
    }
}

fn apply_generate_toc(dm: &mut DocMut, entries: &[TocTarget]) {
    let snapshot = tilo_doc::model::materialize(dm.doc, dm.c);
    for block in snapshot.blocks.iter().filter(|block| block.flag("toc")) {
        dm.tombstone_block_in(Region::Body, &block.id);
    }
    let mut previous = None;
    for entry in entries {
        let block = dm.insert_block(previous.as_deref(), "paragraph");
        dm.set_attr(&block, "toc", true);
        dm.set_attr(&block, "toc_level", entry.level);
        dm.set_attr(&block, "toc_heading", entry.heading.as_str());
        let indent = "  ".repeat(num::usize_of_i64(entry.level.saturating_sub(1)));
        let title = entry.text.trim();
        let line = entry.page.map_or_else(
            || format!("{indent}{title}"),
            |page| format!("{indent}{title} .......... {page}"),
        );
        dm.splice_text(&block, 0, 0, &line);
        previous = Some(block);
    }
}

fn apply_paste_fragment(
    dm: &mut DocMut,
    region: Region,
    selection: &SelectionTarget,
    blocks: &[ClipBlock],
    adopt_first_style: bool,
) {
    apply_replace_selection(
        dm,
        region,
        &selection.start_block,
        selection.start,
        &selection.end_block,
        selection.end,
        "",
    );
    let mut current = selection.start_block.clone();
    let mut offset = selection.start;
    // Pasted tables get ids of this document, so one never joins a table
    // already here.
    let mut tables = std::collections::BTreeMap::<String, String>::new();
    for (index, fragment) in blocks.iter().enumerate() {
        let cell = fragment.kind == "cell";
        if index > 0 || (cell && !adopt_first_style) {
            let text = match region {
                Region::Body => tilo_doc::model::block_text(dm.doc, dm.c, &current),
                other => tilo_doc::model::materialize_region(dm.doc, dm.c, other)
                    .into_iter()
                    .find(|entry| entry.id == current)
                    .map(|entry| entry.text),
            }
            .unwrap_or_default();
            let tail: String = text.chars().skip(offset).collect();
            let remove = text.chars().count().saturating_sub(offset);
            if remove > 0 {
                dm.splice_text_in(region, &current, offset, num::isize_of_usize(remove), "");
            }
            let next = dm.insert_block_in(region, Some(&current), "paragraph");
            if !tail.is_empty() {
                dm.splice_text_in(region, &next, 0, 0, &tail);
            }
            current = next;
            offset = 0;
        }
        if !fragment.text.is_empty() {
            dm.splice_text_in(region, &current, offset, 0, &fragment.text);
        }
        for mark in &fragment.marks {
            let span = Span {
                block: current.clone(),
                start: offset.saturating_add(mark.start),
                end: offset.saturating_add(mark.end),
            };
            let value = match &mark.value {
                tilo_doc::document::MarkValue::Bool(value) => MarkVal::Bool(*value),
                tilo_doc::document::MarkValue::Int(value) => MarkVal::Int(*value),
                tilo_doc::document::MarkValue::Str(value) => MarkVal::Str(value.clone()),
            };
            add_mark_region(dm, region, &span, &mark.name, &value);
        }
        if index > 0 || adopt_first_style || cell {
            set_kind_region(dm, region, &current, &fragment.kind);
            if let Some(level) = fragment.level {
                set_attr_region(dm, region, &current, "level", &AttrVal::Int(level));
            }
            if let Some(ordered) = fragment.ordered {
                set_attr_region(dm, region, &current, "ordered", &AttrVal::Bool(ordered));
            }
            if let Some(align) = &fragment.align {
                set_attr_region(dm, region, &current, "align", &AttrVal::Str(align.clone()));
            }
            for (key, value) in &fragment.attrs {
                let value = match value {
                    crate::ClipAttr::Str(pasted) if cell && key == "table" => AttrVal::Str(
                        tables
                            .entry(pasted.clone())
                            .or_insert_with(|| dm.ids.mint())
                            .clone(),
                    ),
                    other => clip_attr(other),
                };
                set_attr_region(dm, region, &current, key, &value);
            }
        }
        offset = offset.saturating_add(fragment.text.chars().count());
    }
}

fn apply_insert_paragraph(dm: &mut DocMut, after: &str, text: &str) -> Written {
    let new = dm.insert_block(Some(after), "paragraph");
    if !text.is_empty() {
        dm.splice_text(&new, 0, 0, text);
    }
    Written::body(new, 0, text)
}

/// The last body block as `append-paragraph`/`continue-paragraph` resolve it
/// at apply time. Validation, the write and an editor's predicted caret share
/// this one reading.
pub struct BodyEnd {
    pub id: String,
    pub kind: Option<String>,
    /// Characters, the unit [`RichTextAction::SpliceText`] `at` uses.
    pub chars: usize,
    /// A paragraph, heading or list item: text that can be continued.
    pub text_block: bool,
}

/// Read the last body block, `None` for an empty body.
pub fn body_end(doc: &AutoCommit, c: &Containers) -> Option<BodyEnd> {
    let id = tilo_doc::model::blocks_in_order(doc, c).pop()?;
    Some(body_end_at(doc, c, id))
}

/// Read `id` as the last body block, for a caller that knows which block is
/// last (from a kept body order) and so need not list the body.
pub fn body_end_at(doc: &AutoCommit, c: &Containers, id: String) -> BodyEnd {
    let kind = tilo_doc::model::block_kind(doc, c, &id);
    let chars = tilo_doc::model::block_text(doc, c, &id)
        .unwrap_or_default()
        .chars()
        .count();
    let text_block = matches!(kind.as_deref(), Some("paragraph" | "heading" | "list_item"));
    BodyEnd {
        id,
        kind,
        chars,
        text_block,
    }
}

fn apply_append_paragraph(dm: &mut DocMut, text: &str) -> Written {
    let target = match body_end(dm.doc, dm.c) {
        Some(end) if end.chars == 0 && end.kind.as_deref() == Some("paragraph") => end.id,
        Some(end) => dm.insert_block(Some(&end.id), "paragraph"),
        None => dm.insert_block(None, "paragraph"),
    };
    dm.splice_text(&target, 0, 0, text);
    Written::body(target, 0, text)
}

fn apply_continue_paragraph(dm: &mut DocMut, text: &str) -> Option<Written> {
    // Validated before the mutable view: the last body block is a text block.
    let end = body_end(dm.doc, dm.c).filter(|end| end.text_block)?;
    dm.splice_text(&end.id, end.chars, 0, text);
    Some(Written::body(end.id, end.chars, text))
}

fn apply_remove_blocks(dm: &mut DocMut, blocks: &[String]) {
    apply_remove_region_blocks(dm, Region::Body, blocks);
}

fn apply_ensure_region_block(dm: &mut DocMut, region: Region) {
    if !tilo_doc::model::region_blocks_in_order(dm.doc, dm.c, region).is_empty() {
        return;
    }
    dm.insert_block_in(region, None, "paragraph");
}

fn apply_remove_region_blocks(dm: &mut DocMut, region: Region, blocks: &[String]) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        dm.tombstone_block_in(region, block);
    }
}

/// A file an action imports is as long as a text it carries: one document, kept whole.
pub(crate) const MAX_IMPORT_BYTES: usize = MAX_ACTION_TEXT_BYTES;

fn apply_replace_formatted(dm: &mut DocMut, format: ImportFormat, bytes: &[u8]) {
    let existing = tilo_doc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.tombstone_block_in(Region::Body, id);
    }
    let mut inputs = tilo_content::ContentInputs::new();
    // The caller stages a successful import before this write. A replay
    // that still fails keeps the cleared body rather than a half-written
    // codec result mixed with the old one.
    let _ = import_formatted(dm, format, bytes, &mut inputs);
    keep_a_paragraph(dm);
    dm.set_meta_str(FILE_DIGEST, &tilo_content::hash_bytes(bytes));
}

/// The most images an imported file may embed.
pub(crate) const MAX_IMPORTED_CONTENT: usize = 4096;

fn apply_replace_imported(
    dm: &mut DocMut,
    document: &[u8],
    content: &BTreeMap<String, String>,
    file_digest: &str,
) {
    // Validated where it was staged; a document that does not load leaves
    // the body as it was.
    let Ok(imported) = AutoCommit::load(document) else {
        return;
    };
    let Ok(containers) = tilo_doc::model::open(&imported, &automerge::ROOT) else {
        return;
    };
    let existing = tilo_doc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.tombstone_block_in(Region::Body, id);
    }
    // A write automerge refuses leaves what was written before it, as a
    // codec failing midway did.
    let _ = dm.graft(&imported, &containers, content);
    keep_a_paragraph(dm);
    dm.set_meta_str(FILE_DIGEST, file_digest);
}

/// The document meta key holding the content address of the file the
/// document was last read from or written to.
pub const FILE_DIGEST: &str = "file_digest";

/// The document meta key holding whether the document has changed since the
/// file whose content address is [`FILE_DIGEST`] took it: the work a file
/// does not hold yet.
///
/// Every action that changes the body sets it, and those that put a file's
/// content in the document — [`RichTextAction::ReplaceImported`],
/// [`RichTextAction::ReplaceFormatted`] and
/// [`RichTextAction::NoteFileWritten`] — clear it. So an undone edit reads
/// as unsaved too: the document did change since the file was written, and
/// over-reporting that is the safe direction for anyone closing a window.
pub const UNSAVED: &str = "unsaved";

fn import_formatted(
    dm: &mut DocMut,
    format: ImportFormat,
    bytes: &[u8],
    inputs: &mut tilo_content::ContentInputs,
) -> Result<(), tilo_doc_codecs::CodecError> {
    use tilo_doc_codecs::FormatProvider;
    match format {
        ImportFormat::Markdown => tilo_doc_codecs::MarkdownCodec.import(bytes, dm, inputs),
        ImportFormat::Html => tilo_doc_codecs::HtmlCodec.import(bytes, dm, inputs),
        ImportFormat::Rtf => tilo_doc_codecs::RtfCodec.import(bytes, dm, inputs),
        ImportFormat::Docx => tilo_doc_codecs::DocxCodec.import(bytes, dm, inputs),
        ImportFormat::Odt => tilo_doc_codecs::OdtCodec.import(bytes, dm, inputs),
        ImportFormat::Text => import_plain_text(dm, bytes),
    }
}

/// A file read as a format where its import is invoked, before it is an
/// action: the document it reads as, and the images it embeds, which the
/// document names by address and the invoking side keeps in its content
/// store ([`Self::action`]).
pub struct StagedImport {
    document: Vec<u8>,
    file_digest: String,
    inputs: tilo_content::ContentInputs,
}

impl StagedImport {
    /// The images the file embeds, by address.
    #[must_use]
    pub fn inputs(&self) -> &tilo_content::ContentInputs {
        &self.inputs
    }

    /// The [`RichTextAction::ReplaceImported`] that writes the file's
    /// content, its images named by the references `content` answers for
    /// their addresses (what [`tilo_content::ContentStore::put_inputs`]
    /// answers).
    #[must_use]
    pub fn action(self, content: BTreeMap<String, String>) -> RichTextAction {
        RichTextAction::ReplaceImported {
            document: self.document,
            content,
            file_digest: self.file_digest,
        }
    }
}

/// Read `bytes` as `format` once, where the import is invoked, so bytes the
/// codec rejects never become an action, and every replica writes what this
/// read rather than reading the file again ([`StagedImport::action`]).
///
/// # Errors
///
/// Returns why the bytes do not import as `format`.
pub fn stage_import(format: ImportFormat, bytes: &[u8]) -> Result<StagedImport, String> {
    let too_large = || {
        format!(
            "the file is larger than {} MiB",
            MAX_IMPORT_BYTES / (1024 * 1024)
        )
    };
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    let mut scratch = AutoCommit::new();
    let containers = tilo_doc::model::init(&mut scratch, &automerge::ROOT);
    // Ids of the scratch document's own actor: the blocks it writes are
    // new wherever the action applies.
    let mut ids = tilo_doc::IdGen::new(scratch.get_actor().to_hex_string());
    let mut inputs = tilo_content::ContentInputs::new();
    import_formatted(
        &mut DocMut {
            doc: &mut scratch,
            c: &containers,
            ids: &mut ids,
        },
        format,
        bytes,
        &mut inputs,
    )
    .map_err(|error| error.to_string())?;
    if inputs.len() > MAX_IMPORTED_CONTENT {
        return Err(format!(
            "the file embeds more than {MAX_IMPORTED_CONTENT} images"
        ));
    }
    let document = scratch.save();
    if document.len() > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    Ok(StagedImport {
        document,
        file_digest: tilo_content::hash_bytes(bytes),
        inputs,
    })
}

/// What `bytes` read as `format` hold, read from a scratch document without
/// writing any other: a paste's paragraphs, a file compared with the
/// document it was opened into.
///
/// # Errors
///
/// Returns why the bytes do not import as `format`.
pub fn imported_snapshot(
    format: ImportFormat,
    bytes: &[u8],
) -> Result<tilo_doc::DocSnapshot, String> {
    let mut scratch = AutoCommit::new();
    let containers = tilo_doc::model::init(&mut scratch, &automerge::ROOT);
    let mut ids = tilo_doc::IdGen::new("imported-snapshot".into());
    import_formatted(
        &mut DocMut {
            doc: &mut scratch,
            c: &containers,
            ids: &mut ids,
        },
        format,
        bytes,
        &mut tilo_content::ContentInputs::new(),
    )
    .map_err(|error| error.to_string())?;
    Ok(tilo_doc::model::materialize(&scratch, &containers))
}

fn import_plain_text(dm: &mut DocMut, bytes: &[u8]) -> Result<(), tilo_doc_codecs::CodecError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| tilo_doc_codecs::CodecError::Parse(error.to_string()))?;
    dm.bulk(|dm| {
        let mut previous = None;
        for line in text.lines() {
            let block = dm.insert_block(previous.as_deref(), "paragraph");
            dm.splice_text(&block, 0, 0, line);
            previous = Some(block);
        }
    });
    Ok(())
}

fn apply_replace_body(dm: &mut DocMut, markdown: &str) {
    use tilo_doc_codecs::FormatProvider;
    let existing = tilo_doc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.tombstone_block_in(Region::Body, id);
    }
    // A malformed template can't wedge a replay: the body-clear stays in the
    // pinned change either way.
    let _ = tilo_doc_codecs::MarkdownCodec.import(
        markdown.as_bytes(),
        dm,
        &mut tilo_content::ContentInputs::new(),
    );
    keep_a_paragraph(dm);
}

fn apply_replace_clips(dm: &mut DocMut, blocks: &[ClipBlock]) {
    let existing = tilo_doc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.tombstone_block_in(Region::Body, id);
    }
    if !blocks.is_empty() {
        let first = dm.insert_block(None, "paragraph");
        apply_paste_fragment(
            dm,
            Region::Body,
            &SelectionTarget {
                start_block: first.clone(),
                start: 0,
                end_block: first,
                end: 0,
            },
            blocks,
            true,
        );
    }
    keep_a_paragraph(dm);
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "`skip_serializing_if` calls it with the field by reference"
)]
fn is_zero(at: &usize) -> bool {
    *at == 0
}

fn apply_sync_clips(dm: &mut DocMut, blocks: &[ClipBlock], replace: Option<&[String]>, at: usize) {
    let views: Vec<tilo_doc::snapshot::BlockView> = blocks.iter().map(ClipBlock::to_view).collect();
    let Some(replace) = replace else {
        tilo_doc::reconcile_body(dm, &views);
        keep_a_paragraph(dm);
        return;
    };
    // An edit's blocks go where the blocks they stand for are, and only that stretch of the body
    // and the block on each side of it is read: those pair with themselves, so the edit is
    // written where it was made, however long the body.
    let order = tilo_doc::model::region_blocks_in_order(dm.doc, dm.c, Region::Body);
    let named: Vec<usize> = replace
        .iter()
        .filter_map(|id| order.iter().position(|block| block == id))
        .collect();
    let (start, gone) = if !named.is_empty() && named.len() == replace.len() {
        (named.iter().copied().min().unwrap_or(at), named)
    } else {
        let start = at.min(order.len());
        let end = start.saturating_add(replace.len()).min(order.len());
        (start, (start..end).collect())
    };
    let end = gone
        .iter()
        .max()
        .map_or(start, |last| last.saturating_add(1));
    let from = start.saturating_sub(1);
    let ids = order
        .get(from..end.saturating_add(1).min(order.len()))
        .unwrap_or_default();
    let live = tilo_doc::model::materialize_blocks(dm.doc, dm.c, ids);
    let kept = |index: &usize| !gone.contains(&index.saturating_add(from));
    let at = |index: usize| live.get(index);
    let split = start.saturating_sub(from);
    let wanted: Vec<_> = (0..split)
        .filter(kept)
        .filter_map(at)
        .chain(&views)
        .chain((split..live.len()).filter(kept).filter_map(at))
        .collect();
    tilo_doc::reconcile_body_from(dm, &live, &wanted);
    keep_a_paragraph(dm);
}

/// A body replaced by nothing (an empty file, an empty template) still has
/// a paragraph to write in, as a new document does.
fn keep_a_paragraph(dm: &mut DocMut) {
    if tilo_doc::model::blocks_in_order(dm.doc, dm.c).is_empty() {
        dm.insert_block(None, "paragraph");
    }
}

fn ordered_change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let order = tilo_doc::model::blocks_in_order(doc, c);
    let mut ids = tilo_doc::model::change_ids(doc, c);
    ids.sort_by_key(|id| {
        tilo_doc::model::change_range(doc, c, id).map_or_else(
            || (usize::MAX, usize::MAX, id.clone()),
            |(block, start, _)| {
                (
                    order
                        .iter()
                        .position(|candidate| candidate == &block)
                        .unwrap_or(usize::MAX),
                    start,
                    id.clone(),
                )
            },
        )
    });
    ids
}

fn apply_review_changes(dm: &mut DocMut, id: Option<&str>, accept: bool) {
    let mut ids = id.map_or_else(
        || ordered_change_ids(dm.doc, dm.c),
        |one| vec![one.to_string()],
    );
    // Work from the end of the document so removing text cannot shift an
    // earlier change's anchored range before it is processed.
    ids.reverse();
    for id in &ids {
        if accept {
            dm.accept_change(id);
        } else {
            dm.reject_change(id);
        }
    }
}

fn apply_resolve_comment(dm: &mut DocMut, id: &str) {
    // Record the semantic delete independently from the current map entry.
    // A reply authored on a divergent replica may merge after this deletion;
    // the grow-only tombstone keeps that stale thread from reappearing.
    dm.tombstone_comment(id);
    let range = tilo_doc::model::comment_range(dm.doc, dm.c, id);
    if let Some((block, start, end)) = range
        && start < end
    {
        dm.remove_mark(&block, start, end, tilo_doc::schema::MARK_COMMENT);
    }
    // Thread replies are sibling records in the canonical comments map,
    // not dangling private overlay state. Remove them deterministically
    // before their parent so a resolved thread leaves no orphan resources.
    let comments = &dm.c.comments;
    let mut replies = tilo_doc::model::comment_reply_ids(dm.doc, dm.c, id);
    replies.sort();
    for reply in replies {
        let _ = dm.doc.delete(comments, reply);
    }
    let _ = dm.doc.delete(comments, id);
}

fn apply_add_comment(
    dm: &mut DocMut,
    block: &str,
    start: usize,
    end: usize,
    author: &str,
    date: &str,
    text: &str,
) {
    let id = dm.insert_comment(author, date);
    if let Some(body) = dm.insert_comment_block(&id, None, "paragraph") {
        dm.splice_comment_text(&id, &body, 0, 0, text);
        dm.add_comment_mark(block, start, end, &id);
    }
}

/// The text an action typed in: `len` characters of `block` from `start`.
pub(crate) struct Written {
    region: Region,
    block: String,
    start: usize,
    len: usize,
}

impl Written {
    fn new(region: Region, block: &str, start: usize, text: &str) -> Self {
        Self {
            region,
            block: block.to_owned(),
            start,
            len: text.chars().count(),
        }
    }

    pub(crate) fn body(block: String, start: usize, text: &str) -> Self {
        Self {
            region: Region::Body,
            block,
            start,
            len: text.chars().count(),
        }
    }

    /// Mark the written text as `author`'s, as part of the edit that wrote it.
    ///
    /// Every key carries its own mark, even typed straight after `author`'s
    /// own run. Relying on that run's mark to take the key in instead would
    /// lose the key to a collaborator typing at the same place at the same
    /// time: their key's mark, anchored where this key lands, would cover it
    /// and win.
    fn attribute(&self, dm: &mut DocMut<'_>, author: &str) {
        if self.len == 0 || author.is_empty() {
            return;
        }
        dm.mark_author_in(
            self.region,
            &self.block,
            self.start,
            self.start.saturating_add(self.len),
            author,
        );
    }
}

/// Apply a text action; what it typed in, when it typed text in one known
/// place.
fn apply_text_action(dm: &mut DocMut<'_>, action: &RichTextAction) -> Option<Written> {
    match action {
        RichTextAction::ReplaceMatches { region, spans, ins } => {
            let mut sorted: Vec<_> = spans.iter().collect();
            sorted.sort_by(|a, b| a.block.cmp(&b.block).then_with(|| b.start.cmp(&a.start)));
            for span in sorted {
                apply_splice_text(
                    dm,
                    region.to_region(),
                    &span.block,
                    span.start,
                    span.end.saturating_sub(span.start),
                    ins,
                );
            }
            // Typed in many places at once: no one place to mark.
            None
        }
        RichTextAction::SpliceText {
            region,
            block,
            at,
            del,
            ins,
        } => {
            apply_splice_text(dm, region.to_region(), block, *at, *del, ins);
            Some(Written::new(region.to_region(), block, *at, ins))
        }
        RichTextAction::ReplaceSelection {
            region,
            start_block,
            start,
            end_block,
            end,
            ins,
        } => {
            apply_replace_selection(
                dm,
                region.to_region(),
                start_block,
                *start,
                end_block,
                *end,
                ins,
            );
            Some(Written::new(region.to_region(), start_block, *start, ins))
        }
        RichTextAction::InsertParagraph { after, text } => {
            Some(apply_insert_paragraph(dm, after, text))
        }
        RichTextAction::AppendParagraph { text } => Some(apply_append_paragraph(dm, text)),
        RichTextAction::ContinueParagraph { text } => apply_continue_paragraph(dm, text),
        other => {
            apply_block_text_action(dm, other);
            None
        }
    }
}

/// The text actions that set marks, paste or replace blocks wholesale.
fn apply_block_text_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::SetMark {
            region,
            spans,
            name,
            value,
            also_clear,
        } => apply_set_mark(
            dm,
            region.to_region(),
            spans,
            name,
            value.as_ref(),
            also_clear,
        ),
        RichTextAction::PasteFragment {
            region,
            start_block,
            start,
            end_block,
            end,
            blocks,
            adopt_first_style,
        } => apply_paste_fragment(
            dm,
            region.to_region(),
            &SelectionTarget {
                start_block: start_block.clone(),
                start: *start,
                end_block: end_block.clone(),
                end: *end,
            },
            blocks,
            *adopt_first_style,
        ),
        RichTextAction::RemoveBlocks { blocks } => apply_remove_blocks(dm, blocks),
        RichTextAction::RemoveRegionBlocks { region, blocks } => {
            apply_remove_region_blocks(dm, region.to_region(), blocks);
        }
        RichTextAction::EnsureRegionBlock { region } => {
            apply_ensure_region_block(dm, region.to_region());
        }
        RichTextAction::ReplaceBody { markdown } => apply_replace_body(dm, markdown),
        RichTextAction::ReplaceClips { blocks } => apply_replace_clips(dm, blocks),
        RichTextAction::SyncClips {
            blocks,
            replace,
            at,
        } => apply_sync_clips(dm, blocks, replace.as_deref(), *at),
        RichTextAction::ReplaceFormatted { format, bytes } => {
            apply_replace_formatted(dm, *format, bytes);
        }
        RichTextAction::ReplaceImported {
            document,
            content,
            file_digest,
        } => apply_replace_imported(dm, document, content, file_digest),
        _ => {}
    }
}

fn apply_formatting_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::SetParagraphStyle {
            region,
            blocks,
            kind,
            level,
            ordered,
        } => apply_set_paragraph_style(dm, region.to_region(), blocks, kind, *level, *ordered),
        RichTextAction::ApplyNamedStyle {
            region,
            blocks,
            style,
            kind,
            level,
            ordered,
        } => apply_named_style(
            dm,
            region.to_region(),
            blocks,
            style,
            kind.as_ref(),
            *level,
            *ordered,
        ),
        RichTextAction::SetBlockAttr {
            region,
            blocks,
            key,
            value,
        } => apply_set_block_attr(dm, region.to_region(), blocks, key, value),
        RichTextAction::ClearBlockAttr {
            region,
            blocks,
            key,
        } => {
            let mut sorted = blocks.clone();
            sorted.sort();
            for block in &sorted {
                dm.remove_attr_in(region.to_region(), block, key);
            }
        }
        RichTextAction::SetPageMargins {
            top,
            right,
            bottom,
            left,
        } => {
            dm.set_meta_str("page_margin_top", &top.to_string());
            dm.set_meta_str("page_margin_right", &right.to_string());
            dm.set_meta_str("page_margin_bottom", &bottom.to_string());
            dm.set_meta_str("page_margin_left", &left.to_string());
        }
        RichTextAction::NoteFileWritten { digest } => dm.set_meta_str(FILE_DIGEST, digest),
        RichTextAction::SetStyleProperty { style, key, value } => {
            let value = match value {
                AttrVal::Bool(value) => tilo_doc::AttrVal::Bool(*value),
                AttrVal::Int(value) => tilo_doc::AttrVal::Int(*value),
                AttrVal::Float(value) => tilo_doc::AttrVal::Float(*value),
                AttrVal::Str(value) => tilo_doc::AttrVal::Str(value.clone()),
            };
            dm.set_style(style, &[(key.clone(), value)].into_iter().collect());
        }
        RichTextAction::DeleteStyle {
            style,
            fallback_blocks,
        } => {
            let mut blocks = fallback_blocks.clone();
            blocks.sort();
            for block in &blocks {
                dm.set_attr(block, "style", "");
            }
            dm.tombstone_style(style);
        }
        _ => {}
    }
}

fn apply_media_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::SetImageCrop {
            block,
            left,
            top,
            right,
            bottom,
        } => {
            dm.set_attr(block, "crop_left", *left);
            dm.set_attr(block, "crop_top", *top);
            dm.set_attr(block, "crop_right", *right);
            dm.set_attr(block, "crop_bottom", *bottom);
        }
        RichTextAction::SetImageSize {
            block,
            width,
            height,
        } => {
            dm.set_attr(block, "width", *width);
            dm.set_attr(block, "height", *height);
        }
        RichTextAction::SetImagePlacement {
            block,
            anchor,
            wrap,
            x,
            y,
            page,
        } => {
            dm.set_attr(block, "anchor", anchor.as_str());
            dm.set_attr(block, "wrap", wrap.as_str());
            dm.set_attr(block, "offset_x", *x);
            dm.set_attr(block, "offset_y", *y);
            dm.set_attr(block, "page", *page);
        }
        RichTextAction::InsertImage {
            block,
            at,
            content,
            mime,
            len,
            width,
            height,
        } => apply_insert_image(
            dm,
            block,
            *at,
            &ImageInput {
                content: content.clone(),
                mime: mime.clone(),
                len: *len,
                width: *width,
                height: *height,
            },
        ),
        _ => {}
    }
}

fn apply_table_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::InsertTable { after, rows, cols } => {
            apply_insert_table(dm, after, *rows, *cols, |_, _| None);
        }
        RichTextAction::InsertFilledTable { after, cells } => {
            let cols = cells.first().map_or(0, Vec::len);
            apply_insert_table(dm, after, cells.len(), cols, |row, col| {
                cells.get(row)?.get(col).map(String::as_str)
            });
        }
        RichTextAction::EditTable { table, edit } => apply_table_edit(dm, table, *edit),
        _ => {}
    }
}

fn apply_structure_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::InsertPageBreak { region, block, at } => {
            apply_insert_page_break(dm, region.to_region(), block, *at);
        }
        RichTextAction::SplitBlock {
            region,
            block,
            at,
            kind,
            ordered,
            next,
        } => apply_split_block(
            dm,
            region.to_region(),
            block,
            *at,
            kind,
            *ordered,
            next.as_deref(),
        ),
        RichTextAction::InsertField {
            region,
            block,
            at,
            field: _,
        } => dm.insert_page_number_field(region.to_region(), block, *at),
        RichTextAction::GenerateToc { entries } => apply_generate_toc(dm, entries),
        RichTextAction::MoveBlocks { blocks, after } => {
            let mut previous = after.clone();
            for block in blocks {
                dm.move_block(block, previous.as_deref());
                previous = Some(block.clone());
            }
        }
        _ => {}
    }
}

fn apply_review_action(dm: &mut DocMut<'_>, action: &RichTextAction) {
    match action {
        RichTextAction::AcceptChanges { id } => {
            apply_review_changes(dm, id.as_deref(), true);
        }
        RichTextAction::RejectChanges { id } => {
            apply_review_changes(dm, id.as_deref(), false);
        }
        RichTextAction::ResolveComment { id } => apply_resolve_comment(dm, id),
        RichTextAction::TrackInsert {
            block,
            at,
            text,
            author,
            date,
        } => {
            let _ = dm.track_insert(block, *at, text, author, date);
        }
        RichTextAction::AddComment {
            block,
            start,
            end,
            author,
            date,
            text,
        } => apply_add_comment(dm, block, *start, *end, author, date, text),
        RichTextAction::ReplyToComment {
            comment,
            author,
            date,
            text,
            body,
        } => {
            if body.is_empty() {
                let _ = dm.add_comment_reply(comment, author, date, text);
            } else {
                apply_reply_body(dm, comment, author, date, body);
            }
        }
        _ => {}
    }
}

impl RichTextAction {
    /// Apply this action to the rich text stored under `root`: the whole
    /// document when `root` is the document's root, or one rich-text field
    /// of another resource (a message body, a note's page) when it is that
    /// field's map. The owning resource's own action carries this one and
    /// names the field, so the edit replays exactly as it was made.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Refused`] for rich text that is missing under
    /// `root`, and [`ActionError::Declined`] for a text longer than one text may
    /// hold or targets that are not as the action read them, before anything is
    /// written.
    pub fn apply_under(&self, doc: &mut Doc, root: &ObjId) -> Result<(), ActionError> {
        // Bind to the canonical schema under `root`. Refuse an unbound
        // document explicitly so callers never mistake an unavailable target
        // for a successful edit.
        let Ok(c) = tilo_doc::model::open(doc, root) else {
            return Err(ActionError::Refused(
                "Writer document is unavailable or has not been initialized".into(),
            ));
        };
        // Validate every target against this one, immutable document state
        // before creating the mutable editing view.  In particular, do not let
        // a stale multi-block format/delete partially affect still-existing
        // blocks when one peer has already removed another target.
        if self.domain() == ActionDomain::Blocks {
            // Names the stale block or the malformed argument.
            crate::block_edit::check(self, doc, &c).map_err(ActionError::Refused)?;
        } else if let Err(reason) = refusal(self, doc, &c) {
            return Err(ActionError::Declined(reason));
        }
        // Ids come from the action's own deterministic context (the pinned
        // actor, the heads it applies at, and its payload): byte-identical on
        // origin and replay, and minted without scanning the document.
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        let cx = tilo_resource::ActionCx::for_apply(doc, &payload, 0);
        // Who typed the text in: the invoker the envelope pins, so a replay
        // marks exactly what the origin marked.
        let author = cx.author().map(str::to_owned);
        let mut ids = tilo_doc::model::IdGen::for_action(cx);
        let mut dm = DocMut {
            doc,
            c: &c,
            ids: &mut ids,
        };

        match self.domain() {
            ActionDomain::Text => {
                if let (Some(written), Some(author)) =
                    (apply_text_action(&mut dm, self), author.as_deref())
                {
                    written.attribute(&mut dm, author);
                }
            }
            ActionDomain::Formatting => apply_formatting_action(&mut dm, self),
            ActionDomain::Media => apply_media_action(&mut dm, self),
            ActionDomain::Table => apply_table_action(&mut dm, self),
            ActionDomain::Structure => apply_structure_action(&mut dm, self),
            ActionDomain::Review => apply_review_action(&mut dm, self),
            ActionDomain::Blocks => {
                for written in crate::block_edit::apply(&mut dm, self) {
                    if let Some(author) = author.as_deref() {
                        written.attribute(&mut dm, author);
                    }
                }
            }
        }
        // The document now holds work its file does not (or, for the two
        // actions that take a file's content, work it does).
        let file_took_it = matches!(
            self,
            RichTextAction::NoteFileWritten { .. }
                | RichTextAction::ReplaceFormatted { .. }
                | RichTextAction::ReplaceImported { .. }
        );
        dm.set_meta_flag(UNSAVED, !file_took_it);
        Ok(())
    }
}

impl Action for RichTextAction {
    /// Apply this action's mutation to the document's rich text.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Refused`] for an unavailable Writer document or
    /// stale/hostile decoded arguments before opening a mutable view.
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        self.apply_under(doc, &automerge::ROOT)
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            Self::NoteFileWritten { .. } => tilo_actions::ChangeKind::External,
            _ => tilo_actions::ChangeKind::Content,
        }
    }
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::widget`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Apply (or, with `value` `None`, clear) an inline mark over `spans`.
pub fn set_mark_action(
    region: RegionKind,
    spans: Vec<Span>,
    name: &str,
    value: Option<MarkVal>,
    also_clear: Vec<String>,
) -> RichTextAction {
    RichTextAction::SetMark {
        region,
        spans,
        name: name.to_string(),
        value,
        also_clear,
    }
}

/// Apply one external or internal hyperlink target to the selected text.
///
/// The URL remains a normal valued `link` mark, so it uses the same safe
/// export/import/collaboration path as imported HTML/ODT/DOCX links. A target
/// beginning with `#` is a document-local bookmark/cross-reference.
pub fn set_hyperlink_action(region: RegionKind, spans: Vec<Span>, target: &str) -> RichTextAction {
    set_mark_action(
        region,
        spans,
        "link",
        Some(MarkVal::Str(target.trim().to_string())),
        Vec::new(),
    )
}

/// Accept only link targets the editor can safely activate and round-trip.
/// Imported documents may preserve other URI forms for fidelity, but newly
/// authored actions deliberately have a narrower contract.
pub fn valid_hyperlink_target(target: &str) -> bool {
    if target.is_empty() || target.trim() != target || target.chars().count() > 2_048 {
        return false;
    }
    if let Some(fragment) = target.strip_prefix('#') {
        return !fragment.is_empty()
            && fragment.len() <= 128
            && fragment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    }
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    match url.scheme() {
        "https" => {
            url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
        }
        "mailto" => {
            let address = url.path();
            address
                .split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        }
        _ => false,
    }
}

/// Set one block attribute on `blocks`.
pub fn set_block_attr_action(
    region: RegionKind,
    blocks: Vec<String>,
    key: &str,
    value: AttrVal,
) -> RichTextAction {
    RichTextAction::SetBlockAttr {
        region,
        blocks,
        key: key.to_string(),
        value,
    }
}

pub fn set_page_margins_action(margins: [f64; 4]) -> RichTextAction {
    RichTextAction::SetPageMargins {
        top: margins[0],
        right: margins[1],
        bottom: margins[2],
        left: margins[3],
    }
}

/// Set one field of a stable named style.
pub fn set_style_property_action(style: &str, key: &str, value: AttrVal) -> RichTextAction {
    RichTextAction::SetStyleProperty {
        style: style.to_string(),
        key: key.to_string(),
        value,
    }
}

/// Delete one custom style and reassign its exact current instances.
pub fn delete_style_action(style: &str, fallback_blocks: Vec<String>) -> RichTextAction {
    RichTextAction::DeleteStyle {
        style: style.to_string(),
        fallback_blocks,
    }
}

/// Crop an image by source-relative edge insets (0..=9500 per edge).
pub fn set_image_crop_action(
    block: &str,
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
) -> RichTextAction {
    RichTextAction::SetImageCrop {
        block: block.to_string(),
        left,
        top,
        right,
        bottom,
    }
}

/// Resize an image frame in document pixels.
pub fn set_image_size_action(block: &str, width: i64, height: i64) -> RichTextAction {
    RichTextAction::SetImageSize {
        block: block.to_string(),
        width,
        height,
    }
}

/// Put an image in normal flow or at a canonical page-relative free position.
pub fn set_image_placement_action(
    block: &str,
    anchor: &str,
    wrap: &str,
    x: i64,
    y: i64,
    page: i64,
) -> RichTextAction {
    RichTextAction::SetImagePlacement {
        block: block.to_string(),
        anchor: anchor.to_string(),
        wrap: wrap.to_string(),
        x,
        y,
        page,
    }
}

/// Apply a named paragraph style (and, for a built-in, its structural kind) to
/// `blocks` in one action.
pub fn apply_named_style_action(
    region: RegionKind,
    blocks: Vec<String>,
    style: &str,
    kind: Option<&str>,
    level: Option<i64>,
    ordered: Option<bool>,
) -> RichTextAction {
    RichTextAction::ApplyNamedStyle {
        region,
        blocks,
        style: style.to_string(),
        kind: kind.map(std::string::ToString::to_string),
        level,
        ordered,
    }
}

/// Set the paragraph kind/level/ordered on `blocks`.
pub fn set_paragraph_style_action(
    region: RegionKind,
    blocks: Vec<String>,
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) -> RichTextAction {
    RichTextAction::SetParagraphStyle {
        region,
        blocks,
        kind: kind.to_string(),
        level,
        ordered,
    }
}

/// Insert a `rows`×`cols` table after `after`.
pub fn insert_table_action(after: &str, rows: usize, cols: usize) -> RichTextAction {
    RichTextAction::InsertTable {
        after: after.to_string(),
        rows,
        cols,
    }
}

/// Insert a table after `after` holding `cells`, row by row; the first row
/// is the header row.
pub fn insert_filled_table_action(after: &str, cells: Vec<Vec<String>>) -> RichTextAction {
    RichTextAction::InsertFilledTable {
        after: after.into(),
        cells,
    }
}

pub fn edit_table_action(table: &str, edit: TableEdit) -> RichTextAction {
    RichTextAction::EditTable {
        table: table.to_string(),
        edit,
    }
}

/// Insert a manual page break at `(block, at)`.
pub fn insert_page_break_action(block: &str, at: usize) -> RichTextAction {
    RichTextAction::InsertPageBreak {
        region: RegionKind::Body,
        block: block.to_string(),
        at,
    }
}

/// Insert a manual page break in the addressed editing region.
pub fn insert_page_break_in_action(region: RegionKind, block: &str, at: usize) -> RichTextAction {
    RichTextAction::InsertPageBreak {
        region,
        block: block.to_string(),
        at,
    }
}

pub fn insert_page_number_action(region: RegionKind, block: &str, at: usize) -> RichTextAction {
    RichTextAction::InsertField {
        region,
        block: block.to_string(),
        at,
        field: tilo_doc::schema::FIELD_PAGE_NUMBER.to_string(),
    }
}

pub fn generate_toc_action(entries: Vec<TocTarget>) -> RichTextAction {
    RichTextAction::GenerateToc { entries }
}

pub fn paste_fragment_action(
    region: RegionKind,
    selection: SelectionTarget,
    blocks: Vec<ClipBlock>,
    adopt_first_style: bool,
) -> RichTextAction {
    RichTextAction::PasteFragment {
        region,
        start_block: selection.start_block,
        start: selection.start,
        end_block: selection.end_block,
        end: selection.end,
        blocks,
        adopt_first_style,
    }
}

/// Split a body/header/footer block at one absolute character offset.
pub fn split_block_action(
    region: RegionKind,
    block: &str,
    at: usize,
    kind: &str,
    ordered: Option<bool>,
) -> RichTextAction {
    RichTextAction::SplitBlock {
        region,
        block: block.to_string(),
        at,
        kind: kind.to_string(),
        ordered,
        next: None,
    }
}

/// An image already in the caller's content store, as an [`RichTextAction::InsertImage`] refers to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInput {
    /// Content address of the image's bytes.
    pub content: String,
    pub mime: String,
    pub len: u64,
    /// Intrinsic pixel size, when the format states one.
    pub width: Option<i64>,
    pub height: Option<i64>,
}

impl ImageInput {
    /// Describe `bytes` stored under `content`.
    pub fn of(content: String, bytes: &[u8]) -> Self {
        let meta = tilo_content::sniff(bytes);
        Self {
            content,
            mime: meta.mime,
            len: meta.len,
            width: (meta.w > 0).then(|| i64::from(meta.w)),
            height: (meta.h > 0).then(|| i64::from(meta.h)),
        }
    }
}

/// Insert the image `image` at `(block, at)`.
pub fn insert_image_action(block: &str, at: usize, image: ImageInput) -> RichTextAction {
    RichTextAction::InsertImage {
        block: block.to_string(),
        at,
        content: image.content,
        mime: image.mime,
        len: image.len,
        width: image.width,
        height: image.height,
    }
}

/// Append `text` as a new paragraph at the end of the body (resolved at apply).
pub fn append_paragraph_action(text: &str) -> RichTextAction {
    RichTextAction::AppendParagraph {
        text: text.to_string(),
    }
}

/// Append `text` to the end of the last body paragraph (resolved at apply).
pub fn continue_paragraph_action(text: &str) -> RichTextAction {
    RichTextAction::ContinueParagraph {
        text: text.to_string(),
    }
}

/// Replace the whole document body with `markdown`.
pub fn replace_body_action(markdown: &str) -> RichTextAction {
    RichTextAction::ReplaceBody {
        markdown: markdown.to_string(),
    }
}

/// Replace `[start, end)` of `block` with `text` (Find & Replace).
pub fn splice_text_action(
    region: RegionKind,
    block: &str,
    start: usize,
    end: usize,
    text: &str,
) -> RichTextAction {
    RichTextAction::SpliceText {
        region,
        block: block.to_string(),
        at: start,
        del: end.saturating_sub(start),
        ins: text.to_string(),
    }
}

/// Replace a selection that may span several blocks with one canonical action.
pub fn replace_selection_action(
    region: RegionKind,
    start_block: &str,
    start: usize,
    end_block: &str,
    end: usize,
    text: &str,
) -> RichTextAction {
    RichTextAction::ReplaceSelection {
        region,
        start_block: start_block.to_string(),
        start,
        end_block: end_block.to_string(),
        end,
        ins: text.to_string(),
    }
}

/// Add a root comment over one resolved selection range.
pub fn add_comment_action(
    block: &str,
    start: usize,
    end: usize,
    author: &str,
    date: &str,
    text: &str,
) -> RichTextAction {
    RichTextAction::AddComment {
        block: block.to_string(),
        start,
        end,
        author: author.to_string(),
        date: date.to_string(),
        text: text.to_string(),
    }
}

/// A clipped block attribute as the action vocabulary writes it.
pub(crate) fn clip_attr(value: &crate::ClipAttr) -> AttrVal {
    match value {
        crate::ClipAttr::Bool(value) => AttrVal::Bool(*value),
        crate::ClipAttr::Int(value) => AttrVal::Int(*value),
        crate::ClipAttr::Str(value) => AttrVal::Str(value.clone()),
    }
}

/// Write a reply as its paragraphs: each block's kind, text, attributes and
/// marks, in order.
fn apply_reply_body(
    dm: &mut DocMut<'_>,
    comment: &str,
    author: &str,
    date: &str,
    body: &[ClipBlock],
) {
    let Some(reply) = dm.insert_comment_reply(comment, author, date) else {
        return;
    };
    let mut previous: Option<String> = None;
    for block in body {
        let Some(id) =
            dm.insert_comment_reply_block(comment, &reply, previous.as_deref(), &block.kind)
        else {
            return;
        };
        if let Some(level) = block.level {
            dm.set_comment_reply_block_attr(comment, &reply, &id, "level", level);
        }
        if let Some(ordered) = block.ordered {
            dm.set_comment_reply_block_attr(comment, &reply, &id, "ordered", ordered);
        }
        if let Some(align) = &block.align {
            dm.set_comment_reply_block_attr(comment, &reply, &id, "align", align.as_str());
        }
        for (key, value) in &block.attrs {
            let value = match value {
                crate::ClipAttr::Bool(value) => automerge::ScalarValue::Boolean(*value),
                crate::ClipAttr::Int(value) => automerge::ScalarValue::Int(*value),
                crate::ClipAttr::Str(value) => automerge::ScalarValue::Str(value.as_str().into()),
            };
            dm.set_comment_reply_block_attr(comment, &reply, &id, key, value);
        }
        dm.splice_comment_reply_text(comment, &reply, &id, 0, 0, &block.text);
        for mark in &block.marks {
            dm.add_comment_reply_mark(
                comment,
                &reply,
                &id,
                mark.start,
                mark.end,
                &mark.name,
                match &mark.value {
                    tilo_doc::document::MarkValue::Bool(value) => {
                        automerge::ScalarValue::Boolean(*value)
                    }
                    tilo_doc::document::MarkValue::Int(value) => {
                        automerge::ScalarValue::Int(*value)
                    }
                    tilo_doc::document::MarkValue::Str(value) => {
                        automerge::ScalarValue::Str(value.as_str().into())
                    }
                },
            );
        }
        previous = Some(id);
    }
}
