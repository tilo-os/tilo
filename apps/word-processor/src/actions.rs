//! The word processor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation — a text splice, a formatting mark, a
//! paragraph attribute, a structural insert/delete — is a [`WordProcessorAction`]:
//! a small serializable value that deterministically mutates the shared `qdoc`
//! document. The same [`EncodedAction`] is what toolbar/panel/menu affordances
//! dispatch and what Ctrl+K dispatches — one source of truth, one replay path.
//! Presentation (caret, selection, scroll/zoom, open panel) stays out of the
//! document and never becomes an action.
//!
//! **Determinism contract**: [`Action::apply`] never reads the wall-clock or the
//! RNG. New block ids are minted *inside* `apply` from a deterministic
//! [`seeded_id_gen`](qdoc::model::seeded_id_gen); sets of blocks/spans are sorted
//! before iterating, so a replay emits the identical op sequence. A decoded
//! action with stale or hostile arguments is atomically refused before it can
//! write, so callers can surface a useful error without leaving a partial edit.

pub(crate) mod continuation;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ReadDoc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use qdoc::model::Region;
use qdoc::{Containers, DocMut};

use crate::num;

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec,
};

/// Registry `type_id` for every word-processor action.
pub const WORD_PROCESSOR_ACTION_TYPE: &str = "word-processor.action";
pub const WORD_PROCESSOR_OPERATION_TYPE: &str = "word-processor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordProcessorOperation {
    ResolveFileChoice {
        generation: u64,
        use_selected: bool,
    },
    Open {
        path: Option<String>,
    },
    Save {
        path: Option<String>,
    },
    SaveAs {
        path: Option<String>,
    },
    Find,
    ZoomIn,
    ZoomOut,
    /// Return the page canvas to its explicit 100% scale. This matches the
    /// percentage chip in the visible zoom stepper and Ctrl+0.
    ZoomReset,
    ToggleSpellcheck,
    ToggleMarkup,
    ToggleStyles,
    ToggleRuler,
    ToggleOutline,
    ToggleHistory,
    /// Focus one exact body range. Used by bounded search-hit/comment resources
    /// instead of an index into the renderer's transient cache.
    NavigateTo {
        block: String,
        start: usize,
        end: usize,
    },
    /// Open history focused on a stable change-graph position.
    ShowRevision {
        index: usize,
    },
}

/// Decode an application-owned word-processor operation payload.
///
/// # Errors
///
/// Returns a message when a payload bearing
/// [`WORD_PROCESSOR_OPERATION_TYPE`] is not valid JSON for
/// [`WordProcessorOperation`].
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<WordProcessorOperation>, String> {
    if type_id != WORD_PROCESSOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    value: &WordProcessorOperation,
) -> Option<ActionSpec> {
    let Ok(spec) = ActionSpec::operation(id, label, kind, WORD_PROCESSOR_OPERATION_TYPE, value)
    else {
        // A malformed operation must not become an invokable catalogue
        // entry. These variants contain only JSON-native values, but keep the
        // fallible serializer boundary fail-closed rather than panicking.
        return None;
    };
    match value {
        WordProcessorOperation::ResolveFileChoice { use_selected, .. } => Some(
            spec.with_contract(if *use_selected {
                ActionContract::default()
            } else {
                ActionContract::view()
            })
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        ),
        WordProcessorOperation::Open { .. }
        | WordProcessorOperation::Save { .. }
        | WordProcessorOperation::SaveAs { .. } => Some(
            spec.destructive()
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
        ),
        _ => Some(spec.with_contract(ActionContract::view())),
    }
}

/// Which page region a text/formatting op addresses. Mirrors [`qdoc::model::Region`]
/// (which is not itself serializable) so the region travels in the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkVal {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// A serializable block-attribute value. Maps onto the scalar an attr stores
/// (`align`/`style`/`tab_stops` are `Str`, `level`/image dims are `Int`,
/// `indent`/`line_spacing` are `Float`, `page_break_before`/flips are `Bool`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrVal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// One `(block, [start, end))` range a formatting mark covers. A selection that
/// spans several blocks is carried as one span per touched block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub block: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocTarget {
    pub heading: String,
    pub level: i64,
    pub text: String,
    pub page: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableEdit {
    InsertRow { at: usize },
    InsertColumn { at: usize },
    DeleteRow { row: usize },
    DeleteColumn { column: usize },
    Delete,
}

/// Every collaborative, document-mutating operation the word processor exports.
///
/// Deliberately positional: each variant names the exact block ids and offsets
/// it touches, so [`Action::apply`] is a pure, replayable write. Ordinary typing,
/// deletion, paragraph splitting, Find & Replace, and agent-authored edits all
/// enter through these same envelopes; caret and selection updates remain local.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WordProcessorAction {
    /// Replace `[at, at+del)` of `block` with `ins` (insert = `del` 0; delete =
    /// `ins` empty). The deterministic text primitive.
    SpliceText {
        region: RegionKind,
        block: String,
        at: usize,
        del: usize,
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
    /// `width`/`height`/`flip_*`) to `value` on each of `blocks`.
    SetBlockAttr {
        region: RegionKind,
        blocks: Vec<String>,
        key: String,
        value: AttrVal,
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
    /// Mutate one stable table id using an absolute row/column boundary.
    EditTable { table: String, edit: TableEdit },
    /// Insert an image (blob `bytes`, optional intrinsic `width`/`height`) at
    /// `(block, at)`: the caret block is split so the tail follows the image in a
    /// fresh paragraph beneath it.
    InsertImage {
        block: String,
        at: usize,
        bytes: Vec<u8>,
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
        blocks: Vec<crate::word_processor::ClipBlock>,
        adopt_first_style: bool,
    },
    /// Insert a fresh paragraph carrying `text` directly after block `after`.
    /// The action-level way to add a line: Enter is a live-edit path, not an
    /// action, so a headless participant (an agent, a script) had no way to
    /// append a paragraph with the same ops the app produces.
    InsertParagraph { after: String, text: String },
    /// Remove `blocks` (an image, or every cell of a table). Ids are sorted
    /// before removal so replicas emit the identical op sequence.
    RemoveBlocks { blocks: Vec<String> },
    /// Replace the whole document body with `markdown` (re-import a template /
    /// restore a version): every existing body block is removed, then the
    /// Markdown is imported as fresh blocks.
    ReplaceBody { markdown: String },
    /// Accept one tracked change, or every change when `id` is absent.
    AcceptChanges { id: Option<String> },
    /// Reject one tracked change, or every change when `id` is absent.
    RejectChanges { id: Option<String> },
    /// Resolve a comment by removing both its anchor mark and record.
    ResolveComment { id: String },
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
    /// stable resources; stale/deleted parents refuse atomically.
    ReplyToComment {
        comment: String,
        author: String,
        date: String,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionDomain {
    Text,
    Formatting,
    Media,
    Table,
    Structure,
    Review,
}

impl WordProcessorAction {
    fn domain(&self) -> ActionDomain {
        match self {
            Self::SpliceText { .. }
            | Self::ReplaceSelection { .. }
            | Self::SetMark { .. }
            | Self::PasteFragment { .. }
            | Self::InsertParagraph { .. }
            | Self::RemoveBlocks { .. }
            | Self::ReplaceBody { .. } => ActionDomain::Text,
            Self::SetParagraphStyle { .. }
            | Self::ApplyNamedStyle { .. }
            | Self::SetBlockAttr { .. }
            | Self::SetPageMargins { .. }
            | Self::SetStyleProperty { .. }
            | Self::DeleteStyle { .. } => ActionDomain::Formatting,
            Self::SetImageCrop { .. }
            | Self::SetImageSize { .. }
            | Self::SetImagePlacement { .. }
            | Self::InsertImage { .. } => ActionDomain::Media,
            Self::InsertTable { .. } | Self::EditTable { .. } => ActionDomain::Table,
            Self::InsertPageBreak { .. }
            | Self::SplitBlock { .. }
            | Self::InsertField { .. }
            | Self::GenerateToc { .. } => ActionDomain::Structure,
            Self::AcceptChanges { .. }
            | Self::RejectChanges { .. }
            | Self::ResolveComment { .. }
            | Self::AddComment { .. }
            | Self::ReplyToComment { .. } => ActionDomain::Review,
        }
    }
}

// Action payloads may arrive after the document has changed (or from an
// untrusted automation).  Keep the limits deliberately generous for real
// documents while making the action boundary a useful resource boundary too.
const MAX_ACTION_TEXT_CHARS: usize = 1_000_000;
const MAX_ACTION_BLOB_BYTES: usize = 64 * 1024 * 1024;
const MAX_TABLE_DIMENSION: usize = 100;
const MAX_TABLE_CELLS: usize = 10_000;

/// Read only the addressed block while matching qdoc's snapshot membership:
/// a region block must be a map with a byte-valued order key. Missing or
/// non-Text text projects to the empty string, just as `materialize_block` does.
fn splice_target_len(
    doc: &AutoCommit,
    c: &Containers,
    region: RegionKind,
    id: &str,
) -> Option<usize> {
    use automerge::{ScalarValue, Value};
    use qdoc::schema::BLOCK_ORDER;

    // qdoc's own resolution, so a header/footer block that lives in a
    // concurrently created legacy container validates like it writes.
    let block = qdoc::model::region_block_obj(doc, c, region.to_region(), id)?;
    match doc.get(&block, BLOCK_ORDER) {
        Ok(Some((Value::Scalar(value), _))) if matches!(value.as_ref(), ScalarValue::Bytes(_)) => {}
        _ => return None,
    }
    // Same projection as `materialize_block` (including a legacy scalar or
    // conflicted `text` value), so validation and the write agree on offsets.
    let text = qdoc::model::block_text_on(doc, &block).unwrap_or_default();
    Some(text.chars().count())
}

/// Validate an action against one immutable point-in-time view of `doc`.
///
/// This is intentionally separate from the write helpers: an invalid action
/// must make **no mutation**, never a partly-applied collection of valid
/// targets followed by a stale one. It also bounds hostile payloads before an
/// action allocates a large table/blob or asks automerge to index impossible
/// offsets.  The caller maps `false` to an explicit [`ActionError::Refused`],
/// preserving a useful stale-target result without leaving a partial edit.
fn action_is_valid(action: &WordProcessorAction, doc: &AutoCommit, c: &Containers) -> bool {
    // Typing addresses a single block. Avoid materializing unrelated text,
    // marks, images and regions on every keystroke; retain the same bounds.
    if let WordProcessorAction::SpliceText {
        region,
        block,
        at,
        del,
        ins,
    } = action
    {
        return splice_target_len(doc, c, *region, block)
            .is_some_and(|len| *at <= len && *del <= len.saturating_sub(*at))
            && ins.chars().take(MAX_ACTION_TEXT_CHARS + 1).count() <= MAX_ACTION_TEXT_CHARS;
    }
    ActionValidator::new(doc, c).validate(action)
}

struct ActionValidator<'a> {
    doc: &'a AutoCommit,
    containers: &'a Containers,
    snapshot: qdoc::snapshot::DocSnapshot,
}

impl<'a> ActionValidator<'a> {
    fn new(doc: &'a AutoCommit, containers: &'a Containers) -> Self {
        Self {
            doc,
            containers,
            snapshot: qdoc::model::materialize(doc, containers),
        }
    }

    fn blocks_for(&self, region: RegionKind) -> &[qdoc::snapshot::BlockView] {
        match region {
            RegionKind::Body => &self.snapshot.blocks,
            RegionKind::Header => &self.snapshot.header,
            RegionKind::Footer => &self.snapshot.footer,
        }
    }

    fn block_len(&self, region: RegionKind, id: &str) -> Option<usize> {
        self.blocks_for(region)
            .iter()
            .find(|block| block.id == id)
            .map(|block| block.text.chars().count())
    }

    fn unique_existing(&self, region: RegionKind, ids: &[String]) -> bool {
        !ids.is_empty()
            && ids.iter().all(|id| self.block_len(region, id).is_some())
            && ids.iter().collect::<HashSet<_>>().len() == ids.len()
    }

    fn safe_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }

    fn safe_text(text: &str) -> bool {
        text.chars().count() <= MAX_ACTION_TEXT_CHARS
    }

    fn valid_attr(key: &str, value: &AttrVal) -> bool {
        match key {
            "align" => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "left" | "center" | "right" | "justify"))
            }
            "page_break_before" | "flip_h" | "flip_v" => matches!(value, AttrVal::Bool(_)),
            "indent" | "line_spacing" => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && *v >= 0.0 && *v <= 10_000.0)
            }
            "first_line_indent" => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (-10_000.0..=10_000.0).contains(v))
            }
            "tab_stops" | "style" => matches!(value, AttrVal::Str(v) if v.len() <= 4_096),
            "width" | "height" => matches!(value, AttrVal::Int(v) if *v > 0 && *v <= 100_000),
            _ => false,
        }
    }

    fn valid_style_property(key: &str, value: &AttrVal) -> bool {
        match key {
            qdoc::schema::STYLE_NAME => {
                matches!(value, AttrVal::Str(v) if v.chars().count() <= 256)
            }
            qdoc::schema::STYLE_FONT => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "sans" | "serif" | "mono"))
            }
            qdoc::schema::STYLE_SIZE => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (6.0..=200.0).contains(v))
            }
            qdoc::schema::STYLE_BOLD | qdoc::schema::STYLE_ITALIC => {
                matches!(value, AttrVal::Bool(_))
            }
            qdoc::schema::STYLE_COLOR => {
                matches!(value, AttrVal::Str(v) if v.len() == 7 && v.starts_with('#'))
            }
            qdoc::schema::STYLE_ALIGN => {
                matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "left" | "center" | "right" | "justify"))
            }
            qdoc::schema::STYLE_SPACE_BEFORE
            | qdoc::schema::STYLE_SPACE_AFTER
            | qdoc::schema::STYLE_INDENT => {
                matches!(value, AttrVal::Float(v) if v.is_finite() && (0.0..=10_000.0).contains(v))
            }
            _ => false,
        }
    }

    fn validate(&self, action: &WordProcessorAction) -> bool {
        match action.domain() {
            ActionDomain::Text => {
                self.validate_text_edit(action) || self.validate_paste_and_body(action)
            }
            ActionDomain::Formatting => self.validate_formatting(action),
            ActionDomain::Media => self.validate_media(action),
            ActionDomain::Table => self.validate_tables(action),
            ActionDomain::Structure => self.validate_structure(action),
            ActionDomain::Review => self.validate_review(action),
        }
    }

    fn validate_text_edit(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::SpliceText {
                region,
                block,
                at,
                del,
                ins,
            } => {
                self.block_len(*region, block)
                    .is_some_and(|len| *at <= len && *del <= len.saturating_sub(*at))
                    && Self::safe_text(ins)
            }
            WordProcessorAction::ReplaceSelection {
                region,
                start_block,
                start,
                end_block,
                end,
                ins,
            } => {
                let blocks = self.blocks_for(*region);
                let start_index = blocks.iter().position(|block| block.id == *start_block);
                let end_index = blocks.iter().position(|block| block.id == *end_block);
                match (
                    start_index,
                    end_index,
                    self.block_len(*region, start_block),
                    self.block_len(*region, end_block),
                ) {
                    (Some(start_index), Some(end_index), Some(start_len), Some(end_len)) => {
                        (start_index < end_index || (start_index == end_index && start <= end))
                            && *start <= start_len
                            && *end <= end_len
                            && Self::safe_text(ins)
                    }
                    _ => false,
                }
            }
            WordProcessorAction::SetMark {
                region,
                spans,
                name,
                value,
                also_clear,
            } => {
                Self::safe_name(name)
                    && also_clear.iter().all(|sibling| Self::safe_name(sibling))
                    && spans.len() <= 10_000
                    && !spans.is_empty()
                    && spans.iter().all(|span| {
                        self.block_len(*region, &span.block)
                            .is_some_and(|len| span.start < span.end && span.end <= len)
                    })
                    && match (name.as_str(), value) {
                        ("link", Some(MarkVal::Str(target))) => valid_hyperlink_target(target),
                        ("link", Some(_)) => false,
                        (_, Some(MarkVal::Str(text))) => Self::safe_text(text),
                        _ => true,
                    }
            }
            _ => false,
        }
    }

    fn validate_paste_and_body(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::PasteFragment {
                region,
                start_block,
                start,
                end_block,
                end,
                blocks,
                ..
            } => {
                let region_blocks = self.blocks_for(*region);
                let start_index = region_blocks
                    .iter()
                    .position(|block| block.id == *start_block);
                let end_index = region_blocks
                    .iter()
                    .position(|block| block.id == *end_block);
                let valid_range = match (
                    start_index,
                    end_index,
                    self.block_len(*region, start_block),
                    self.block_len(*region, end_block),
                ) {
                    (Some(si), Some(ei), Some(sl), Some(el)) => {
                        (si < ei || (si == ei && start <= end)) && *start <= sl && *end <= el
                    }
                    _ => false,
                };
                valid_range
                    && !blocks.is_empty()
                    && blocks.len() <= 10_000
                    && blocks.iter().all(|block| {
                        Self::safe_text(&block.text)
                            && Self::safe_name(&block.kind)
                            && block.level.is_none_or(|level| (0..=9).contains(&level))
                            && block.marks.iter().all(|mark| {
                                Self::safe_name(&mark.name)
                                    && mark.start < mark.end
                                    && mark.end <= block.text.chars().count()
                            })
                    })
            }
            WordProcessorAction::InsertParagraph { after, text } => {
                self.block_len(RegionKind::Body, after).is_some() && Self::safe_text(text)
            }
            WordProcessorAction::RemoveBlocks { blocks } => {
                self.unique_existing(RegionKind::Body, blocks)
            }
            WordProcessorAction::ReplaceBody { markdown } => Self::safe_text(markdown),
            _ => false,
        }
    }

    fn validate_formatting(&self, action: &WordProcessorAction) -> bool {
        match action {
        WordProcessorAction::SetParagraphStyle {
            region,
            blocks,
            kind,
            level,
            ..
        } => {
            self.unique_existing(*region, blocks)
                && Self::safe_name(kind)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        WordProcessorAction::ApplyNamedStyle {
            region,
            blocks,
            style,
            kind,
            level,
            ..
        } => {
            self.unique_existing(*region, blocks)
                && !style.is_empty()
                && style.len() <= 128
                && kind.as_deref().is_none_or(Self::safe_name)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        WordProcessorAction::SetBlockAttr {
            region,
            blocks,
            key,
            value,
        } => self.unique_existing(*region, blocks) && Self::valid_attr(key, value),
        WordProcessorAction::SetPageMargins { top, right, bottom, left } => [top, right, bottom, left]
            .into_iter()
            .all(|value| value.is_finite() && (4.0..=10_000.0).contains(value)),
        WordProcessorAction::SetStyleProperty { style, key, value } => {
            Self::safe_name(style)
                && !qdoc::model::style_is_tombstoned(self.doc, self.containers, style)
                && Self::valid_style_property(key, value)
        }
        WordProcessorAction::DeleteStyle {
            style,
            fallback_blocks,
        } => {
            Self::safe_name(style)
                && !crate::styles::StyleSheet::is_builtin(style)
                && qdoc::model::style_ids(self.doc, self.containers).contains(style)
                && fallback_blocks.iter().all(|block| {
                    self.snapshot.blocks.iter().any(|entry| {
                        entry.id == *block
                            && matches!(entry.attrs.get("style"), Some(qdoc::AttrVal::Str(id)) if id == style)
                    })
                })
        }
            _ => false,
        }
    }

    fn validate_media(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::SetImageCrop {
                block,
                left,
                top,
                right,
                bottom,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                && self.snapshot.blocks.iter().any(|b| b.id == *block && b.kind == "image")
                && [left, top, right, bottom]
                    .iter()
                    .all(|edge| (0_i64..=9_500).contains(*edge))
                // Always retain at least five percent of the source in both
                // dimensions. This prevents a hostile crop from creating an
                // infinite scale factor in the renderer.
                && (*left).saturating_add(*right) <= 9_500
                && (*top).saturating_add(*bottom) <= 9_500
            }
            WordProcessorAction::SetImageSize {
                block,
                width,
                height,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                    && self
                        .snapshot
                        .blocks
                        .iter()
                        .any(|b| b.id == *block && b.kind == "image")
                    && (16..=100_000).contains(width)
                    && (16..=100_000).contains(height)
            }
            WordProcessorAction::SetImagePlacement {
                block,
                anchor,
                wrap,
                x,
                y,
                page,
            } => {
                self.block_len(RegionKind::Body, block).is_some()
                    && self
                        .snapshot
                        .blocks
                        .iter()
                        .any(|b| b.id == *block && b.kind == "image")
                    && matches!(anchor.as_str(), "inline" | "paragraph" | "page")
                    && matches!(wrap.as_str(), "none" | "left" | "right" | "both")
                    && (-100_000..=100_000).contains(x)
                    && (-100_000..=100_000).contains(y)
                    && (0..=10_000).contains(page)
            }
            WordProcessorAction::InsertImage {
                block,
                at,
                bytes,
                width,
                height,
            } => {
                self.block_len(RegionKind::Body, block)
                    .is_some_and(|len| *at <= len)
                    && !bytes.is_empty()
                    && bytes.len() <= MAX_ACTION_BLOB_BYTES
                    && width.is_none_or(|value| (1..=100_000).contains(&value))
                    && height.is_none_or(|value| (1..=100_000).contains(&value))
            }
            _ => false,
        }
    }

    fn validate_tables(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::InsertTable { after, rows, cols } => {
                self.block_len(RegionKind::Body, after).is_some()
                    && *rows > 0
                    && *cols > 0
                    && *rows <= MAX_TABLE_DIMENSION
                    && *cols <= MAX_TABLE_DIMENSION
                    && rows.saturating_mul(*cols) <= MAX_TABLE_CELLS
            }
            WordProcessorAction::EditTable { table, edit } => {
                let cells: Vec<_> = self
                    .snapshot
                    .blocks
                    .iter()
                    .filter(|block| {
                        block.kind == "cell"
                            && matches!(block.attrs.get("table"), Some(qdoc::AttrVal::Str(id)) if id == table)
                    })
                    .collect();
                let coord =
                    |block: &qdoc::snapshot::BlockView, key: &str| match block.attrs.get(key) {
                        Some(qdoc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
                        _ => 0,
                    };
                let rows = cells
                    .iter()
                    .map(|block| coord(block, "row"))
                    .max()
                    .map_or(0, |value| value.saturating_add(1));
                let cols = cells
                    .iter()
                    .map(|block| coord(block, "col"))
                    .max()
                    .map_or(0, |value| value.saturating_add(1));
                !table.is_empty()
                    && !cells.is_empty()
                    && cells.len() == rows.saturating_mul(cols)
                    && match edit {
                        TableEdit::InsertRow { at } => *at <= rows && rows < MAX_TABLE_DIMENSION,
                        TableEdit::InsertColumn { at } => *at <= cols && cols < MAX_TABLE_DIMENSION,
                        TableEdit::DeleteRow { row } => *row < rows,
                        TableEdit::DeleteColumn { column } => *column < cols,
                        TableEdit::Delete => true,
                    }
            }
            _ => false,
        }
    }

    fn validate_structure(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::InsertPageBreak { region, block, at } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
            }
            WordProcessorAction::SplitBlock {
                region,
                block,
                at,
                kind,
                ..
            } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
                    && matches!(kind.as_str(), "paragraph" | "list_item")
            }
            WordProcessorAction::InsertField {
                region,
                block,
                at,
                field,
            } => {
                self.block_len(*region, block).is_some_and(|len| *at <= len)
                    && field == qdoc::schema::FIELD_PAGE_NUMBER
            }
            WordProcessorAction::GenerateToc { entries } => {
                !entries.is_empty()
                    && entries.len() <= 10_000
                    && entries
                        .iter()
                        .map(|entry| entry.heading.as_str())
                        .collect::<HashSet<_>>()
                        .len()
                        == entries.len()
                    && entries.iter().all(|entry| {
                        (1..=9).contains(&entry.level)
                            && Self::safe_text(&entry.text)
                            && entry.page.is_none_or(|page| page <= 100_000)
                            && self
                                .snapshot
                                .blocks
                                .iter()
                                .any(|block| block.id == entry.heading && block.kind == "heading")
                    })
            }
            _ => false,
        }
    }

    fn validate_review(&self, action: &WordProcessorAction) -> bool {
        match action {
            WordProcessorAction::AcceptChanges { id }
            | WordProcessorAction::RejectChanges { id } => id
                .as_ref()
                .is_none_or(|id| self.snapshot.changes.iter().any(|change| &change.id == id)),
            WordProcessorAction::ResolveComment { id } => {
                !id.is_empty()
                    && self
                        .snapshot
                        .comments
                        .iter()
                        .any(|comment| comment.id == *id)
            }
            WordProcessorAction::AddComment {
                block,
                start,
                end,
                author,
                date,
                text,
            } => {
                self.block_len(RegionKind::Body, block)
                    .is_some_and(|len| *start < *end && *end <= len)
                    && !author.trim().is_empty()
                    && author.chars().count() <= 256
                    && date.chars().count() <= 128
                    && Self::safe_text(text)
            }
            WordProcessorAction::ReplyToComment {
                comment,
                author,
                date,
                text,
            } => {
                self.snapshot
                    .comments
                    .iter()
                    .any(|entry| entry.id == *comment)
                    && !author.trim().is_empty()
                    && author.chars().count() <= 256
                    && date.chars().count() <= 128
                    && Self::safe_text(text)
            }
            _ => false,
        }
    }
}

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

fn remove_mark_region(dm: &mut DocMut, c: &Containers, region: Region, span: &Span, name: &str) {
    if region == Region::Body {
        dm.remove_mark(&span.block, span.start, span.end, name);
        return;
    }
    // Header/Footer: no `remove_mark_in` op — clear the mark directly on the
    // region block's Text object (mirrors the body `remove_mark`'s
    // `unmark(.., ExpandMark::After)`).
    if let Some(obj) = qdoc::model::region_block_obj(dm.doc, c, region, &span.block)
        && let Some(tid) = qdoc::model::ensure_block_text(dm.doc, &obj)
    {
        let _ = dm.doc.unmark(
            &tid,
            name,
            span.start,
            span.end,
            automerge::marks::ExpandMark::After,
        );
    }
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
    if region == Region::Body {
        dm.set_kind(block, kind);
    } else if let Some(block_obj) = qdoc::model::region_block_obj(dm.doc, dm.c, region, block) {
        let _ = dm.doc.put(&block_obj, qdoc::schema::BLOCK_KIND, kind);
    }
}

// ---------------------------------------------------------------------------
// Per-variant `apply` bodies. Split out of `Action::apply` below (one match
// arm each) so that single trait method stays short; kept as free functions
// (not methods on `WordProcessorAction`) because they live outside the trait
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
            num::isize_of_usize(end - start),
            ins,
        );
        return;
    }
    // Validation has already established both block positions/offsets against
    // one snapshot, so this body cannot partly merge one valid end-point and
    // then discover a stale second end-point.
    let blocks = match region {
        Region::Body => qdoc::model::blocks_in_order(dm.doc, dm.c),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
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
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, end_block),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .find(|block| block.id == end_block)
            .map(|block| block.text),
    }
    .unwrap_or_default();
    let tail: String = end_text.chars().skip(end).collect();
    let start_len = match region {
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, start_block),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
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
        dm.splice_text_in(region, start_block, start + ins.chars().count(), 0, &tail);
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
    let c = dm.c;
    let mut sorted = spans.to_vec();
    sorted.sort_by(|a, b| {
        (a.block.as_str(), a.start, a.end).cmp(&(b.block.as_str(), b.start, b.end))
    });
    for span in &sorted {
        if span.start >= span.end {
            continue;
        }
        for sib in also_clear {
            remove_mark_region(dm, c, region, span, sib);
        }
        match value {
            None => remove_mark_region(dm, c, region, span, name),
            Some(v) => {
                // Replace: drop any prior value of the same mark before
                // re-adding so values overwrite, not stack.
                remove_mark_region(dm, c, region, span, name);
                add_mark_region(dm, region, span, name, v);
            }
        }
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
        set_kind_region(dm, region, block, kind);
        if let Some(l) = level {
            set_attr_region(dm, region, block, "level", &AttrVal::Int(l));
        }
        if let Some(o) = ordered {
            set_attr_region(dm, region, block, "ordered", &AttrVal::Bool(o));
        }
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
            set_kind_region(dm, region, block, k);
            if let Some(l) = level {
                set_attr_region(dm, region, block, "level", &AttrVal::Int(l));
            }
            if let Some(o) = ordered {
                set_attr_region(dm, region, block, "ordered", &AttrVal::Bool(o));
            }
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

fn apply_insert_table(dm: &mut DocMut, after: &str, rows: usize, cols: usize) {
    let table_id = dm.ids.mint(); // one id shared by every cell
    let mut prev = Some(after.to_string());
    for row in 0..rows {
        for col in 0..cols {
            let id = dm.insert_block(prev.as_deref(), "cell");
            dm.set_attr(&id, "table", table_id.as_str());
            dm.set_attr(&id, "row", num::i64_of_usize(row));
            dm.set_attr(&id, "col", num::i64_of_usize(col));
            prev = Some(id);
        }
    }
}

type TableCells = Vec<(String, usize, usize)>;

fn table_cells(dm: &DocMut<'_>, table: &str) -> TableCells {
    qdoc::model::materialize(dm.doc, dm.c)
        .blocks
        .into_iter()
        .filter(|block| {
            block.kind == "cell"
                && matches!(block.attrs.get("table"), Some(qdoc::AttrVal::Str(id)) if id == table)
        })
        .map(|block| {
            let coordinate = |key: &str| match block.attrs.get(key) {
                Some(qdoc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
                _ => 0,
            };
            (block.id, coordinate("row"), coordinate("col"))
        })
        .collect()
}

fn table_head_anchor(dm: &DocMut<'_>, cells: &TableCells) -> Option<String> {
    let order = qdoc::model::blocks_in_order(dm.doc, dm.c);
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

fn apply_insert_image(
    dm: &mut DocMut,
    block: &str,
    at: usize,
    bytes: &[u8],
    width: Option<i64>,
    height: Option<i64>,
) {
    let text = qdoc::model::block_text(dm.doc, dm.c, block).unwrap_or_default();
    let char_count = text.chars().count();
    let tail: String = text.chars().skip(at).collect();
    // Refuse the whole insertion if the bounded blob store cannot retain the
    // bytes; splitting text and creating a `blob` reference would otherwise
    // leave a document pointing at content this replica never stored.
    let Some(hash) = dm.put_blob(bytes) else {
        return;
    };
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
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, block),
        other => qdoc::model::materialize_region(dm.doc, dm.c, other)
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
) {
    let text = match region {
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, block),
        other => qdoc::model::materialize_region(dm.doc, dm.c, other)
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
    let new = dm.insert_block_in(region, Some(block), kind);
    if let Some(value) = ordered {
        dm.set_attr_in(region, &new, "ordered", value);
    }
    if !tail.is_empty() {
        dm.splice_text_in(region, &new, 0, 0, &tail);
    }
}

fn apply_generate_toc(dm: &mut DocMut, entries: &[TocTarget]) {
    let snapshot = qdoc::model::materialize(dm.doc, dm.c);
    for block in snapshot
        .blocks
        .iter()
        .filter(|block| matches!(block.attrs.get("toc"), Some(qdoc::AttrVal::Bool(true))))
    {
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
    blocks: &[crate::word_processor::ClipBlock],
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
    for (index, fragment) in blocks.iter().enumerate() {
        if index > 0 {
            let text = match region {
                Region::Body => qdoc::model::block_text(dm.doc, dm.c, &current),
                other => qdoc::model::materialize_region(dm.doc, dm.c, other)
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
                qdoc::document::MarkValue::Bool(value) => MarkVal::Bool(*value),
                qdoc::document::MarkValue::Int(value) => MarkVal::Int(*value),
                qdoc::document::MarkValue::Str(value) => MarkVal::Str(value.clone()),
            };
            add_mark_region(dm, region, &span, &mark.name, &value);
        }
        if index > 0 || adopt_first_style {
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
        }
        offset = offset.saturating_add(fragment.text.chars().count());
    }
}

fn apply_insert_paragraph(dm: &mut DocMut, after: &str, text: &str) {
    let new = dm.insert_block(Some(after), "paragraph");
    if !text.is_empty() {
        dm.splice_text(&new, 0, 0, text);
    }
}

fn apply_remove_blocks(dm: &mut DocMut, blocks: &[String]) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        dm.tombstone_block_in(Region::Body, block);
    }
}

fn apply_replace_body(dm: &mut DocMut, markdown: &str) {
    use qdoc::FormatProvider;
    let existing = qdoc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.tombstone_block_in(Region::Body, id);
    }
    // A malformed template can't wedge a replay: the body-clear stays in the
    // pinned change either way.
    let _ = qdoc::MarkdownCodec.import(markdown.as_bytes(), dm);
}

fn ordered_change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let order = qdoc::model::blocks_in_order(doc, c);
    let mut ids = qdoc::model::change_ids(doc, c);
    ids.sort_by_key(|id| {
        qdoc::model::change_range(doc, c, id).map_or_else(
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
    let range = qdoc::model::comment_range(dm.doc, dm.c, id);
    if let Some((block, start, end)) = range
        && start < end
    {
        dm.remove_mark(&block, start, end, qdoc::schema::MARK_COMMENT);
    }
    if let Some(comments) = dm.c.comments.as_ref() {
        // Thread replies are sibling records in the canonical comments map,
        // not dangling private overlay state. Remove them deterministically
        // before their parent so a resolved thread leaves no orphan resources.
        let mut replies = qdoc::model::comment_reply_ids(dm.doc, dm.c, id);
        replies.sort();
        for reply in replies {
            let _ = dm.doc.delete(comments, reply);
        }
        let _ = dm.doc.delete(comments, id);
    }
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

fn apply_text_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::SpliceText {
            region,
            block,
            at,
            del,
            ins,
        } => apply_splice_text(dm, region.to_region(), block, *at, *del, ins),
        WordProcessorAction::ReplaceSelection {
            region,
            start_block,
            start,
            end_block,
            end,
            ins,
        } => apply_replace_selection(
            dm,
            region.to_region(),
            start_block,
            *start,
            end_block,
            *end,
            ins,
        ),
        WordProcessorAction::SetMark {
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
        WordProcessorAction::PasteFragment {
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
        WordProcessorAction::InsertParagraph { after, text } => {
            apply_insert_paragraph(dm, after, text);
        }
        WordProcessorAction::RemoveBlocks { blocks } => apply_remove_blocks(dm, blocks),
        WordProcessorAction::ReplaceBody { markdown } => apply_replace_body(dm, markdown),
        _ => {}
    }
}

fn apply_formatting_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::SetParagraphStyle {
            region,
            blocks,
            kind,
            level,
            ordered,
        } => apply_set_paragraph_style(dm, region.to_region(), blocks, kind, *level, *ordered),
        WordProcessorAction::ApplyNamedStyle {
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
        WordProcessorAction::SetBlockAttr {
            region,
            blocks,
            key,
            value,
        } => apply_set_block_attr(dm, region.to_region(), blocks, key, value),
        WordProcessorAction::SetPageMargins {
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
        WordProcessorAction::SetStyleProperty { style, key, value } => {
            let value = match value {
                AttrVal::Bool(value) => qdoc::AttrVal::Bool(*value),
                AttrVal::Int(value) => qdoc::AttrVal::Int(*value),
                AttrVal::Float(value) => qdoc::AttrVal::Float(*value),
                AttrVal::Str(value) => qdoc::AttrVal::Str(value.clone()),
            };
            dm.set_style(style, &[(key.clone(), value)].into_iter().collect());
        }
        WordProcessorAction::DeleteStyle {
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

fn apply_media_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::SetImageCrop {
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
        WordProcessorAction::SetImageSize {
            block,
            width,
            height,
        } => {
            dm.set_attr(block, "width", *width);
            dm.set_attr(block, "height", *height);
        }
        WordProcessorAction::SetImagePlacement {
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
        WordProcessorAction::InsertImage {
            block,
            at,
            bytes,
            width,
            height,
        } => apply_insert_image(dm, block, *at, bytes, *width, *height),
        _ => {}
    }
}

fn apply_table_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::InsertTable { after, rows, cols } => {
            apply_insert_table(dm, after, *rows, *cols);
        }
        WordProcessorAction::EditTable { table, edit } => apply_table_edit(dm, table, *edit),
        _ => {}
    }
}

fn apply_structure_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::InsertPageBreak { region, block, at } => {
            apply_insert_page_break(dm, region.to_region(), block, *at);
        }
        WordProcessorAction::SplitBlock {
            region,
            block,
            at,
            kind,
            ordered,
        } => apply_split_block(dm, region.to_region(), block, *at, kind, *ordered),
        WordProcessorAction::InsertField {
            region,
            block,
            at,
            field: _,
        } => dm.insert_page_number_field(region.to_region(), block, *at),
        WordProcessorAction::GenerateToc { entries } => apply_generate_toc(dm, entries),
        _ => {}
    }
}

fn apply_review_action(dm: &mut DocMut<'_>, action: &WordProcessorAction) {
    match action {
        WordProcessorAction::AcceptChanges { id } => {
            apply_review_changes(dm, id.as_deref(), true);
        }
        WordProcessorAction::RejectChanges { id } => {
            apply_review_changes(dm, id.as_deref(), false);
        }
        WordProcessorAction::ResolveComment { id } => apply_resolve_comment(dm, id),
        WordProcessorAction::AddComment {
            block,
            start,
            end,
            author,
            date,
            text,
        } => apply_add_comment(dm, block, *start, *end, author, date, text),
        WordProcessorAction::ReplyToComment {
            comment,
            author,
            date,
            text,
        } => {
            let _ = dm.add_comment_reply(comment, author, date, text);
        }
        _ => {}
    }
}

impl Action for WordProcessorAction {
    /// Apply this action's mutation to `doc`.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Refused`] for an unavailable Writer document or
    /// stale/hostile decoded arguments before opening a mutable view.
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Bind to the canonical schema under ROOT. Refuse an unbound document
        // explicitly so callers never mistake an unavailable target for a
        // successful edit.
        let Ok(c) = qdoc::model::open(doc, &automerge::ROOT) else {
            return Err(ActionError::Refused(
                "Writer document is unavailable or has not been initialized".into(),
            ));
        };
        // Validate every target against this one, immutable document state
        // before creating the mutable editing view.  In particular, do not let
        // a stale multi-block format/delete partially affect still-existing
        // blocks when one peer has already removed another target.
        if !action_is_valid(self, doc, &c) {
            return Err(ActionError::Refused(
                "action targets are stale or its arguments are outside Writer's safe limits".into(),
            ));
        }
        // A generator seeded past every existing id, under the executor's pinned
        // actor — so any freshly-minted block id is `{action-actor}.{n}`,
        // byte-identical on origin and replay.
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);
        let mut dm = DocMut {
            doc,
            c: &c,
            ids: &mut ids,
        };

        match self.domain() {
            ActionDomain::Text => apply_text_action(&mut dm, self),
            ActionDomain::Formatting => apply_formatting_action(&mut dm, self),
            ActionDomain::Media => apply_media_action(&mut dm, self),
            ActionDomain::Table => apply_table_action(&mut dm, self),
            ActionDomain::Structure => apply_structure_action(&mut dm, self),
            ActionDomain::Review => apply_review_action(&mut dm, self),
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the word processor's action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<WordProcessorAction>(WORD_PROCESSOR_ACTION_TYPE);
}

/// Encode any [`WordProcessorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns an action
/// into a replayable envelope.
pub fn encode(action: &WordProcessorAction) -> EncodedAction {
    // The action is a plain serde enum over owned data, so the JSON encode
    // cannot fail. Were it ever to, an empty payload fails decoding at the far
    // end as an explicit action error instead of aborting the process.
    EncodedAction::encode(WORD_PROCESSOR_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: WORD_PROCESSOR_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
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
) -> WordProcessorAction {
    WordProcessorAction::SetMark {
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
pub fn set_hyperlink_action(
    region: RegionKind,
    spans: Vec<Span>,
    target: &str,
) -> WordProcessorAction {
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
) -> WordProcessorAction {
    WordProcessorAction::SetBlockAttr {
        region,
        blocks,
        key: key.to_string(),
        value,
    }
}

pub fn set_page_margins_action(margins: [f64; 4]) -> WordProcessorAction {
    WordProcessorAction::SetPageMargins {
        top: margins[0],
        right: margins[1],
        bottom: margins[2],
        left: margins[3],
    }
}

/// Set one field of a stable named style.
pub fn set_style_property_action(style: &str, key: &str, value: AttrVal) -> WordProcessorAction {
    WordProcessorAction::SetStyleProperty {
        style: style.to_string(),
        key: key.to_string(),
        value,
    }
}

/// Delete one custom style and reassign its exact current instances.
pub fn delete_style_action(style: &str, fallback_blocks: Vec<String>) -> WordProcessorAction {
    WordProcessorAction::DeleteStyle {
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
) -> WordProcessorAction {
    WordProcessorAction::SetImageCrop {
        block: block.to_string(),
        left,
        top,
        right,
        bottom,
    }
}

/// Resize an image frame in document pixels.
pub fn set_image_size_action(block: &str, width: i64, height: i64) -> WordProcessorAction {
    WordProcessorAction::SetImageSize {
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
) -> WordProcessorAction {
    WordProcessorAction::SetImagePlacement {
        block: block.to_string(),
        anchor: anchor.to_string(),
        wrap: wrap.to_string(),
        x,
        y,
        page,
    }
}

/// Set alignment (`left`/`center`/`right`/`justify`) on `blocks`.
pub fn set_align_action(blocks: Vec<String>, align: &str) -> WordProcessorAction {
    set_block_attr_action(
        RegionKind::Body,
        blocks,
        "align",
        AttrVal::Str(align.to_string()),
    )
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
) -> WordProcessorAction {
    WordProcessorAction::ApplyNamedStyle {
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
) -> WordProcessorAction {
    WordProcessorAction::SetParagraphStyle {
        region,
        blocks,
        kind: kind.to_string(),
        level,
        ordered,
    }
}

/// Insert a `rows`×`cols` table after `after`.
pub fn insert_table_action(after: &str, rows: usize, cols: usize) -> WordProcessorAction {
    WordProcessorAction::InsertTable {
        after: after.to_string(),
        rows,
        cols,
    }
}

pub fn edit_table_action(table: &str, edit: TableEdit) -> WordProcessorAction {
    WordProcessorAction::EditTable {
        table: table.to_string(),
        edit,
    }
}

/// Insert a manual page break at `(block, at)`.
pub fn insert_page_break_action(block: &str, at: usize) -> WordProcessorAction {
    WordProcessorAction::InsertPageBreak {
        region: RegionKind::Body,
        block: block.to_string(),
        at,
    }
}

/// Insert a manual page break in the addressed editing region.
pub fn insert_page_break_in_action(
    region: RegionKind,
    block: &str,
    at: usize,
) -> WordProcessorAction {
    WordProcessorAction::InsertPageBreak {
        region,
        block: block.to_string(),
        at,
    }
}

pub fn insert_page_number_action(
    region: RegionKind,
    block: &str,
    at: usize,
) -> WordProcessorAction {
    WordProcessorAction::InsertField {
        region,
        block: block.to_string(),
        at,
        field: qdoc::schema::FIELD_PAGE_NUMBER.to_string(),
    }
}

pub fn generate_toc_action(entries: Vec<TocTarget>) -> WordProcessorAction {
    WordProcessorAction::GenerateToc { entries }
}

pub fn paste_fragment_action(
    region: RegionKind,
    selection: SelectionTarget,
    blocks: Vec<crate::word_processor::ClipBlock>,
    adopt_first_style: bool,
) -> WordProcessorAction {
    WordProcessorAction::PasteFragment {
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
) -> WordProcessorAction {
    WordProcessorAction::SplitBlock {
        region,
        block: block.to_string(),
        at,
        kind: kind.to_string(),
        ordered,
    }
}

/// Insert an image at `(block, at)`.
pub fn insert_image_action(
    block: &str,
    at: usize,
    bytes: Vec<u8>,
    width: Option<i64>,
    height: Option<i64>,
) -> WordProcessorAction {
    WordProcessorAction::InsertImage {
        block: block.to_string(),
        at,
        bytes,
        width,
        height,
    }
}

/// Remove `blocks` (an image, or a table's cells).
/// Insert a paragraph carrying `text` after block `after`.
pub fn insert_paragraph_action(after: &str, text: &str) -> WordProcessorAction {
    WordProcessorAction::InsertParagraph {
        after: after.to_string(),
        text: text.to_string(),
    }
}

pub fn remove_blocks_action(blocks: Vec<String>) -> WordProcessorAction {
    WordProcessorAction::RemoveBlocks { blocks }
}

/// Replace the whole document body with `markdown`.
pub fn replace_body_action(markdown: &str) -> WordProcessorAction {
    WordProcessorAction::ReplaceBody {
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
) -> WordProcessorAction {
    WordProcessorAction::SpliceText {
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
) -> WordProcessorAction {
    WordProcessorAction::ReplaceSelection {
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
) -> WordProcessorAction {
    WordProcessorAction::AddComment {
        block: block.to_string(),
        start,
        end,
        author: author.to_string(),
        date: date.to_string(),
        text: text.to_string(),
    }
}

/// Append one durable reply to the stable parent comment resource.
pub fn reply_to_comment_action(
    comment: &str,
    author: &str,
    date: &str,
    text: &str,
) -> WordProcessorAction {
    WordProcessorAction::ReplyToComment {
        comment: comment.to_string(),
        author: author.to_string(),
        date: date.to_string(),
        text: text.to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Ctrl+K needs to build the document's resource tree:
/// the caret block/offset the structural inserts anchor on, and the block ids
/// the block-level formatting targets (the selection, or the caret block).
/// Everything here derives from state the app owns; the resource children
/// (images / tables / sections) are enumerated from the shared document.
#[derive(Debug, Clone)]
pub struct WpResourceCtx {
    pub caret_block: String,
    pub caret_offset: usize,
    /// The blocks a block-level action targets — the selection, or the caret
    /// block when there is no selection.
    pub target_blocks: Vec<String>,
    /// Exact non-empty character spans selected at palette invocation time.
    /// This keeps a prompted hyperlink action resource-addressed and replayable
    /// instead of consulting mutable presentation state after the user enters
    /// a target.
    pub format_spans: Vec<Span>,
    /// The selection in document order when it is non-empty.  This semantic
    /// context, rather than a caller-provided block id, binds the palette's
    /// Replace selection operation to exactly what the user has selected.
    pub selection: Option<SelectionTarget>,
    pub region: RegionKind,
    /// Current Find results, capped before they enter the global resource tree.
    /// Each carries CRDT block identity + exact character range, never a
    /// renderer index that would drift under a collaborator's edit.
    pub search_hits: Vec<SearchTarget>,
}

/// One non-empty selection expressed as stable block ids and character offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionTarget {
    pub start_block: String,
    pub start: usize,
    pub end_block: String,
    pub end: usize,
}

/// A bounded, precisely addressable Find result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTarget {
    pub block: String,
    pub start: usize,
    pub end: usize,
}

/// Resource enumeration is intentionally bounded: a giant document can still
/// search completely, but Ctrl+K/global search gets the first useful page and
/// never allocates one resource object per occurrence forever.
pub const MAX_RESOURCE_SEARCH_HITS: usize = 64;
pub const MAX_RESOURCE_REVISIONS: usize = 64;

fn immediate_spec(id: &str, label: &str, action: &WordProcessorAction) -> ActionSpec {
    // Built from `encode`, which is total, rather than the fallible
    // `ActionSpec::immediate` — the two produce the same spec.
    //
    // Through the constructor rather than a struct literal: `ActionSpec`
    // carries private `params`/`build` fields, so a literal cannot name
    // them and the shared defaults would be bypassed if it could.
    let spec =
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action)).reversible();
    if id.contains("delete") || id.contains("remove") {
        spec.destructive()
    } else {
        spec
    }
}

/// A short label for a structural block in the palette.
fn short(text: &str, fallback: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim();
    if t.is_empty() {
        fallback.to_string()
    } else if t.chars().count() > 48 {
        t.chars().take(45).collect::<String>() + "…"
    } else {
        t.to_string()
    }
}

/// Human label for a stable document range without retaining a separate search
/// text cache. The caller materializes once, then each bounded result borrows
/// from that snapshot.
fn range_label(
    blocks: &[qdoc::snapshot::BlockView],
    block: &str,
    start: usize,
    end: usize,
) -> Option<String> {
    let text: String = blocks
        .iter()
        .find(|entry| entry.id == block)?
        .text
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    Some(format!("Result: {}", short(&text, "Empty match")))
}

fn document_resource() -> Resource {
    Resource::new(
        "word-processor.document",
        "word_processor.document",
        "Document",
    )
    .with_action(crate::semantic_read::action::spec())
}

fn add_file_operations(root: &mut Resource, file: Option<&continuation::FileContext>) {
    for (id, label, variant) in [
        ("open", "Open document", 0_u8),
        ("save", "Save document", 1_u8),
        ("save-as", "Save document as", 2_u8),
    ] {
        root.actions.push(continuation::file(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system file picker)",
                OperationKind::Effect,
                WORD_PROCESSOR_OPERATION_TYPE,
                vec![ActionParam::optional_owned(
                    "path",
                    ParamType::Text,
                    "Direct filesystem path; omit to open the system file picker",
                )],
                move |args: &ActionArgs| {
                    let path = args
                        .text("path")
                        .filter(|path| !path.trim().is_empty())
                        .map(str::to_string);
                    let operation = match variant {
                        0 => WordProcessorOperation::Open { path },
                        1 => WordProcessorOperation::Save { path },
                        _ => WordProcessorOperation::SaveAs { path },
                    };
                    EncodedAction::encode_operation(WORD_PROCESSOR_OPERATION_TYPE, &operation)
                        .unwrap_or_else(|_| EncodedAction {
                            type_id: WORD_PROCESSOR_OPERATION_TYPE.into(),
                            payload: Vec::new(),
                        })
                },
            )
            .destructive()
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
            variant,
            file,
        ));
    }
}

fn add_view_operations(root: &mut Resource) {
    for (id, label, value) in [
        ("find", "Find in document", WordProcessorOperation::Find),
        ("zoom-in", "Zoom in", WordProcessorOperation::ZoomIn),
        ("zoom-out", "Zoom out", WordProcessorOperation::ZoomOut),
        (
            "zoom-reset",
            "Reset zoom to 100%",
            WordProcessorOperation::ZoomReset,
        ),
        (
            "spellcheck",
            "Toggle spellcheck",
            WordProcessorOperation::ToggleSpellcheck,
        ),
        (
            "markup",
            "Toggle review markup",
            WordProcessorOperation::ToggleMarkup,
        ),
        (
            "styles",
            "Toggle styles panel",
            WordProcessorOperation::ToggleStyles,
        ),
        ("ruler", "Toggle ruler", WordProcessorOperation::ToggleRuler),
        (
            "outline",
            "Toggle outline",
            WordProcessorOperation::ToggleOutline,
        ),
        (
            "history",
            "Toggle history",
            WordProcessorOperation::ToggleHistory,
        ),
    ] {
        if let Some(spec) = operation(id, label, OperationKind::View, &value) {
            root.actions.push(spec);
        }
    }
}

fn add_caret_actions(root: &mut Resource, ctx: &WpResourceCtx) {
    root.actions.push(immediate_spec(
        "insert-table",
        "Insert table",
        &insert_table_action(&ctx.caret_block, 2, 2),
    ));
    if ctx.format_spans.is_empty() {
        return;
    }
    let spans = ctx.format_spans.clone();
    let region = ctx.region;
    root.actions.push(continuation::pure(
        ActionSpec::typed(
            "set-hyperlink",
            "Add or change hyperlink",
            "Web URL (https://… or mailto:…) or #bookmark",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "target",
                ParamType::Text,
                "A safe external URL, mail address, or document-local #bookmark target",
            )],
            move |args: &ActionArgs| {
                let target = args.text("target").unwrap_or_default().trim();
                if !valid_hyperlink_target(target) {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"hyperlink target must use https:, mailto:, or #bookmark"
                            .to_vec(),
                    };
                }
                encode(&set_hyperlink_action(region, spans.clone(), target))
            },
        ),
        "set-hyperlink.v1",
        continuation::spans(region, &ctx.format_spans),
    ));
}

fn add_selection_resources(root: &mut Resource, ctx: &WpResourceCtx) {
    let Some(selection) = &ctx.selection else {
        return;
    };
    let selection = selection.clone();
    let root_selection = selection.clone();
    let region = ctx.region;
    let domain = continuation::selection(region, &selection);
    root.actions.push(continuation::pure(
        ActionSpec::typed(
            "replace-selection",
            "Replace selection",
            "Replacement text for the selected document range",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "text",
                ParamType::Text,
                "Text that replaces the current selection (empty deletes it)",
            )],
            move |args: &ActionArgs| {
                let Some(text) = args.text("text") else {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"replacement text is required".to_vec(),
                    };
                };
                encode(&replace_selection_action(
                    region,
                    &root_selection.start_block,
                    root_selection.start,
                    &root_selection.end_block,
                    root_selection.end,
                    text,
                ))
            },
        ),
        "replace-selection.v1",
        domain.clone(),
    ));
    root.children.push(
        Resource::new(
            format!(
                "word-processor.selection:{}:{}:{}:{}",
                selection.start_block, selection.start, selection.end_block, selection.end
            ),
            "word_processor.selection",
            "Current selection",
        )
        .with_action(continuation::pure(
            ActionSpec::typed(
                "replace",
                "Replace selection",
                "Replacement text for this exact selection",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required_owned(
                    "text",
                    ParamType::Text,
                    "Replacement text",
                )],
                move |args: &ActionArgs| {
                    let Some(text) = args.text("text") else {
                        return EncodedAction {
                            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                            payload: b"replacement text is required".to_vec(),
                        };
                    };
                    encode(&replace_selection_action(
                        region,
                        &selection.start_block,
                        selection.start,
                        &selection.end_block,
                        selection.end,
                        text,
                    ))
                },
            ),
            "replace-selection.v1",
            domain,
        )),
    );
}

fn add_comment_resource_action(root: &mut Resource, ctx: &WpResourceCtx) {
    let Some(selection) = &ctx.selection else {
        return;
    };
    if ctx.region != RegionKind::Body
        || selection.start_block != selection.end_block
        || selection.start >= selection.end
    {
        return;
    }
    let target = selection.clone();
    let domain = continuation::selection(RegionKind::Body, &target);
    root.actions.push(continuation::pure(
        ActionSpec::typed(
            "add-comment",
            "Add comment",
            "Comment text and author for the selected text",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned("author", ParamType::Text, "Comment author"),
                ActionParam::required_owned("text", ParamType::Text, "Comment text"),
                ActionParam::optional_owned("date", ParamType::Text, "ISO-8601 comment date"),
            ],
            move |args: &ActionArgs| {
                let author = args.text("author").unwrap_or_default().trim();
                let text = args.text("text").unwrap_or_default();
                if author.is_empty() || text.chars().count() > MAX_ACTION_TEXT_CHARS {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"comment author is required and comment text is too long"
                            .to_vec(),
                    };
                }
                encode(&add_comment_action(
                    &target.start_block,
                    target.start,
                    target.end,
                    author,
                    args.text("date").unwrap_or_default(),
                    text,
                ))
            },
        ),
        "add-comment.v1",
        domain,
    ));
}

fn add_search_resources(
    root: &mut Resource,
    ctx: &WpResourceCtx,
    snapshot: &qdoc::snapshot::DocSnapshot,
) {
    if ctx.search_hits.is_empty() {
        return;
    }
    let mut search = Resource::new(
        "word-processor.search-results",
        "word_processor.search-results",
        "Search results",
    );
    let mut seen = HashSet::new();
    for (result_index, hit) in ctx
        .search_hits
        .iter()
        .filter(|hit| seen.insert((hit.block.as_str(), hit.start, hit.end)))
        .take(MAX_RESOURCE_SEARCH_HITS)
        .enumerate()
    {
        let label = range_label(&snapshot.blocks, &hit.block, hit.start, hit.end)
            .unwrap_or_else(|| format!("Result {}", result_index + 1));
        let view_operation = WordProcessorOperation::NavigateTo {
            block: hit.block.clone(),
            start: hit.start,
            end: hit.end,
        };
        if let Some(spec) = operation(
            "go-to",
            "Go to result",
            OperationKind::View,
            &view_operation,
        ) {
            search.children.push(
                Resource::new(
                    format!(
                        "word-processor.search-hit:{}:{}:{}",
                        hit.block, hit.start, hit.end
                    ),
                    "word_processor.search-hit",
                    label,
                )
                .with_action(spec),
            );
        }
    }
    root.children.push(search);
}

fn add_document_edit_actions(root: &mut Resource, ctx: &WpResourceCtx) {
    let image_anchor = ctx.caret_block.clone();
    let image_offset = ctx.caret_offset;
    root.actions.push(
        ActionSpec::prompted_content(
            "place-image",
            "Place image",
            "Image content produced by the preceding action",
            "content hash",
            ExecutionLocus::ClientLocal,
            move |hash| {
                let Some(bytes) = qdoc::blobstore::get(hash) else {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: format!("content `{hash}` is not available on this machine")
                            .into_bytes(),
                    };
                };
                let meta = qdoc::blobstore::sniff(&bytes);
                encode(&insert_image_action(
                    &image_anchor,
                    image_offset,
                    bytes,
                    (meta.w > 0).then(|| i64::from(meta.w)),
                    (meta.h > 0).then(|| i64::from(meta.h)),
                ))
            },
        )
        .with_description(
            "Place the single content result of the immediately preceding action at the caret",
        )
        .reversible(),
    );
    root.actions.push(immediate_spec(
        "insert-page-break",
        "Insert page break",
        &insert_page_break_action(&ctx.caret_block, ctx.caret_offset),
    ));
    for (id, label, align) in [
        ("align-left", "Align left", "left"),
        ("align-center", "Align centre", "center"),
        ("align-right", "Align right", "right"),
        ("align-justify", "Justify", "justify"),
    ] {
        root.actions.push(immediate_spec(
            id,
            label,
            &set_align_action(ctx.target_blocks.clone(), align),
        ));
    }
}

fn comment_resource(comment: &qdoc::snapshot::CommentView) -> Resource {
    let text = comment
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut resource = Resource::new(
        format!("word-processor.comment:{}", comment.id),
        "word_processor.comment",
        format!(
            "Comment by {}: {}",
            short(&comment.author, "Unknown author"),
            short(&text, "Empty comment")
        ),
    )
    .with_action(immediate_spec(
        "resolve",
        "Resolve comment",
        &WordProcessorAction::ResolveComment {
            id: comment.id.clone(),
        },
    ));
    if let Some((block, start, end)) = &comment.range
        && let Some(spec) = operation(
            "go-to-anchor",
            "Go to commented text",
            OperationKind::View,
            &WordProcessorOperation::NavigateTo {
                block: block.clone(),
                start: *start,
                end: *end,
            },
        )
    {
        resource.actions.push(spec);
    }
    for reply in comment.replies.iter().take(MAX_RESOURCE_SEARCH_HITS) {
        let reply_text = reply
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        resource.children.push(Resource::new(
            format!("word-processor.comment-reply:{}:{}", comment.id, reply.id),
            "word_processor.comment-reply",
            format!(
                "Reply by {}: {}",
                short(&reply.author, "Unknown author"),
                short(&reply_text, "Empty reply")
            ),
        ));
    }
    let parent = comment.id.clone();
    let domain = vec![parent.as_bytes().to_vec()];
    resource.actions.push(continuation::pure(
        ActionSpec::typed(
            "reply",
            "Reply to comment",
            "Reply text and author, stored in the shared comment thread",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned("author", ParamType::Text, "Reply author"),
                ActionParam::required_owned("text", ParamType::Text, "Reply text"),
                ActionParam::optional_owned("date", ParamType::Text, "ISO-8601 reply date"),
            ],
            move |args: &ActionArgs| {
                let author = args.text("author").unwrap_or_default().trim();
                let text = args.text("text").unwrap_or_default();
                if author.is_empty() || text.chars().count() > MAX_ACTION_TEXT_CHARS {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"reply author is required and reply text is too long".to_vec(),
                    };
                }
                encode(&reply_to_comment_action(
                    &parent,
                    author,
                    args.text("date").unwrap_or_default(),
                    text,
                ))
            },
        ),
        "reply-to-comment.v1",
        domain,
    ));
    resource
}

fn add_review_resources(root: &mut Resource, snapshot: &qdoc::snapshot::DocSnapshot) {
    if snapshot.comments.is_empty() && snapshot.changes.is_empty() {
        return;
    }
    let mut review = Resource::new("word-processor.review", "word_processor.review", "Review");
    if !snapshot.changes.is_empty() {
        review.actions.push(immediate_spec(
            "accept-all",
            "Accept all changes",
            &WordProcessorAction::AcceptChanges { id: None },
        ));
        review.actions.push(immediate_spec(
            "reject-all",
            "Reject all changes",
            &WordProcessorAction::RejectChanges { id: None },
        ));
    }
    review
        .children
        .extend(snapshot.comments.iter().map(comment_resource));
    for change in &snapshot.changes {
        let kind = match change.kind {
            qdoc::snapshot::ChangeKind::Ins => "Insertion",
            qdoc::snapshot::ChangeKind::Del => "Deletion",
        };
        review.children.push(
            Resource::new(
                format!("word-processor.change:{}", change.id),
                "word_processor.change",
                format!("{kind} by {}", short(&change.author, "Unknown author")),
            )
            .with_action(immediate_spec(
                "accept",
                "Accept change",
                &WordProcessorAction::AcceptChanges {
                    id: Some(change.id.clone()),
                },
            ))
            .with_action(immediate_spec(
                "reject",
                "Reject change",
                &WordProcessorAction::RejectChanges {
                    id: Some(change.id.clone()),
                },
            )),
        );
    }
    root.children.push(review);
}

fn add_revision_resources(root: &mut Resource, doc: &AutoCommit) {
    let mut history_doc = doc.clone();
    // Revision resources need labels and hashes, not reconstructed operation
    // payloads for every change in the document's history.
    let changes = history_doc.get_changes_meta(&[]);
    if changes.is_empty() {
        return;
    }
    let mut history = Resource::new(
        "word-processor.revisions",
        "word_processor.revisions",
        "Revision history",
    );
    let start = changes.len().saturating_sub(MAX_RESOURCE_REVISIONS);
    for (index, change) in changes.iter().enumerate().skip(start) {
        let label = change.message.as_deref().map_or_else(
            || format!("Revision {}", index + 1),
            |message| short(message, "Revision"),
        );
        if let Some(spec) = operation(
            "show",
            "Show revision",
            OperationKind::View,
            &WordProcessorOperation::ShowRevision { index },
        ) {
            history.children.push(
                Resource::new(
                    format!("word-processor.revision:{}", change.hash),
                    "word_processor.revision",
                    label,
                )
                .with_action(spec),
            );
        }
    }
    root.children.push(history);
}

fn add_metadata_resources(root: &mut Resource, snapshot: &qdoc::snapshot::DocSnapshot) {
    if !snapshot.notes.is_empty() {
        let mut notes = Resource::new("word-processor.notes", "word_processor.notes", "Notes");
        for note in &snapshot.notes {
            let text = note
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            notes.children.push(Resource::new(
                format!("word-processor.note:{}", note.id),
                "word_processor.note",
                format!("{}: {}", note.kind, short(&text, "Empty note")),
            ));
        }
        root.children.push(notes);
    }
    if !snapshot.bookmarks.is_empty() {
        let mut bookmarks = Resource::new(
            "word-processor.bookmarks",
            "word_processor.bookmarks",
            "Bookmarks",
        );
        for bookmark in &snapshot.bookmarks {
            bookmarks.children.push(Resource::new(
                format!("word-processor.bookmark:{}", bookmark.name),
                "word_processor.bookmark",
                bookmark.name.clone(),
            ));
        }
        root.children.push(bookmarks);
    }
}

fn add_style_resources(root: &mut Resource, snapshot: &qdoc::snapshot::DocSnapshot) {
    if snapshot.styles.is_empty() {
        return;
    }
    let mut styles = Resource::new(
        "word-processor.styles",
        "word_processor.styles",
        "Paragraph styles",
    );
    for (style, properties) in &snapshot.styles {
        let bold = matches!(
            properties.get(qdoc::schema::STYLE_BOLD),
            Some(qdoc::AttrVal::Bool(true))
        );
        let mut child = Resource::new(
            format!("word-processor.style:{style}"),
            "word_processor.style",
            style.clone(),
        )
        .with_action(immediate_spec(
            "toggle-bold",
            if bold { "Remove bold" } else { "Make bold" },
            &set_style_property_action(style, qdoc::schema::STYLE_BOLD, AttrVal::Bool(!bold)),
        ));
        if !crate::styles::StyleSheet::is_builtin(style) {
            let instances = snapshot
                .blocks
                .iter()
                .filter(|block| {
                    matches!(block.attrs.get("style"), Some(qdoc::AttrVal::Str(id)) if id == style)
                })
                .map(|block| block.id.clone())
                .collect();
            child.actions.push(immediate_spec(
                "delete",
                "Delete style",
                &delete_style_action(style, instances),
            ));
        }
        styles.children.push(child);
    }
    root.children.push(styles);
}

fn image_resource(block: &qdoc::snapshot::BlockView) -> Resource {
    let edge = |key: &str| match block.attrs.get(key) {
        Some(qdoc::AttrVal::Int(value)) => (*value).clamp(0, 9_500),
        _ => 0,
    };
    let left = edge("crop_left");
    let top = edge("crop_top");
    let right = edge("crop_right");
    let bottom = edge("crop_bottom");
    let can_crop_further =
        left.saturating_add(right) <= 8_500 && top.saturating_add(bottom) <= 8_500;
    let mut resource = Resource::new(
        format!("word-processor.image:{}", block.id),
        "word_processor.image",
        "Image",
    )
    .with_action(immediate_spec(
        "delete",
        "Delete image",
        &remove_blocks_action(vec![block.id.clone()]),
    ));
    if can_crop_further {
        resource.actions.push(immediate_spec(
            "crop-inset",
            "Crop image by 5%",
            &set_image_crop_action(&block.id, left + 250, top + 250, right + 250, bottom + 250),
        ));
    }
    if left != 0 || top != 0 || right != 0 || bottom != 0 {
        resource.actions.push(immediate_spec(
            "reset-crop",
            "Reset crop",
            &set_image_crop_action(&block.id, 0, 0, 0, 0),
        ));
    }
    for (id, label, anchor, wrap) in [
        ("free-position", "Place freely on page", "page", "none"),
        ("flow-left", "Wrap text on right", "paragraph", "left"),
        ("flow-right", "Wrap text on left", "paragraph", "right"),
    ] {
        resource.actions.push(immediate_spec(
            id,
            label,
            &set_image_placement_action(&block.id, anchor, wrap, 0, 0, 0),
        ));
    }
    resource
}

fn table_resource(table_id: &str, table_blocks: &[&qdoc::snapshot::BlockView]) -> Resource {
    let coordinate = |block: &qdoc::snapshot::BlockView, key: &str| match block.attrs.get(key) {
        Some(qdoc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
        _ => 0,
    };
    let rows = table_blocks
        .iter()
        .map(|block| coordinate(block, "row"))
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let columns = table_blocks
        .iter()
        .map(|block| coordinate(block, "col"))
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut resource = Resource::new(
        format!("word-processor.table:{table_id}"),
        "word_processor.table",
        "Table",
    )
    .with_action(immediate_spec(
        "append-row",
        "Add row",
        &edit_table_action(table_id, TableEdit::InsertRow { at: rows }),
    ))
    .with_action(immediate_spec(
        "append-column",
        "Add column",
        &edit_table_action(table_id, TableEdit::InsertColumn { at: columns }),
    ))
    .with_action(immediate_spec(
        "delete",
        "Delete table",
        &edit_table_action(table_id, TableEdit::Delete),
    ));
    for cell in table_blocks {
        let row = coordinate(cell, "row");
        let column = coordinate(cell, "col");
        let mut cell_resource = Resource::new(
            format!("word-processor.cell:{}", cell.id),
            "word_processor.table-cell",
            format!(
                "Cell {}, {}",
                row.saturating_add(1),
                column.saturating_add(1)
            ),
        )
        .with_action(immediate_spec(
            "delete-row",
            "Delete row",
            &edit_table_action(table_id, TableEdit::DeleteRow { row }),
        ))
        .with_action(immediate_spec(
            "delete-column",
            "Delete column",
            &edit_table_action(table_id, TableEdit::DeleteColumn { column }),
        ));
        if let Some(spec) = operation(
            "go-to",
            "Go to cell",
            OperationKind::View,
            &WordProcessorOperation::NavigateTo {
                block: cell.id.clone(),
                start: 0,
                end: cell.text.chars().count(),
            },
        ) {
            cell_resource.actions.push(spec);
        }
        resource.children.push(cell_resource);
    }
    resource
}

fn add_structural_resources(root: &mut Resource, snapshot: &qdoc::snapshot::DocSnapshot) {
    root.children.extend(
        snapshot
            .blocks
            .iter()
            .filter(|block| block.kind == "image")
            .map(image_resource),
    );
    let mut table_ids = Vec::new();
    for block in snapshot.blocks.iter().filter(|block| block.kind == "cell") {
        if let Some(qdoc::AttrVal::Str(table_id)) = block.attrs.get("table")
            && !table_ids.contains(table_id)
        {
            table_ids.push(table_id.clone());
        }
    }
    for table_id in &table_ids {
        let blocks = snapshot
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(qdoc::AttrVal::Str(id)) if id == table_id)
            })
            .collect::<Vec<_>>();
        root.children.push(table_resource(table_id, &blocks));
    }
    for block in snapshot
        .blocks
        .iter()
        .filter(|block| block.kind == "heading")
    {
        let mut section = Resource::new(
            format!("word-processor.section:{}", block.id),
            "word_processor.section",
            short(&block.text, "Heading"),
        );
        for (id, label, align) in [
            ("align-left", "Align left", "left"),
            ("align-center", "Align centre", "center"),
            ("align-right", "Align right", "right"),
        ] {
            section.actions.push(immediate_spec(
                id,
                label,
                &set_align_action(vec![block.id.clone()], align),
            ));
        }
        root.children.push(section);
    }
}

/// Build the resource tree the word processor currently has open.
pub fn provide_resources(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    provide_resources_with_file(ctx, doc, None)
}

fn provide_resources_with_file(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    file: Option<&continuation::FileContext>,
) -> Vec<Resource> {
    let mut root = document_resource();
    add_file_operations(&mut root, file);
    add_view_operations(&mut root);
    add_caret_actions(&mut root, ctx);
    add_selection_resources(&mut root, ctx);
    add_comment_resource_action(&mut root, ctx);

    let Ok(containers) = qdoc::model::open(doc, &automerge::ROOT) else {
        return vec![root];
    };
    let snapshot = qdoc::model::materialize(doc, &containers);
    add_search_resources(&mut root, ctx, &snapshot);
    add_document_edit_actions(&mut root, ctx);
    add_review_resources(&mut root, &snapshot);
    add_revision_resources(&mut root, doc);
    add_metadata_resources(&mut root, &snapshot);
    add_style_resources(&mut root, &snapshot);
    add_structural_resources(&mut root, &snapshot);
    vec![root]
}

pub(crate) fn file_choice_resource(generation: u64) -> Result<Resource, String> {
    let mut resource = Resource::new(
        format!("word-processor.file-choice:{generation}"),
        "word_processor.file_choice",
        crate::i18n::t("file-choice-title"),
    );
    for (id, label, use_selected) in [
        ("keep-workspace", "file-choice-keep", false),
        ("use-selected", "file-choice-use", true),
    ] {
        resource.actions.push(
            operation(
                id,
                &crate::i18n::t(label),
                OperationKind::Effect,
                &WordProcessorOperation::ResolveFileChoice {
                    generation,
                    use_selected,
                },
            )
            .ok_or("file choice operation could not be encoded")?,
        );
    }
    Ok(resource)
}

/// Exact inputs of `provide_resources_with_file` for a committed document.
fn resource_memo_key(
    doc: &AutoCommit,
    ctx: &WpResourceCtx,
    file: &continuation::FileContext,
) -> String {
    let heads: Vec<String> = doc.base_heads().iter().map(ToString::to_string).collect();
    format!(
        "{heads:?}|{ctx:?}|{:?}|{:?}|{}|{}",
        file.path,
        file.format,
        file.canonical,
        crate::i18n::t("file-choice-title"),
    )
}

impl ResourceProvider for crate::widget::WordProcessorWidget {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if let Some(resource) = self.file_choice_resource() {
            return vec![resource];
        }
        let Some(ctx) = self.wp_resource_ctx(doc) else {
            return Vec::new();
        };
        let file = self.file_continuation_context();
        // Heads identify the committed document exactly; pending (uncommitted)
        // operations are not covered by them, so those frames rebuild.
        if doc.pending_ops() != 0 {
            return provide_resources_with_file(&ctx, doc, Some(&file));
        }
        let key = resource_memo_key(doc, &ctx, &file);
        if let Some((cached, resources)) = self.resource_memo.borrow().as_ref()
            && *cached == key
        {
            return resources.clone();
        }
        let resources = provide_resources_with_file(&ctx, doc, Some(&file));
        *self.resource_memo.borrow_mut() = Some((key, resources.clone()));
        resources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionInput;
    use tilo_actions::LocalTransport;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    #[test]
    fn view_catalogue_exposes_zoom_reset_as_a_typed_operation() {
        let mut root = document_resource();
        add_view_operations(&mut root);
        let spec = root
            .actions
            .iter()
            .find(|spec| spec.id == "zoom-reset")
            .expect("the visible zoom reset has a resource action");
        let encoded = immediate_encode(spec);
        assert_eq!(encoded.type_id, WORD_PROCESSOR_OPERATION_TYPE);
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(WordProcessorOperation::ZoomReset),
            "the resource action decodes to the one canonical reset operation"
        );
    }

    /// A `ui::State` holding a canonical document seeded from `markdown`, plus the
    /// ordered body block ids.
    fn seeded(markdown: &str) -> (ui::State, Vec<String>) {
        let mut state = ui::State::new();
        let mut ed = crate::EditorDoc::bind(&mut state.doc, &state.root);
        {
            use qdoc::FormatProvider;
            let mut dm = ed.edit(&mut state.doc);
            qdoc::MarkdownCodec
                .import(markdown.as_bytes(), &mut dm)
                .unwrap();
        }
        let ids = qdoc::model::blocks_in_order(&state.doc, &ed.containers);
        (state, ids)
    }

    fn body_blocks(doc: &AutoCommit) -> Vec<qdoc::snapshot::BlockView> {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::materialize(doc, &c).blocks
    }

    fn text_of(doc: &AutoCommit, block: &str) -> String {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::block_text(doc, &c, block).unwrap_or_default()
    }

    /// Dispatch an action through the Tilo executor with a discarding transport
    /// (the local, single-peer path the affordances take).
    fn dispatch(doc: &mut AutoCommit, action: &WordProcessorAction) {
        let encoded = encode(action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, action, encoded, &mut tx).unwrap();
    }

    #[test]
    fn splice_validation_matches_snapshot_without_scanning_regions() {
        use automerge::{ObjType, ScalarValue};
        use qdoc::schema::{BLOCK_ORDER, BLOCK_TEXT, KEY_FOOTER, KEY_HEADER};
        let (mut state, ids) = seeded("Aé🦀é\n");
        let c = qdoc::model::open(&state.doc, &state.root).unwrap();
        for region in [RegionKind::Header, RegionKind::Footer] {
            let action = WordProcessorAction::SpliceText {
                region,
                block: ids[0].clone(),
                at: 0,
                del: 0,
                ins: "x".into(),
            };
            assert!(!action_is_valid(&action, &state.doc, &c), "absent region");
        }
        // Keep c from before lazy header/footer creation to exercise fallback lookup.
        let header = state
            .doc
            .put_object(&c.root, KEY_HEADER, ObjType::Map)
            .unwrap();
        let footer = state
            .doc
            .put_object(&c.root, KEY_FOOTER, ObjType::Map)
            .unwrap();
        for container in [&c.blocks, &header, &footer] {
            for id in [
                "valid",
                "missing_text",
                "scalar_text",
                "object_text",
                "missing_order",
                "bad_order",
            ] {
                let obj = state.doc.put_object(container, id, ObjType::Map).unwrap();
                if id != "missing_order" {
                    if id == "bad_order" {
                        state.doc.put(&obj, BLOCK_ORDER, "invalid").unwrap();
                    } else {
                        state
                            .doc
                            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(vec![]))
                            .unwrap();
                    }
                }
                if id == "scalar_text" {
                    state
                        .doc
                        .put(&obj, BLOCK_TEXT, "not a text object")
                        .unwrap();
                } else if id == "object_text" {
                    state
                        .doc
                        .put_object(&obj, BLOCK_TEXT, ObjType::Map)
                        .unwrap();
                } else if id != "missing_text" {
                    let text = state
                        .doc
                        .put_object(&obj, BLOCK_TEXT, ObjType::Text)
                        .unwrap();
                    state.doc.splice_text(&text, 0, 0, "Aé🦀é").unwrap();
                }
            }
            state
                .doc
                .put(container, "scalar_block", "not a block")
                .unwrap();
            state
                .doc
                .put_object(container, "deleted", ObjType::Map)
                .unwrap();
            state.doc.delete(container, "deleted").unwrap();
        }
        let snap = qdoc::model::materialize(&state.doc, &c);
        for region in [RegionKind::Body, RegionKind::Header, RegionKind::Footer] {
            let blocks = match region {
                RegionKind::Body => &snap.blocks,
                RegionKind::Header => &snap.header,
                RegionKind::Footer => &snap.footer,
            };
            for id in [
                ids[0].as_str(),
                "valid",
                "missing_text",
                "scalar_text",
                "object_text",
                "missing_order",
                "bad_order",
                "scalar_block",
                "deleted",
            ] {
                for at in [0, 1, 5, 6, usize::MAX] {
                    for del in [0, 1, 5, usize::MAX] {
                        let action = WordProcessorAction::SpliceText {
                            region,
                            block: id.into(),
                            at,
                            del,
                            ins: "🦀".into(),
                        };
                        let expected = blocks.iter().find(|b| b.id == id).is_some_and(|b| {
                            let len = b.text.chars().count();
                            at <= len && del <= len.saturating_sub(at)
                        });
                        let before = qdoc::model::ordered_keys_scan_count();
                        assert_eq!(
                            action_is_valid(&action, &state.doc, &c),
                            expected,
                            "{action:?}"
                        );
                        assert_eq!(
                            qdoc::model::ordered_keys_scan_count(),
                            before,
                            "single-block validation must not scan regions"
                        );
                    }
                }
            }
        }
        for (count, expected) in [
            (MAX_ACTION_TEXT_CHARS, true),
            (MAX_ACTION_TEXT_CHARS + 1, false),
        ] {
            let action = WordProcessorAction::SpliceText {
                region: RegionKind::Body,
                block: ids[0].clone(),
                at: 0,
                del: 0,
                ins: "é".repeat(count),
            };
            assert_eq!(action_is_valid(&action, &state.doc, &c), expected);
        }
        state.doc.commit();
        let before = state.save();
        let action = WordProcessorAction::SpliceText {
            region: RegionKind::Body,
            block: ids[0].clone(),
            at: usize::MAX,
            del: 1,
            ins: "x".into(),
        };
        assert!(matches!(
            action.apply(&mut state.doc),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(state.save(), before, "invalid splice must make no writes");
    }

    /// A text splice through an action mutates the document, and replays
    /// byte-identically on a second, independent replica (equal head hashes).
    #[test]
    fn splice_text_mutates_and_replays_byte_identical() {
        let (mut a, ids) = seeded("Hello world\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "precondition: same base state"
        );

        // Replace "world" with "there".
        let action = splice_text_action(RegionKind::Body, &block, 6, 11, "there");
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(text_of(&a.doc, &block), "Hello there", "the splice landed");
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "replay reproduced the identical change"
        );
    }

    /// A formatting mark over a selection replays byte-identically (the marks
    /// path, not just a scalar write).
    #[test]
    fn set_mark_replays_byte_identical() {
        let (mut a, ids) = seeded("Hello world\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = set_mark_action(
            RegionKind::Body,
            vec![Span {
                block: block.clone(),
                start: 0,
                end: 5,
            }],
            "strong",
            Some(MarkVal::Bool(true)),
            vec![],
        );
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "mark replayed identically"
        );
        let marks = body_blocks(&b.doc)
            .into_iter()
            .find(|x| x.id == block)
            .map(|x| x.marks)
            .unwrap_or_default();
        assert!(
            marks
                .iter()
                .any(|m| m.name == "strong" && m.start == 0 && m.end == 5),
            "the bold run is present on the peer: {marks:?}"
        );
    }

    /// A paragraph-style change (kind + level) replays byte-identically.
    #[test]
    fn set_paragraph_style_replays_byte_identical() {
        let (mut a, ids) = seeded("Title\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = set_paragraph_style_action(
            RegionKind::Body,
            vec![block.clone()],
            "heading",
            Some(1),
            None,
        );
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "style replayed identically"
        );
        assert_eq!(
            body_blocks(&b.doc)
                .into_iter()
                .find(|x| x.id == block)
                .unwrap()
                .kind,
            "heading",
            "the block became a heading on the peer"
        );
    }

    /// Inserting a table through an action creates the cells (ids minted under
    /// the pinned actor) and replays byte-identically — proving deterministic
    /// id minting inside `apply`.
    #[test]
    fn insert_table_replays_byte_identical() {
        let (mut a, ids) = seeded("Intro\n");
        let after = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = insert_table_action(&after, 2, 3);
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "table replayed identically"
        );
        let cells_a = body_blocks(&a.doc)
            .into_iter()
            .filter(|x| x.kind == "cell")
            .count();
        let cells_b = body_blocks(&b.doc)
            .into_iter()
            .filter(|x| x.kind == "cell")
            .count();
        assert_eq!(cells_a, 6, "2x3 = six cells");
        assert_eq!(cells_a, cells_b, "the same cells on both replicas");
    }

    /// A page break splits the block and marks the tail's fresh block to start a
    /// new page, and replays byte-identically.
    #[test]
    fn insert_page_break_replays_byte_identical() {
        let (mut a, ids) = seeded("onetwo\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        // Break in the middle: "one" | "two".
        let action = insert_page_break_action(&block, 3);
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "page break replayed identically"
        );
        assert_eq!(
            text_of(&a.doc, &block),
            "one",
            "head keeps the pre-caret text"
        );
        let broke = body_blocks(&a.doc).into_iter().any(|x| {
            x.text == "two"
                && matches!(
                    x.attrs.get("page_break_before"),
                    Some(qdoc::AttrVal::Bool(true))
                )
        });
        assert!(broke, "the tail block starts on a fresh page");
    }

    /// Replacing the body (template apply / version restore) replays
    /// byte-identically: same fresh blocks on both replicas.
    #[test]
    fn replace_body_replays_byte_identical() {
        let (mut a, _ids) = seeded("Old content\n");
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = replace_body_action("# Fresh\n\nBrand new body\n");
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "body replace replayed identically"
        );
        let first = body_blocks(&a.doc).into_iter().next().unwrap();
        assert_eq!(first.kind, "heading");
        assert_eq!(first.text, "Fresh");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, reverted
    /// precisely by Ctrl+Z. Uses a paragraph-style change (a structural `kind`
    /// write) since inline formatting *marks* are, by design, not undo-tracked.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (mut state, ids) = seeded("Title\n");
        let block = ids[0].clone();
        let before_heads = state.doc.get_heads();

        let kind_of = |doc: &AutoCommit| {
            body_blocks(doc)
                .into_iter()
                .find(|x| x.id == block)
                .map(|x| x.kind)
        };
        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("paragraph"),
            "starts as a paragraph"
        );

        let action = set_paragraph_style_action(
            RegionKind::Body,
            vec![block.clone()],
            "heading",
            Some(1),
            None,
        );
        let result = action.apply(&mut state.doc);
        assert!(result.is_ok());
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("heading"),
            "the action made it a heading"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("paragraph"),
            "undo reverted the paragraph style and nothing else"
        );
    }

    /// Single source of truth: the palette's document-level specs encode
    /// byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (state, ids) = seeded("Intro\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block.clone(),
            caret_offset: 0,
            target_blocks: vec![block.clone()],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
        };
        let resources = provide_resources(&ctx, &state.doc);
        let doc_res = &resources[0];
        assert_eq!(doc_res.kind, "word_processor.document");

        // Insert-table spec == insert_table builder.
        let spec = doc_res
            .actions
            .iter()
            .find(|a| a.id == "insert-table")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&insert_table_action(&block, 2, 2));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "insert-table is one source of truth"
        );

        // Align-center spec == set_align builder over the same target blocks.
        let spec = doc_res
            .actions
            .iter()
            .find(|a| a.id == "align-center")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&set_align_action(vec![block], "center"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "align is one source of truth"
        );
    }

    #[test]
    fn selected_text_exposes_a_typed_hyperlink_action_that_replays() {
        let (mut state, ids) = seeded("Read this\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            format_spans: vec![Span {
                block: block.clone(),
                start: 0,
                end: 4,
            }],
            selection: Some(SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block.clone(),
                end: 4,
            }),
            region: RegionKind::Body,
            caret_block: block.clone(),
            caret_offset: 4,
            target_blocks: vec![block.clone()],
            search_hits: Vec::new(),
        };
        let root = provide_resources(&ctx, &state.doc).pop().unwrap();
        let replace = root
            .actions
            .iter()
            .find(|action| action.id == "replace-selection")
            .expect("selected text exposes canonical replacement");
        let replacement = replace
            .build_args([(
                "text".to_string(),
                tilo_actions::ArgValue::Text("guide".into()),
            )])
            .unwrap();
        assert_eq!(
            replacement,
            encode(&replace_selection_action(
                RegionKind::Body,
                &block,
                0,
                &block,
                4,
                "guide",
            )),
            "palette binding is byte-identical to the canonical selection action"
        );
        let link = root
            .actions
            .iter()
            .find(|action| action.id == "set-hyperlink")
            .expect("selected text exposes link authoring");
        assert_eq!(link.params().len(), 1);
        assert_eq!(link.params()[0].name, "target");
        let encoded = link
            .build_args([(
                "target".to_string(),
                tilo_actions::ArgValue::Text("https://example.com/guide".into()),
            )])
            .unwrap();
        let action: WordProcessorAction = serde_json::from_slice(&encoded.payload).unwrap();
        action.apply(&mut state.doc).unwrap();
        assert!(body_blocks(&state.doc)[0].marks.iter().any(|mark| {
            mark.name == "link"
                && mark.start == 0
                && mark.end == 4
                && mark.value.as_str() == Some("https://example.com/guide")
        }));
    }

    #[test]
    fn hyperlink_targets_are_limited_to_safe_authored_schemes() {
        for target in [
            "https://example.com/guide?q=writer#links",
            "mailto:writer@example.com",
            "#introduction",
            "#_heading_actor-42",
        ] {
            assert!(valid_hyperlink_target(target), "expected valid: {target}");
        }
        for target in [
            "",
            " https://example.com",
            "http://example.com",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:secret@example.com",
            "mailto:not-an-address",
            "#",
            "#two words",
        ] {
            assert!(
                !valid_hyperlink_target(target),
                "expected refusal: {target}"
            );
        }
    }

    #[test]
    fn hyperlink_validation_is_enforced_by_both_resource_and_executor() {
        let (mut state, ids) = seeded("Read this\n");
        let block = ids[0].clone();
        let span = Span {
            block: block.clone(),
            start: 0,
            end: 4,
        };
        let ctx = WpResourceCtx {
            format_spans: vec![span.clone()],
            selection: None,
            region: RegionKind::Body,
            caret_block: block,
            caret_offset: 4,
            target_blocks: vec![ids[0].clone()],
            search_hits: Vec::new(),
        };
        let root = provide_resources(&ctx, &state.doc).pop().unwrap();
        let link = root
            .actions
            .iter()
            .find(|action| action.id == "set-hyperlink")
            .expect("selected text exposes link authoring");
        let encoded = link
            .build_args([(
                "target".to_string(),
                tilo_actions::ArgValue::Text("javascript:alert(1)".into()),
            )])
            .unwrap();
        assert_eq!(
            encoded.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
            "the prompt reports invalid input before dispatch"
        );

        let action = set_hyperlink_action(RegionKind::Body, vec![span], "javascript:alert(1)");
        assert!(matches!(
            action.apply(&mut state.doc),
            Err(ActionError::Refused(_))
        ));
        assert!(
            body_blocks(&state.doc)[0]
                .marks
                .iter()
                .all(|mark| mark.name != "link"),
            "a refused target leaves the document untouched"
        );
    }

    #[test]
    fn file_operations_are_typed_background_jobs() {
        let (state, ids) = seeded("Intro\n");
        let ctx = WpResourceCtx {
            caret_block: ids[0].clone(),
            caret_offset: 0,
            target_blocks: vec![ids[0].clone()],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
        };
        let resources = provide_resources(&ctx, &state.doc);
        for id in ["open", "save", "save-as"] {
            let action = resources[0]
                .actions
                .iter()
                .find(|action| action.id == id)
                .expect("file action");
            assert_eq!(action.contract.result, ResultSpec::Job);
            assert!(matches!(
                action.contract.execution,
                ExecutionMode::Background { .. }
            ));
            assert!(action.contract.validate_declaration().is_empty());
        }
    }

    /// The hand-off operation consumes bytes an earlier action actually put
    /// in the content store. The model supplies neither the hash nor image
    /// metadata; the app resolves both while building its own document action.
    #[test]
    fn place_image_consumes_a_published_content_hash() {
        let (mut state, ids) = seeded("Intro\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block,
            caret_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
        };
        let bytes = b"content supplied by a preceding action";
        let hash = qdoc::blobstore::put(bytes).expect("test fixture fits blob quota");
        let resources = provide_resources(&ctx, &state.doc);
        let descriptor = tilo_actions::door::survey(&resources)
            .into_iter()
            .find(|descriptor| descriptor.address.action == "place-image")
            .expect("the document publishes its content hand-off action");
        assert!(descriptor.input.wants_content());

        let arguments = serde_json::Map::from_iter([(
            tilo_actions::PROMPTED_PARAM.to_string(),
            serde_json::Value::String(hash.clone()),
        )]);
        tilo_actions::door::invoke_json(
            &mut state.doc,
            &resources,
            &descriptor.address,
            &arguments,
            b"agent",
            &registry(),
            &mut LocalTransport::new(),
        )
        .expect("the app resolves and places available content");

        assert!(body_blocks(&state.doc).iter().any(|block| {
            block.kind == "image"
                && block.attrs.get("blob") == Some(&qdoc::AttrVal::Str(hash.clone()))
        }));
    }

    /// The catalogue publishes each structural document object as a stable,
    /// directly actionable resource.
    #[test]
    fn resource_catalogue_exposes_structural_document_contract() {
        let (mut state, ids) = seeded("# Section\n\nBody\n");
        let para = ids[1].clone();
        dispatch(
            &mut state.doc,
            &insert_image_action(&para, 0, vec![1, 2, 3, 4], Some(20), Some(10)),
        );
        let after_img = qdoc::model::blocks_in_order(
            &state.doc,
            &qdoc::model::open(&state.doc, &automerge::ROOT).unwrap(),
        );
        let last = after_img.last().cloned().unwrap();
        dispatch(&mut state.doc, &insert_table_action(&last, 2, 2));

        let ctx = WpResourceCtx {
            caret_block: para.clone(),
            caret_offset: 0,
            target_blocks: vec![para],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
        };
        let resources = provide_resources(&ctx, &state.doc);
        assert_eq!(resources.len(), 1, "one document root");
        let root = &resources[0];
        let ids_of = |kind: &str| root.children.iter().filter(|r| r.kind == kind).count();
        assert_eq!(ids_of("word_processor.image"), 1, "an image resource");
        assert_eq!(ids_of("word_processor.table"), 1, "a table resource");
        let table = root
            .children
            .iter()
            .find(|resource| resource.kind == "word_processor.table")
            .unwrap();
        assert_eq!(table.children.len(), 4, "every stable cell is navigable");
        assert!(table.children.iter().all(|cell| {
            cell.kind == "word_processor.table-cell"
                && cell.actions.iter().any(|action| action.id == "go-to")
                && cell.actions.iter().any(|action| action.id == "delete-row")
                && cell
                    .actions
                    .iter()
                    .any(|action| action.id == "delete-column")
        }));
        assert!(
            ids_of("word_processor.section") >= 1,
            "a heading section resource"
        );
        let image = root
            .children
            .iter()
            .find(|resource| resource.kind == "word_processor.image")
            .unwrap();
        assert!(image.actions.iter().any(|action| action.id == "delete"));
        assert!(image.actions.iter().any(|action| action.id == "crop-inset"));
        assert!(table.actions.iter().any(|action| action.id == "delete"));
        assert!(table.actions.iter().any(|action| action.id == "append-row"));
    }

    #[test]
    fn image_object_actions_are_atomic_replayable_and_refuse_stale_targets() {
        let (mut a, ids) = seeded("Object\n");
        let block = ids[0].clone();
        dispatch(
            &mut a.doc,
            &insert_image_action(&block, 0, vec![1, 2, 3, 4], Some(40), Some(30)),
        );
        let image = body_blocks(&a.doc)
            .into_iter()
            .find(|view| view.kind == "image")
            .expect("inserted image")
            .id;
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).expect("fork document");
        let actions = [
            set_image_size_action(&image, 120, 90),
            set_image_crop_action(&image, 500, 250, 500, 250),
            set_image_placement_action(&image, "page", "none", 24, 36, 0),
        ];
        for action in &actions {
            let envelope = tilo_actions::apply_local(&mut a.doc, action, encode(action))
                .expect("apply object action");
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).expect("replay object action");
        }
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "object actions replay byte-identically"
        );
        let image_view = body_blocks(&a.doc)
            .into_iter()
            .find(|view| view.id == image)
            .expect("image remains");
        assert_eq!(
            image_view.attrs.get("crop_left"),
            Some(&qdoc::AttrVal::Int(500))
        );
        assert_eq!(
            image_view.attrs.get("offset_x"),
            Some(&qdoc::AttrVal::Int(24))
        );

        let before = a.save();
        let stale = set_image_crop_action("already-removed", 0, 0, 0, 0);
        assert!(matches!(
            stale.apply(&mut a.doc),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(a.save(), before, "refusal has no partial mutation");
    }

    #[test]
    fn review_entities_are_resources_and_decisions_replay_identically() {
        let (mut a, ids) = seeded("Review this sentence\n");
        let block = ids[0].clone();
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let mut id_gen = qdoc::IdGen::new(a.doc.get_actor().to_hex_string());
        let (comment, change) = {
            let mut dm = qdoc::DocMut {
                doc: &mut a.doc,
                c: &c,
                ids: &mut id_gen,
            };
            (
                dm.add_comment(&block, 0, 6, "Maya", "Please clarify this."),
                dm.track_insert(&block, 20, " now", "Jonas", "2026-09-05"),
            )
        };
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let ctx = WpResourceCtx {
            caret_block: block,
            caret_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
        };
        let resources = provide_resources(&ctx, &a.doc);
        let review = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "word-processor.review")
            .expect("review group is navigable");
        let comment_resource_id = format!("word-processor.comment:{comment}");
        let comment_resource = review
            .children
            .iter()
            .find(|resource| resource.id.as_str() == comment_resource_id.as_str())
            .expect("comment is a resource");
        assert!(
            comment_resource
                .actions
                .iter()
                .any(|action| action.id == "resolve")
        );
        let change_resource_id = format!("word-processor.change:{change}");
        let change_resource = review
            .children
            .iter()
            .find(|resource| resource.id.as_str() == change_resource_id.as_str())
            .expect("tracked change is a resource");
        let accept = immediate_encode(
            change_resource
                .actions
                .iter()
                .find(|action| action.id == "accept")
                .unwrap(),
        );
        let action: WordProcessorAction = serde_json::from_slice(&accept.payload).unwrap();
        let envelope = tilo_actions::apply_local(&mut a.doc, &action, accept).unwrap();
        tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(a.doc.get_heads(), b.doc.get_heads());
        let snap = qdoc::model::materialize(
            &a.doc,
            &qdoc::model::open(&a.doc, &automerge::ROOT).unwrap(),
        );
        assert!(snap.changes.is_empty(), "accepted change left review state");
        assert_eq!(snap.comments.len(), 1, "unrelated comment remains");
    }

    #[test]
    fn durable_comment_reply_action_replays_and_resolve_cleans_the_thread() {
        let (mut a, ids) = seeded("Review this paragraph\n");
        let block = ids[0].clone();
        let containers = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let comment = {
            let mut ids = qdoc::model::seeded_id_gen(&a.doc, &containers);
            let mut dm = DocMut {
                doc: &mut a.doc,
                c: &containers,
                ids: &mut ids,
            };
            dm.add_comment(&block, 0, 6, "Maya", "Please clarify")
        };
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let reply = reply_to_comment_action(&comment, "Jonas", "2026-09-07", "Will do.");
        let envelope = tilo_actions::apply_local(&mut a.doc, &reply, encode(&reply)).unwrap();
        tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "reply replays exactly"
        );
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let comments = qdoc::model::materialize_comments(&a.doc, &c);
        assert_eq!(comments[0].replies.len(), 1);
        assert_eq!(comments[0].replies[0].author, "Jonas");
        assert_eq!(comments[0].replies[0].blocks[0].text, "Will do.");

        let resolve = WordProcessorAction::ResolveComment { id: comment };
        let envelope = tilo_actions::apply_local(&mut a.doc, &resolve, encode(&resolve)).unwrap();
        tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "resolution replays exactly"
        );
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        assert!(qdoc::model::materialize_comments(&a.doc, &c).is_empty());
    }

    #[test]
    fn resolved_comment_tombstone_wins_over_divergent_reply() {
        let (mut a, ids) = seeded("Review this paragraph\n");
        let block = ids[0].clone();
        let add = add_comment_action(&block, 0, 6, "Maya", "2026-09-07", "Please clarify");
        dispatch(&mut a.doc, &add);
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let comment = qdoc::model::comment_ids(&a.doc, &c)
            .into_iter()
            .next()
            .unwrap();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        b.doc.set_actor(automerge::ActorId::random());

        let resolve = WordProcessorAction::ResolveComment {
            id: comment.clone(),
        };
        let reply = reply_to_comment_action(&comment, "Jonas", "2026-09-08", "Will do");
        tilo_actions::apply_local(&mut a.doc, &resolve, encode(&resolve)).unwrap();
        tilo_actions::apply_local(&mut b.doc, &reply, encode(&reply)).unwrap();
        a.doc.merge(&mut b.doc).unwrap();

        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        assert!(qdoc::model::comment_is_tombstoned(&a.doc, &c, &comment));
        assert!(
            qdoc::model::materialize_comments(&a.doc, &c).is_empty(),
            "a reply from a divergent replica cannot resurrect a resolved thread"
        );
    }

    #[test]
    fn resource_results_and_revisions_are_bounded_and_precisely_addressable() {
        let (mut state, ids) = seeded(&format!("{}\n", "x".repeat(MAX_RESOURCE_SEARCH_HITS + 16)));
        for index in 0..MAX_RESOURCE_REVISIONS + 8 {
            state
                .doc
                .put(automerge::ROOT, "revision-fixture", index as u64)
                .unwrap();
            state.doc.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(format!("Edit {index}")),
            );
        }
        let mut remote = state.doc.fork();
        remote.put(automerge::ROOT, "remote-fixture", true).unwrap();
        remote.commit();
        state
            .doc
            .put(automerge::ROOT, "local-fixture", true)
            .unwrap();
        state.doc.commit();
        state.doc.merge(&mut remote).unwrap();
        state
            .doc
            .put(automerge::ROOT, "unfinished-fixture", true)
            .unwrap();
        let pending = (state.doc.base_heads(), state.doc.pending_ops());
        let expected = state.doc.clone().get_changes(&[]);
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block.clone(),
            caret_offset: 0,
            target_blocks: vec![block.clone()],
            format_spans: Vec::new(),
            selection: Some(SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block.clone(),
                end: 4,
            }),
            region: RegionKind::Body,
            search_hits: (0..MAX_RESOURCE_SEARCH_HITS + 8)
                .map(|start| SearchTarget {
                    block: block.clone(),
                    start,
                    end: start + 1,
                })
                .collect(),
        };
        let resources = provide_resources(&ctx, &state.doc);
        let root = &resources[0];
        assert!(
            root.children
                .iter()
                .any(|resource| resource.kind == "word_processor.selection")
        );
        let results = root
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "word-processor.search-results")
            .expect("bounded search result group");
        assert_eq!(results.children.len(), MAX_RESOURCE_SEARCH_HITS);
        assert!(results.children.iter().all(|hit| {
            hit.actions.iter().any(|action| action.id == "go-to")
                && hit.id.as_str().contains(&block)
        }));
        let revisions = root
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "word-processor.revisions")
            .expect("history resource group");
        assert_eq!(revisions.children.len(), MAX_RESOURCE_REVISIONS);
        let start = expected.len() - MAX_RESOURCE_REVISIONS;
        for (index, (revision, change)) in revisions
            .children
            .iter()
            .zip(&expected[start..])
            .enumerate()
        {
            let index = start + index;
            assert_eq!(
                revision.id.as_str(),
                format!("word-processor.revision:{}", change.hash())
            );
            let label = change.message().map_or_else(
                || format!("Revision {}", index + 1),
                |message| short(message, "Revision"),
            );
            assert_eq!(revision.label, label);
            let show = revision
                .actions
                .iter()
                .find(|action| action.id == "show")
                .unwrap();
            assert_eq!(
                show.build_args(std::iter::empty::<(String, tilo_actions::ArgValue)>())
                    .unwrap(),
                EncodedAction::encode_operation(
                    WORD_PROCESSOR_OPERATION_TYPE,
                    &WordProcessorOperation::ShowRevision { index }
                )
                .unwrap()
            );
        }
        assert_eq!(
            (state.doc.base_heads(), state.doc.pending_ops()),
            pending,
            "resource discovery must not commit the owner"
        );
        assert!(
            revisions
                .children
                .iter()
                .all(|revision| revision.actions.iter().any(|action| action.id == "show"))
        );
    }

    #[test]
    fn stale_multi_target_action_is_an_atomic_noop() {
        let (mut state, ids) = seeded("first\nsecond\n");
        let before = state.doc.save();
        let action = set_block_attr_action(
            RegionKind::Body,
            vec![ids[0].clone(), "deleted-by-a-peer".to_string()],
            "align",
            AttrVal::Str("center".to_string()),
        );
        let result = action.apply(&mut state.doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(
            state.doc.save(),
            before,
            "one stale target must not leave the valid target partly changed"
        );
    }

    #[test]
    fn hostile_table_dimensions_are_rejected_before_any_write() {
        let (mut state, ids) = seeded("anchor\n");
        let before = state.doc.save();
        let result = insert_table_action(&ids[0], MAX_TABLE_DIMENSION + 1, 1).apply(&mut state.doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(state.doc.save(), before);
    }

    #[test]
    fn unbound_document_is_refused_without_a_write() {
        let mut doc = AutoCommit::new();
        let before = doc.save();
        let result = WordProcessorAction::ReplaceBody {
            markdown: "# should not initialize an arbitrary document".to_string(),
        }
        .apply(&mut doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn cross_block_selection_replacement_is_one_replayable_action() {
        let (mut a, ids) = seeded("alpha\n\nbeta\n\ngamma\n");
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        let action = replace_selection_action(RegionKind::Body, &ids[0], 2, &ids[2], 3, "X");
        let encoded = encode(&action);
        let envelope = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(a.doc.get_heads(), b.doc.get_heads());
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let blocks = qdoc::model::materialize(&a.doc, &c).blocks;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "alXma");
    }

    #[test]
    fn encoded_split_and_concurrent_splice_converge_on_divergent_replicas() {
        let (mut a, ids) = seeded("alpha beta\n");
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        b.doc.set_actor(automerge::ActorId::random());
        let block = ids[0].clone();

        let split = split_block_action(RegionKind::Body, &block, 5, "paragraph", None);
        let encoded_split = encode(&split);
        let decoded_split: WordProcessorAction =
            serde_json::from_slice(&encoded_split.payload).unwrap();
        tilo_actions::apply_local(&mut a.doc, &decoded_split, encoded_split).unwrap();

        let splice = splice_text_action(RegionKind::Body, &block, 0, 0, "Shared ");
        let encoded_splice = encode(&splice);
        let decoded_splice: WordProcessorAction =
            serde_json::from_slice(&encoded_splice.payload).unwrap();
        tilo_actions::apply_local(&mut b.doc, &decoded_splice, encoded_splice).unwrap();

        let mut merged_a = a.doc.clone();
        let mut b_for_a = b.doc.clone();
        merged_a.merge(&mut b_for_a).unwrap();
        let mut merged_b = b.doc.clone();
        let mut a_for_b = a.doc.clone();
        merged_b.merge(&mut a_for_b).unwrap();
        assert_eq!(
            body_blocks(&merged_a),
            body_blocks(&merged_b),
            "encoded actions converge regardless of merge direction"
        );
    }

    #[test]
    fn style_fields_merge_granularly_and_style_tombstone_wins() {
        let (mut base, ids) = seeded("Styled paragraph\n");
        let create = set_style_property_action(
            "custom1",
            qdoc::schema::STYLE_NAME,
            AttrVal::Str("Callout".into()),
        );
        dispatch(&mut base.doc, &create);
        dispatch(
            &mut base.doc,
            &set_block_attr_action(
                RegionKind::Body,
                vec![ids[0].clone()],
                "style",
                AttrVal::Str("custom1".into()),
            ),
        );
        let bytes = base.save();
        let mut a = ui::State::load(&bytes).unwrap();
        let mut b = ui::State::load(&bytes).unwrap();
        b.doc.set_actor(automerge::ActorId::random());

        let bold =
            set_style_property_action("custom1", qdoc::schema::STYLE_BOLD, AttrVal::Bool(true));
        let size =
            set_style_property_action("custom1", qdoc::schema::STYLE_SIZE, AttrVal::Float(28.0));
        tilo_actions::apply_local(&mut a.doc, &bold, encode(&bold)).unwrap();
        tilo_actions::apply_local(&mut b.doc, &size, encode(&size)).unwrap();
        a.doc.merge(&mut b.doc).unwrap();
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let properties = qdoc::model::style_props(&a.doc, &c, "custom1");
        assert_eq!(
            properties.get(qdoc::schema::STYLE_BOLD),
            Some(&qdoc::AttrVal::Bool(true))
        );
        assert_eq!(
            properties.get(qdoc::schema::STYLE_SIZE),
            Some(&qdoc::AttrVal::Float(28.0))
        );

        let bytes = a.save();
        let mut deleted = ui::State::load(&bytes).unwrap();
        let mut divergent = ui::State::load(&bytes).unwrap();
        divergent.doc.set_actor(automerge::ActorId::random());
        let remove = delete_style_action("custom1", vec![ids[0].clone()]);
        let rename = set_style_property_action(
            "custom1",
            qdoc::schema::STYLE_NAME,
            AttrVal::Str("Late rename".into()),
        );
        tilo_actions::apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
        tilo_actions::apply_local(&mut divergent.doc, &rename, encode(&rename)).unwrap();
        deleted.doc.merge(&mut divergent.doc).unwrap();
        let c = qdoc::model::open(&deleted.doc, &automerge::ROOT).unwrap();
        assert!(qdoc::model::style_is_tombstoned(
            &deleted.doc,
            &c,
            "custom1"
        ));
        assert!(!qdoc::model::style_ids(&deleted.doc, &c).contains(&"custom1".to_string()));
    }

    #[test]
    fn toc_field_rich_paste_and_table_edits_encode_and_replay_identically() {
        let (mut origin, ids) = seeded("# Heading\n\nBody\n");
        let bytes = origin.save();
        let mut replay = ui::State::load(&bytes).unwrap();
        let heading = ids[0].clone();
        let body = ids[1].clone();
        let fragment = crate::word_processor::ClipBlock {
            kind: "heading".into(),
            text: "Pasted title".into(),
            marks: vec![qdoc::document::MarkSpan {
                name: "strong".into(),
                start: 0,
                end: 6,
                value: qdoc::document::MarkValue::Bool(true),
            }],
            level: Some(2),
            ordered: None,
            align: Some("center".into()),
        };
        let actions = [
            generate_toc_action(vec![TocTarget {
                heading,
                level: 1,
                text: "Heading".into(),
                page: Some(1),
            }]),
            insert_page_number_action(RegionKind::Body, &body, 0),
            paste_fragment_action(
                RegionKind::Body,
                SelectionTarget {
                    start_block: body.clone(),
                    start: 1,
                    end_block: body.clone(),
                    end: 1,
                },
                vec![fragment],
                false,
            ),
            insert_table_action(&body, 2, 2),
        ];
        for action in actions {
            let encoded = encode(&action);
            let decoded: WordProcessorAction = serde_json::from_slice(&encoded.payload).unwrap();
            let envelope = tilo_actions::apply_local(&mut origin.doc, &decoded, encoded).unwrap();
            tilo_actions::replay(&mut replay.doc, &envelope, &registry()).unwrap();
        }
        let table = body_blocks(&origin.doc)
            .iter()
            .find_map(|block| match block.attrs.get("table") {
                Some(qdoc::AttrVal::Str(id)) => Some(id.clone()),
                _ => None,
            })
            .unwrap();
        for edit in [
            TableEdit::InsertRow { at: 1 },
            TableEdit::InsertColumn { at: 1 },
            TableEdit::DeleteRow { row: 0 },
            TableEdit::DeleteColumn { column: 0 },
        ] {
            let action = edit_table_action(&table, edit);
            let encoded = encode(&action);
            let decoded: WordProcessorAction = serde_json::from_slice(&encoded.payload).unwrap();
            let envelope = tilo_actions::apply_local(&mut origin.doc, &decoded, encoded).unwrap();
            tilo_actions::replay(&mut replay.doc, &envelope, &registry()).unwrap();
        }
        assert_eq!(origin.doc.get_heads(), replay.doc.get_heads());
        assert_eq!(body_blocks(&origin.doc), body_blocks(&replay.doc));
    }

    #[test]
    fn table_tombstone_wins_over_a_divergent_cell_edit() {
        let (mut base, ids) = seeded("Anchor\n");
        dispatch(&mut base.doc, &insert_table_action(&ids[0], 2, 2));
        let blocks = body_blocks(&base.doc);
        let cell = blocks.iter().find(|block| block.kind == "cell").unwrap();
        let table = match cell.attrs.get("table") {
            Some(qdoc::AttrVal::Str(id)) => id.clone(),
            other => panic!("cell has stable table id: {other:?}"),
        };
        let bytes = base.save();
        let mut deleted = ui::State::load(&bytes).unwrap();
        let mut divergent = ui::State::load(&bytes).unwrap();
        divergent.doc.set_actor(automerge::ActorId::random());

        let remove = edit_table_action(&table, TableEdit::Delete);
        let late_edit = splice_text_action(RegionKind::Body, &cell.id, 0, 0, "late");
        tilo_actions::apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
        tilo_actions::apply_local(&mut divergent.doc, &late_edit, encode(&late_edit)).unwrap();

        let mut merged_a = deleted.doc.clone();
        let mut divergent_for_a = divergent.doc.clone();
        merged_a.merge(&mut divergent_for_a).unwrap();
        let mut merged_b = divergent.doc.clone();
        let mut deleted_for_b = deleted.doc.clone();
        merged_b.merge(&mut deleted_for_b).unwrap();
        assert_eq!(body_blocks(&merged_a), body_blocks(&merged_b));
        assert!(body_blocks(&merged_a)
            .iter()
            .all(|block| !matches!(block.attrs.get("table"), Some(qdoc::AttrVal::Str(id)) if id == &table)));
    }
}
