//! Pure editor input primitives.
//!
//! Every function here is what the keyboard, toolbar, menus, Ctrl+K and an
//! agent all call to turn "what the participant did" into positional
//! [`RichTextAction`] envelopes plus the participant's next view:
//!
//! ```text
//! (&ParticipantView, &AutoCommit, args) -> Result<(Vec<RichTextAction>, ParticipantView), EditingError>
//! ```
//!
//! - Pure: no widget state, clock, or RNG. The returned envelopes stay
//!   positional (`SpliceText`, `ReplaceSelection`, `SplitBlock`, …) so replay
//!   is unchanged; only the build step reads the invoker's view.
//! - View-only operations (`move_caret`, `select_*`, `find`) return no
//!   envelopes and only a new view.
//! - An empty envelope list with an unchanged view is a no-op (for example
//!   Backspace at document start), not an error.
//! - An edit that creates a block whose id is minted at apply time (Enter,
//!   a multi-paragraph paste) returns a view with a
//!   [`PendingCaret`](crate::actions::participant::PendingCaret); it resolves through
//!   [`ParticipantView::settle`](crate::actions::participant::ParticipantView::settle) once the envelope is applied, and every
//!   function here settles its input first.
//!
//! The same logic backs the root operations published on
//! `word-processor.document` (`add_operations`): each operation resolves a
//! small *target* from the invoker's view at tree-build time and its build
//! closure finishes the same computation from the arguments, so an operation
//! invoked through Ctrl+K or by an agent encodes byte-for-byte what the
//! matching keyboard/toolbar path dispatches for the same view.

use automerge::AutoCommit;
#[cfg(test)]
use ui::widget::Edits;

use tilo_document::{BodyOrder, ClipAttr};

use super::participant::{FindState, ParticipantView, PendingCaret, ViewPoint};
use super::{
    MAX_ACTION_TEXT_CHARS, MAX_RESOURCE_SEARCH_HITS, MarkVal, RegionKind, RichTextAction,
    SearchTarget, SelectionTarget, Span, WpResourceCtx, apply_named_style_action,
    paste_fragment_action, replace_selection_action, set_mark_action, set_paragraph_style_action,
    splice_text_action, split_block_action, valid_hyperlink_target,
};
use crate::word_processor::ClipBlock;

/// Most characters one `erase-text` may remove without a selection
/// (an unattended caller cannot wipe a document one large delete at a time).
pub const MAX_ERASE_COUNT: usize = 256;
/// A selection larger than this (characters, a paragraph join counting as
/// one) is refused by the published `type-text`, `erase-text`, `paste` and
/// `replace-selection` operations, for every invoker, so a remote caller cannot
/// replace a large selection in one step;
/// the keyboard path is unaffected.
pub const LARGE_REMOVAL_CHARS: usize = 2_048;
/// Longest Find query accepted, bounding the per-participant state and the
/// search work one remote caller can request.
pub const MAX_FIND_QUERY_BYTES: usize = 256;

/// Why a primitive produced no edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditingError {
    /// The view has no caret, or its caret block is not in the document.
    NoCaret,
    /// The primitive needs a non-empty selection.
    NeedsSelection,
    /// Arguments are out of range or stale for this document.
    Invalid(String),
}

impl std::fmt::Display for EditingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCaret => f.write_str("needs-caret: Place the caret in the document first"),
            Self::NeedsSelection => f.write_str("needs-selection: Select some text first"),
            Self::Invalid(reason) => write!(f, "invalid arguments: {reason}"),
        }
    }
}

impl std::error::Error for EditingError {}

/// Envelopes to dispatch, and the participant's view after them.
pub type EditOutcome = (Vec<RichTextAction>, ParticipantView);
pub type EditResult = Result<EditOutcome, EditingError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteDirection {
    /// Backspace.
    Backward,
    Forward,
}

/// Toggleable inline marks (Ctrl+B/I/U, toolbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMark {
    Strong,
    Em,
    Underline,
    Strike,
    Superscript,
    Subscript,
}

impl InlineMark {
    pub const ALL: [Self; 6] = [
        Self::Strong,
        Self::Em,
        Self::Underline,
        Self::Strike,
        Self::Superscript,
        Self::Subscript,
    ];

    /// The tilo-doc mark name (`strong`, `em`, `underline`, `strike`, `super`,
    /// `sub`); also the `toggle-mark` argument spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Em => "em",
            Self::Underline => "underline",
            Self::Strike => "strike",
            Self::Superscript => "super",
            Self::Subscript => "sub",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mark| mark.name() == name)
    }

    /// Super/subscript are a mutually exclusive pair that, like the toolbar,
    /// also apply to the whole caret block without a selection.
    fn sibling(self) -> Option<Self> {
        match self {
            Self::Superscript => Some(Self::Subscript),
            Self::Subscript => Some(Self::Superscript),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Strong => 0,
            Self::Em => 1,
            Self::Underline => 2,
            Self::Strike => 3,
            Self::Superscript => 4,
            Self::Subscript => 5,
        }
    }
}

/// Paragraph kind for the style menu. `kind` is a tilo-doc block kind
/// (`paragraph`, `heading`, `list_item`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParagraphStyle {
    pub kind: String,
    pub level: Option<i64>,
    pub ordered: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteContent {
    Text(String),
    Markdown(String),
}

/// Where `move-caret` puts the caret. Serialized inside the
/// `word-processor.view-operation` payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "to", rename_all = "kebab-case")]
pub enum CaretTarget {
    /// A character offset in one block of the view's region.
    Point {
        block: String,
        offset: usize,
    },
    DocumentStart,
    DocumentEnd,
    BlockEnd,
}

// ---------------------------------------------------------------------------
// Reading the participant's region
// ---------------------------------------------------------------------------

type Range3 = (String, usize, usize);

/// Read access to one region. The body's order comes from the caller's kept
/// [`BodyOrder`] when it has one, so finding a block's neighbours costs
/// `O(log n)`; otherwise the region is listed (a scan and sort of the whole
/// region) only when an operation needs it, so typing and erasing inside one
/// block touch only that block (keystroke latency).
struct RegionDoc<'a> {
    doc: &'a AutoCommit,
    containers: tilo_doc::Containers,
    region: RegionKind,
    /// The kept body order, in step with `doc`; used only for the body.
    body: Option<&'a BodyOrder>,
    listed: std::cell::OnceCell<Vec<String>>,
}

impl<'a> RegionDoc<'a> {
    fn open(doc: &'a AutoCommit, region: RegionKind) -> Result<Self, EditingError> {
        Self::open_in(doc, None, region)
    }

    fn open_in(
        doc: &'a AutoCommit,
        body: Option<&'a BodyOrder>,
        region: RegionKind,
    ) -> Result<Self, EditingError> {
        let containers = crate::model::open(doc).map_err(|_| EditingError::NoCaret)?;
        Ok(Self {
            doc,
            containers,
            region,
            body: body.filter(|_| region == RegionKind::Body),
            listed: std::cell::OnceCell::new(),
        })
    }

    fn listed(&self) -> &[String] {
        self.listed.get_or_init(|| {
            tilo_doc::model::region_blocks_in_order(
                self.doc,
                &self.containers,
                self.region.to_region(),
            )
        })
    }

    fn index(&self, block: &str) -> Option<usize> {
        match self.body {
            Some(body) => body.position(block),
            None => self.listed().iter().position(|id| id == block),
        }
    }

    /// The block at position `index` of the region.
    fn id_at(&self, index: usize) -> Option<&str> {
        match self.body {
            Some(body) => body.id_at(index),
            None => self.listed().get(index).map(String::as_str),
        }
    }

    /// Blocks in the region.
    fn count(&self) -> usize {
        match self.body {
            Some(body) => body.len(),
            None => self.listed().len(),
        }
    }

    fn first(&self) -> Option<&str> {
        self.id_at(0)
    }

    fn last(&self) -> Option<&str> {
        self.count()
            .checked_sub(1)
            .and_then(|last| self.id_at(last))
    }

    /// Characters of a region block, `None` when it is not a block of this
    /// region — the executor's own membership/length reading, one block only.
    fn block_len(&self, block: &str) -> Option<usize> {
        super::splice_target_len(self.doc, &self.containers, self.region, block)
    }

    fn len(&self, block: &str) -> usize {
        self.block_len(block).unwrap_or(0)
    }

    /// Text of a region block, reading only that block in the body.
    fn text(&self, block: &str) -> Option<String> {
        match self.region {
            RegionKind::Body => tilo_doc::model::block_text(self.doc, &self.containers, block),
            _ => self.block(block).map(|view| view.text),
        }
    }

    fn block(&self, block: &str) -> Option<tilo_doc::snapshot::BlockView> {
        match self.region {
            RegionKind::Body => {
                tilo_doc::model::materialize_blocks(self.doc, &self.containers, &[block.to_owned()])
                    .pop()
            }
            region => {
                tilo_doc::model::materialize_region(self.doc, &self.containers, region.to_region())
                    .into_iter()
                    .find(|view| view.id == block)
            }
        }
    }

    fn point(&self, block: &str, offset: usize) -> Result<ViewPoint, EditingError> {
        if self.block_len(block).is_none() {
            return Err(EditingError::Invalid(format!(
                "block `{block}` is not in this region"
            )));
        }
        if offset > self.len(block) {
            return Err(EditingError::Invalid(format!(
                "offset {offset} is past the end of block `{block}`"
            )));
        }
        Ok(ViewPoint::new(block, offset))
    }
}

/// A settled view whose caret is a real block of its region.
struct Resolved<'a> {
    rd: RegionDoc<'a>,
    view: ParticipantView,
    caret_len: usize,
    /// Anchor set and different from the caret (the widget's `has_selection`).
    has_anchor: bool,
    /// Ordered selection when both ends are blocks of the region.
    selection: Option<(ViewPoint, ViewPoint)>,
}

fn resolve<'a>(view: &ParticipantView, doc: &'a AutoCommit) -> Result<Resolved<'a>, EditingError> {
    resolve_in(view, doc, None)
}

/// [`resolve`] reading the body's order from `body`, the kept order of `doc`.
fn resolve_in<'a>(
    view: &ParticipantView,
    doc: &'a AutoCommit,
    body: Option<&'a BodyOrder>,
) -> Result<Resolved<'a>, EditingError> {
    let mut view = if view.pending.is_some() {
        view.settle_in(doc, body)
    } else {
        view.clone()
    };
    let rd = RegionDoc::open_in(doc, body, view.region)?;
    let caret_len = rd
        .block_len(&view.caret.block)
        .ok_or(EditingError::NoCaret)?;
    view.caret.offset = view.caret.offset.min(caret_len);
    if let Some(anchor) = view.anchor.as_mut()
        && let Some(len) = rd.block_len(&anchor.block)
    {
        anchor.offset = anchor.offset.min(len);
    }
    let has_anchor = view
        .anchor
        .as_ref()
        .is_some_and(|anchor| *anchor != view.caret);
    let selection = match &view.anchor {
        Some(anchor) if has_anchor && anchor.block == view.caret.block => {
            // One block: ordered by offset, no region scan.
            Some(if anchor.offset <= view.caret.offset {
                (anchor.clone(), view.caret.clone())
            } else {
                (view.caret.clone(), anchor.clone())
            })
        }
        Some(_) if has_anchor => view
            .ordered_selection_by(|block| rd.index(block))
            .map(|(start, end)| (start.clone(), end.clone())),
        _ => None,
    };
    Ok(Resolved {
        rd,
        view,
        caret_len,
        has_anchor,
        selection,
    })
}

impl Resolved<'_> {
    fn caret_index(&self) -> usize {
        self.rd.index(&self.view.caret.block).unwrap_or(0)
    }

    /// Per-block `(block, start, end)` over the selection, empty ranges kept
    /// (`WordProcessor::selection_blocks`).
    fn selection_blocks(&self) -> Vec<Range3> {
        let Some((start, end)) = &self.selection else {
            return Vec::new();
        };
        if start.block == end.block {
            return vec![(start.block.clone(), start.offset, end.offset)];
        }
        let (Some(si), Some(ei)) = (self.rd.index(&start.block), self.rd.index(&end.block)) else {
            return Vec::new();
        };
        (si..=ei)
            .filter_map(|i| {
                let block = self.rd.id_at(i)?;
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei {
                    end.offset
                } else {
                    self.rd.len(block)
                };
                Some((block.to_owned(), s, e))
            })
            .collect()
    }

    /// The selection, or the whole caret block without one
    /// (`WordProcessor::font_ranges`).
    fn font_ranges(&self) -> Vec<Range3> {
        if self.has_anchor {
            self.selection_blocks()
        } else {
            vec![(self.view.caret.block.clone(), 0, self.caret_len)]
        }
    }

    /// The selection's blocks, or the caret block
    /// (`WordProcessor::selected_block_ids`).
    fn selected_block_ids(&self) -> Vec<String> {
        let selected = self.selection_blocks();
        if selected.is_empty() {
            vec![self.view.caret.block.clone()]
        } else {
            selected.into_iter().map(|(block, _, _)| block).collect()
        }
    }

    /// Characters a replacement of the selection removes, each paragraph
    /// join counting as one.
    fn removed_chars(&self) -> usize {
        let blocks = self.selection_blocks();
        let joins = blocks.len().saturating_sub(1);
        blocks
            .iter()
            .map(|(_, s, e)| e.saturating_sub(*s))
            .sum::<usize>()
            .saturating_add(joins)
    }

    fn has_mark_over_region(&self, block: &str, start: usize, end: usize, name: &str) -> bool {
        match self.rd.region {
            RegionKind::Body => tilo_doc::model::has_mark_over(
                self.rd.doc,
                &self.rd.containers,
                block,
                start,
                end,
                name,
            ),
            _ => self.rd.block(block).is_some_and(|view| {
                view.marks
                    .iter()
                    .any(|mark| mark.name == name && mark.start <= start && mark.end >= end)
            }),
        }
    }
}

fn non_empty_spans(ranges: &[Range3]) -> Vec<Span> {
    ranges
        .iter()
        .filter(|(_, s, e)| s < e)
        .map(|(block, start, end)| Span {
            block: block.clone(),
            start: *start,
            end: *end,
        })
        .collect()
}

fn collapsed(view: &ParticipantView, caret: ViewPoint) -> ParticipantView {
    ParticipantView {
        caret,
        anchor: None,
        pending: None,
        ..view.clone()
    }
}

// ---------------------------------------------------------------------------
// Targets: what an operation resolves from the view before its arguments
// ---------------------------------------------------------------------------

/// Typing target: the selection it replaces, or the caret it inserts at.
#[derive(Debug, Clone)]
pub(crate) struct TypeTarget {
    region: RegionKind,
    view: ParticipantView,
    selection: Option<(ViewPoint, ViewPoint)>,
}

impl TypeTarget {
    fn new(r: &Resolved<'_>) -> Self {
        Self {
            region: r.rd.region,
            view: r.view.clone(),
            selection: r.selection.clone(),
        }
    }

    fn build(&self, text: &str) -> EditOutcome {
        let typed = text.chars().count();
        if let Some((start, end)) = &self.selection {
            let action = replace_selection_action(
                self.region,
                &start.block,
                start.offset,
                &end.block,
                end.offset,
                text,
            );
            let caret = ViewPoint::new(&start.block, start.offset.saturating_add(typed));
            return (vec![action], collapsed(&self.view, caret));
        }
        let caret = &self.view.caret;
        let action =
            splice_text_action(self.region, &caret.block, caret.offset, caret.offset, text);
        let mut next = self.view.clone();
        next.caret.offset = caret.offset.saturating_add(typed);
        (vec![action], next)
    }
}

/// Erase target: the selection, or a bounded window of blocks around the
/// caret (enough for [`MAX_ERASE_COUNT`] characters in either direction).
#[derive(Debug, Clone)]
pub(crate) struct EraseTarget {
    typing: TypeTarget,
    /// The caret block's text, so one keystroke erases a whole grapheme
    /// cluster.
    caret_text: String,
    caret_kind: String,
    /// `(block, chars)` of the region's blocks around the caret.
    window: Vec<(String, usize)>,
    caret_at: usize,
    starts_region: bool,
    ends_region: bool,
}

impl EraseTarget {
    /// `need`: the one erase about to be built (the keyboard path), or `None`
    /// for a published operation that may be invoked with any arguments.
    /// A keystroke inside the caret block reads only that block; a block
    /// boundary, a larger count or a published operation reads the bounded
    /// window of neighbours.
    fn new(r: &Resolved<'_>, need: Option<(DeleteDirection, usize)>) -> Self {
        let offset = r.view.caret.offset;
        let local = r.selection.is_some()
            || need.is_some_and(|(direction, count)| {
                count == 1
                    && match direction {
                        DeleteDirection::Backward => offset > 0,
                        DeleteDirection::Forward => offset < r.caret_len,
                    }
            });
        if local {
            return Self {
                typing: TypeTarget::new(r),
                caret_text: r.rd.text(&r.view.caret.block).unwrap_or_default(),
                caret_kind: String::new(),
                window: vec![(r.view.caret.block.clone(), r.caret_len)],
                caret_at: 0,
                starts_region: false,
                ends_region: false,
            };
        }
        let caret_index = r.caret_index();
        let first = caret_index.saturating_sub(MAX_ERASE_COUNT + 1);
        let last = caret_index
            .saturating_add(MAX_ERASE_COUNT + 2)
            .min(r.rd.count());
        let window = (first..last)
            .filter_map(|index| r.rd.id_at(index))
            .map(|block| (block.to_owned(), r.rd.len(block)))
            .collect();
        Self {
            typing: TypeTarget::new(r),
            caret_text: r.rd.text(&r.view.caret.block).unwrap_or_default(),
            caret_kind: r
                .rd
                .block(&r.view.caret.block)
                .map_or_else(|| "paragraph".to_owned(), |view| view.kind),
            window,
            caret_at: caret_index - first,
            starts_region: first == 0,
            ends_region: last == r.rd.count(),
        }
    }

    fn build(&self, direction: DeleteDirection, count: usize) -> EditResult {
        if count == 0 || count > MAX_ERASE_COUNT {
            return Err(EditingError::Invalid(format!(
                "count must be between 1 and {MAX_ERASE_COUNT}"
            )));
        }
        if self.typing.selection.is_some() {
            return Ok(self.typing.build(""));
        }
        if count == 1 {
            return Ok(self.keystroke(direction));
        }
        self.extend_and_erase(direction, count)
    }

    /// One Backspace/Delete exactly as the keyboard handles it.
    fn keystroke(&self, direction: DeleteDirection) -> EditOutcome {
        let view = &self.typing.view;
        let region = self.typing.region;
        let caret = &view.caret;
        let unchanged = (Vec::new(), view.clone());
        let Some((_, len)) = self.window.get(self.caret_at) else {
            return unchanged;
        };
        let len = *len;
        let backward = direction == DeleteDirection::Backward;
        if backward && caret.offset == 0 && self.caret_kind == "list_item" {
            let action = set_paragraph_style_action(
                region,
                vec![caret.block.clone()],
                "paragraph",
                None,
                None,
            );
            return (vec![action], view.clone());
        }
        if (backward && caret.offset > 0) || (!backward && caret.offset < len) {
            let (start, end) = self.grapheme_at_caret(backward);
            let action = splice_text_action(region, &caret.block, start, end, "");
            let next = ViewPoint::new(&caret.block, start);
            return (vec![action], collapsed(view, next));
        }
        if backward && self.caret_at > 0 {
            let Some((previous, previous_len)) = self.window.get(self.caret_at - 1) else {
                return unchanged;
            };
            let action =
                replace_selection_action(region, previous, *previous_len, &caret.block, 0, "");
            return (
                vec![action],
                collapsed(view, ViewPoint::new(previous, *previous_len)),
            );
        }
        if !backward && let Some((next, _)) = self.window.get(self.caret_at + 1) {
            let action = replace_selection_action(region, &caret.block, len, next, 0, "");
            return (vec![action], collapsed(view, caret.clone()));
        }
        (Vec::new(), collapsed(view, caret.clone()))
    }

    /// The char range one Backspace (`backward`) or Delete removes inside the
    /// caret block: the whole grapheme cluster, through the shared core.
    fn grapheme_at_caret(&self, backward: bool) -> (usize, usize) {
        let offset = self.typing.view.caret.offset;
        let text = &self.caret_text;
        let byte = text
            .char_indices()
            .nth(offset)
            .map_or(text.len(), |(byte, _)| byte);
        let mut core = ui::text_edit::EditCore::at(byte);
        let edit = if backward {
            core.delete_backward(text, false)
        } else {
            core.delete_forward(text, false)
        };
        match edit.char_splice(text) {
            Some(splice) if splice.removed > 0 => {
                (splice.at, splice.at.saturating_add(splice.removed))
            }
            // The block's text could not be read: one character, as before.
            _ if backward => (offset.saturating_sub(1), offset),
            _ => (offset, offset.saturating_add(1)),
        }
    }

    /// Shift+Arrow `count` times, then Backspace: a paragraph boundary is one
    /// character, the extent stops at the region's edge, and the extended
    /// selection is erased as one `ReplaceSelection`.
    fn extend_and_erase(&self, direction: DeleteDirection, count: usize) -> EditResult {
        let view = &self.typing.view;
        let mut index = self.caret_at;
        let mut offset = view.caret.offset;
        let mut remaining = count;
        while remaining > 0 {
            let len = self.window.get(index).map_or(0, |(_, len)| *len);
            match direction {
                DeleteDirection::Backward if offset > 0 => {
                    let take = offset.min(remaining);
                    offset -= take;
                    remaining -= take;
                }
                DeleteDirection::Backward if index > 0 => {
                    index -= 1;
                    offset = self.window.get(index).map_or(0, |(_, len)| *len);
                    remaining -= 1;
                }
                DeleteDirection::Backward if self.starts_region => break,
                DeleteDirection::Forward if offset < len => {
                    let take = (len - offset).min(remaining);
                    offset += take;
                    remaining -= take;
                }
                DeleteDirection::Forward if index + 1 < self.window.len() => {
                    index += 1;
                    offset = 0;
                    remaining -= 1;
                }
                DeleteDirection::Forward if self.ends_region => break,
                _ => {
                    return Err(EditingError::Invalid(
                        "erase extent is outside the resolved window".into(),
                    ));
                }
            }
        }
        let Some((block, _)) = self.window.get(index) else {
            return Err(EditingError::NoCaret);
        };
        let other = ViewPoint::new(block, offset);
        if other == view.caret {
            return Ok((Vec::new(), view.clone()));
        }
        let (start, end) = match direction {
            DeleteDirection::Backward => (other, view.caret.clone()),
            DeleteDirection::Forward => (view.caret.clone(), other),
        };
        let action = replace_selection_action(
            self.typing.region,
            &start.block,
            start.offset,
            &end.block,
            end.offset,
            "",
        );
        Ok((vec![action], collapsed(view, start)))
    }
}

/// Inline-mark target: the ranges each mark family covers and whether each
/// toggle mark is already on over them.
#[derive(Debug, Clone)]
pub(crate) struct MarkTarget {
    region: RegionKind,
    view: ParticipantView,
    selection_blocks: Vec<Range3>,
    font_ranges: Vec<Range3>,
    active: [bool; 6],
}

impl MarkTarget {
    fn new(r: &Resolved<'_>) -> Self {
        let selection_blocks = r.selection_blocks();
        let font_ranges = r.font_ranges();
        // `ALL` is in `index()` order, so the mapped array is indexed by mark.
        let active = InlineMark::ALL.map(|mark| {
            if mark.sibling().is_none() {
                // `plan_toggle_mark`: over the selection, region-aware.
                let spans: Vec<_> = selection_blocks.iter().filter(|(_, s, e)| s < e).collect();
                !spans.is_empty()
                    && spans
                        .iter()
                        .all(|(b, s, e)| r.has_mark_over_region(b, *s, *e, mark.name()))
            } else {
                // `plan_exclusive_mark`: over the font ranges, body reader.
                let spans: Vec<_> = font_ranges.iter().filter(|(_, s, e)| s < e).collect();
                !spans.is_empty()
                    && spans.iter().all(|(b, s, e)| {
                        tilo_doc::model::has_mark_over(
                            r.rd.doc,
                            &r.rd.containers,
                            b,
                            *s,
                            *e,
                            mark.name(),
                        )
                    })
            }
        });
        Self {
            region: r.rd.region,
            view: r.view.clone(),
            selection_blocks,
            font_ranges,
            active,
        }
    }

    fn toggle(&self, mark: InlineMark) -> EditOutcome {
        let active = self.active.get(mark.index()).copied().unwrap_or(false);
        self.turn(mark, !active)
    }

    /// Turn `mark` on or off over the selection whatever it covers now, so
    /// repeating it changes nothing.
    fn turn(&self, mark: InlineMark, on: bool) -> EditOutcome {
        let value = on.then_some(MarkVal::Bool(true));
        let (ranges, also_clear) = match mark.sibling() {
            None => (&self.selection_blocks, Vec::new()),
            Some(sibling) => (
                &self.font_ranges,
                if on {
                    vec![sibling.name().to_owned()]
                } else {
                    Vec::new()
                },
            ),
        };
        let spans = non_empty_spans(ranges);
        if spans.is_empty() {
            return (Vec::new(), self.view.clone());
        }
        let action = set_mark_action(self.region, spans, mark.name(), value, also_clear);
        (vec![action], self.view.clone())
    }

    fn set(&self, name: &str, value: Option<MarkVal>) -> EditOutcome {
        let spans = non_empty_spans(&self.font_ranges);
        if spans.is_empty() {
            return (Vec::new(), self.view.clone());
        }
        let action = set_mark_action(self.region, spans, name, value, Vec::new());
        (vec![action], self.view.clone())
    }
}

/// Paragraph-style target: the selected blocks (or caret block), minus
/// table cells.
#[derive(Debug, Clone)]
pub(crate) struct StyleTarget {
    region: RegionKind,
    view: ParticipantView,
    blocks: Vec<String>,
}

impl StyleTarget {
    fn new(r: &Resolved<'_>) -> Self {
        let blocks = r
            .selected_block_ids()
            .into_iter()
            .filter(|id| {
                tilo_doc::model::block_kind(r.rd.doc, &r.rd.containers, id).as_deref()
                    != Some("cell")
            })
            .collect();
        Self {
            region: r.rd.region,
            view: r.view.clone(),
            blocks,
        }
    }

    fn paragraph(&self, style: &ParagraphStyle) -> EditOutcome {
        if self.blocks.is_empty() {
            return (Vec::new(), self.view.clone());
        }
        let action = set_paragraph_style_action(
            self.region,
            self.blocks.clone(),
            &style.kind,
            style.level,
            style.ordered,
        );
        (vec![action], self.view.clone())
    }

    fn named(&self, style: &str) -> EditOutcome {
        if self.blocks.is_empty() {
            return (Vec::new(), self.view.clone());
        }
        let (kind, level, ordered) = match crate::styles::structural_kind_for(style) {
            Some((kind, level, ordered)) => (Some(kind), level, ordered),
            None => (None, None, None),
        };
        let action = apply_named_style_action(
            self.region,
            self.blocks.clone(),
            style,
            kind,
            level,
            ordered,
        );
        (vec![action], self.view.clone())
    }
}

/// Paste target: the range the fragment replaces (the selection, or the
/// collapsed caret) and what the widget reads to adopt the first style.
#[derive(Debug, Clone)]
pub(crate) struct PasteTarget {
    region: RegionKind,
    view: ParticipantView,
    start: ViewPoint,
    end: ViewPoint,
    end_len: usize,
    end_next: Option<String>,
}

impl PasteTarget {
    fn new(r: &Resolved<'_>) -> Self {
        let caret = r.view.caret.clone();
        let (start, end) = r
            .selection
            .clone()
            .unwrap_or_else(|| (caret.clone(), caret));
        let end_next =
            r.rd.index(&end.block)
                .and_then(|index| r.rd.id_at(index + 1).map(str::to_owned));
        Self {
            region: r.rd.region,
            view: r.view.clone(),
            end_len: r.rd.len(&end.block),
            start,
            end,
            end_next,
        }
    }

    fn build(&self, blocks: Vec<ClipBlock>) -> EditOutcome {
        let adopt_first_style = self.start.offset == 0 && self.end.offset == self.end_len;
        let final_offset = blocks.last().map_or(0, |block| block.text.chars().count());
        let count = blocks.len();
        let action = paste_fragment_action(
            self.region,
            SelectionTarget {
                start_block: self.start.block.clone(),
                start: self.start.offset,
                end_block: self.end.block.clone(),
                end: self.end.offset,
            },
            blocks,
            adopt_first_style,
        );
        if count <= 1 {
            let caret = ViewPoint::new(
                &self.start.block,
                self.start.offset.saturating_add(final_offset),
            );
            return (vec![action], collapsed(&self.view, caret));
        }
        // The widget falls back to the start block at the last fragment's
        // length when it cannot find the new blocks.
        let mut next = collapsed(&self.view, ViewPoint::new(&self.start.block, final_offset));
        next.pending = Some(PendingCaret {
            after: self.start.block.clone(),
            was_next: self.end_next.clone(),
            steps: count - 1,
            offset: final_offset,
        });
        (vec![action], next)
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// Typing: replace the selection, or insert at the caret; the caret advances
/// past the inserted text and the selection collapses.
pub fn type_text(view: &ParticipantView, doc: &AutoCommit, text: &str) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(TypeTarget::new(&r).build(text))
}

/// Enter: split the caret block (`SplitBlock`), or turn an empty list item
/// back into a paragraph. Replaces a non-empty selection first (two
/// envelopes, like the keyboard). The caret lands in the new block once the
/// split is applied ([`ParticipantView::settle`]).
pub fn new_paragraph(view: &ParticipantView, doc: &AutoCommit) -> EditResult {
    new_paragraph_in(view, doc, None)
}

/// [`new_paragraph`] reading the body's order from `body`, the kept order of
/// `doc`, so the paragraph that follows is found without listing the body.
///
/// # Errors
///
/// As [`new_paragraph`].
pub fn new_paragraph_in(
    view: &ParticipantView,
    doc: &AutoCommit,
    body: Option<&BodyOrder>,
) -> EditResult {
    let r = resolve_in(view, doc, body)?;
    let region = r.rd.region;
    let mut actions = Vec::new();
    let mut next = r.view.clone();
    let (caret, following) = if let Some((start, end)) = &r.selection {
        let (replace, typed) = TypeTarget::new(&r).build("");
        actions.extend(replace);
        next = typed;
        let end_index = r.rd.index(&end.block).unwrap_or_else(|| r.caret_index());
        (start.clone(), r.rd.id_at(end_index + 1).map(str::to_owned))
    } else {
        (
            r.view.caret.clone(),
            r.rd.id_at(r.caret_index() + 1).map(str::to_owned),
        )
    };
    // Like the widget, kind and emptiness are read before the replacement.
    let block = r.rd.block(&caret.block);
    let kind = block
        .as_ref()
        .map_or("paragraph", |entry| entry.kind.as_str());
    if kind == "list_item" && block.as_ref().is_some_and(|entry| entry.text.is_empty()) {
        actions.push(set_paragraph_style_action(
            region,
            vec![caret.block],
            "paragraph",
            None,
            None,
        ));
        return Ok((actions, next));
    }
    let ordered = block
        .as_ref()
        .filter(|_| kind == "list_item")
        .and_then(|entry| entry.bool_attr("ordered"));
    let new_kind = if kind == "list_item" {
        "list_item"
    } else {
        "paragraph"
    };
    actions.push(split_block_action(
        region,
        &caret.block,
        caret.offset,
        new_kind,
        ordered,
    ));
    next.pending = Some(PendingCaret {
        after: caret.block,
        was_next: following,
        steps: 1,
        offset: 0,
    });
    Ok((actions, next))
}

/// Backspace/Delete. With a selection, erase it (one `ReplaceSelection`).
/// `count == 1` is exactly one keystroke, including turning a list item back
/// into a paragraph at its start. A larger `count` (≤ [`MAX_ERASE_COUNT`])
/// is Shift+Arrow `count` times then Backspace: one `ReplaceSelection` over
/// that extent, a paragraph join counting as one character.
pub fn delete(
    view: &ParticipantView,
    doc: &AutoCommit,
    direction: DeleteDirection,
    count: usize,
) -> EditResult {
    let r = resolve(view, doc)?;
    EraseTarget::new(&r, Some((direction, count))).build(direction, count)
}

/// Toggle one inline mark on the selection (on unless it already covers every
/// selected span). Super/subscript, like the toolbar, cover the whole caret
/// block without a selection and clear their sibling when turned on.
pub fn toggle_mark(view: &ParticipantView, doc: &AutoCommit, mark: InlineMark) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(MarkTarget::new(&r).toggle(mark))
}

/// Set a valued inline mark (font, size, colour, highlight) on the selection,
/// or the whole caret block without one.
pub fn set_mark(
    view: &ParticipantView,
    doc: &AutoCommit,
    name: &str,
    value: MarkVal,
) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(MarkTarget::new(&r).set(name, Some(value)))
}

/// Remove one inline mark from the selection, or the whole caret block.
pub fn clear_mark(view: &ParticipantView, doc: &AutoCommit, name: &str) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(MarkTarget::new(&r).set(name, None))
}

/// Set paragraph kind/level/ordered on the selected blocks, or the caret block.
pub fn set_paragraph_style(
    view: &ParticipantView,
    doc: &AutoCommit,
    style: &ParagraphStyle,
) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(StyleTarget::new(&r).paragraph(style))
}

/// Apply a named document style to the selected blocks, or the caret block.
pub fn apply_named_style(view: &ParticipantView, doc: &AutoCommit, style: &str) -> EditResult {
    let r = resolve(view, doc)?;
    Ok(StyleTarget::new(&r).named(style))
}

/// Paste text or Markdown over the selection or at the caret
/// (`PasteFragment`); the caret lands after the pasted content.
pub fn paste(view: &ParticipantView, doc: &AutoCommit, content: &PasteContent) -> EditResult {
    let blocks = clip_blocks(content)?;
    let r = resolve(view, doc)?;
    Ok(PasteTarget::new(&r).build(blocks))
}

/// Move (and collapse) the caret. View-only: returns no envelopes.
pub fn move_caret(view: &ParticipantView, doc: &AutoCommit, to: &CaretTarget) -> EditResult {
    let base = view.settle(doc);
    let rd = RegionDoc::open(doc, base.region)?;
    let caret = match to {
        CaretTarget::Point { block, offset } => rd.point(block, *offset)?,
        CaretTarget::DocumentStart => ViewPoint::new(rd.first().ok_or(EditingError::NoCaret)?, 0),
        CaretTarget::DocumentEnd => {
            let last = rd.last().ok_or(EditingError::NoCaret)?;
            ViewPoint::new(last, rd.len(last))
        }
        CaretTarget::BlockEnd => {
            let len = rd
                .block_len(&base.caret.block)
                .ok_or(EditingError::NoCaret)?;
            ViewPoint::new(&base.caret.block, len)
        }
    };
    Ok((Vec::new(), collapsed(&base, caret)))
}

fn selected(base: &ParticipantView, anchor: ViewPoint, caret: ViewPoint) -> ParticipantView {
    let mut next = collapsed(base, caret);
    if anchor != next.caret {
        next.anchor = Some(anchor);
    }
    next
}

/// Select an explicit range (anchor at `start`, caret at `end`). View-only.
pub fn select_range(
    view: &ParticipantView,
    doc: &AutoCommit,
    range: &SelectionTarget,
) -> EditResult {
    let base = view.settle(doc);
    let rd = RegionDoc::open(doc, base.region)?;
    let anchor = rd.point(&range.start_block, range.start)?;
    let caret = rd.point(&range.end_block, range.end)?;
    Ok((Vec::new(), selected(&base, anchor, caret)))
}

/// Select one whole block. View-only.
pub fn select_block(view: &ParticipantView, doc: &AutoCommit, block: &str) -> EditResult {
    let base = view.settle(doc);
    let rd = RegionDoc::open(doc, base.region)?;
    let anchor = rd.point(block, 0)?;
    let caret = ViewPoint::new(block, rd.len(block));
    Ok((Vec::new(), selected(&base, anchor, caret)))
}

/// Select the whole region (Ctrl+A). View-only.
pub fn select_all(view: &ParticipantView, doc: &AutoCommit) -> EditResult {
    let base = view.settle(doc);
    let rd = RegionDoc::open(doc, base.region)?;
    let (Some(first), Some(last)) = (rd.first(), rd.last()) else {
        return Err(EditingError::NoCaret);
    };
    let caret = ViewPoint::new(last, rd.len(last));
    Ok((Vec::new(), selected(&base, ViewPoint::new(first, 0), caret)))
}

/// Run Find for `query` (case-insensitive literal, over the body like the
/// Find bar), filling the view's bounded hits (≤ 64) and selecting the first
/// hit as the Find bar does. An empty query clears Find. View-only; the
/// existing `search-hit#go-to` then selects another hit.
pub fn find(view: &ParticipantView, doc: &AutoCommit, query: &str) -> EditResult {
    if query.len() > MAX_FIND_QUERY_BYTES {
        return Err(EditingError::Invalid(format!(
            "a find query is at most {MAX_FIND_QUERY_BYTES} bytes"
        )));
    }
    let mut next = view.settle(doc);
    if query.is_empty() {
        next.find = FindState::default();
        return Ok((Vec::new(), next));
    }
    let containers = crate::model::open(doc).map_err(|_| EditingError::NoCaret)?;
    let blocks = tilo_doc::model::materialize(doc, &containers).blocks;
    next.find = FindState::new(query, literal_matches(&blocks, query));
    if let Some(first) = next.find.hits.first().cloned() {
        next.region = RegionKind::Body;
        next.pending = None;
        next.anchor = Some(ViewPoint::new(&first.block, first.start));
        next.caret = ViewPoint::new(&first.block, first.end);
        if first.start == first.end {
            next.anchor = None;
        }
    }
    Ok((Vec::new(), next))
}

/// The Find bar's default literal search (case-insensitive, not whole-word),
/// stopping at the resource bound.
fn literal_matches(blocks: &[tilo_doc::snapshot::BlockView], query: &str) -> Vec<SearchTarget> {
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for block in blocks {
        let haystack: Vec<char> = block.text.to_lowercase().chars().collect();
        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            let end = start + needle.len();
            if haystack.get(start..end) == Some(&needle[..]) {
                hits.push(SearchTarget {
                    block: block.id.clone(),
                    start,
                    end,
                });
                if hits.len() == MAX_RESOURCE_SEARCH_HITS {
                    return hits;
                }
                start = end;
            } else {
                start += 1;
            }
        }
    }
    hits
}

/// Plain text pastes as one paragraph per line (the widget's plain paste);
/// Markdown is imported into a scratch document and copied as a rich
/// fragment. Link marks keep only targets Writer's hyperlink rule accepts.
fn clip_blocks(content: &PasteContent) -> Result<Vec<ClipBlock>, EditingError> {
    match content {
        PasteContent::Text(text) => {
            if text.chars().count() > MAX_ACTION_TEXT_CHARS {
                return Err(EditingError::Invalid("pasted text is too long".into()));
            }
            Ok(text
                .replace('\r', "")
                .split('\n')
                .map(|line| ClipBlock {
                    kind: "paragraph".to_string(),
                    text: line.to_string(),
                    marks: Vec::new(),
                    level: None,
                    ordered: None,
                    align: None,
                    attrs: std::collections::BTreeMap::default(),
                })
                .collect())
        }
        PasteContent::Markdown(markdown) => markdown_clip_blocks(markdown),
    }
}

fn markdown_clip_blocks(markdown: &str) -> Result<Vec<ClipBlock>, EditingError> {
    if markdown.chars().count() > MAX_ACTION_TEXT_CHARS {
        return Err(EditingError::Invalid("pasted Markdown is too long".into()));
    }
    // Pasted Markdown references no content this document keeps.
    let snapshot = tilo_document::action::imported_snapshot(
        tilo_document::action::ImportFormat::Markdown,
        markdown.as_bytes(),
    )
    .map_err(|error| EditingError::Invalid(format!("Markdown: {error}")))?;
    let mut blocks: Vec<ClipBlock> = snapshot
        .blocks
        .into_iter()
        .map(|block| {
            let int = |key: &str| block.int_attr(key);
            let flag = |key: &str| block.bool_attr(key);
            let cell = block.kind == "cell";
            let mut attrs = std::collections::BTreeMap::default();
            if cell {
                if let Some(table) = block.str_attr("table") {
                    attrs.insert("table".to_owned(), ClipAttr::Str(table.to_owned()));
                }
                for key in ["row", "col"] {
                    if let Some(value) = int(key) {
                        attrs.insert(key.to_owned(), ClipAttr::Int(value));
                    }
                }
            }
            let text_kind =
                cell || matches!(block.kind.as_str(), "paragraph" | "heading" | "list_item");
            ClipBlock {
                kind: if text_kind {
                    block.kind.clone()
                } else {
                    "paragraph".to_owned()
                },
                marks: block
                    .marks
                    .iter()
                    .filter(|mark| {
                        mark.name != "link"
                            || matches!(&mark.value, tilo_doc::document::MarkValue::Str(target) if valid_hyperlink_target(target))
                    })
                    .cloned()
                    .collect(),
                level: int("level"),
                ordered: flag("ordered"),
                align: None,
                attrs,
                text: block.text,
            }
        })
        .collect();
    if blocks.is_empty() {
        return Err(EditingError::Invalid("nothing to paste".into()));
    }
    // What followed the caret continues below the table, not in its last cell.
    if blocks.last().is_some_and(|block| block.kind == "cell") {
        blocks.push(ClipBlock {
            kind: "paragraph".to_owned(),
            text: String::new(),
            marks: Vec::new(),
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::default(),
        });
    }
    Ok(blocks)
}

// ---------------------------------------------------------------------------
// Edits the published edit effects take at a participant's caret
// ---------------------------------------------------------------------------

/// Refusal of an edit replacing more than [`LARGE_REMOVAL_CHARS`] through a
/// published edit effect. The contract is the same for every participant,
/// so a large replacement is refused rather than made confirmable; the
/// keyboard path is not affected.
pub const LARGE_REFUSAL: &str =
    "large-removal: this replaces more than 2048 characters; select less or erase in steps";

/// Whether replacing `view`'s selection removes more than
/// [`LARGE_REMOVAL_CHARS`].
pub fn large_selection(view: &ParticipantView, doc: &AutoCommit) -> bool {
    resolve(view, doc)
        .is_ok_and(|r| r.selection.is_some() && r.removed_chars() > LARGE_REMOVAL_CHARS)
}

/// Set formatting on the selection whatever it has now: a toggle mark
/// (`strong`, `em`, …) `on` or off, or a valued mark (`font`, `size`,
/// `color`, `highlight`) to `value`.
pub fn set_format(
    view: &ParticipantView,
    doc: &AutoCommit,
    name: &str,
    on: Option<bool>,
    value: Option<&str>,
) -> EditResult {
    let r = resolve(view, doc)?;
    let target = MarkTarget::new(&r);
    if let Some(mark) = InlineMark::from_name(name) {
        let on = on.ok_or_else(|| {
            EditingError::Invalid("on is required: true sets the mark, false removes it".into())
        })?;
        let outcome = target.turn(mark, on);
        if outcome.0.is_empty() {
            return Err(EditingError::NeedsSelection);
        }
        return Ok(outcome);
    }
    let value = mark_value(name, value.unwrap_or_default())
        .map_err(|reason| EditingError::Invalid(reason.into()))?;
    Ok(target.set(name, Some(value)))
}

/// Remove one inline format, valued or toggled, from the selection or the
/// caret paragraph.
pub fn clear_format(view: &ParticipantView, doc: &AutoCommit, name: &str) -> EditResult {
    if !VALUED_MARKS.contains(&name) && InlineMark::from_name(name).is_none() {
        return Err(EditingError::Invalid("unknown mark".into()));
    }
    clear_mark(view, doc, name)
}

/// The caret context of `view`, refusing a view without a caret in `doc`.
fn context(view: &ParticipantView, doc: &AutoCommit) -> Result<WpResourceCtx, EditingError> {
    view.ctx(doc).ok_or(EditingError::NoCaret)
}

/// Align the caret paragraph `left`, `center`, `right` or `justify`.
pub fn align(view: &ParticipantView, doc: &AutoCommit, align: &str) -> EditResult {
    if !matches!(align, "left" | "center" | "right" | "justify") {
        return Err(EditingError::Invalid(
            "align is left, center, right or justify".into(),
        ));
    }
    let ctx = context(view, doc)?;
    Ok((
        vec![super::set_block_attr_action(
            ctx.region,
            ctx.target_blocks,
            "align",
            super::AttrVal::Str(align.to_owned()),
        )],
        view.clone(),
    ))
}

/// Insert an empty table of `rows` by `columns` after the caret paragraph.
pub fn insert_table_here(
    view: &ParticipantView,
    doc: &AutoCommit,
    rows: usize,
    columns: usize,
) -> EditResult {
    if rows == 0
        || columns == 0
        || rows > super::MAX_TABLE_DIMENSION
        || columns > super::MAX_TABLE_DIMENSION
    {
        return Err(EditingError::Invalid(format!(
            "a table has 1 to {} rows and columns",
            super::MAX_TABLE_DIMENSION
        )));
    }
    let ctx = context(view, doc)?;
    Ok((
        vec![super::insert_table_action(&ctx.caret_block, rows, columns)],
        view.clone(),
    ))
}

/// Start a new page at the caret.
pub fn insert_page_break(view: &ParticipantView, doc: &AutoCommit) -> EditResult {
    let ctx = context(view, doc)?;
    Ok((
        vec![super::insert_page_break_action(
            &ctx.caret_block,
            ctx.caret_offset,
        )],
        view.clone(),
    ))
}

/// Link the selected text to `target`: a web address, a mail address or a
/// `#bookmark` in the document.
pub fn set_hyperlink(view: &ParticipantView, doc: &AutoCommit, target: &str) -> EditResult {
    let target = target.trim();
    if !valid_hyperlink_target(target) {
        return Err(EditingError::Invalid(
            "hyperlink target must use https:, mailto:, or #bookmark".into(),
        ));
    }
    let ctx = context(view, doc)?;
    if ctx.format_spans.is_empty() {
        return Err(EditingError::NeedsSelection);
    }
    Ok((
        vec![super::set_hyperlink_action(
            ctx.region,
            ctx.format_spans,
            target,
        )],
        view.clone(),
    ))
}

/// Replace the selection with `text` (empty deletes it); the caret lands
/// after the text.
pub fn replace_selection(view: &ParticipantView, doc: &AutoCommit, text: &str) -> EditResult {
    let r = resolve(view, doc)?;
    if r.selection.is_none() {
        return Err(EditingError::NeedsSelection);
    }
    Ok(TypeTarget::new(&r).build(text))
}

/// The one-paragraph range a comment on the selection is about.
pub fn comment_range(view: &ParticipantView, doc: &AutoCommit) -> Result<Span, EditingError> {
    let ctx = context(view, doc)?;
    let selection = ctx.selection.ok_or(EditingError::NeedsSelection)?;
    if ctx.region != RegionKind::Body
        || selection.start_block != selection.end_block
        || selection.start >= selection.end
    {
        return Err(EditingError::Invalid(
            "a comment is about text within one body paragraph".into(),
        ));
    }
    Ok(Span {
        block: selection.start_block,
        start: selection.start,
        end: selection.end,
    })
}

/// Place the image `hash` (or its `tilo.content:<hash>` address), kept in
/// `content`, at the caret.
pub fn place_image(
    view: &ParticipantView,
    doc: &AutoCommit,
    content: &tilo_content::ContentStore,
    hash: &str,
) -> EditResult {
    let hash = tilo_actions::content_hash(hash.trim()).unwrap_or(hash);
    let bytes = content.read(hash).ok_or_else(|| {
        EditingError::Invalid(format!("content `{hash}` is not available on this machine"))
    })?;
    let ctx = context(view, doc)?;
    Ok((
        vec![super::insert_image_action(
            &ctx.caret_block,
            ctx.caret_offset,
            super::ImageInput::of(hash.to_owned(), &bytes),
        )],
        view.clone(),
    ))
}

const VALUED_MARKS: [&str; 4] = ["font", "size", "color", "highlight"];

/// Parse a valued mark argument the way the toolbar produces it.
fn mark_value(name: &str, value: &str) -> Result<MarkVal, &'static str> {
    let value = value.trim();
    match name {
        "font" if matches!(value, "sans" | "serif" | "mono") => Ok(MarkVal::Str(value.into())),
        "font" => Err("font is sans, serif or mono"),
        "size" => value
            .parse::<i64>()
            .ok()
            .filter(|size| (6..=200).contains(size))
            .map(MarkVal::Int)
            .ok_or("size is a whole number of points, 6-200"),
        "color" | "highlight"
            if value.len() == 7
                && value.starts_with('#')
                && value.bytes().skip(1).all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Ok(MarkVal::Str(value.to_ascii_lowercase()))
        }
        "color" | "highlight" => Err("colours are #rrggbb"),
        _ => Err("name is font, size, color or highlight"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{published_for, view_ops::ViewOperation};
    use crate::effects::WordProcessorEffect;
    use crate::word_processor::{Caret, WordProcessor};
    use tilo_actions::{
        ActionSpec, ConfirmationPolicy, ContextRequirement, EncodedAction, LocalTransport,
        OperationKind, Resource,
    };
    use ui::tilo_resource::Effect as _;
    use ui::widget::Widget as _;

    /// The published ids of the edit and view effects.
    mod op {
        use crate::effects::WordProcessorEffect as E;
        pub const TYPE_TEXT: &str = E::TYPE_TEXT.id;
        pub const NEW_PARAGRAPH: &str = E::NEW_PARAGRAPH.id;
        pub const DELETE: &str = E::ERASE_TEXT.id;
        pub const TOGGLE_MARK: &str = E::TOGGLE_MARK.id;
        pub const SET_MARK: &str = E::SET_FORMAT.id;
        pub const CLEAR_MARK: &str = E::CLEAR_FORMAT.id;
        pub const SET_PARAGRAPH_STYLE: &str = E::SET_PARAGRAPH_STYLE.id;
        pub const APPLY_NAMED_STYLE: &str = E::APPLY_NAMED_STYLE.id;
        pub const PASTE: &str = E::PASTE.id;
        pub const MOVE_CARET: &str = E::MOVE_CARET.id;
        pub const SELECT_RANGE: &str = E::SELECT_RANGE.id;
        pub const SELECT_BLOCK: &str = E::SELECT_BLOCK.id;
        pub const SELECT_ALL: &str = E::SELECT_ALL.id;
        pub const FIND: &str = E::FIND_TEXT.id;
    }

    /// A body edit as the document's action on the wire.
    fn encode(action: &RichTextAction) -> EncodedAction {
        crate::model::encoded(action)
    }

    /// The refusal an edit effect answers with.
    fn invalid(reason: impl Into<String>) -> EncodedAction {
        EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: reason.into().into_bytes(),
        }
    }

    /// A bound widget whose body holds `paragraphs`; returns the body ids.
    fn writer(paragraphs: &[&str]) -> (ui::State, crate::WordProcessorWidget, Vec<String>) {
        let mut state = crate::initialized_state();
        let mut wp = crate::WordProcessorWidget::detached();
        wp.sync(&state.doc, &state.root);
        for (index, text) in paragraphs.iter().enumerate() {
            if index > 0 {
                wp.model_mut().enter(&mut Edits::lend(&mut state.doc));
            }
            wp.model_mut()
                .insert_str(&mut Edits::lend(&mut state.doc), text);
        }
        let ids = order(&state.doc);
        (state, wp, ids)
    }

    fn order(doc: &AutoCommit) -> Vec<String> {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c)
    }

    fn texts(doc: &AutoCommit) -> Vec<String> {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.text)
            .collect()
    }

    fn apply(doc: &mut AutoCommit, action: &RichTextAction) {
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, &crate::model::action(action), encode(action), &mut tx)
            .unwrap();
    }

    fn apply_all(doc: &mut AutoCommit, actions: &[RichTextAction]) {
        for action in actions {
            apply(doc, action);
        }
    }

    /// Put the model's caret/anchor where `view` says and return the view.
    fn place(wp: &mut crate::WordProcessorWidget, view: &ParticipantView) {
        let model = wp.model_mut();
        model.clear_anchor();
        if let Some(anchor) = &view.anchor {
            model.set_caret(Caret {
                block: anchor.block.clone(),
                offset: anchor.offset,
            });
            model.set_anchor();
        }
        model.set_caret(Caret {
            block: view.caret.block.clone(),
            offset: view.caret.offset,
        });
    }

    fn at(block: &str, offset: usize) -> ParticipantView {
        ParticipantView::at(ViewPoint::new(block, offset), RegionKind::Body)
    }

    fn sel(anchor: (&str, usize), caret: (&str, usize)) -> ParticipantView {
        ParticipantView {
            anchor: Some(ViewPoint::new(anchor.0, anchor.1)),
            ..at(caret.0, caret.1)
        }
    }

    fn bytes(actions: &[RichTextAction]) -> Vec<Vec<u8>> {
        actions
            .iter()
            .map(|action| encode(action).payload)
            .collect()
    }

    /// Everything published for `view`, folded into one resource for
    /// lookup: the document's actions with the view's own operations and
    /// rows beside them.
    fn root(view: &ParticipantView, doc: &AutoCommit) -> Resource {
        let mut published =
            published_for(&view.ctx(doc).expect("view has a caret"), doc).into_iter();
        let mut root = published.next().expect("the document is published");
        for resource in published {
            for action in resource.actions {
                if !root.actions.iter().any(|existing| existing.id == action.id) {
                    root.actions.push(action);
                }
            }
            root.children.extend(resource.children);
        }
        root
    }

    fn spec(root: &Resource, id: &str) -> ActionSpec {
        root.actions
            .iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("{id} is published on the root"))
            .clone()
    }

    /// What invoking the published effect `id` with `args` at `view` submits:
    /// its one body edit, the effect itself for a view effect, or the
    /// refusal.
    fn invoke(
        view: &ParticipantView,
        doc: &AutoCommit,
        id: &str,
        args: serde_json::Value,
    ) -> EncodedAction {
        let serde_json::Value::Object(args) = args else {
            panic!("operation arguments are a JSON object");
        };
        let effect = match WordProcessorEffect::decode(id, &args) {
            Ok(effect) => effect,
            Err(error) => return invalid(error),
        };
        let Some(built) = effect.edit(view, doc, &tilo_content::ContentStore::memory()) else {
            return spec(&root(view, doc), id).build_json(&args).unwrap();
        };
        match built {
            Ok((actions, _)) => match actions.as_slice() {
                [action] => encode(action),
                [] => invalid("nothing to change here"),
                _ => invalid("this edit needs more than one step here"),
            },
            Err(EditingError::Invalid(reason)) => invalid(reason),
            Err(error) => invalid(error.to_string()),
        }
    }

    // ---- the widget's current envelope builders, verbatim over the model ----
    //
    // These copy the widget's `*_via_action` / `dispatch_*` bodies (widget.rs,
    // 2026-09-23) onto the public `WordProcessor` model, so the primitives are
    // pinned to what the keyboard dispatches today even while the widget is
    // being switched over to call them.

    fn body_block(doc: &AutoCommit, id: &str) -> Option<tilo_doc::snapshot::BlockView> {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .find(|block| block.id == id)
    }

    fn old_insert_text(m: &WordProcessor, doc: &AutoCommit, text: &str) -> Vec<RichTextAction> {
        if let Some((start, end)) = m.selection_span(doc) {
            return vec![replace_selection_action(
                m.region_kind(),
                &start.block,
                start.offset,
                &end.block,
                end.offset,
                text,
            )];
        }
        let caret = m.caret().clone();
        vec![splice_text_action(
            m.region_kind(),
            &caret.block,
            caret.offset,
            caret.offset,
            text,
        )]
    }

    fn old_enter(m: &WordProcessor, doc: &AutoCommit) -> Vec<RichTextAction> {
        let mut out = Vec::new();
        let caret = if let Some((start, _)) = m.selection_span(doc) {
            out.extend(old_insert_text(m, doc, ""));
            start
        } else {
            m.caret().clone()
        };
        let region = m.region_kind();
        let block = body_block(doc, &caret.block);
        let kind = block
            .as_ref()
            .map_or("paragraph", |entry| entry.kind.as_str());
        if kind == "list_item" && block.as_ref().is_some_and(|entry| entry.text.is_empty()) {
            out.push(set_paragraph_style_action(
                region,
                vec![caret.block],
                "paragraph",
                None,
                None,
            ));
            return out;
        }
        let ordered = block
            .as_ref()
            .filter(|_| kind == "list_item")
            .and_then(|entry| entry.bool_attr("ordered"));
        let new_kind = if kind == "list_item" {
            "list_item"
        } else {
            "paragraph"
        };
        out.push(split_block_action(
            region,
            &caret.block,
            caret.offset,
            new_kind,
            ordered,
        ));
        out
    }

    fn old_delete(m: &WordProcessor, doc: &AutoCommit, backward: bool) -> Vec<RichTextAction> {
        if m.has_selection() {
            return old_insert_text(m, doc, "");
        }
        let caret = m.caret().clone();
        let region = m.region_kind();
        let blocks = m.ordered_blocks(doc);
        let Some(index) = blocks.iter().position(|id| id == &caret.block) else {
            return vec![];
        };
        let entry = body_block(doc, &caret.block);
        let len = entry.as_ref().map_or(0, |entry| entry.text.chars().count());
        if backward && caret.offset == 0 && entry.is_some_and(|entry| entry.kind == "list_item") {
            return vec![set_paragraph_style_action(
                region,
                vec![caret.block],
                "paragraph",
                None,
                None,
            )];
        }
        if backward && caret.offset > 0 {
            vec![splice_text_action(
                region,
                &caret.block,
                caret.offset - 1,
                caret.offset,
                "",
            )]
        } else if !backward && caret.offset < len {
            vec![splice_text_action(
                region,
                &caret.block,
                caret.offset,
                caret.offset + 1,
                "",
            )]
        } else if backward && index > 0 {
            let previous = blocks[index - 1].clone();
            let previous_len = body_block(doc, &previous).map_or(0, |b| b.text.chars().count());
            vec![replace_selection_action(
                region,
                &previous,
                previous_len,
                &caret.block,
                0,
                "",
            )]
        } else if !backward && index + 1 < blocks.len() {
            vec![replace_selection_action(
                region,
                &caret.block,
                len,
                &blocks[index + 1],
                0,
                "",
            )]
        } else {
            vec![]
        }
    }

    fn spans(ranges: Vec<(String, usize, usize)>) -> Vec<Span> {
        ranges
            .into_iter()
            .map(|(block, start, end)| Span { block, start, end })
            .collect()
    }

    fn old_toggle(m: &WordProcessor, doc: &AutoCommit, name: &str) -> Vec<RichTextAction> {
        let Some((ranges, active)) = m.plan_toggle_mark(doc, name) else {
            return vec![];
        };
        let value = (!active).then_some(MarkVal::Bool(true));
        vec![set_mark_action(
            m.region_kind(),
            spans(ranges),
            name,
            value,
            vec![],
        )]
    }

    fn old_exclusive(
        m: &WordProcessor,
        doc: &AutoCommit,
        name: &str,
        sibling: &str,
    ) -> Vec<RichTextAction> {
        let (ranges, active) = m.plan_exclusive_mark(doc, name);
        if ranges.iter().all(|(_, s, e)| s >= e) {
            return vec![];
        }
        let value = (!active).then_some(MarkVal::Bool(true));
        let also = if active {
            vec![]
        } else {
            vec![sibling.to_string()]
        };
        vec![set_mark_action(
            m.region_kind(),
            spans(ranges),
            name,
            value,
            also,
        )]
    }

    fn old_set_value(
        m: &WordProcessor,
        doc: &AutoCommit,
        name: &str,
        value: Option<MarkVal>,
    ) -> Vec<RichTextAction> {
        let ranges = m.format_spans(doc);
        if ranges.iter().all(|(_, s, e)| s >= e) {
            return vec![];
        }
        vec![set_mark_action(
            m.region_kind(),
            spans(ranges),
            name,
            value,
            vec![],
        )]
    }

    fn old_paragraph_style(
        m: &WordProcessor,
        doc: &AutoCommit,
        style: &ParagraphStyle,
    ) -> Vec<RichTextAction> {
        let blocks = m.paragraph_style_targets(doc);
        if blocks.is_empty() {
            return vec![];
        }
        vec![set_paragraph_style_action(
            m.region_kind(),
            blocks,
            &style.kind,
            style.level,
            style.ordered,
        )]
    }

    fn old_named_style(m: &WordProcessor, doc: &AutoCommit, id: &str) -> Vec<RichTextAction> {
        let blocks = m.paragraph_style_targets(doc);
        if blocks.is_empty() {
            return vec![];
        }
        let (kind, level, ordered) = match crate::styles::structural_kind_for(id) {
            Some((k, l, o)) => (Some(k), l, o),
            None => (None, None, None),
        };
        vec![apply_named_style_action(
            m.region_kind(),
            blocks,
            id,
            kind,
            level,
            ordered,
        )]
    }

    fn old_paste_plain(m: &WordProcessor, doc: &AutoCommit, text: &str) -> Vec<RichTextAction> {
        let blocks: Vec<ClipBlock> = text
            .replace('\r', "")
            .split('\n')
            .map(|line| ClipBlock {
                kind: "paragraph".to_string(),
                text: line.to_string(),
                marks: Vec::new(),
                level: None,
                ordered: None,
                align: None,
                attrs: std::collections::BTreeMap::default(),
            })
            .collect();
        let caret = m.caret().clone();
        let (start, end) = m
            .selection_span(doc)
            .unwrap_or_else(|| (caret.clone(), caret));
        let end_len = body_block(doc, &end.block).map_or(end.offset, |b| b.text.chars().count());
        let adopt = start.offset == 0 && end.offset == end_len;
        vec![paste_fragment_action(
            m.region_kind(),
            SelectionTarget {
                start_block: start.block.clone(),
                start: start.offset,
                end_block: end.block.clone(),
                end: end.offset,
            },
            blocks,
            adopt,
        )]
    }

    /// Views every primitive is compared on: collapsed carets at edges and
    /// inside, forward and backward selections within and across blocks.
    fn views(ids: &[String]) -> Vec<ParticipantView> {
        vec![
            at(&ids[0], 0),
            at(&ids[0], 3),
            at(&ids[0], 5),
            at(&ids[1], 0),
            at(&ids[2], 4),
            sel((&ids[0], 1), (&ids[0], 4)),
            sel((&ids[0], 4), (&ids[0], 1)),
            sel((&ids[0], 2), (&ids[2], 3)),
            sel((&ids[2], 3), (&ids[1], 1)),
        ]
    }

    #[test]
    fn typing_enter_and_erase_match_the_widget_envelopes_byte_for_byte() {
        let (mut state, mut wp, ids) = writer(&["alpha", "beta", "gamma"]);
        // A list item, so Enter/Backspace's list handling is covered too.
        apply(
            &mut state.doc,
            &set_paragraph_style_action(
                RegionKind::Body,
                vec![ids[1].clone()],
                "list_item",
                None,
                Some(true),
            ),
        );
        let doc = &state.doc;
        for view in views(&ids) {
            place(&mut wp, &view);
            let m = wp.model();
            let (typed, _) = type_text(&view, doc, "Xy").unwrap();
            assert_eq!(
                bytes(&typed),
                bytes(&old_insert_text(m, doc, "Xy")),
                "type {view:?}"
            );
            let (enter, _) = new_paragraph(&view, doc).unwrap();
            assert_eq!(bytes(&enter), bytes(&old_enter(m, doc)), "enter {view:?}");
            for (direction, backward) in [
                (DeleteDirection::Backward, true),
                (DeleteDirection::Forward, false),
            ] {
                let (erased, _) = delete(&view, doc, direction, 1).unwrap();
                assert_eq!(
                    bytes(&erased),
                    bytes(&old_delete(m, doc, backward)),
                    "erase {backward} {view:?}"
                );
            }
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one table-driven parity proof over every published editing operation"
    )]
    fn formatting_and_styles_match_the_widget_envelopes_byte_for_byte() {
        let (mut state, mut wp, ids) = writer(&["alpha", "beta", "gamma"]);
        apply(
            &mut state.doc,
            &set_mark_action(
                RegionKind::Body,
                vec![Span {
                    block: ids[0].clone(),
                    start: 0,
                    end: 5,
                }],
                "strong",
                Some(MarkVal::Bool(true)),
                vec![],
            ),
        );
        let doc = &state.doc;
        for view in views(&ids) {
            place(&mut wp, &view);
            let m = wp.model();
            // Selections that end at a block start carry an empty span the
            // widget still sends (and apply refuses); the primitive drops it.
            let clean = |actions: Vec<RichTextAction>| -> Vec<RichTextAction> {
                actions
                    .into_iter()
                    .map(|action| match action {
                        RichTextAction::SetMark {
                            region,
                            spans,
                            name,
                            value,
                            also_clear,
                        } => RichTextAction::SetMark {
                            region,
                            spans: spans.into_iter().filter(|s| s.start < s.end).collect(),
                            name,
                            value,
                            also_clear,
                        },
                        other => other,
                    })
                    .collect()
            };
            for mark in [
                InlineMark::Strong,
                InlineMark::Em,
                InlineMark::Underline,
                InlineMark::Strike,
            ] {
                let (ours, _) = toggle_mark(&view, doc, mark).unwrap();
                assert_eq!(
                    bytes(&ours),
                    bytes(&clean(old_toggle(m, doc, mark.name()))),
                    "{mark:?} {view:?}"
                );
            }
            for (mark, sibling) in [
                (InlineMark::Superscript, "sub"),
                (InlineMark::Subscript, "super"),
            ] {
                let (ours, _) = toggle_mark(&view, doc, mark).unwrap();
                assert_eq!(
                    bytes(&ours),
                    bytes(&clean(old_exclusive(m, doc, mark.name(), sibling))),
                    "{mark:?} {view:?}"
                );
            }
            let red = MarkVal::Str("#ff0000".into());
            let (ours, _) = set_mark(&view, doc, "color", red.clone()).unwrap();
            assert_eq!(
                bytes(&ours),
                bytes(&clean(old_set_value(m, doc, "color", Some(red))))
            );
            let (ours, _) = clear_mark(&view, doc, "highlight").unwrap();
            assert_eq!(
                bytes(&ours),
                bytes(&clean(old_set_value(m, doc, "highlight", None)))
            );
            for style in [
                ParagraphStyle {
                    kind: "heading".into(),
                    level: Some(2),
                    ordered: None,
                },
                ParagraphStyle {
                    kind: "list_item".into(),
                    level: None,
                    ordered: Some(false),
                },
            ] {
                let (ours, _) = set_paragraph_style(&view, doc, &style).unwrap();
                assert_eq!(bytes(&ours), bytes(&old_paragraph_style(m, doc, &style)));
            }
            for id in ["heading1", "quote", "custom-style"] {
                let (ours, _) = apply_named_style(&view, doc, id).unwrap();
                assert_eq!(bytes(&ours), bytes(&old_named_style(m, doc, id)));
            }
            for text in ["one", "one\r\ntwo\nthree", ""] {
                let (ours, _) = paste(&view, doc, &PasteContent::Text(text.into())).unwrap();
                assert_eq!(
                    bytes(&ours),
                    bytes(&old_paste_plain(m, doc, text)),
                    "paste {text:?} {view:?}"
                );
            }
        }
    }

    // ---- published operations = primitives ---------------------------------

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one table-driven parity proof over every published editing operation"
    )]
    fn published_operations_encode_exactly_the_primitive_for_the_same_view() {
        let (state, _, ids) = writer(&["alpha", "beta", "gamma"]);
        let doc = &state.doc;
        // What the op must encode for a primitive result: the one envelope,
        // or a refusal when the primitive is a no-op.
        let one = |result: EditResult| match result.unwrap().0.as_slice() {
            [action] => encode(action),
            [] => invalid("no-op"),
            _ => panic!("more than one envelope"),
        };
        let same = |published: EncodedAction, expected: EncodedAction| {
            if expected.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
                assert_eq!(published.type_id, expected.type_id);
            } else {
                assert_eq!(published, expected);
            }
        };
        for view in views(&ids) {
            assert_eq!(
                invoke(&view, doc, op::TYPE_TEXT, serde_json::json!({"text": "Hi"})),
                one(type_text(&view, doc, "Hi"))
            );
            same(
                invoke(&view, doc, op::DELETE, serde_json::json!({})),
                one(delete(&view, doc, DeleteDirection::Backward, 1)),
            );
            same(
                invoke(
                    &view,
                    doc,
                    op::DELETE,
                    serde_json::json!({"direction": "forward", "count": 7}),
                ),
                one(delete(&view, doc, DeleteDirection::Forward, 7)),
            );
            assert_eq!(
                invoke(
                    &view,
                    doc,
                    op::TOGGLE_MARK,
                    serde_json::json!({"mark": "super"})
                ),
                one(toggle_mark(&view, doc, InlineMark::Superscript))
            );
            assert_eq!(
                invoke(
                    &view,
                    doc,
                    op::SET_MARK,
                    serde_json::json!({"name": "size", "value": "14"})
                ),
                one(set_mark(&view, doc, "size", MarkVal::Int(14)))
            );
            assert_eq!(
                invoke(
                    &view,
                    doc,
                    op::CLEAR_MARK,
                    serde_json::json!({"name": "color"})
                ),
                one(clear_mark(&view, doc, "color"))
            );
            assert_eq!(
                invoke(
                    &view,
                    doc,
                    op::SET_PARAGRAPH_STYLE,
                    serde_json::json!({"kind": "heading", "level": 1})
                ),
                one(set_paragraph_style(
                    &view,
                    doc,
                    &ParagraphStyle {
                        kind: "heading".into(),
                        level: Some(1),
                        ordered: None
                    }
                ))
            );
            assert_eq!(
                invoke(
                    &view,
                    doc,
                    op::APPLY_NAMED_STYLE,
                    serde_json::json!({"style": "quote"})
                ),
                one(apply_named_style(&view, doc, "quote"))
            );
            assert_eq!(
                invoke(&view, doc, op::PASTE, serde_json::json!({"text": "a\nb"})),
                one(paste(&view, doc, &PasteContent::Text("a\nb".into())))
            );
            if view.selection(doc).is_none() {
                assert_eq!(
                    invoke(&view, doc, op::NEW_PARAGRAPH, serde_json::json!({})),
                    one(new_paragraph(&view, doc))
                );
            } else {
                let paragraph = spec(&root(&view, doc), op::NEW_PARAGRAPH);
                let collapsed = spec(&root(&at(&ids[0], 0), doc), op::NEW_PARAGRAPH);
                assert_eq!(paragraph.contract, collapsed.contract);
                // With a selection Enter refuses: nothing is encoded.
                let refused = invoke(&view, doc, op::NEW_PARAGRAPH, serde_json::json!({}));
                assert_eq!(
                    refused.type_id,
                    tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                );
            }
            if view.selection(doc).is_some() {
                let toggled = invoke(
                    &view,
                    doc,
                    op::TOGGLE_MARK,
                    serde_json::json!({"mark": "strong"}),
                );
                assert_eq!(toggled, one(toggle_mark(&view, doc, InlineMark::Strong)));
            }
        }
    }

    #[test]
    fn root_operations_have_the_published_kinds_and_contracts() {
        let (state, _, ids) = writer(&["alpha"]);
        let root = root(&at(&ids[0], 2), &state.doc);
        // Edits at the invoker's caret: views of that participant that
        // submit a body edit as theirs, checked at the door like any edit.
        for id in [
            op::TYPE_TEXT,
            op::NEW_PARAGRAPH,
            op::DELETE,
            op::TOGGLE_MARK,
            op::SET_MARK,
            op::CLEAR_MARK,
            op::SET_PARAGRAPH_STYLE,
            op::APPLY_NAMED_STYLE,
            op::PASTE,
            op::MOVE_CARET,
            op::SELECT_RANGE,
            op::SELECT_BLOCK,
            op::SELECT_ALL,
            op::FIND,
        ] {
            let spec = spec(&root, id);
            assert_eq!(spec.kind, OperationKind::View, "{id}");
            assert!(spec.contract.is_participant_view(), "{id}");
            assert!(
                matches!(spec.contract.confirmation, ConfirmationPolicy::None),
                "{id}"
            );
            assert!(spec.contract.requirements().is_empty(), "{id}");
            assert!(spec.contract.validate_declaration().is_empty(), "{id}");
            assert!(
                spec.params()
                    .iter()
                    .all(|p| p.validate_declaration().is_empty()),
                "{id}"
            );
        }
        // `find` still just opens the bar.
        assert_eq!(spec(&root, "find").kind, OperationKind::View);
        assert!(spec(&root, "find").params().is_empty());
        let _ = ContextRequirement::Caret;
    }

    #[test]
    fn enter_lands_in_the_new_block_once_applied_and_typing_follows() {
        let (mut state, _, ids) = writer(&["hello world"]);
        let view = at(&ids[0], 5);
        let (actions, next) = new_paragraph(&view, &state.doc).unwrap();
        assert!(next.pending.is_some());
        // Before the split is applied the caret stays put.
        assert_eq!(next.settle(&state.doc).caret, ViewPoint::new(&ids[0], 5));
        apply_all(&mut state.doc, &actions);
        let settled = next.settle(&state.doc);
        let new_block = order(&state.doc)[1].clone();
        assert_eq!(settled.caret, ViewPoint::new(&new_block, 0));
        assert!(settled.pending.is_none());
        let (typed, _) = type_text(&next, &state.doc, "big ").unwrap();
        apply_all(&mut state.doc, &typed);
        assert_eq!(texts(&state.doc), ["hello", "big  world"]);
    }

    #[test]
    fn agent_style_streaming_through_published_operations_builds_one_paragraph() {
        let (mut state, _, ids) = writer(&["Intro", "Middle"]);
        // A person's caret elsewhere must not matter: the agent has its own.
        let mut agent = at(&ids[0], 0);
        let step = |agent: &mut ParticipantView,
                    doc: &mut AutoCommit,
                    id: &str,
                    args: serde_json::Value| {
            let encoded = invoke(agent, doc, id, args.clone());
            if let Some(action) = crate::model::decoded(&encoded) {
                // The view the invoker's host keeps: the primitive's result.
                let (actions, next) = match id {
                    op::NEW_PARAGRAPH => new_paragraph(agent, doc).unwrap(),
                    op::TYPE_TEXT => type_text(agent, doc, args["text"].as_str().unwrap()).unwrap(),
                    _ => unreachable!(),
                };
                assert_eq!(actions, vec![action.clone()]);
                apply(doc, &action);
                *agent = next.settle(doc);
            } else {
                let Some(crate::effects::ViewMove::Operation(operation)) =
                    crate::effects::decode(&encoded)
                        .expect("a published effect")
                        .view_operation()
                        .unwrap()
                else {
                    panic!("{id} moves the caret");
                };
                *agent = crate::actions::view_ops::apply(agent, doc, &operation)
                    .unwrap()
                    .1;
            }
        };
        step(
            &mut agent,
            &mut state.doc,
            op::MOVE_CARET,
            serde_json::json!({"to": "document-end"}),
        );
        step(
            &mut agent,
            &mut state.doc,
            op::NEW_PARAGRAPH,
            serde_json::json!({}),
        );
        for chunk in ["The ocean ", "covers most ", "of the planet."] {
            step(
                &mut agent,
                &mut state.doc,
                op::TYPE_TEXT,
                serde_json::json!({"text": chunk}),
            );
        }
        assert_eq!(
            texts(&state.doc),
            ["Intro", "Middle", "The ocean covers most of the planet."]
        );
        let last = order(&state.doc)[2].clone();
        assert_eq!(agent.caret, ViewPoint::new(&last, 36));
    }

    #[test]
    fn multi_paragraph_paste_settles_after_the_last_pasted_block() {
        let (mut state, _, ids) = writer(&["abXYcd"]);
        let view = sel((&ids[0], 2), (&ids[0], 4));
        let (actions, next) = paste(
            &view,
            &state.doc,
            &PasteContent::Text("one\ntwo\nthree".into()),
        )
        .unwrap();
        apply_all(&mut state.doc, &actions);
        assert_eq!(texts(&state.doc), ["abone", "two", "threecd"]);
        let settled = next.settle(&state.doc);
        assert_eq!(settled.caret, ViewPoint::new(&order(&state.doc)[2], 5));
    }

    #[test]
    fn erase_extends_across_paragraphs_and_stays_bounded() {
        let (mut state, _, ids) = writer(&["abc", "", "def"]);
        let view = at(&ids[2], 1);
        // "d", the two paragraph joins and "c" = 4 characters back.
        let (actions, next) = delete(&view, &state.doc, DeleteDirection::Backward, 4).unwrap();
        assert_eq!(
            actions,
            vec![replace_selection_action(
                RegionKind::Body,
                &ids[0],
                2,
                &ids[2],
                1,
                ""
            )]
        );
        assert_eq!(next.caret, ViewPoint::new(&ids[0], 2));
        apply_all(&mut state.doc, &actions);
        assert_eq!(texts(&state.doc), ["abef"]);
        // Past the region edge it stops; at the edge it is a no-op.
        let (actions, _) =
            delete(&at(&ids[0], 2), &state.doc, DeleteDirection::Backward, 200).unwrap();
        assert_eq!(
            actions,
            vec![replace_selection_action(
                RegionKind::Body,
                &ids[0],
                0,
                &ids[0],
                2,
                ""
            )]
        );
        let (actions, _) =
            delete(&at(&ids[0], 0), &state.doc, DeleteDirection::Backward, 3).unwrap();
        assert!(actions.is_empty());
        for count in [0, MAX_ERASE_COUNT + 1] {
            assert!(matches!(
                delete(&at(&ids[0], 1), &state.doc, DeleteDirection::Forward, count),
                Err(EditingError::Invalid(_))
            ));
        }
        let refused = invoke(
            &at(&ids[0], 1),
            &state.doc,
            op::DELETE,
            serde_json::json!({"count": 1}),
        );
        assert_eq!(
            refused.type_id,
            <crate::model::Document as ui::tilo_resource::Resource>::ACTION_TYPE
        );
        let edge = invoke(
            &at(&ids[0], 0),
            &state.doc,
            op::DELETE,
            serde_json::json!({}),
        );
        assert_eq!(edge.type_id, tilo_actions::resource::INVALID_ARGUMENTS_TYPE);
        let over = invoke(
            &at(&ids[0], 1),
            &state.doc,
            op::DELETE,
            serde_json::json!({"count": MAX_ERASE_COUNT + 1}),
        );
        assert_eq!(over.type_id, tilo_actions::resource::INVALID_ARGUMENTS_TYPE);
    }

    #[test]
    fn large_selection_replacements_are_refused_with_an_unchanged_contract() {
        let long = "x".repeat(LARGE_REMOVAL_CHARS + 10);
        let (state, _, ids) = writer(&[&long, "tail"]);
        let small = sel((&ids[0], 0), (&ids[0], 10));
        let large = sel((&ids[0], 0), (&ids[1], 2));
        let refused = |encoded: EncodedAction| {
            encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                && encoded.payload.starts_with(b"large-removal")
        };
        for (id, args) in [
            (op::TYPE_TEXT, serde_json::json!({"text": "x"})),
            (op::DELETE, serde_json::json!({})),
            (op::PASTE, serde_json::json!({"text": "x"})),
            ("replace-selection", serde_json::json!({"text": "x"})),
        ] {
            let calm = spec(&root(&small, &state.doc), id);
            let loud = spec(&root(&large, &state.doc), id);
            // The schema must not depend on a view: same contract either way.
            assert_eq!(calm.contract, loud.contract, "{id}");
            assert!(
                matches!(calm.contract.confirmation, ConfirmationPolicy::None),
                "{id}"
            );
            assert!(
                !refused(invoke(&small, &state.doc, id, args.clone())),
                "{id}"
            );
            assert!(refused(invoke(&large, &state.doc, id, args)), "{id}");
        }
        // select-all + type-text "" on a large document is refused; the
        // keyboard primitive itself still erases.
        let (_, all) = select_all(&at(&ids[0], 0), &state.doc).unwrap();
        assert!(refused(invoke(
            &all,
            &state.doc,
            op::TYPE_TEXT,
            serde_json::json!({"text": ""})
        )));
        assert_eq!(type_text(&all, &state.doc, "").unwrap().0.len(), 1);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one table-driven parity proof over every published editing operation"
    )]
    fn view_operations_move_select_and_find_without_touching_the_document() {
        let (state, _, ids) = writer(&["Hello world", "hello again"]);
        let doc = &state.doc;
        let heads = state.doc.clone().get_heads();
        let base = at(&ids[0], 0);
        let (none, end) = move_caret(&base, doc, &CaretTarget::DocumentEnd).unwrap();
        assert!(none.is_empty());
        assert_eq!(end.caret, ViewPoint::new(&ids[1], 11));
        let (_, block_end) = move_caret(&base, doc, &CaretTarget::BlockEnd).unwrap();
        assert_eq!(block_end.caret, ViewPoint::new(&ids[0], 11));
        assert!(
            move_caret(
                &base,
                doc,
                &CaretTarget::Point {
                    block: ids[0].clone(),
                    offset: 99
                }
            )
            .is_err()
        );
        assert!(
            move_caret(
                &base,
                doc,
                &CaretTarget::Point {
                    block: "gone".into(),
                    offset: 0
                }
            )
            .is_err()
        );
        let (_, block) = select_block(&base, doc, &ids[1]).unwrap();
        assert_eq!(
            block.selection(doc),
            Some(SelectionTarget {
                start_block: ids[1].clone(),
                start: 0,
                end_block: ids[1].clone(),
                end: 11
            })
        );
        let (_, all) = select_all(&base, doc).unwrap();
        assert_eq!(all.anchor, Some(ViewPoint::new(&ids[0], 0)));
        assert_eq!(all.caret, ViewPoint::new(&ids[1], 11));
        let (_, found) = find(&base, doc, "HELLO").unwrap();
        assert_eq!(found.find.hits.len(), 2);
        assert_eq!(found.find.query, "HELLO");
        assert_eq!(found.selection(doc).unwrap().end, 5);
        let tree = root(&found, doc);
        assert!(
            published_for(&found.ctx(doc).unwrap(), doc)
                .iter()
                .any(|c| c.id.0 == "word-processor.search-results")
        );
        assert!(tree.actions.iter().any(|a| a.id == "replace-selection"));
        assert!(matches!(
            find(&base, doc, &"q".repeat(MAX_FIND_QUERY_BYTES + 1)),
            Err(EditingError::Invalid(_))
        ));
        let (_, cleared) = find(&found, doc, "").unwrap();
        assert!(cleared.find.hits.is_empty());
        // Published: a view-operation payload that applies to the same view.
        for (id, args, expected) in [
            (
                op::SELECT_ALL,
                serde_json::json!({}),
                ViewOperation::SelectAll,
            ),
            (
                op::FIND,
                serde_json::json!({"query": "again"}),
                ViewOperation::FindText {
                    query: "again".into(),
                },
            ),
            (
                op::MOVE_CARET,
                serde_json::json!({"block": ids[1], "offset": 3}),
                ViewOperation::MoveCaret {
                    to: CaretTarget::Point {
                        block: ids[1].clone(),
                        offset: 3,
                    },
                },
            ),
            (
                op::SELECT_RANGE,
                serde_json::json!({"start_block": ids[0], "start": 1, "end_block": ids[1], "end": 2}),
                ViewOperation::SelectRange {
                    range: SelectionTarget {
                        start_block: ids[0].clone(),
                        start: 1,
                        end_block: ids[1].clone(),
                        end: 2,
                    },
                },
            ),
        ] {
            let encoded = invoke(&base, doc, id, args);
            let Some(crate::effects::ViewMove::Operation(decoded)) =
                crate::effects::decode(&encoded)
                    .expect("a published effect")
                    .view_operation()
                    .unwrap()
            else {
                panic!("{id} moves the view");
            };
            assert_eq!(decoded, expected);
            assert!(
                crate::actions::view_ops::apply(&base, doc, &decoded)
                    .unwrap()
                    .0
                    .is_empty()
            );
        }
        let bad_point = spec(&root(&base, doc), op::MOVE_CARET)
            .build_json(serde_json::json!({"to": "point"}).as_object().unwrap())
            .unwrap();
        assert!(
            crate::effects::decode(&bad_point)
                .expect("a published effect")
                .view_operation()
                .is_err(),
            "a point without a block is refused"
        );
        assert_eq!(state.doc.clone().get_heads(), heads);
    }

    #[test]
    fn markdown_paste_keeps_structure_and_drops_unsafe_links() {
        let (mut state, _, ids) = writer(&["start"]);
        let md = "# Title\n\nSome **bold** and [bad](javascript:alert(1)) and [ok](https://example.test).\n";
        let (actions, next) = paste(
            &at(&ids[0], 5),
            &state.doc,
            &PasteContent::Markdown(md.into()),
        )
        .unwrap();
        let RichTextAction::PasteFragment { blocks, .. } = &actions[0] else {
            panic!("paste is one PasteFragment");
        };
        assert_eq!(blocks[0].kind, "heading");
        let links: Vec<_> = blocks
            .iter()
            .flat_map(|block| block.marks.iter())
            .filter(|mark| mark.name == "link")
            .collect();
        assert_eq!(links.len(), 1, "{links:?}");
        assert!(
            blocks
                .iter()
                .flat_map(|b| b.marks.iter())
                .any(|m| m.name == "strong")
        );
        apply_all(&mut state.doc, &actions);
        let settled = next.settle(&state.doc);
        assert_eq!(settled.caret.block, *order(&state.doc).last().unwrap());
        let refused = invoke(
            &at(&ids[0], 0),
            &state.doc,
            op::PASTE,
            serde_json::json!({"text": "a", "markdown": "b"}),
        );
        assert_eq!(
            refused.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
    }

    #[test]
    fn markdown_table_pastes_as_a_table_of_this_document() {
        let (mut state, _, ids) = writer(&["intro outro"]);
        let md = "| Person | Blocked |\n| --- | --- |\n| Maya | no |\n";
        for _ in 0..2 {
            let (actions, _) = paste(
                &at(&ids[0], 6),
                &state.doc,
                &PasteContent::Markdown(md.into()),
            )
            .unwrap();
            apply_all(&mut state.doc, &actions);
        }
        let c = crate::model::open(&state.doc).unwrap();
        let blocks = tilo_doc::model::materialize(&state.doc, &c).blocks;
        let rows: Vec<_> = blocks
            .iter()
            .map(|block| {
                let attr = |key: &str| match block.attrs.get(key) {
                    Some(tilo_doc::AttrVal::Int(value)) => value.to_string(),
                    Some(tilo_doc::AttrVal::Str(value)) => value.clone(),
                    _ => String::new(),
                };
                (
                    block.kind.as_str(),
                    block.text.as_str(),
                    attr("row"),
                    attr("col"),
                )
            })
            .collect();
        let cell = |text, row: &str, col: &str| ("cell", text, row.to_owned(), col.to_owned());
        let para = |text| ("paragraph", text, String::new(), String::new());
        assert_eq!(
            rows,
            [
                para("intro "),
                cell("Person", "0", "0"),
                cell("Blocked", "0", "1"),
                cell("Maya", "1", "0"),
                cell("no", "1", "1"),
                para(""),
                cell("Person", "0", "0"),
                cell("Blocked", "0", "1"),
                cell("Maya", "1", "0"),
                cell("no", "1", "1"),
                para("outro"),
            ]
        );
        let tables: std::collections::BTreeSet<_> = blocks
            .iter()
            .filter_map(|block| block.str_attr("table").map(str::to_owned))
            .collect();
        assert_eq!(tables.len(), 2, "each paste is its own table: {tables:?}");
    }

    #[test]
    fn stale_or_missing_carets_refuse_instead_of_guessing() {
        let (state, _, ids) = writer(&["abc"]);
        let doc = &state.doc;
        assert_eq!(
            type_text(&ParticipantView::default(), doc, "x").unwrap_err(),
            EditingError::NoCaret
        );
        assert_eq!(
            new_paragraph(&at("gone", 0), doc).unwrap_err(),
            EditingError::NoCaret
        );
        let (actions, next) = type_text(&at(&ids[0], 99), doc, "!").unwrap();
        assert_eq!(
            actions,
            vec![splice_text_action(RegionKind::Body, &ids[0], 3, 3, "!")]
        );
        assert_eq!(next.caret.offset, 4);
        // Toggling a plain mark without a selection is a no-op, and the
        // published op says why.
        assert!(
            toggle_mark(&at(&ids[0], 1), doc, InlineMark::Strong)
                .unwrap()
                .0
                .is_empty()
        );
        let refused = invoke(
            &at(&ids[0], 1),
            doc,
            op::TOGGLE_MARK,
            serde_json::json!({"mark": "strong"}),
        );
        assert_eq!(
            refused.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
        assert!(refused.payload.starts_with(b"needs-selection"));
        let refused = invoke(
            &at(&ids[0], 1),
            doc,
            op::TYPE_TEXT,
            serde_json::json!({"text": ""}),
        );
        assert_eq!(
            refused.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
        let refused = invoke(
            &at(&ids[0], 1),
            doc,
            op::SET_MARK,
            serde_json::json!({"name": "color", "value": "red"}),
        );
        assert_eq!(
            refused.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
    }

    #[test]
    fn set_mark_on_keeps_bold_that_a_toggle_would_remove() {
        let (mut state, _, ids) = writer(&["Deadline"]);
        let view = sel((&ids[0], 0), (&ids[0], 8));
        let (bold, _) = toggle_mark(&view, &state.doc, InlineMark::Strong).unwrap();
        apply_all(&mut state.doc, &bold);
        let doc = &state.doc;
        let line = vec![Span {
            block: ids[0].clone(),
            start: 0,
            end: 8,
        }];
        let strong = |value| {
            encode(&set_mark_action(
                RegionKind::Body,
                line.clone(),
                "strong",
                value,
                vec![],
            ))
        };
        let set = |on| {
            invoke(
                &view,
                doc,
                op::SET_MARK,
                serde_json::json!({"name": "strong", "on": on}),
            )
        };
        assert_eq!(set(true), strong(Some(MarkVal::Bool(true))));
        assert_eq!(
            invoke(
                &view,
                doc,
                op::TOGGLE_MARK,
                serde_json::json!({"mark": "strong"})
            ),
            strong(None)
        );
        assert_eq!(set(false), strong(None));
        let missing = invoke(
            &view,
            doc,
            op::SET_MARK,
            serde_json::json!({"name": "strong"}),
        );
        assert_eq!(
            missing.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
        let caret_only = invoke(
            &at(&ids[0], 1),
            doc,
            op::SET_MARK,
            serde_json::json!({"name": "em", "on": true}),
        );
        assert!(caret_only.payload.starts_with(b"needs-selection"));
    }

    #[test]
    fn valued_and_exclusive_marks_fall_back_to_the_caret_block() {
        let (state, _, ids) = writer(&["alpha", "beta"]);
        let view = at(&ids[1], 2);
        let caret_block = vec![Span {
            block: ids[1].clone(),
            start: 0,
            end: 4,
        }];
        let (actions, _) = set_mark(&view, &state.doc, "size", MarkVal::Int(14)).unwrap();
        assert_eq!(
            actions,
            vec![set_mark_action(
                RegionKind::Body,
                caret_block.clone(),
                "size",
                Some(MarkVal::Int(14)),
                vec![]
            )]
        );
        let (actions, _) = clear_mark(&view, &state.doc, "color").unwrap();
        assert_eq!(
            actions,
            vec![set_mark_action(
                RegionKind::Body,
                caret_block.clone(),
                "color",
                None,
                vec![]
            )]
        );
        let (actions, _) = toggle_mark(&view, &state.doc, InlineMark::Superscript).unwrap();
        assert_eq!(
            actions,
            vec![set_mark_action(
                RegionKind::Body,
                caret_block,
                "super",
                Some(MarkVal::Bool(true)),
                vec!["sub".into()]
            )]
        );
    }

    /// Keystroke primitives inside one block must not scan the region: on a
    /// long document they cost about what they cost on a short one.
    #[test]
    fn in_block_keystrokes_do_not_scan_the_whole_document() {
        let mut doc = AutoCommit::new();
        let containers = crate::model::init(&mut doc).unwrap();
        let mut next_ids = tilo_doc::model::seeded_id_gen(&doc, &containers);
        let mut ids = Vec::new();
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &containers,
                ids: &mut next_ids,
            };
            let mut after: Option<String> = None;
            for i in 0..800 {
                let id = dm.insert_block(after.as_deref(), "paragraph");
                dm.splice_text(&id, 0, 0, &format!("paragraph {i}"));
                after = Some(id.clone());
                ids.push(id);
            }
        }
        doc.commit();
        let doc = &doc;
        let view = at(&ids[400], 4);
        let scans = tilo_doc::model::ordered_keys_scan_count();
        for _ in 0..30 {
            type_text(&view, doc, "x").unwrap();
            delete(&view, doc, DeleteDirection::Backward, 1).unwrap();
            delete(&view, doc, DeleteDirection::Forward, 1).unwrap();
        }
        assert_eq!(
            tilo_doc::model::ordered_keys_scan_count(),
            scans,
            "a keystroke inside a block listed the document's paragraphs"
        );
    }
}
