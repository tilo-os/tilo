//! `BlockEditor`: a stack of [`RichTextLine`]-backed blocks (paragraph,
//! heading, lists, code, divider) with split-on-Enter, merge-on-Backspace,
//! arrow-key navigation, and markdown-shortcut promotion (`# ` -> H1, etc).
//!
//! No demotion once a block is a heading. Promotion only fires on an
//! *empty* paragraph, matching the slash-menu convention.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::fmt::Write as _;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{Icon, draw_icon};
use crate::presence::{PathSegment, PresenceCtx, PresenceKey};
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::{clipboard_copy, clipboard_copy_accepted, clipboard_paste};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::code_editor::{SyntaxHighlighter, TokenKind};
use crate::widget::link_popover::{LinkPopover, PageEntry};
use crate::widget::marks::{Mark, MarkSpan, PageId};
use crate::widget::rich_text_line::{LineSignal, RichTextLine, TransientColorSpan};
use crate::widget::slash_menu::SlashMenu;
use crate::widget::{Button, SearchField, TextInput};
use crate::widget::{EventCtx, Widget};

/// Deeper nesting runs off the panel's right edge.
pub const MAX_LIST_INDENT: u8 = 5;
/// Matches the 28px indent step Notion uses in its dense block view.
pub const LIST_INDENT_STEP_PX: f32 = 28.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Page(PageId),
    External(String),
}

/// Case-insensitive byte ranges without assuming that Unicode case folding
/// preserves byte length (for example, dotted I and sharp S). Notes are small
/// enough that checking character-boundary slices is preferable to returning
/// an invalid caret offset from a lower-cased copy.
fn case_folded_match_ranges(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let folded = needle.to_lowercase();
    let boundaries = haystack
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(haystack.len()))
        .collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut start_slot = 0usize;
    while start_slot < boundaries.len().saturating_sub(1) {
        let Some(&start) = boundaries.get(start_slot) else {
            break;
        };
        let mut matched_end = None;
        for &end in boundaries.iter().skip(start_slot.saturating_add(1)) {
            let Some(candidate) = haystack.get(start..end) else {
                continue;
            };
            let candidate_folded = candidate.to_lowercase();
            if candidate_folded == folded {
                matched_end = Some(end);
                break;
            }
            if candidate_folded.len() > folded.len().saturating_add(4) {
                break;
            }
        }
        if let Some(end) = matched_end {
            ranges.push((start, end));
            start_slot = boundaries
                .iter()
                .position(|boundary| *boundary == end)
                .unwrap_or_else(|| start_slot.saturating_add(1));
        } else {
            start_slot = start_slot.saturating_add(1);
        }
    }
    ranges
}

/// Whether a grammar token gets its own syntax colour in a code block; body
/// text, identifiers and prose emphasis stay in the block's own colour.
fn colours_code(kind: TokenKind) -> bool {
    !matches!(
        kind,
        TokenKind::Text | TokenKind::Variable | TokenKind::Emphasis | TokenKind::Strong
    )
}

#[expect(
    clippy::indexing_slicing,
    reason = "the parser advances one shared index while both slices are bounds-checked by the loop"
)]
fn apply_syntax_colors(blocks: &mut [Block], languages: &[Option<String>]) {
    let mut index = 0usize;
    while index < blocks.len() {
        let Some(language) = languages.get(index).and_then(Option::as_deref) else {
            let Some(block) = blocks.get_mut(index) else {
                break;
            };
            block.line.set_transient_colors(Vec::new());
            index = index.saturating_add(1);
            continue;
        };
        let mut state = SyntaxHighlighter::for_token_or_plain(language).start();
        let start = index;
        while index < blocks.len()
            && languages.get(index).and_then(Option::as_deref) == Some(language)
            && blocks[start].same_code_group(&blocks[index])
        {
            let Some(block) = blocks.get_mut(index) else {
                break;
            };
            let colors = state
                .highlight_line(block.text())
                .into_iter()
                .filter(|span| colours_code(span.kind))
                .map(|span| TransientColorSpan {
                    range: span.range,
                    kind: span.kind,
                })
                .collect();
            block.line.set_transient_colors(colors);
            index = index.saturating_add(1);
        }
    }
}

/// Each `CodeBlock` stores one authored source line. It may wrap visually;
/// Enter creates the next source row without changing existing line breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// Markdown block quote, rendered with an accent rule and subdued surface.
    Quote,
    /// Constructor helpers clamp to [1, 3]; other values are invalid.
    Heading(u8),
    Bullet {
        indent: u8,
    },
    /// The number is computed from position within its run at the same
    /// indent (`numbered_ordinals`), not stored — renumbers automatically.
    Numbered {
        indent: u8,
    },
    Checklist {
        indent: u8,
        done: bool,
    },
    /// `lang` is the fence's tag (e.g. `rust`), `None` for a bare fence.
    CodeBlock {
        lang: Option<String>,
    },
    /// GitHub-flavoured Markdown table stored as editable source and rendered
    /// as a native grid.
    Table {
        visual: bool,
    },
    /// Non-editable, non-focusable.
    Divider,
}

impl BlockKind {
    /// The role a block's line is set in, so a page's body, headings and
    /// code use the same ladder as every other surface: body `Body`, H1
    /// `LargeTitle`, H2 `Title`, H3+ `Heading`, code and tables `Code`.
    pub fn text_role(&self) -> TextRole {
        match self {
            BlockKind::Paragraph
            | BlockKind::Quote
            | BlockKind::Bullet { .. }
            | BlockKind::Numbered { .. }
            | BlockKind::Checklist { .. }
            | BlockKind::Divider => TextRole::Body,
            BlockKind::Heading(1) => TextRole::LargeTitle,
            BlockKind::Heading(2) => TextRole::Title,
            BlockKind::Heading(_) => TextRole::Heading,
            BlockKind::CodeBlock { .. } | BlockKind::Table { .. } => TextRole::Code,
        }
    }

    /// Unscaled type size of a block's line: its role's 1× size. The
    /// accessibility text scale and the document zoom are applied where this
    /// is consumed (`RichTextLine`, the prefix painters).
    pub(crate) fn font_size(&self) -> f32 {
        crate::tokens::typography::stock().size(self.text_role())
    }

    pub fn supports_indent(&self) -> bool {
        matches!(
            self,
            BlockKind::Bullet { .. } | BlockKind::Numbered { .. } | BlockKind::Checklist { .. }
        )
    }

    pub fn is_editable(&self) -> bool {
        !matches!(self, BlockKind::Divider)
    }

    pub fn indent(&self) -> u8 {
        match self {
            BlockKind::Bullet { indent }
            | BlockKind::Numbered { indent }
            | BlockKind::Checklist { indent, .. } => *indent,
            _ => 0,
        }
    }

    /// The kind a bound block list spells as `stored` (`paragraph`,
    /// `heading2`, `bullet:1`, `checklist:0:1`, `code:rust`, …), when it is
    /// a valid spelling: how an owner reads blocks it stored in that form.
    pub fn from_stored(stored: &str) -> Option<Self> {
        let kind = kind_from_str(stored);
        (kind_to_str(&kind) == stored).then_some(kind)
    }

    /// This kind's stored spelling, the inverse of [`Self::from_stored`].
    pub fn stored(&self) -> String {
        kind_to_str(self)
    }
}

pub struct Block {
    /// Durable identity.  List indexes are deliberately not used by action
    /// payloads: concurrent insertions make them an unstable address.
    pub id: String,
    pub kind: BlockKind,
    /// Identity of the authored fence, shared by its source rows. None is the
    /// read-only compatibility form for older contiguous same-language runs.
    pub code_group: Option<String>,
    /// Outline depth for non-list content. List kinds retain their wire-compatible depth.
    pub nesting: u8,
    pub line: RichTextLine,
    /// Set by Shift+Enter: renders with no inter-block top spacing so
    /// the two lines read as one paragraph.
    pub soft_break: bool,
}

/// Ephemeral pointer state for the editor's block-order handle.  The durable
/// result is still produced by the normal semantic-diff bridge as a stable-id
/// [`BlockEditOp::Move`]; this only owns the gesture before that action exists.
#[derive(Debug, Clone, Copy)]
struct BlockDrag {
    source: usize,
    target: usize,
    start: Point,
    active: bool,
}

impl Block {
    fn with_id(kind: BlockKind, line: RichTextLine) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind,
            code_group: None,
            nesting: 0,
            line,
            soft_break: false,
        }
    }

    pub fn paragraph(text: &str) -> Self {
        Self::with_id(
            BlockKind::Paragraph,
            RichTextLine::new()
                .with_text(text)
                .with_size(BlockKind::Paragraph.font_size()),
        )
    }

    pub fn quote(text: &str) -> Self {
        let kind = BlockKind::Quote;
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
        )
    }

    pub fn heading(level: u8, text: &str) -> Self {
        let kind = BlockKind::Heading(level.clamp(1, 3));
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
        )
    }

    pub fn bullet(indent: u8, text: &str) -> Self {
        let kind = BlockKind::Bullet {
            indent: indent.min(MAX_LIST_INDENT),
        };
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
        )
    }

    pub fn numbered(indent: u8, text: &str) -> Self {
        let kind = BlockKind::Numbered {
            indent: indent.min(MAX_LIST_INDENT),
        };
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
        )
    }

    pub fn checklist(indent: u8, done: bool, text: &str) -> Self {
        let kind = BlockKind::Checklist {
            indent: indent.min(MAX_LIST_INDENT),
            done,
        };
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
        )
    }

    pub fn code_block(lang: Option<String>, text: &str) -> Self {
        let kind = BlockKind::CodeBlock { lang };
        let mut line = RichTextLine::new()
            .with_text(text)
            .with_size(kind.font_size())
            .with_mono();
        line.set_inline_formatting(false);
        Self::with_id(kind, line)
    }

    pub fn table(text: &str, visual: bool) -> Self {
        let kind = BlockKind::Table { visual };
        Self::with_id(
            kind.clone(),
            RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size())
                .with_mono(),
        )
    }

    /// Empty inner line keeps a uniform shape; never focused or edited.
    pub fn divider() -> Self {
        Self::with_id(
            BlockKind::Divider,
            RichTextLine::new().with_size(BlockKind::Divider.font_size()),
        )
    }

    pub fn text(&self) -> &str {
        self.line.value()
    }

    pub fn indent(&self) -> u8 {
        if self.kind.supports_indent() {
            self.kind.indent()
        } else {
            self.nesting
        }
    }

    pub fn set_indent(&mut self, depth: u8) {
        let depth = depth.min(MAX_LIST_INDENT);
        match &mut self.kind {
            BlockKind::Bullet { indent }
            | BlockKind::Numbered { indent }
            | BlockKind::Checklist { indent, .. } => {
                *indent = depth;
                self.nesting = 0;
            }
            _ => self.nesting = depth,
        }
    }

    pub fn same_code_group(&self, other: &Self) -> bool {
        matches!(self.kind, BlockKind::CodeBlock { .. })
            && self.kind == other.kind
            && self.code_group == other.code_group
            && self.indent() == other.indent()
    }
}

/// A semantic, UUID-addressed mutation emitted by the optional action bridge.
/// It intentionally contains no Automerge object ids, making it safe to replay
/// on another replica or use from apps with a different document layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockEditOp {
    TextSplice {
        block_id: String,
        start: usize,
        end: usize,
        text: String,
    },
    Insert {
        block_id: String,
        after: Option<String>,
        kind: String,
        text: String,
    },
    Delete {
        block_id: String,
    },
    Move {
        block_id: String,
        after: Option<String>,
    },
    SetKind {
        block_id: String,
        kind: String,
    },
    SetChecklist {
        block_id: String,
        done: bool,
    },
    SetNesting {
        block_id: String,
        depth: u8,
    },
    SetCodeGroup {
        block_id: String,
        group_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockSnapshot {
    id: String,
    kind: String,
    text: String,
    code_group: Option<String>,
    nesting: u8,
}

/// Display-only, bounded projection for a live page embed. The source CRDT
/// keeps only the stable UUID mark; hosts refresh this projection on sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransclusionPreview {
    pub title: String,
    pub excerpt: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent, orthogonal piece of transient \
              widget state (focus, paint-dirty, taffy-structure-dirty, \
              edit-in-flight, commit-pending, doc-structure-dirty); merging \
              them into a bitflags/enum would not change behavior and this \
              struct is private to the module"
)]
struct ImageAttachment {
    source: String,
    editing: bool,
    command: std::rc::Rc<std::cell::Cell<Option<bool>>>,
    edit: Button,
    done: Button,
    node: Option<NodeId>,
}

impl ImageAttachment {
    fn new(id: &str, source: &str) -> Self {
        let command = std::rc::Rc::new(std::cell::Cell::new(None));
        let edit_command = command.clone();
        let done_command = command.clone();
        Self {
            source: source.into(),
            editing: false,
            node: None,
            edit: Button::new(&format!("notes.image-edit:{id}"), "notes-image-edit")
                .on_view(move || edit_command.set(Some(true))),
            done: Button::new(&format!("notes.image-done:{id}"), "notes-image-done")
                .on_view(move || done_command.set(Some(false))),
            command,
        }
    }

    fn button(&self) -> &Button {
        if self.editing { &self.done } else { &self.edit }
    }
    fn button_mut(&mut self) -> &mut Button {
        if self.editing {
            &mut self.done
        } else {
            &mut self.edit
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent editor interaction and lifecycle flags"
)]
pub struct BlockEditor {
    blocks: Vec<Block>,
    focused: Option<usize>,
    /// Decoupled from `focused` so the index survives blur/refocus.
    is_focused: bool,
    /// Escape releases the next Tab to the host; editing or refocusing resumes nesting.
    tab_navigation: bool,
    caret_paint_suppressed: bool,
    rect: Rect,
    /// Current accessibility text scale, retained for prefix and gutter
    /// geometry that is resolved after the taffy pass.
    text_scale: f64,
    /// Drag-handle geometry captured from the active theme.
    handle: HandleMetrics,
    /// The text inset of the block lines: where a paragraph's text starts
    /// inside its line rect, and so the editor's one content edge.
    text_pad: f64,
    /// Text sizes are the document's own, not grown with the text scale.
    content_sized: bool,
    /// Reflow text blocks to the available width instead of giving each
    /// block a horizontally scrolling single line.
    wrap_lines: bool,
    /// Use a tighter prose rhythm between adjacent paragraphs while retaining
    /// the larger separation around headings, lists, media, and code.
    compact_paragraph_spacing: bool,
    /// The measured leaf that stands for the block stack in the layout tree.
    content_node: Option<NodeId>,
    stack_leaf: Box<BlockStackLeaf>,
    /// Per-block margins and height bounds, resolved when the tree is built.
    stack_boxes: Vec<StackBox>,
    /// The last stacking pass: each block's unrounded top relative to the
    /// editor, its height, the width it ran at, and the stack's total height.
    stack: BlockStack,
    /// The theme lines measure under, shared by all of them and replaced only
    /// when the theme's content changes.
    layout_theme: std::sync::Arc<Theme>,
    layout_theme_fingerprint: Option<u64>,
    /// Lines measured and blocks painted since a test last reset them.
    #[cfg(test)]
    stack_measures: usize,
    /// Shows its blocks without taking input: a message or comment as sent.
    read_only: bool,
    /// Whether blocks show a drag handle beside them (off in a composer).
    block_handles: bool,
    /// The colours mentions are painted in, from the owning model.
    mention_paint: Option<crate::widget::marks::MentionPaint>,
    /// How every line is aligned within the editor's width (a centred
    /// slide title); left unless the owner says otherwise.
    text_align: crate::richtext::TextAlign,
    /// Every line's type size relative to its kind's: a slide's text box
    /// sets its own body size and headings grow with it.
    font_scale: f32,
    #[cfg(test)]
    painted_blocks: usize,
    dirty: bool,
    paint_damage: crate::widget::paint_damage::PaintDamage,
    /// Where each line this draw painted past its own rect (a correction
    /// chip floats above or below its field), by block id.
    line_overhang: HashMap<String, Rect>,
    /// [`Self::line_overhang`] as the last drawn frame painted it.
    drawn_overhang: HashMap<String, Rect>,
    /// Where each visual table's focused cell painted in the last draw,
    /// by block id.
    table_focus_paint: HashMap<String, Rect>,
    /// [`Self::table_focus_paint`] as the last drawn frame painted it.
    drawn_table_focus: HashMap<String, Rect>,
    /// A table cell's text changed since the last layout: the table's line
    /// measures again, as its rows may have grown or shrunk.
    table_relayout: bool,
    /// Which blocks the person may have changed since the owning model last
    /// asked ([`Self::take_edit_scope`]).
    edit_scope: EditScope,
    /// Set when the block list grows/shrinks; drained by
    /// `take_structure_dirty` so the host rebuilds taffy.
    structure_dirty: bool,
    edit_pending: bool,
    /// Structural text replacement must flush as one host undo boundary even
    /// when triggered by a character key (which ordinary `TextInput` debounces).
    commit_pending: bool,
    last_mouse_pos: Point,
    /// `(block_idx, byte)` start of an in-flight cross-block selection
    /// drag. While set, `CursorMoved` paints selection bands across every
    /// block between start and pointer — the only way to extend a
    /// selection past a paragraph boundary, since a block's own
    /// `TextInput` drag only tracks x within its own rect.
    cross_block_drag_start: Option<(usize, usize)>,
    /// Furthest range touched this drag, so retreating (down then back
    /// up) knows which blocks to clear.
    cross_block_drag_touched: Option<(usize, usize)>,
    /// Canonical document-level selection. Per-line selections are only its
    /// painted representation; clipboard and replacement semantics read this.
    document_selection: Option<DocumentSelection>,
    /// Exact membership retained after a structural batch command. Text alone
    /// cannot distinguish an included empty last block from a column-zero end.
    retained_block_selection: Option<RetainedBlockSelection>,
    slash_menu: SlashMenu,
    /// Byte offset where `/` was typed, so `/<filter>` can be spliced
    /// back out on commit.
    slash_open_at: Option<usize>,
    link_popover: LinkPopover,
    /// Byte offset of the leading `[[`, spliced back out once a page
    /// is selected.
    link_open_at: Option<usize>,
    /// The popovers open while handling a key event, before a draw has the
    /// current glyph geometry.  Retaining the block + byte lets the overlay
    /// refresh against the actual rendered caret (and its real theme) just
    /// before it paints instead of guessing from a line edge.
    slash_anchor_caret: Option<(usize, usize)>,
    link_anchor_caret: Option<(usize, usize)>,
    /// Actual painted link geometry from the most recent draw. This keeps
    /// app-level navigation hit-testing independent of a guessed theme,
    /// padding, font advances, or window scale.
    live_link_hits: Vec<(Rect, LinkTarget)>,
    transclusion_previews: HashMap<PageId, TransclusionPreview>,
    transclusion_rects: Vec<Option<(Rect, PageId)>>,
    pending_transclusion_opens: Vec<PageId>,
    pending_external_link_opens: Vec<String>,
    /// Images pasted into the editor, for its host to store and place after
    /// the caret: the editor holds text, and an image becomes a block once
    /// the host keeps its bytes with the document.
    pasted_images: Vec<ImagePasteRequest>,
    /// Painted copy controls for fenced-code runs. Each tuple retains the
    /// inclusive block range so pointer and accessibility activation copy the
    /// exact source, including newlines, without touching document state.
    code_copy_hits: Vec<(Rect, usize, usize)>,
    copied_code_start: Option<usize>,
    copied_code_feedback: Option<(String, crate::time::Instant)>,
    /// A freshly typed fence edits its language until Enter. The language is
    /// canonical block metadata; this only remembers the local input mode.
    opening_code_fence: Option<(String, Option<String>)>,
    /// Reordering begins only from the quiet six-dot handle, preserving normal
    /// text selection drags everywhere else in a block.
    block_drag: Option<BlockDrag>,
    /// Host clip in window coordinates, including the visible editor gutter.
    /// Tooltip discovery runs independently of paint clipping.
    tooltip_viewport: Option<Rect>,
    /// `Some`: backed by this Automerge block-Map list, bound per-line
    /// so edits sync. `None`: local-only (examples/tests).
    blocks_list: Option<ObjId>,
    local_author: Option<String>,
    /// Anchor of a keyboard cross-block selection, latched on the first
    /// `Ctrl+Shift+Arrow` and held across extends. Cleared by any
    /// non-extend key or mouse press.
    kbd_sel_anchor: Option<(usize, usize)>,
    /// Placeholder for a single empty paragraph; host-customisable via
    /// [`Self::with_empty_hint`].
    empty_hint: String,
    /// Reserved preview geometry for the final source line in each TeX/Typst
    /// fenced block. Source remains an ordinary CRDT-backed code line.
    formula_preview_rects: Vec<Option<Rect>>,
    table_preview_rects: Vec<Option<Rect>>,
    table_editors: HashMap<String, crate::widget::block_editor_table::DocumentTable>,
    table_mode_controls: HashMap<String, TableModeControl>,
    image_preview_rects: Vec<Option<Rect>>,
    image_store: Option<crate::ImageStore>,
    /// Retain decoded dimensions even if the bounded image cache evicts pixels.
    /// Layout must not alternate between a placeholder and the actual aspect ratio.
    image_dimensions: HashMap<String, (u32, u32)>,
    image_attachments: HashMap<String, ImageAttachment>,
    formula_chrome: Vec<Option<FormulaChrome>>,
    visual_formulas: HashMap<usize, VisualFormulaState>,
    visual_formula_focus: Option<usize>,
    preferred_formula_mode: crate::math::FormulaMode,
    /// Canonical binding stays read-only even after its last block is removed.
    canonical_existing: bool,
    /// An empty canonical list has a local paragraph until the first effective edit.
    canonical_empty: bool,
    interception_before: Option<Vec<BlockSnapshot>>,
    /// Operations initiated by a synthetic accessibility control. Unlike
    /// pointer/key input there is no enclosing window event for `MainPanel` to
    /// drain, so retain them until the app's next sync dispatches the same
    /// action-owned delta.
    pending_semantic_ops: Vec<BlockEditOp>,
    /// Shared, document-local find UI. It is an absolute overlay so opening it
    /// never reflows authored content or creates an undo boundary.
    find_field: SearchField,
    replace_field: TextInput,
    replace_button: Button,
    replace_all_button: Button,
    replace_all_node: Option<NodeId>,
    replace_all_requested: std::rc::Rc<std::cell::Cell<bool>>,
    find_snapshot: Vec<BlockContent>,
    replace_node: Option<NodeId>,
    replace_button_node: Option<NodeId>,
    replace_enabled: bool,
    replace_requested: std::rc::Rc<std::cell::Cell<bool>>,
    find_target: Option<FindReplaceTarget>,
    find_node: Option<NodeId>,
    find_visible: bool,
    find_matches: Vec<FindMatch>,
    find_active: Option<usize>,
    find_previous_rect: Rect,
    find_next_rect: Rect,
    find_close_rect: Rect,
    /// Presence keys from the host whose model owns the blocks
    /// ([`Self::set_presence_key`]); the editor's own block-id keys otherwise.
    presence_key: Option<present::PresenceKeyFn>,
}

/// Structural command membership, invalidated until a local editing gesture.
enum RetainedBlockSelection {
    Active(Vec<String>),
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FindMatch {
    block: usize,
    start: usize,
    end: usize,
}

/// A captured find match. Canonical hosts validate this snapshot before writing.
#[derive(Clone, Debug)]
pub struct FindReplaceTarget {
    pub block: BlockContent,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// One search snapshot, with byte ranges into its blocks.
#[derive(Clone, Debug)]
pub struct FindReplaceBatch {
    pub blocks: Vec<BlockContent>,
    pub matches: Vec<(usize, std::ops::Range<usize>)>,
    pub replacement: String,
}

const DEFAULT_EMPTY_HINT: &str = "Start typing — / for blocks, [[ to link, ![[ to embed";
const CODE_HEADER_HEIGHT: f64 = 28.0;
/// How far a code run's ground reaches above its first line's header and
/// below its last line: 4 px stays inside the inter-block gap, never
/// overlapping a neighbour.
const CODE_PAD_Y: f64 = 4.0;
const FORMULA_BLOCK_MIN_HEIGHT: f32 = 164.0;
const FORMULA_VISUAL_MIN_HEIGHT: f32 = 132.0;
const FORMULA_HEADER_HEIGHT: f64 = 28.0;
const FORMULA_SOURCE_HEIGHT: f64 = 24.0;
const FORMULA_PREVIEW_GAP: f64 = 6.0;
const TABLE_ROW_HEIGHT: f64 = 34.0;
const TRANSCLUSION_MIN_HEIGHT: f32 = 76.0;

impl BlockEditor {
    /// Starts with one empty paragraph — there must be a block to focus.
    pub fn new() -> Self {
        crate::i18n::I18nManager::register_widget_messages();
        let replace_requested = std::rc::Rc::new(std::cell::Cell::new(false));
        let requested = replace_requested.clone();
        let replace_all_requested = std::rc::Rc::new(std::cell::Cell::new(false));
        let all_requested = replace_all_requested.clone();
        let mut s = Self {
            blocks: Vec::new(),
            focused: None,
            is_focused: false,
            tab_navigation: false,
            caret_paint_suppressed: false,
            rect: Rect::ZERO,
            text_scale: 1.0,
            handle: HandleMetrics::default(),
            text_pad: 0.0,
            content_sized: false,

            wrap_lines: false,
            compact_paragraph_spacing: false,
            content_node: None,
            stack_leaf: Box::new(BlockStackLeaf {
                editor: std::ptr::null_mut(),
            }),
            stack_boxes: Vec::new(),
            stack: BlockStack::default(),
            layout_theme: std::sync::Arc::new(Theme::dark()),
            layout_theme_fingerprint: None,
            #[cfg(test)]
            stack_measures: 0,
            read_only: false,
            block_handles: true,
            mention_paint: None,
            text_align: crate::richtext::TextAlign::Left,
            font_scale: 1.0,
            #[cfg(test)]
            painted_blocks: 0,
            dirty: true,
            paint_damage: crate::widget::paint_damage::PaintDamage::default(),
            line_overhang: HashMap::new(),
            drawn_overhang: HashMap::new(),
            table_focus_paint: HashMap::new(),
            drawn_table_focus: HashMap::new(),
            table_relayout: false,
            structure_dirty: true,
            edit_pending: false,
            edit_scope: EditScope::Anywhere,
            commit_pending: false,
            last_mouse_pos: Point::ZERO,
            cross_block_drag_start: None,
            cross_block_drag_touched: None,
            document_selection: None,
            retained_block_selection: None,
            slash_menu: SlashMenu::new(),
            slash_open_at: None,
            link_popover: LinkPopover::new(),
            link_open_at: None,
            slash_anchor_caret: None,
            link_anchor_caret: None,
            live_link_hits: Vec::new(),
            transclusion_previews: HashMap::new(),
            transclusion_rects: Vec::new(),
            pending_transclusion_opens: Vec::new(),
            pending_external_link_opens: Vec::new(),
            pasted_images: Vec::new(),
            code_copy_hits: Vec::new(),
            copied_code_start: None,
            copied_code_feedback: None,
            opening_code_fence: None,
            block_drag: None,
            tooltip_viewport: None,
            blocks_list: None,
            local_author: None,
            kbd_sel_anchor: None,
            empty_hint: DEFAULT_EMPTY_HINT.to_string(),
            formula_preview_rects: Vec::new(),
            table_preview_rects: Vec::new(),
            table_editors: HashMap::new(),
            table_mode_controls: HashMap::new(),
            image_preview_rects: Vec::new(),
            image_store: None,
            image_dimensions: HashMap::new(),
            image_attachments: HashMap::new(),
            formula_chrome: Vec::new(),
            visual_formulas: HashMap::new(),
            visual_formula_focus: None,
            preferred_formula_mode: crate::math::FormulaMode::Visual,
            canonical_existing: false,
            canonical_empty: false,
            interception_before: None,
            pending_semantic_ops: Vec::new(),
            find_field: SearchField::new()
                .with_id("block-editor-find")
                .with_placeholder("block-editor-find-placeholder")
                .without_clear(),
            replace_field: TextInput::local()
                .with_id("block-editor-replacement")
                .with_placeholder("block-editor-replace-placeholder"),
            replace_button: Button::new("block-editor-replace", "block-editor-replace")
                .control_size(crate::widget::ControlSize::Regular)
                .on_click(move || requested.set(true)),
            replace_all_button: Button::new("block-editor-replace-all", "block-editor-replace-all")
                .control_size(crate::widget::ControlSize::Regular)
                .tooltip("block-editor-replace-all-help")
                .on_click(move || all_requested.set(true)),
            replace_all_node: None,
            replace_all_requested,
            find_snapshot: Vec::new(),
            replace_node: None,
            replace_button_node: None,
            replace_enabled: false,
            replace_requested,
            find_target: None,
            find_node: None,
            find_visible: false,
            find_matches: Vec::new(),
            find_active: None,
            find_previous_rect: Rect::ZERO,
            find_next_rect: Rect::ZERO,
            find_close_rect: Rect::ZERO,
            presence_key: None,
        };
        s.blocks.push(Block::paragraph(""));
        s
    }

    #[must_use]
    pub fn with_empty_hint(mut self, hint: &str) -> Self {
        self.empty_hint = hint.to_string();
        self
    }

    #[must_use]
    pub fn with_wrap_lines(mut self, wrap: bool) -> Self {
        self.wrap_lines = wrap;
        self
    }

    #[must_use]
    pub fn with_compact_paragraph_spacing(mut self, compact: bool) -> Self {
        self.compact_paragraph_spacing = compact;
        self
    }

    pub fn set_empty_hint(&mut self, hint: &str) {
        self.empty_hint = hint.to_string();
    }

    /// Restrict tooltip targets to the host's visible document viewport.
    /// Hosts must refresh this after scrolling or resizing, using the same
    /// window-coordinate clip as painting (including the handle gutter).
    pub fn set_tooltip_viewport(&mut self, viewport: Option<Rect>) {
        self.tooltip_viewport = viewport;
    }

    pub fn empty_hint(&self) -> &str {
        &self.empty_hint
    }

    /// Note the blocks as they are before an edit, unless an earlier edit the
    /// owner has not taken yet is already noted.
    fn begin_edit(&mut self) {
        if self.interception_before.is_none() {
            self.interception_before = Some(self.block_snapshot());
        }
    }

    /// The user's edits since the owner last took them, as semantic block
    /// operations for the owner to submit as actions. The editor never
    /// writes the document itself.
    pub fn take_edit_ops(&mut self) -> Vec<BlockEditOp> {
        let Some(before) = self.interception_before.take() else {
            return Vec::new();
        };
        let after = self.block_snapshot();
        if self.canonical_empty && before != after {
            self.canonical_empty = false;
            semantic_block_ops(&[], &after)
        } else {
            semantic_block_ops(&before, &after)
        }
    }

    /// Drain UUID-addressed edits requested through synthetic AT controls.
    pub fn take_pending_semantic_ops(&mut self) -> Vec<BlockEditOp> {
        std::mem::take(&mut self.pending_semantic_ops)
    }

    fn block_snapshot(&self) -> Vec<BlockSnapshot> {
        self.blocks
            .iter()
            .map(|block| BlockSnapshot {
                id: block.id.clone(),
                kind: kind_to_str(&block.kind),
                text: block.text().to_string(),
                code_group: block.code_group.clone(),
                nesting: block.nesting,
            })
            .collect()
    }

    /// Minimal markdown parser; empty input still produces one empty
    /// paragraph.
    pub fn from_markdown(md: &str) -> Self {
        let mut editor = Self::new();
        editor.blocks.clear();
        let mut lines = md.split('\n').peekable();
        while let Some(line) = lines.next() {
            if line.trim().is_empty() {
                continue;
            }
            let (depth, body) = parse_list_indent(line);
            if depth > 0
                && !body.starts_with("- ")
                && !body.starts_with("* ")
                && strip_ordered_prefix(body).is_none()
            {
                let mut nested = body.to_string();
                while let Some(next) = lines.peek().copied() {
                    if !next.trim().is_empty() && parse_list_indent(next).0 < depth {
                        break;
                    }
                    let _ = lines.next();
                    nested.push('\n');
                    let mut content = next;
                    for _ in 0..depth {
                        content = content
                            .strip_prefix("  ")
                            .or_else(|| content.strip_prefix('\t'))
                            .unwrap_or(content);
                    }
                    nested.push_str(content);
                }
                for mut block in Self::from_markdown(&nested).blocks {
                    block.set_indent(block.indent().saturating_add(depth));
                    editor.blocks.push(block);
                }
                continue;
            }
            // A fenced body's `\n`s join into one RichTextLine as literal
            // newline characters (code blocks are single-line per Block).
            if let Some(rest) = line.strip_prefix("```") {
                let lang = if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                };
                let mut body: Vec<&str> = Vec::new();
                for next in lines.by_ref() {
                    if next == "```" {
                        break;
                    }
                    body.push(next);
                }
                let mut block = Block::code_block(lang, &body.join("\n"));
                block.code_group = Some(block.id.clone());
                editor.blocks.push(block);
                continue;
            }
            // Three hyphens only, for byte-stable round-trip.
            if line == "---" {
                editor.blocks.push(Block::divider());
                continue;
            }
            // Checklist must be checked before bullet: `- [ ]` also
            // matches the bullet prefix `- `.
            let (indent, body) = parse_list_indent(line);
            if let Some(check_body) = body
                .strip_prefix("- [ ] ")
                .or_else(|| body.strip_prefix("* [ ] "))
            {
                editor
                    .blocks
                    .push(Block::checklist(indent, false, check_body));
                continue;
            }
            if let Some(check_body) = body
                .strip_prefix("- [x] ")
                .or_else(|| body.strip_prefix("* [x] "))
                .or_else(|| body.strip_prefix("- [X] "))
                .or_else(|| body.strip_prefix("* [X] "))
            {
                editor
                    .blocks
                    .push(Block::checklist(indent, true, check_body));
                continue;
            }
            if let Some(bullet_body) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
                editor.blocks.push(Block::bullet(indent, bullet_body));
                continue;
            }
            if let Some(num_body) = strip_ordered_prefix(body) {
                editor.blocks.push(Block::numbered(indent, num_body));
                continue;
            }
            // Headings are never indented — use the raw `line`, not `body`.
            if let Some((level, prefix_len)) = heading_markdown_prefix(line) {
                editor.blocks.push(Block::heading(
                    level,
                    line.get(prefix_len..).unwrap_or_default(),
                ));
            } else {
                editor.blocks.push(Block::paragraph(line));
            }
        }
        if editor.blocks.is_empty() {
            editor.blocks.push(Block::paragraph(""));
        }
        editor
    }

    /// Inverse of `from_markdown`; round-trip is byte-stable.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let ordinals = numbered_ordinals(&self.blocks);
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n");
            }
            let start = out.len();
            match &b.kind {
                BlockKind::Paragraph | BlockKind::Table { .. } => out.push_str(b.text()),
                BlockKind::Quote => {
                    out.push_str("> ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(1) => {
                    out.push_str("# ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(2) => {
                    out.push_str("## ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(_) => {
                    out.push_str("### ");
                    out.push_str(b.text());
                }
                BlockKind::Bullet { indent } => {
                    push_indent(&mut out, *indent);
                    out.push_str("- ");
                    out.push_str(b.text());
                }
                BlockKind::Numbered { indent } => {
                    push_indent(&mut out, *indent);
                    let _ = write!(out, "{}. ", ordinals.get(i).copied().unwrap_or(0).max(1));
                    out.push_str(b.text());
                }
                BlockKind::Checklist { indent, done } => {
                    push_indent(&mut out, *indent);
                    out.push_str(if *done { "- [x] " } else { "- [ ] " });
                    out.push_str(b.text());
                }
                BlockKind::CodeBlock { lang } => {
                    out.push_str("```");
                    if let Some(l) = lang {
                        out.push_str(l);
                    }
                    out.push('\n');
                    out.push_str(b.text());
                    out.push_str("\n```");
                }
                BlockKind::Divider => {
                    out.push_str("---");
                }
            }
            if b.nesting > 0 {
                let content = out.split_off(start);
                for (index, line) in content.split('\n').enumerate() {
                    if index > 0 {
                        out.push('\n');
                    }
                    push_indent(&mut out, b.nesting);
                    out.push_str(line);
                }
            }
        }
        out.push('\n');
        out
    }

    /// Drains per-block + edit-pending dirty; `structure_dirty` is only
    /// peeked here, and stays set until `take_structure_dirty`.
    pub fn is_dirty(&mut self) -> bool {
        for b in &mut self.blocks {
            if b.line.take_dirty().is_some() {
                self.edit_pending = true;
            }
        }
        let was = self.edit_pending || self.structure_dirty;
        self.edit_pending = false;
        was
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// Hide insertion carets while a modal owns input; retain editing state.
    pub fn suppress_caret_paint(&mut self, suppressed: bool) {
        self.caret_paint_suppressed = suppressed;
    }

    pub fn set_focus(&mut self, focused: bool) {
        if focused != self.is_focused {
            self.tab_navigation = false;
        }
        if focused && self.read_only {
            return;
        }
        if self.find_visible {
            self.is_focused = focused;
            let replacement_focus = self.replace_enabled && self.replace_field.is_focused();
            self.find_field.set_focused(focused && !replacement_focus);
            self.replace_field.set_focus(focused && replacement_focus);
            for block in &mut self.blocks {
                block.line.set_focus(false);
            }
            self.dirty = true;
            return;
        }
        if focused
            && self
                .table_editors
                .values()
                .any(|table| table.projected_focus().is_some())
        {
            self.is_focused = true;
            for block in &mut self.blocks {
                block.line.set_focus(false);
            }
            self.dirty = true;
            return;
        }
        if !focused {
            for table in self.table_editors.values_mut() {
                table.blur_projection();
            }
        }
        if !focused && self.block_drag.take().is_some() {
            self.dirty = true;
        }
        if focused == self.is_focused {
            // The host may transfer focus from the registered line to this
            // aggregate editor, blurring that line before calling us.
            if let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index))
                && block.line.is_focused() != focused
            {
                block.line.set_focus(focused);
                self.dirty = true;
            }
            return;
        }
        self.is_focused = focused;
        if focused {
            if self.focused.is_none() && self.visual_formula_focus.is_none() {
                self.focused = Some(0);
            }
            if let Some(idx) = self.focused
                && let Some(b) = self.blocks.get_mut(idx)
            {
                b.line.set_focus(true);
            }
        } else {
            self.visual_formula_focus = None;
            if let Some(idx) = self.focused
                && let Some(b) = self.blocks.get_mut(idx)
            {
                b.line.set_focus(false);
            }
        }
        self.dirty = true;
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Lowest painted edge, including formula previews that intentionally sit
    /// below their editable source line inside the same layout child.
    pub fn content_bottom(&self) -> f64 {
        (0..self.blocks.len())
            .filter_map(|index| self.block_paint_rect(index).map(|rect| rect.y1))
            .fold(self.rect.y0, f64::max)
    }

    fn block_paint_rect(&self, index: usize) -> Option<Rect> {
        let block = self.blocks.get(index)?;
        let mut rect = block.line.get_rect();
        rect.x0 = content_edge(
            rect,
            prefix_gutter_for(&block.kind) * self.text_scale,
            block.line.text_inset(&self.layout_theme),
        );
        rect.y0 -= code_header_height(&self.blocks, index, self.text_scale);
        for preview in [
            &self.formula_preview_rects,
            &self.table_preview_rects,
            &self.image_preview_rects,
        ] {
            if let Some(Some(preview)) = preview.get(index) {
                rect = rect.union(*preview);
            }
        }
        if let Some(Some((preview, _))) = self.transclusion_rects.get(index) {
            rect = rect.union(*preview);
        }
        Some(rect)
    }

    /// Everything the last draw painted: the editor's rect and what its
    /// lines and tables' focused cells painted past it.
    fn paint_extent(&self) -> Rect {
        self.line_overhang
            .values()
            .chain(self.table_focus_paint.values())
            .fold(self.rect, |extent, painted| extent.union(*painted))
    }

    /// For tests + notes-app persistence to seed blocks directly;
    /// production code should prefer the public mutators.
    pub fn blocks_mut(&mut self) -> &mut Vec<Block> {
        self.edit_scope = EditScope::Anywhere;
        &mut self.blocks
    }

    /// Which blocks the person may have changed since the last call: a host
    /// that owns the document compares only those with what it presented.
    /// Plain typing in a paragraph names that paragraph and its neighbours;
    /// anything else (a structural edit, a pointer, an action, direct access
    /// to the blocks) may have changed any block.
    pub fn take_edit_scope(&mut self) -> EditScope {
        std::mem::replace(&mut self.edit_scope, EditScope::Nothing)
    }

    /// Treat any block as possibly changed: for a host that changes blocks
    /// through [`Self::blocks_mut`] or other direct access.
    pub fn note_edit_anywhere(&mut self) {
        self.edit_scope = EditScope::Anywhere;
    }

    /// For the notes app's markdown parser, which produces a richer
    /// representation (inline marks) than this widget's `from_markdown`.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.edit_scope = EditScope::Anywhere;
        self.blocks = blocks;
        if self.blocks.is_empty() {
            self.blocks.push(Block::paragraph(""));
        }
        self.focused = None;
        self.is_focused = false;
        self.retained_block_selection = None;
        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.formula_preview_rects.clear();
        self.table_preview_rects.clear();
        self.formula_chrome.clear();
        self.visual_formulas.clear();
        self.visual_formula_focus = None;
        self.structure_dirty = true;
        self.dirty = true;
    }

    /// Show the blocks without taking input or focus: a read-only view of
    /// rich text (a sent message, a comment reply).
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.set_read_only(read_only);
        self
    }

    /// The handle gutter of a block document: the lane its drag handles
    /// hang in, outside the text column, as wide as a handle and the gap to
    /// the text. Zero when the editor shows no handles.
    ///
    /// A page that holds the editor keeps its one content gutter outside
    /// this lane: it places the editor's rect on its gutter and lines its
    /// title and metadata up with the text ([`Self::text_inset`] in from the
    /// rect), so a hovered handle stands a full gutter clear of the sidebar
    /// or window edge beside the page.
    pub fn handle_gutter(&self, theme: &Theme) -> f64 {
        if self.block_handles && !self.read_only {
            let handle = HandleMetrics::from_tokens(&theme.tokens.spacing, &theme.tokens.metrics);
            handle.width + handle.gap
        } else {
            0.0
        }
    }

    /// How far a paragraph's text starts inside the editor's rect: the
    /// handle gutter (when shown) or the lines' own text inset, whichever
    /// is wider. Every block kind starts at this edge, so a host lines a
    /// title, a caption or a composer's controls up with it instead of
    /// measuring a laid-out caret.
    pub fn text_inset(&self, theme: &Theme) -> f64 {
        let line = self.blocks.first().map_or_else(
            || RichTextLine::new().text_inset(theme),
            |b| b.line.text_inset(theme),
        );
        line.max(self.handle_gutter(theme))
    }

    /// Show or hide the drag handle beside each block. A composer or a
    /// read-only row has none: its blocks are not rearranged by dragging.
    #[must_use]
    pub fn with_block_handles(mut self, shown: bool) -> Self {
        self.block_handles = shown;
        self
    }

    /// Paint mentions in the colours `paint` gives each participant.
    pub fn set_mention_paint(&mut self, paint: Option<crate::widget::marks::MentionPaint>) {
        for block in &mut self.blocks {
            block.line.set_mention_paint(paint.clone());
        }
        self.mention_paint = paint;
        self.dirty = true;
    }

    /// The colours mentions are painted in, if the owner set them.
    pub(crate) fn mention_paint(&self) -> Option<&crate::widget::marks::MentionPaint> {
        self.mention_paint.as_ref()
    }

    /// Put `text` at the caret of the focused block, replacing a selection
    /// within it: an emoji picked from a menu, a pasted snippet. Returns
    /// whether there was a block to put it in.
    pub fn insert_text(&mut self, text: &str) -> bool {
        self.insert_marked(text, None, "")
    }

    /// Put a mention of the participant `id`, shown as `label` (`@Ada`), at
    /// the caret of the focused block, followed by a space. The label is one
    /// marked run, so it reads back as a reference, not as text that happens
    /// to start with `@`.
    pub fn insert_mention(&mut self, id: &str, label: &str) -> bool {
        self.insert_marked(label, Some(Mark::Mention(id.to_owned())), " ")
    }

    /// The focused block's text before the caret: what an autocomplete
    /// reads its query from (`@ad` while a mention is typed).
    #[must_use]
    pub fn text_before_caret(&self) -> Option<&str> {
        let block = self.focused.and_then(|index| self.blocks.get(index))?;
        let text = block.line.value();
        text.get(..block.line.cursor_pos().min(text.len()))
    }

    /// Select the `bytes` before the caret in the focused block, so the next
    /// insertion replaces them (the typed `@ad` a picked mention replaces).
    pub fn select_before_caret(&mut self, bytes: usize) {
        let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index)) else {
            return;
        };
        let caret = block.line.cursor_pos().min(block.line.value().len());
        let start = block
            .line
            .value()
            .floor_char_boundary(caret.saturating_sub(bytes));
        block.line.inner_mut().set_selection(start, caret);
        self.dirty = true;
    }

    fn insert_marked(&mut self, text: &str, mark: Option<Mark>, after: &str) -> bool {
        if text.is_empty() || self.read_only {
            return false;
        }
        let Some(index) = self.focused.filter(|index| *index < self.blocks.len()) else {
            return false;
        };
        self.edit_scope
            .widen(EditScope::Blocks(index..index.saturating_add(1)));
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        let line = &mut block.line;
        let value = line.value().to_owned();
        let (start, end) = line
            .selection_range()
            .filter(|(start, end)| start < end)
            .unwrap_or_else(|| {
                let at = line.cursor_pos().min(value.len());
                (at, at)
            });
        let (start, end) = (
            value.floor_char_boundary(start),
            value.floor_char_boundary(end.max(start)),
        );
        let mut next = value;
        next.replace_range(start..end, &format!("{text}{after}"));
        let inserted = text.len().saturating_add(after.len());
        let mut spans = line.marks().to_vec();
        crate::widget::marks::shift_after_edit(&mut spans, start, inserted, end - start);
        if let Some(mark) = mark {
            spans.push(MarkSpan::new(start..start.saturating_add(text.len()), mark));
        }
        line.set_text(&next);
        line.set_marks(spans);
        line.set_cursor_pos(start.saturating_add(inserted));
        self.dirty = true;
        true
    }

    /// Stop or resume taking input and focus.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
        if read_only {
            Self::set_focus(self, false);
        }
        self.dirty = true;
    }

    /// Align every line within the editor's width: a text box whose
    /// alignment belongs to the box rather than to each paragraph.
    #[must_use]
    pub fn with_text_align(mut self, align: crate::richtext::TextAlign) -> Self {
        self.set_text_align(align);
        self
    }

    /// Change how every line is aligned.
    pub fn set_text_align(&mut self, align: crate::richtext::TextAlign) {
        if self.text_align != align {
            self.text_align = align;
            self.dirty = true;
        }
    }

    /// How every line is aligned.
    #[must_use]
    pub fn text_align(&self) -> crate::richtext::TextAlign {
        self.text_align
    }

    /// Size body text at `px` logical pixels; every other kind keeps its
    /// size relative to body text.
    #[must_use]
    pub fn with_body_size(mut self, px: f32) -> Self {
        self.set_body_size(px);
        self
    }

    /// Change the body text size (see [`Self::with_body_size`]).
    pub fn set_body_size(&mut self, px: f32) {
        let body = BlockKind::Paragraph.font_size();
        let scale = if px.is_finite() && px > 0.0 && body > 0.0 {
            px / body
        } else {
            1.0
        };
        if self.font_scale.to_bits() != scale.to_bits() {
            self.font_scale = scale;
            self.structure_dirty = true;
            self.dirty = true;
        }
    }

    /// The body text size, in logical pixels.
    #[must_use]
    pub fn body_size(&self) -> f32 {
        BlockKind::Paragraph.font_size() * self.font_scale
    }

    /// Whether the editor shows its blocks without taking input.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Select every block's text, as Ctrl+A does.
    pub fn select_all(&mut self) {
        self.handle_document_selection_ctrl_key(&Key::Character("a".into()));
        self.dirty = true;
    }

    /// Whether a menu or popover of the editor is open and takes Escape and
    /// Enter itself (the slash menu, a link picker, find).
    #[must_use]
    pub fn has_open_popup(&self) -> bool {
        self.slash_menu.is_visible() || self.link_popover.is_visible() || self.find_visible
    }

    /// The id local edits are attributed to, if any.
    #[must_use]
    pub fn local_author(&self) -> Option<&str> {
        self.local_author.as_deref()
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        for b in &mut self.blocks {
            b.line.set_local_author(author.clone());
        }
        self.local_author = author;
    }

    /// Bind a canonical existing list without initializing its durable contents.
    /// # Errors
    /// Rejects malformed identities, kinds or Text objects before installing local state.
    pub fn bind_existing_blocks(
        &mut self,
        state: &AutoCommit,
        list: ObjId,
    ) -> Result<(), crate::view::ThawError> {
        let invalid = |message: &str| crate::view::ThawError::State(message.into());
        if state.object_type(&list).ok() != Some(ObjType::List) {
            return Err(invalid("canonical blocks must be a List"));
        }
        let len = state.length(&list);
        let mut objects = Vec::with_capacity(len);
        for index in 0..len {
            let values = state
                .get_all(&list, index)
                .map_err(|e| invalid(&e.to_string()))?;
            let [(Value::Object(ObjType::Map), map)] = values.as_slice() else {
                return Err(invalid("canonical block must be one Map"));
            };
            objects.push(map.clone());
        }
        self.bind_existing_block_objects(state, &objects)?;
        self.blocks_list = Some(list);
        Ok(())
    }

    /// Adopt an action owner's ordered projection of existing block objects.
    ///
    /// The owner resolves hierarchy and ordering, then reprojects after document
    /// changes. Storage order need not match display order, and moving an object
    /// never requires replacing its collaborative Text. Like list binding, this
    /// method is read-only and local edits are emitted as semantic operations.
    ///
    /// # Errors
    /// Rejects malformed or duplicate identities and non-Text bodies before
    /// changing the editor. The owner supplies its visible canonical block Maps.
    pub fn bind_existing_block_objects(
        &mut self,
        state: &AutoCommit,
        objects: &[ObjId],
    ) -> Result<(), crate::view::ThawError> {
        let invalid = |message: &str| crate::view::ThawError::State(message.into());
        let mut blocks = Vec::with_capacity(objects.len());
        let mut ids = std::collections::HashSet::new();
        for map in objects {
            if state.object_type(map).ok() != Some(ObjType::Map) {
                return Err(invalid("canonical block must be a Map"));
            }
            let read_string = |key: &str| -> Result<String, crate::view::ThawError> {
                let values = state
                    .get_all(map, key)
                    .map_err(|e| invalid(&e.to_string()))?;
                match values.as_slice() {
                    [(Value::Scalar(value), _)] => value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| invalid("canonical block field must be a string")),
                    _ => Err(invalid("canonical block field is missing or conflicted")),
                }
            };
            let id = read_string("id")?;
            let uuid = Uuid::parse_str(&id).map_err(|e| invalid(&e.to_string()))?;
            if uuid.is_nil() || !ids.insert(uuid) {
                return Err(invalid("nil or duplicate block UUID"));
            }
            let spelling = read_string("kind")?;
            let kind = kind_from_str(&spelling);
            if !valid_existing_kind(&spelling) {
                return Err(invalid("invalid canonical block kind"));
            }
            let values = state
                .get_all(map, "text")
                .map_err(|e| invalid(&e.to_string()))?;
            let [(Value::Object(ObjType::Text), text)] = values.as_slice() else {
                return Err(invalid("canonical block text must be one Text"));
            };
            let mut block = block_for_kind(&kind);
            block.id = id;
            block.nesting = read_nesting(state, map)?;
            if kind.supports_indent() && block.nesting > 0 {
                return Err(invalid("list depth belongs to its kind"));
            }
            block.code_group = read_code_group(state, map)?;
            if block.code_group.is_some() && !matches!(kind, BlockKind::CodeBlock { .. }) {
                return Err(invalid("code group on a non-code block"));
            }
            block.line.set_local_author(self.local_author.clone());
            block.line.bind_text(text.clone());
            block.line.adopt_bound_text(state)?;
            blocks.push(block);
        }
        // The placeholder is local and deliberately unbound; it never has a stable caret.
        if blocks.is_empty() {
            let mut block = Block::paragraph("");
            block.line.set_local_author(self.local_author.clone());
            blocks.push(block);
        }
        self.set_blocks(blocks);
        self.canonical_existing = true;
        self.canonical_empty = objects.is_empty();
        // Structural sync belongs to the action owner's projection. Looking
        // through a physical List here would silently undo its logical order.
        self.blocks_list = None;
        self.edit_pending = false;
        self.interception_before = None;
        self.pending_semantic_ops.clear();
        Ok(())
    }

    /// Preserves focus + caret across the rebuild so a remote edit
    /// elsewhere doesn't yank the local caret.
    #[expect(
        clippy::indexing_slicing,
        reason = "the restored focus index is clamped to the non-empty rebuilt projection"
    )]
    fn rebuild_from_doc(&mut self, state: &AutoCommit, list: &ObjId, doc_len: usize) {
        let saved_focus = self.focused;
        let saved_id = self
            .focused
            .and_then(|i| self.blocks.get(i))
            .map(|b| b.id.clone());
        let saved_table_cell = self.focused_table_cell();
        let saved_table_caret = saved_table_cell
            .as_ref()
            .and_then(|(id, _, _)| self.table_editors.get(id))
            .and_then(super::block_editor_table::DocumentTable::projected_caret);
        let saved_caret = self
            .focused
            .and_then(|i| self.blocks.get(i))
            .map(|b| b.line.cursor_pos());

        let mut new_blocks = Vec::with_capacity(doc_len);
        for i in 0..doc_len {
            let kind = kind_from_str(&read_block_kind(state, list, i));
            let mut b = block_for_kind(&kind);
            b.id = read_block_id_at(state, list, i, &kind);
            b.nesting = list_map(state, list, i)
                .and_then(|map| read_nesting(state, &map).ok())
                .unwrap_or(0);
            b.code_group = list_map(state, list, i)
                .and_then(|map| read_code_group(state, &map).ok().flatten());
            b.line.set_local_author(self.local_author.clone());
            if let Some(t) = doc_text_obj(state, list, i) {
                b.line.bind_text(t);
                b.line.sync(state, list);
            }
            new_blocks.push(b);
        }
        if new_blocks.is_empty() {
            new_blocks.push(Block::paragraph(""));
        }
        self.blocks = new_blocks;
        // Structural changes invalidate index-addressed endpoints. A fresh
        // local selection is safer than painting stale bytes on new blocks.
        self.retained_block_selection = None;
        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.structure_dirty = true;

        if let Some(fi) = saved_focus {
            let fi = saved_id
                .as_ref()
                .and_then(|id| self.blocks.iter().position(|b| &b.id == id))
                .unwrap_or_else(|| fi.min(self.blocks.len().saturating_sub(1)));
            self.focus_block(fi, CursorPlacement::Start);
            if let Some((id, row, column)) = saved_table_cell
                && self.blocks[fi].id == id
                && matches!(self.blocks[fi].kind, BlockKind::Table { visual: true })
                && let Some(table) = self.table_editors.get_mut(&id)
            {
                let rows = markdown_table_data_rows(self.blocks[fi].text());
                let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
                if !rows.is_empty() && columns > 0 {
                    table.focus_projected_cell(
                        row.min(rows.len().saturating_sub(1)),
                        column.min(columns.saturating_sub(1)),
                    );
                    if let Some(caret) = saved_table_caret {
                        table.restore_projected_caret(&caret);
                    }
                }
            } else if let Some(b) = self.blocks.get_mut(fi)
                && let Some(c) = saved_caret
            {
                b.line.set_cursor_pos(c);
            }
        }
    }

    pub fn focused_index(&self) -> Option<usize> {
        self.focused
    }

    /// Whether the active source line or projected table cell owns an IME preedit.
    pub fn is_composing(&self) -> bool {
        (self.find_visible && (self.find_field.is_composing() || self.replace_field.is_composing()))
            || self
                .focused
                .and_then(|index| self.blocks.get(index))
                .is_some_and(|block| {
                    block.line.inner().is_composing()
                        || self.table_editors.get(&block.id).is_some_and(
                            super::block_editor_table::DocumentTable::projected_is_composing,
                        )
                })
    }

    pub fn focused_table_cell(&self) -> Option<(String, usize, usize)> {
        self.blocks.iter().find_map(|block| {
            self.table_editors
                .get(&block.id)
                .and_then(super::block_editor_table::DocumentTable::projected_focus)
                .map(|(row, column)| (block.id.clone(), row, column))
        })
    }

    /// Opens the shared in-document find overlay without touching document
    /// state. Hosts may call this for a window-level Find command even when a
    /// sibling field currently owns focus.
    pub fn open_find(&mut self) {
        if self.is_composing() {
            return;
        }
        self.find_visible = true;
        self.structure_dirty = true;
        self.replace_field.set_focus(false);
        self.find_field.set_focused(true);
        self.find_field.select_all();
        for block in &mut self.blocks {
            block.line.set_focus(false);
        }
        self.refresh_find_matches(true);
        self.dirty = true;
    }

    fn close_find(&mut self) {
        self.find_visible = false;
        self.structure_dirty = true;
        self.replace_field.set_focus(false);
        self.replace_requested.set(false);
        self.replace_all_requested.set(false);
        self.find_snapshot.clear();
        self.find_field.set_focused(false);
        if let Some(index) = self.focused
            && let Some(block) = self.blocks.get_mut(index)
        {
            block.line.set_focus(true);
        }
        self.dirty = true;
    }

    fn refresh_find_matches(&mut self, select_first: bool) {
        let query = self.find_field.value().to_string();
        self.find_matches.clear();
        self.find_active = None;
        self.find_target = None;
        self.find_snapshot.clear();
        if query.is_empty() {
            return;
        }
        if self.replace_enabled {
            self.find_snapshot = self.blocks.iter().map(Block::content).collect();
        }
        self.clear_find_document_selection();
        for (block, item) in self.blocks.iter().enumerate() {
            if !item.kind.is_editable() {
                continue;
            }
            self.find_matches.extend(
                case_folded_match_ranges(item.text(), &query)
                    .into_iter()
                    .map(|(start, end)| FindMatch { block, start, end }),
            );
        }
        if select_first && !self.find_matches.is_empty() {
            let current_block = self.focused.unwrap_or(0);
            let current_byte = self
                .blocks
                .get(current_block)
                .map_or(0, |block| block.line.cursor_pos());
            let active = self
                .find_matches
                .iter()
                .position(|item| {
                    item.block > current_block
                        || (item.block == current_block && item.start >= current_byte)
                })
                .unwrap_or(0);
            self.activate_find_match(active);
        }
    }

    fn activate_find_match(&mut self, active: usize) {
        let replacement_focus = self.replace_field.is_focused();
        let Some(found) = self.find_matches.get(active).copied() else {
            self.find_active = None;
            return;
        };
        self.find_target = self.blocks.get(found.block).map(|block| FindReplaceTarget {
            block: block.content(),
            start_byte: found.start,
            end_byte: found.end,
        });
        self.clear_find_document_selection();
        self.retained_block_selection = None;
        self.document_selection = None;
        self.focus_block(found.block, CursorPlacement::Byte(found.end));
        if let Some(block) = self.blocks.get_mut(found.block) {
            block.line.inner_mut().set_selection(found.start, found.end);
            // The query continues to own keyboard focus while the matching
            // document range remains visibly selected.
            block.line.set_focus(false);
        }
        self.focus_find_input(replacement_focus);
        self.find_active = Some(active);
        self.dirty = true;
    }

    fn clear_find_document_selection(&mut self) {
        for block in &mut self.blocks {
            let cursor = block.line.cursor_pos();
            block.line.inner_mut().set_selection(cursor, cursor);
        }
        debug_assert!(
            self.blocks
                .iter()
                .all(|block| block.line.selection_range().is_none()),
            "find navigation must leave only the newly activated match selected"
        );
    }

    fn step_find(&mut self, reverse: bool) {
        if self.find_matches.is_empty() {
            self.refresh_find_matches(true);
            return;
        }
        let len = self.find_matches.len();
        let next = match (self.find_active, reverse) {
            (Some(0) | None, true) => len.saturating_sub(1),
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) if index < len.saturating_sub(1) => index.saturating_add(1),
            (Some(_) | None, false) => 0,
        };
        self.activate_find_match(next);
    }

    fn focus_find_input(&mut self, replacement: bool) {
        self.find_field.set_focused(!replacement);
        self.replace_field.set_focus(replacement);
        for block in &mut self.blocks {
            block.line.set_focus(false);
        }
        self.dirty = true;
    }

    /// Enable replacement only when a canonical host consumes its requests.
    pub fn set_find_replace_enabled(&mut self, enabled: bool) {
        self.replace_enabled = enabled;
    }

    pub fn take_find_replace_request(&mut self) -> bool {
        self.replace_requested.replace(false)
    }

    /// The replacement payload keeps byte endpoints and original block identity.
    pub fn find_replace_request(&self) -> Option<(FindReplaceTarget, String)> {
        if !self.replace_enabled || !self.find_visible || self.read_only || self.is_composing() {
            return None;
        }
        let target = self.find_target.as_ref()?;
        let matched = target.block.text.get(target.start_byte..target.end_byte)?;
        if matched.is_empty() || matched.to_lowercase() != self.find_field.value().to_lowercase() {
            return None;
        }
        let replacement = self.replace_field.value();
        if replacement.chars().any(|c| c.is_control() && c != '\t') {
            return None;
        }
        Some((target.clone(), replacement.to_owned()))
    }

    pub fn take_find_replace_all_request(&mut self) -> bool {
        self.replace_all_requested.replace(false)
    }

    pub fn can_replace_all(&self) -> bool {
        self.replace_enabled
            && self.find_visible
            && !self.read_only
            && !self.is_composing()
            && !self.find_matches.is_empty()
            && self.find_matches.len() <= 10_000
            // Match the canonical action's aggregate text budget before enabling
            // a control whose payload the host would necessarily refuse.
            && self.replace_field.value().chars().count().checked_mul(self.find_matches.len())
                .is_some_and(|total| total <= 1_000_000)
            && !self
                .replace_field
                .value()
                .chars()
                .any(|c| c.is_control() && c != '\t')
    }

    /// Clone the snapshot only when a canonical host consumes the action.
    pub fn find_replace_all_request(&self) -> Option<FindReplaceBatch> {
        self.can_replace_all().then(|| FindReplaceBatch {
            blocks: self.find_snapshot.clone(),
            matches: self
                .find_matches
                .iter()
                .map(|item| (item.block, item.start..item.end))
                .collect(),
            replacement: self.replace_field.value().to_owned(),
        })
    }

    pub fn finish_find_replace_all(&mut self) {
        let replacement_focus = self.replace_field.is_focused();
        self.refresh_find_matches(true);
        self.focus_find_input(replacement_focus);
    }

    /// Move beyond inserted text, refresh matches, and retain overlay focus.
    pub fn finish_find_replace(&mut self, target: &FindReplaceTarget, replacement_len: usize) {
        let replacement_focus = self.replace_field.is_focused();
        if let Some(index) = self
            .blocks
            .iter()
            .position(|block| block.id == target.block.id)
        {
            self.focus_block(
                index,
                CursorPlacement::Byte(target.start_byte.saturating_add(replacement_len)),
            );
        }
        self.refresh_find_matches(true);
        self.focus_find_input(replacement_focus);
    }

    fn handle_find_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::MouseButton;

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse_pos =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
            && key.eq_ignore_ascii_case("f")
        {
            self.open_find();
            *ctx.handled = true;
            return true;
        }
        if !self.find_visible {
            return false;
        }

        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Tab) if self.replace_enabled && !self.is_composing() => {
                    let replacement_focus = !self.replace_field.is_focused();
                    self.focus_find_input(replacement_focus);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if self.replace_enabled
                        && self.replace_field.is_focused()
                        && !self.is_composing() =>
                {
                    if ctx.modifiers.state().shift_key()
                        || ctx.modifiers.state().control_key()
                        || ctx.modifiers.state().super_key()
                    {
                        if self.can_replace_all() {
                            self.replace_all_requested.set(true);
                        }
                    } else {
                        self.replace_requested.set(true);
                    }
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) if !self.is_composing() => {
                    self.close_find();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter | NamedKey::F3) if !self.is_composing() => {
                    self.step_find(ctx.modifiers.state().shift_key());
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }

        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.is_composing() {
                let input_rect = if self.replace_field.is_focused() {
                    self.replace_field.get_rect()
                } else {
                    self.find_field.get_rect()
                };
                if !input_rect.contains(self.last_mouse_pos) {
                    *ctx.handled = true;
                    return true;
                }
            }
            if self.find_previous_rect.contains(self.last_mouse_pos) {
                self.step_find(true);
                *ctx.handled = true;
                return true;
            }
            if self.find_next_rect.contains(self.last_mouse_pos) {
                self.step_find(false);
                *ctx.handled = true;
                return true;
            }
            if self.find_close_rect.contains(self.last_mouse_pos) {
                self.close_find();
                *ctx.handled = true;
                return true;
            }
            if self.replace_enabled
                && (self.replace_field.get_rect().contains(self.last_mouse_pos)
                    || self.replace_button.get_rect().contains(self.last_mouse_pos)
                    || self
                        .replace_all_button
                        .get_rect()
                        .contains(self.last_mouse_pos))
            {
                self.focus_find_input(true);
            } else if self.find_field.get_rect().contains(self.last_mouse_pos) {
                self.focus_find_input(false);
            }
            let controls = self.find_previous_rect.union(self.find_close_rect);
            if !(self.replace_enabled
                && (self.replace_field.get_rect().contains(self.last_mouse_pos)
                    || self.replace_button.get_rect().contains(self.last_mouse_pos)
                    || self
                        .replace_all_button
                        .get_rect()
                        .contains(self.last_mouse_pos)))
                && !self.find_field.get_rect().contains(self.last_mouse_pos)
                && !controls.contains(self.last_mouse_pos)
            {
                self.close_find();
                return false;
            }
        }

        if self.replace_enabled {
            self.replace_button
                .set_enabled(self.find_replace_request().is_some());
            if let Some(node) = self.replace_button_node {
                self.replace_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            self.replace_all_button.set_enabled(self.can_replace_all());
            if let Some(node) = self.replace_all_node {
                self.replace_all_button
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            if self.replace_field.is_focused() {
                if let Some(node) = self.replace_node {
                    self.replace_field
                        .handle_event(event, &mut ctx.reborrow(ctx.path, node));
                }
                if self.replace_field.take_dirty().is_some() {
                    self.dirty = true;
                }
                *ctx.handled = true;
                return true;
            }
        }
        if let Some(node) = self.find_node {
            self.find_field
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if self.find_field.take_dirty().is_some() {
                self.refresh_find_matches(true);
            }
        }
        *ctx.handled
    }

    #[expect(
        clippy::arithmetic_side_effects,
        reason = "finite theme-scaled drawing coordinates intentionally use affine arithmetic"
    )]
    fn draw_find_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.find_visible {
            return;
        }
        self.find_field
            .draw(scene, transform, text_renderer, theme, presence);
        if self.replace_enabled {
            self.replace_field
                .draw(scene, transform, text_renderer, theme, presence);
            self.replace_button
                .draw(scene, transform, text_renderer, theme, presence);
            self.replace_all_button
                .draw(scene, transform, text_renderer, theme, presence);
        }

        let controls = self
            .find_previous_rect
            .union(self.find_close_rect)
            .inflate(3.0 * self.text_scale, 3.0 * self.text_scale);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &vello::kurbo::RoundedRect::from_rect(controls, 8.0 * self.text_scale),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            crate::surface::edge_on(theme.tokens.colors.surfaces.raised),
            None,
            &vello::kurbo::RoundedRect::from_rect(controls, 8.0 * self.text_scale),
        );
        for (rect, icon) in [
            (self.find_previous_rect, Icon::ChevronUp),
            (self.find_next_rect, Icon::ChevronDown),
            (self.find_close_rect, Icon::X),
        ] {
            if rect.contains(self.last_mouse_pos) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.lift(0.25),
                    None,
                    &vello::kurbo::RoundedRect::from_rect(rect, 6.0 * self.text_scale),
                );
            }
            draw_icon(
                scene,
                transform,
                rect.inflate(-7.0 * self.text_scale, -7.0 * self.text_scale),
                icon,
                theme.tokens.colors.text_primary,
                1.6 * self.text_scale,
            );
        }

        let status = if self.find_field.value().is_empty() {
            String::new()
        } else if self.find_matches.is_empty() {
            crate::i18n!("block-editor-find-no-results")
        } else {
            format!(
                "{} / {}",
                self.find_active.map_or(0, |index| index.saturating_add(1)),
                self.find_matches.len()
            )
        };
        if !status.is_empty() {
            let field = self.find_field.get_rect();
            let (width, height) = text_renderer.measure(&status, 11.0, None);
            let chip_width = f64::from(width) + 12.0 * self.text_scale;
            let chip = Rect::new(
                field.x1 - chip_width - 5.0 * self.text_scale,
                field.y0 + 6.0 * self.text_scale,
                field.x1 - 5.0 * self.text_scale,
                field.y1 - 6.0 * self.text_scale,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &vello::kurbo::RoundedRect::from_rect(chip, 6.0 * self.text_scale),
            );
            text_renderer.draw_text(
                scene,
                &status,
                11.0,
                if self.find_matches.is_empty() {
                    theme.tokens.colors.semantic.danger
                } else {
                    theme.tokens.colors.text_dim
                },
                transform
                    * Affine::translate((
                        chip.x0 + 6.0 * self.text_scale,
                        chip.center().y - f64::from(height) / 2.0,
                    )),
                None,
            );
        }
    }

    pub fn focus_block(&mut self, idx: usize, cursor_at: CursorPlacement) {
        self.retained_block_selection = None;
        self.tab_navigation = false;
        for table in self.table_editors.values_mut() {
            table.blur_projection();
        }
        let target = idx.min(self.blocks.len().saturating_sub(1));
        if let Some(block) = self.blocks.get(target)
            && matches!(block.kind, BlockKind::Table { visual: true })
        {
            let rows: Vec<Vec<String>> = markdown_table_data_rows(block.text())
                .into_iter()
                .map(|row| row.into_iter().map(decode_table_cell).collect())
                .collect();
            let row = if matches!(cursor_at, CursorPlacement::End) {
                rows.len().saturating_sub(1)
            } else {
                0
            };
            let column = if matches!(cursor_at, CursorPlacement::End) {
                rows.get(row).map_or(0, |row| row.len().saturating_sub(1))
            } else {
                0
            };
            let table = self
                .table_editors
                .entry(block.id.clone())
                .or_insert_with(|| {
                    crate::widget::block_editor_table::DocumentTable::new(format!(
                        "table-{}",
                        block.id
                    ))
                });
            table.project_cells(&rows);
            table.focus_projected_cell(row, column);
            for block in &mut self.blocks {
                block.line.set_focus(false);
            }
            self.focused = Some(target);
            self.is_focused = true;
            self.dirty = true;
            return;
        }
        if let Some(prev) = self.focused
            && prev != target
            && let Some(b) = self.blocks.get_mut(prev)
        {
            b.line.set_focus(false);
        }
        if let Some(b) = self.blocks.get_mut(target) {
            b.line.set_focus(true);
            match cursor_at {
                CursorPlacement::Start => b.line.set_cursor_pos(0),
                CursorPlacement::End => {
                    let end = b.line.value().len();
                    b.line.set_cursor_pos(end);
                }
                CursorPlacement::Byte(p) => b.line.set_cursor_pos(p),
            }
        }
        self.focused = Some(target);
        self.is_focused = true;
        self.dirty = true;
    }

    /// Splitting at byte 0 of an empty block still produces two blocks
    /// — how the user gets a blank line by hitting Enter twice.
    pub fn split_at_cursor(&mut self, idx: usize, split_at: usize) {
        let Some(block) = self.blocks.get_mut(idx) else {
            return;
        };
        let cur_kind = block.kind.clone();
        let text = block.line.value().to_string();
        let split = split_at.min(text.len());
        let split = text.floor_char_boundary(split);
        let (prefix, suffix) = text.split_at(split);
        let prefix_marks = marks_before(block.line.marks(), split);
        let suffix_marks = marks_after(block.line.marks(), split);
        block.line.set_text(prefix);
        block.line.set_marks(prefix_marks);
        let mut suffix_block = match cur_kind {
            BlockKind::Bullet { indent } => Block::bullet(indent, suffix),
            BlockKind::Numbered { indent } => Block::numbered(indent, suffix),
            BlockKind::Checklist { indent, .. } => {
                // Unchecked even if the source was checked — "add a new todo".
                Block::checklist(indent, false, suffix)
            }
            BlockKind::CodeBlock { ref lang } => Block::code_block(lang.clone(), suffix),
            BlockKind::Quote => Block::quote(suffix),
            _ => Block::paragraph(suffix),
        };
        suffix_block.line.set_marks(suffix_marks);
        block.code_group.clone_into(&mut suffix_block.code_group);
        suffix_block.set_indent(block.indent());
        let next_idx = idx.saturating_add(1);
        self.blocks.insert(next_idx, suffix_block);
        self.structure_dirty = true;
        self.dirty = true;
        self.focus_block(next_idx, CursorPlacement::Start);
    }

    /// No-op at `idx == 0`. The surviving block keeps the *previous*
    /// block's kind — backspacing a paragraph into a heading folds the
    /// text into the heading, not the reverse.
    pub fn merge_with_previous(&mut self, idx: usize) {
        if idx == 0 {
            return;
        }
        let prev_idx = idx.saturating_sub(1);
        let Some(cur_text) = self.blocks.get(idx).map(|b| b.line.value().to_string()) else {
            return;
        };
        let Some(prev_text) = self
            .blocks
            .get(prev_idx)
            .map(|b| b.line.value().to_string())
        else {
            return;
        };
        let join_at = prev_text.len();
        let merged = format!("{prev_text}{cur_text}");
        let mut marks = self.blocks[prev_idx].line.marks().to_vec();
        marks.extend(self.blocks[idx].line.marks().iter().map(|span| {
            MarkSpan::new(
                span.range.start.saturating_add(join_at)..span.range.end.saturating_add(join_at),
                span.mark.clone(),
            )
        }));
        if let Some(b) = self.blocks.get_mut(prev_idx) {
            b.line.set_text(&merged);
            b.line.set_marks(marks);
        }
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.dirty = true;
        self.focus_block(prev_idx, CursorPlacement::Byte(join_at));
    }

    /// Never deletes the last block — the editor always retains at
    /// least one paragraph to receive focus.
    pub fn delete_empty(&mut self, idx: usize) {
        if self.blocks.len() <= 1 {
            // A lone block resets to an empty Paragraph rather than vanishing.
            if let Some(b) = self.blocks.get_mut(0) {
                b.kind = BlockKind::Paragraph;
                b.line.set_text("");
                b.line = RichTextLine::new().with_size(BlockKind::Paragraph.font_size());
                b.line.set_focus(self.is_focused);
            }
            self.focused = Some(0);
            self.structure_dirty = true;
            self.dirty = true;
            return;
        }
        let target = idx.saturating_sub(1);
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.dirty = true;
        let target = target.min(self.blocks.len().saturating_sub(1));
        self.focus_block(target, CursorPlacement::End);
    }

    /// The caller must restrict *when* this runs — it must only fire on
    /// the keystroke right after an empty paragraph, or typing `#`
    /// mid-paragraph would promote wrongly; see `handle_event`.
    pub fn try_promote_to_heading(&mut self, idx: usize) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        if !matches!(b.kind, BlockKind::Paragraph) {
            return false;
        }
        let text = b.line.value().to_string();
        let Some((level, prefix_len)) = heading_markdown_prefix(&text) else {
            return false;
        };
        Self::remove_line_range(&mut b.line, 0..prefix_len);
        b.line.set_cursor_pos(0);
        self.try_convert_to(idx, BlockKind::Heading(level))
    }

    /// Converts a freshly typed `> ` paragraph prefix into a semantic quote.
    pub fn try_promote_to_quote(&mut self, idx: usize) -> bool {
        let Some(block) = self.blocks.get_mut(idx) else {
            return false;
        };
        if !matches!(block.kind, BlockKind::Paragraph) || !block.line.value().starts_with("> ") {
            return false;
        }
        Self::remove_line_range(&mut block.line, 0..2);
        block.line.set_cursor_pos(0);
        self.try_convert_to(idx, BlockKind::Quote)
    }

    /// Remove a typed command without discarding formatting on its contents.
    fn remove_line_range(line: &mut RichTextLine, range: std::ops::Range<usize>) {
        let text = line.value();
        let (Some(head), Some(tail)) = (text.get(..range.start), text.get(range.end..)) else {
            return;
        };
        if range.start > range.end {
            return;
        }
        let updated = format!("{head}{tail}");
        let mut spans = line.marks().to_vec();
        crate::widget::marks::shift_after_edit(&mut spans, range.start, 0, range.len());
        let cursor = line.cursor_pos();
        let cursor = if cursor >= range.end {
            cursor.saturating_sub(range.len())
        } else {
            cursor.min(range.start)
        };
        line.set_text(&updated);
        line.set_marks(spans);
        line.set_cursor_pos(cursor);
    }

    /// Toggle `mark` as a toolbar button does: over the focused block's
    /// selection, or, with nothing selected, for the text typed next.
    /// `false` when no block is focused.
    pub fn toggle_mark(&mut self, mark: Mark) -> bool {
        let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index)) else {
            return false;
        };
        block.line.toggle_mark(mark);
        self.dirty = true;
        true
    }

    /// The marks a toolbar shows as on: those covering the whole selection
    /// in the focused block, or the caret, with the ones set for the text
    /// typed next.
    pub fn active_marks(&self) -> Vec<Mark> {
        let Some(line) = self
            .focused
            .and_then(|index| self.blocks.get(index))
            .map(|block| &block.line)
        else {
            return Vec::new();
        };
        let (start, end) = line
            .selection_range()
            .filter(|(start, end)| start < end)
            .unwrap_or_else(|| (line.cursor_pos(), line.cursor_pos()));
        let covers = |span: &MarkSpan| {
            if start == end {
                span.range.start < start && start <= span.range.end
            } else {
                span.range.start <= start && end <= span.range.end
            }
        };
        let mut active: Vec<Mark> = Vec::new();
        for span in line.marks().iter().filter(|span| covers(span)) {
            if !active.contains(&span.mark) {
                active.push(span.mark.clone());
            }
        }
        for pending in line.pending_marks() {
            if !active.iter().any(|mark| mark.same_kind(pending)) {
                active.push(pending.clone());
            }
        }
        active
    }

    /// Link the focused block's selection to `target`, or, with nothing
    /// selected, insert `target` at the caret as its own linked text.
    /// `false` when no block is focused or `target` is blank.
    pub fn insert_link(&mut self, target: &str) -> bool {
        let target = target.trim();
        if target.is_empty() {
            return false;
        }
        let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index)) else {
            return false;
        };
        let line = &mut block.line;
        let mark = Mark::ExternalLink(target.to_owned());
        if let Some((start, end)) = line.selection_range().filter(|(start, end)| start < end) {
            let mut spans = line.marks().to_vec();
            spans.retain(|span| {
                !(matches!(span.mark, Mark::ExternalLink(_))
                    && span.range.start < end
                    && start < span.range.end)
            });
            spans.push(MarkSpan::new(start..end, mark));
            line.set_marks(spans);
        } else {
            let at = line.cursor_pos().min(line.value().len());
            let mut text = line.value().to_owned();
            text.insert_str(at, target);
            let mut spans = line.marks().to_vec();
            crate::widget::marks::shift_after_edit(&mut spans, at, target.len(), 0);
            spans.push(MarkSpan::new(at..at.saturating_add(target.len()), mark));
            line.set_text(&text);
            line.set_marks(spans);
            line.set_cursor_pos(at.saturating_add(target.len()));
        }
        self.dirty = true;
        true
    }

    /// The focused block's kind.
    pub fn focused_kind(&self) -> Option<&BlockKind> {
        self.focused
            .and_then(|index| self.blocks.get(index))
            .map(|block| &block.kind)
    }

    /// Preserve authored formatting, caret, binding and outline context.
    /// Converting to a divider intentionally removes the contents.
    pub fn try_convert_to(&mut self, idx: usize, new_kind: BlockKind) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        if b.kind == new_kind {
            return false;
        }
        let depth = b.indent();
        let same_kind = std::mem::discriminant(&b.kind) == std::mem::discriminant(&new_kind);
        let font_size = new_kind.font_size();
        let is_divider = matches!(new_kind, BlockKind::Divider);
        b.line = std::mem::take(&mut b.line).with_size(font_size);
        let is_code = matches!(new_kind, BlockKind::CodeBlock { .. });
        b.line.set_mono(is_code);
        b.line.set_inline_formatting(!is_code);
        if is_divider {
            b.line.set_text("");
            b.line.set_focus(false);
        }
        b.code_group = if matches!(new_kind, BlockKind::CodeBlock { .. }) {
            b.code_group.clone().or_else(|| Some(b.id.clone()))
        } else {
            None
        };
        b.kind = new_kind;
        if !b.kind.supports_indent() || !same_kind {
            b.set_indent(depth.max(b.kind.indent()));
        } else {
            b.nesting = 0;
        }
        self.structure_dirty = true;
        self.dirty = true;
        // Dividers aren't focusable — push focus to the next block (or a
        // fresh trailing paragraph) so the user can keep typing.
        if is_divider && self.focused == Some(idx) {
            let next_idx = idx.saturating_add(1);
            if next_idx < self.blocks.len() {
                self.focus_block(next_idx, CursorPlacement::Start);
            } else {
                self.blocks.push(Block::paragraph(""));
                self.focus_block(next_idx, CursorPlacement::Start);
            }
        }
        true
    }

    /// The contiguous descendants of a list item belong to the same outline
    /// operation, even when their bullet, number, and checklist kinds differ.
    fn list_subtree_end(&self, index: usize) -> usize {
        let Some(parent) = self.blocks.get(index) else {
            return index.saturating_add(1).min(self.blocks.len());
        };
        let depth = parent.indent();
        let mut end = index.saturating_add(1);
        while self
            .blocks
            .get(end)
            .is_some_and(|child| child.indent() > depth)
        {
            end = end.saturating_add(1);
        }
        end
    }

    /// Shift an item and all descendants together, preserving their relative
    /// depths. Clamp the entire operation rather than flattening deep children.
    pub fn try_indent(&mut self, idx: usize, delta: i8) -> bool {
        let (idx, last) = self.fenced_code_run(idx).unwrap_or((idx, idx));
        self.indent_blocks(idx..self.list_subtree_end(last), delta)
    }

    /// Selected roots and their descendants, including entire code
    /// fences. A selection ending at column zero does not include that block.
    pub fn selected_block_range(&self) -> Option<std::ops::Range<usize>> {
        if let Some(retained) = &self.retained_block_selection {
            let RetainedBlockSelection::Active(ids) = retained else {
                return None;
            };
            let start = self
                .blocks
                .iter()
                .position(|block| Some(&block.id) == ids.first())?;
            let end = start.checked_add(ids.len())?;
            if !self
                .blocks
                .get(start..end)?
                .iter()
                .map(|block| &block.id)
                .eq(ids)
            {
                return None;
            }
            let mut root = start;
            while root < end {
                let group = self.movable_group(root);
                if group.start != root || group.end > end {
                    return None;
                }
                root = group.end;
            }
            return Some(start..end);
        }
        let Some(selection) = self.document_selection else {
            return self.focused.and_then(|index| self.block_group_range(index));
        };
        let (first, last) = selection.ordered();
        let last = if last.byte == 0 && last.block > first.block {
            last.block.saturating_sub(1)
        } else {
            last.block
        };
        let start = self
            .fenced_code_run(first.block)
            .map_or(first.block, |(start, _)| start);
        let last = self.fenced_code_run(last).map_or(last, |(_, end)| end);
        // Walk selected roots, skipping each expanded subtree. Extending only
        // the last selected child's subtree would leave its siblings behind
        // even when their parent is also selected.
        let mut end = start;
        while end <= last && end < self.blocks.len() {
            let row = self.fenced_code_run(end).map_or(end, |(_, end)| end);
            end = self.list_subtree_end(row);
        }
        self.blocks.get(start..end)?;
        Some(start..end)
    }

    /// One block's complete authored fence and outline descendants.
    pub fn block_group_range(&self, index: usize) -> Option<std::ops::Range<usize>> {
        self.blocks.get(index)?;
        Some(self.movable_group(index))
    }

    /// Plan a selected sibling batch's move past its adjacent sibling subtree.
    /// Returns the source rows and the insertion boundary in the current order.
    /// A move never implicitly changes a root's parent or splits a code fence.
    pub fn selected_block_move(&self, up: bool) -> Option<(std::ops::Range<usize>, usize)> {
        let range = self.selected_block_range()?;
        let depth = self.blocks.get(range.start)?.indent();
        let mut root = range.start;
        while root < range.end {
            if self.blocks.get(root)?.indent() != depth {
                return None;
            }
            root = self.movable_group(root).end;
        }
        if root != range.end {
            return None;
        }
        let mut adjacent = if up {
            range.start.checked_sub(1)?
        } else {
            range.end
        };
        if up {
            while self.blocks.get(adjacent)?.indent() > depth {
                adjacent = adjacent.checked_sub(1)?;
            }
        }
        if self.blocks.get(adjacent)?.indent() != depth {
            return None;
        }
        let neighbor = self.movable_group(adjacent);
        let boundary = if up { neighbor.start } else { neighbor.end };
        Some((range, boundary))
    }

    fn try_indent_selection(&mut self, delta: i8) -> bool {
        self.selected_block_range()
            .is_some_and(|range| self.indent_blocks(range, delta))
    }

    fn indent_blocks(&mut self, range: std::ops::Range<usize>, delta: i8) -> bool {
        if self.read_only {
            return false;
        }
        let Some(parent) = self.blocks.get(range.start) else {
            return false;
        };
        // Prose can nest under preceding prose as well as lists. Preserve the
        // established first-list-item indentation, but never orphan first prose.
        if !parent.kind.supports_indent() && parent.indent() == 0 && range.start == 0 {
            return false;
        }
        let Some(subtree) = self.blocks.get(range.clone()) else {
            return false;
        };
        let shallowest = subtree.iter().map(Block::indent).min().unwrap_or(0);
        let deepest = subtree.iter().map(Block::indent).max().unwrap_or(0);
        let minimum_shift = i8::try_from(shallowest).unwrap_or(i8::MAX).saturating_neg();
        let maximum_shift =
            i8::try_from(MAX_LIST_INDENT.saturating_sub(deepest)).unwrap_or(i8::MAX);
        let shift = delta.max(minimum_shift).min(maximum_shift);
        if shift == 0 {
            return false;
        }
        let Some(subtree) = self.blocks.get_mut(range) else {
            return false;
        };
        for block in subtree {
            block.set_indent(clamp_indent(block.indent(), shift));
        }
        // Backspace is normally classified as local typing. An outdent may
        // touch arbitrarily many descendants, all of which must be committed.
        self.note_edit_anywhere();
        self.structure_dirty = true;
        self.dirty = true;
        self.edit_pending = true;
        self.commit_pending = true;
        true
    }
    /// Checkbox rect is centered in the gutter to the left of
    /// `line_rect`, matching where it's drawn.
    pub(crate) fn checkbox_hit_test_last_mouse(&self) -> Option<usize> {
        self.checkbox_hit_test(self.last_mouse_pos)
    }

    fn checkbox_hit_test(&self, p: Point) -> Option<usize> {
        for (idx, b) in self.blocks.iter().enumerate() {
            if !matches!(b.kind, BlockKind::Checklist { .. }) {
                continue;
            }
            let line_rect = b.line.get_rect();
            let gutter = prefix_gutter_for(&b.kind) * self.text_scale;
            let r = checklist_box(
                line_rect,
                gutter,
                b.line.text_inset(&self.layout_theme),
                self.text_scale,
            );
            // ~4px slop so users don't have to land pixel-perfect.
            let slop = 4.0;
            let slopped = Rect::new(r.x0 - slop, r.y0 - slop, r.x1 + slop, r.y1 + slop);
            if slopped.contains(p) {
                return Some(idx);
            }
        }
        None
    }

    /// Idempotent — if the focused block changed since the menu opened,
    /// still applies the conversion and leaves the text alone.
    fn commit_slash_selection(&mut self, mut new_kind: BlockKind) {
        let Some(focused_idx) = self.focused else {
            return;
        };
        if let Some(open_byte) = self.slash_open_at
            && let Some(b) = self.blocks.get_mut(focused_idx)
        {
            let text = b.line.value().to_string();
            let cursor = b.line.cursor_pos().min(text.len());
            let start = open_byte.min(text.len());
            if cursor > start {
                Self::remove_line_range(&mut b.line, start..cursor);
            }
        }
        self.slash_open_at = None;
        if let Some(mut descriptor) = formula_descriptor(&new_kind) {
            descriptor.mode = self.preferred_formula_mode;
            new_kind = BlockKind::CodeBlock {
                lang: Some(descriptor.fence_language()),
            };
        }
        if matches!(new_kind, BlockKind::Table { .. })
            && let Some(block) = self.blocks.get_mut(focused_idx)
            && block.text().trim().is_empty()
        {
            const TABLE_TEMPLATE: &str = "| Column | Value |\n| --- | --- |\n|  |  |";
            block.line.set_text(TABLE_TEMPLATE);
            block.line.set_cursor_pos(TABLE_TEMPLATE.len());
        }
        let _ = self.try_convert_to(focused_idx, new_kind);
    }

    fn set_formula_mode(&mut self, start: usize, mode: crate::math::FormulaMode) {
        let Some(mut descriptor) = self
            .blocks
            .get(start)
            .and_then(|block| formula_descriptor(&block.kind))
        else {
            return;
        };
        if descriptor.mode == mode {
            return;
        }
        let end = formula_run_end(&self.blocks, start);
        let Some(run) = self.blocks.get(start..=end) else {
            return;
        };
        let source = run.iter().map(Block::text).collect::<Vec<_>>().join("\n");
        descriptor.mode = mode;
        let language = descriptor.fence_language();
        let Some(run_mut) = self.blocks.get_mut(start..=end) else {
            return;
        };
        for block in run_mut {
            block.kind = BlockKind::CodeBlock {
                lang: Some(language.clone()),
            };
            block.line.set_focus(false);
        }
        self.preferred_formula_mode = mode;
        if mode == crate::math::FormulaMode::Visual {
            self.visual_formulas
                .insert(start, VisualFormulaState::new(descriptor.syntax, &source));
            self.visual_formula_focus = Some(start);
            self.focused = None;
        } else {
            self.visual_formula_focus = None;
            self.focused = Some(start);
            if let Some(block) = self.blocks.get_mut(start) {
                block.line.set_focus(true);
                block.line.set_cursor_pos(block.line.value().len());
            }
        }
        self.structure_dirty = true;
        self.commit_pending = true;
        self.dirty = true;
    }

    fn commit_visual_formula_edit(
        &mut self,
        start: usize,
        end: usize,
        syntax: crate::math::FormulaSyntax,
    ) {
        let Some(visual) = self.visual_formulas.get_mut(&start) else {
            return;
        };
        let source = syntax.serialize(&visual.editor.expr);
        visual.source.clone_from(&source);
        if let Some(block) = self.blocks.get_mut(start) {
            block.line.set_text(&source);
        }
        for block in self
            .blocks
            .iter_mut()
            .take(end.saturating_add(1))
            .skip(start.saturating_add(1))
        {
            block.line.set_text("");
        }
        self.commit_pending = true;
        self.edit_pending = true;
        self.dirty = true;
    }

    /// Host calls this AFTER the main draw pass — popovers must paint
    /// last so they land above every other widget.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.refresh_popover_anchors(theme);
        self.slash_menu
            .draw_overlay(scene, transform, text_renderer, theme);
        self.link_popover
            .draw_overlay(scene, transform, text_renderer, theme);
    }

    /// Re-anchor overlays from rendered caret geometry.  Event handling cannot
    /// safely invent a theme: plain and padded fields have different text
    /// origins, and custom fonts can change every advance.  This runs in the
    /// overlay paint pass, after the line has shaped with the active theme.
    fn refresh_popover_anchors(&mut self, theme: &Theme) {
        if self.slash_menu.is_visible()
            && let Some((index, byte)) = self.slash_anchor_caret
            && let Some(line) = self.blocks.get(index).map(|block| &block.line)
        {
            let caret = line.caret_rect(theme, byte);
            self.slash_menu
                .set_anchor(Point::new(caret.x0, caret.y1 + 2.0));
        }
        if self.link_popover.is_visible()
            && let Some((index, byte)) = self.link_anchor_caret
            && let Some(line) = self.blocks.get(index).map(|block| &block.line)
        {
            let caret = line.caret_rect(theme, byte);
            self.link_popover
                .set_anchor(Point::new(caret.x0, caret.y1 + 2.0));
        }
    }

    fn fallback_popover_anchor(&self, index: usize) -> Point {
        self.blocks.get(index).map_or(Point::ZERO, |block| {
            let rect = block.line.get_rect();
            Point::new(rect.x0, rect.y1 + 2.0)
        })
    }

    /// The padding the editor adds before its lines for the handle gutter:
    /// the gutter less the lines' own text inset, so the text starts
    /// [`Self::handle_gutter`] in from the editor's rect. Zero when handles
    /// are hidden.
    fn handle_column(&self) -> f64 {
        if self.block_handles && !self.read_only {
            (self.handle.width + self.handle.gap - self.text_pad).max(0.0)
        } else {
            0.0
        }
    }

    /// Reserved target in the handle gutter just left of an editable block,
    /// so it never narrows text or steals list/checkbox clicks. Geometry
    /// from [`HandleMetrics`]: a `width` column ending `gap` before the
    /// block's leading edge, `height` tall at 1× and growing with the text
    /// scale like the line beside it. The rect is also the hit target, so it
    /// grows to the pointer minimum where the block itself is that tall —
    /// never taller than the block, or two single-line paragraphs would
    /// share one handle.
    fn block_drag_handle_rect(&self, index: usize) -> Option<Rect> {
        if !self.block_handles || self.read_only {
            return None;
        }
        let block = self.blocks.get(index)?;
        let line = self.block_paint_rect(index)?;
        let leading_edge = line.x0;
        let source = block.line.get_rect();
        let center_y = first_row_center(
            if source.height() > 0.0 { source } else { line },
            block.kind.font_size(),
            self.text_scale,
        );
        let HandleMetrics {
            width,
            height,
            gap,
            min_target,
        } = self.handle;
        let half_height = ((height * self.text_scale) / 2.0)
            .max(min_target.min(line.height()) / 2.0)
            .min(line.height() / 2.0);
        let right = leading_edge - gap;
        Some(Rect::new(
            right - width,
            center_y - half_height,
            right,
            center_y + half_height,
        ))
    }

    fn movable_group(&self, index: usize) -> std::ops::Range<usize> {
        if let Some((start, end)) = self.fenced_code_run(index) {
            start..self.list_subtree_end(end)
        } else {
            index..self.list_subtree_end(index)
        }
    }

    fn neighboring_group(&self, source: usize, up: bool) -> Option<usize> {
        let group = self.movable_group(source);
        let mut target = if up {
            group.start.checked_sub(1)?
        } else {
            group.end
        };
        let current = self.blocks.get(group.start)?;
        let depth = current.indent();
        {
            if up {
                while target > 0
                    && self
                        .blocks
                        .get(target)
                        .is_some_and(|block| block.indent() > depth)
                {
                    target = target.saturating_sub(1);
                }
            }
            let adjacent = self.blocks.get(target)?;
            if depth > 0 && adjacent.indent() < depth {
                return None; // Moving is not implicit outdenting/reparenting.
            }
        }
        self.blocks.get(target).map(|_| target)
    }

    fn group_move_boundary(
        &self,
        source: usize,
        target: usize,
    ) -> Option<(std::ops::Range<usize>, usize)> {
        if source >= self.blocks.len() || target >= self.blocks.len() {
            return None;
        }
        let group = self.movable_group(source);
        if group.contains(&target) {
            return None;
        }
        let mut destination = target;
        let depth = self.blocks.get(group.start)?.indent();
        while destination > 0
            && self
                .blocks
                .get(destination)
                .is_some_and(|block| block.indent() > depth)
        {
            destination = destination.saturating_sub(1);
        }
        let other = self.movable_group(destination);
        if other.start < group.end && group.start < other.end {
            return None;
        }
        let boundary = if other.start > group.start {
            other.end
        } else {
            other.start
        };
        Some((group, boundary))
    }

    /// Move a group in the local projection. With action interception enabled
    /// this is converted into a UUID-addressed `Move` operation immediately
    /// after the input event; non-intercepting consumers keep their existing
    /// direct-CRDT reconciliation path.
    fn move_block_to(&mut self, source: usize, target: usize) -> bool {
        let Some((group, boundary)) = self.group_move_boundary(source, target) else {
            return false;
        };
        let focus_offset = self
            .focused
            .filter(|index| group.contains(index))
            .unwrap_or(source)
            .saturating_sub(group.start);
        self.clear_cross_block_selection();
        self.cross_block_drag_start = None;
        self.kbd_sel_anchor = None;
        let count = group.len();
        let insertion = if boundary > group.start {
            boundary.saturating_sub(count)
        } else {
            boundary
        };
        let moving: Vec<_> = self.blocks.drain(group).collect();
        self.blocks.splice(insertion..insertion, moving);
        self.focused = Some(insertion.saturating_add(focus_offset));
        for (index, block) in self.blocks.iter_mut().enumerate() {
            block
                .line
                .set_focus(self.is_focused && self.focused == Some(index));
        }
        self.structure_dirty = true;
        self.commit_pending = true;
        self.edit_pending = true;
        self.dirty = true;
        true
    }

    fn block_move_target(block_id: &str, direction: &str) -> String {
        format!("notes.block-move:{block_id}:{direction}")
    }

    fn parse_block_move_target(target: &str) -> Option<(&str, bool)> {
        let (id, direction) = target.strip_prefix("notes.block-move:")?.rsplit_once(':')?;
        match direction {
            "up" => Some((id, true)),
            "down" => Some((id, false)),
            _ => None,
        }
    }

    fn move_block_from_semantics(&mut self, target: &str) -> bool {
        let Some((block_id, up)) = Self::parse_block_move_target(target) else {
            return false;
        };
        let Some(source) = self.blocks.iter().position(|block| block.id == block_id) else {
            return false; // Stable target from a stale AT tree: refuse safely.
        };
        let Some(target_index) = self.neighboring_group(source, up) else {
            return false;
        };
        self.begin_edit();
        if !self.move_block_to(source, target_index) {
            return false;
        }
        let ops = self.take_edit_ops();
        self.pending_semantic_ops.extend(ops);
        true
    }

    fn edit_from_semantics(&mut self, command: &str) -> bool {
        if command.starts_with("table-") {
            let Some((id, row, column)) = self.focused_table_cell() else {
                return false;
            };
            let Some(index) = self.blocks.iter().position(|block| block.id == id) else {
                return false;
            };
            self.begin_edit();
            let changed = self.edit_table_structure(index, row, column, command);
            let ops = self.take_edit_ops();
            self.pending_semantic_ops.extend(ops);
            return changed;
        }
        if let Some((index, result)) = self.blocks.iter().enumerate().find_map(|(index, block)| {
            self.table_editors
                .get_mut(&block.id)
                .and_then(|table| table.projected_clipboard(command))
                .map(|result| (index, result))
        }) {
            if let Some((row, column, value)) = result {
                self.begin_edit();
                self.apply_table_cell_edit(index, row, column, &value);
                let ops = self.take_edit_ops();
                self.pending_semantic_ops.extend(ops);
            }
            self.dirty = true;
            return true;
        }
        if !matches!(command, "cut" | "copy" | "paste" | "select-all") {
            return false;
        }
        if command == "select-all" {
            self.handle_document_selection_ctrl_key(&Key::Character("a".into()));
            self.dirty = true;
            return true;
        }
        // A preview's source is the clipboard candidate, not a live text
        // selection until a replacement actually succeeds.
        let image_selection = self.focused.and_then(|index| {
            let block = self.blocks.get(index)?;
            self.image_attachments
                .get(&block.id)
                .filter(|image| !image.editing)
                .map(|_| DocumentSelection {
                    anchor: DocumentPosition {
                        block: index,
                        byte: 0,
                    },
                    cursor: DocumentPosition {
                        block: index,
                        byte: block.text().len(),
                    },
                })
        });
        let selection = self.document_selection.or(image_selection).or_else(|| {
            let index = self.focused?;
            let line = &self.blocks.get(index)?.line;
            let (start, end) = line
                .selection_range()
                .unwrap_or_else(|| (line.cursor_pos(), line.cursor_pos()));
            Some(DocumentSelection {
                anchor: DocumentPosition {
                    block: index,
                    byte: start,
                },
                cursor: DocumentPosition {
                    block: index,
                    byte: end,
                },
            })
        });
        let Some(selection) = selection else {
            return true;
        };
        if matches!(command, "copy" | "cut") && selection.anchor == selection.cursor {
            return true;
        }
        if command == "paste" && self.paste_image() {
            return true;
        }
        // An unavailable clipboard must never turn Paste into Delete.
        let replacement = if command == "paste" {
            let Some(value) = clipboard_paste() else {
                return true;
            };
            value
        } else {
            String::new()
        };
        let prior = self.document_selection;
        self.document_selection = Some(selection);
        if matches!(command, "copy" | "cut") {
            let accepted = self
                .selected_text()
                .is_some_and(|text| clipboard_copy_accepted(&text));
            if command == "cut" && !accepted {
                self.document_selection = prior;
                return true;
            }
        }
        if command == "copy" {
            self.document_selection = prior;
        } else {
            self.begin_edit();
            self.replace_document_selection(&replacement);
            self.set_focus(true);
            let ops = self.take_edit_ops();
            self.pending_semantic_ops.extend(ops);
        }
        self.dirty = true;
        true
    }

    fn try_move_focused_block_shortcut(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent};
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let modifiers = ctx.modifiers.state();
        // Alt+Arrow is intentionally chosen: arrows retain caret navigation,
        // Ctrl/Cmd+Shift+Arrow retains cross-block selection, and this maps to
        // the conventional "move current item" accelerator.
        if !modifiers.alt_key() || modifiers.control_key() || modifiers.super_key() {
            return false;
        }
        let Some(source) = self.focused else {
            return false;
        };
        if let Some(target) = self.neighboring_group(source, *key == NamedKey::ArrowUp) {
            self.move_block_to(source, target);
        }
        *ctx.handled = true;
        // Consume edge attempts too: Alt+Arrow must never unexpectedly move
        // the text caret when a screen-reader user is moving blocks.
        true
    }

    fn draw_block_drag_affordance(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let tokens = &theme.tokens;
        let hovered = self.hovered_block();
        for index in 0..self.blocks.len() {
            let Some(handle) = self.block_drag_handle_rect(index) else {
                continue;
            };
            let active = self.block_drag.is_some_and(|drag| drag.source == index);
            let contains_caret = self.is_focused && self.focused == Some(index);
            if !active && !contains_caret && hovered != Some(index) {
                continue;
            }
            let color = tokens.colors.text_primary.with_alpha_factor(0.72);
            let center = handle.center();
            let scale = self.text_scale;
            let dot_r = 1.25 * scale;
            let col_pitch = (3.0 * scale).min(handle.width() / 2.0 - dot_r);
            let row_pitch = 5.0 * scale;
            if active || handle.contains(self.last_mouse_pos) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens
                        .colors
                        .text_primary
                        .with_alpha_factor(if active { 0.22 } else { 0.12 }),
                    None,
                    &vello::kurbo::RoundedRect::from_rect(handle, 4.0),
                );
            }
            for col in [-col_pitch, col_pitch] {
                for row in [-row_pitch, 0.0, row_pitch] {
                    let dot = Rect::new(
                        center.x + col - dot_r,
                        center.y + row - dot_r,
                        center.x + col + dot_r,
                        center.y + row + dot_r,
                    );
                    scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &dot);
                }
            }
        }
    }

    fn hovered_block(&self) -> Option<usize> {
        (0..self.blocks.len()).find_map(|index| {
            let line = self.block_paint_rect(index)?;
            let handle = self.block_drag_handle_rect(index)?;
            Rect::new(handle.x0, line.y0, line.x1, line.y1)
                .contains(self.last_mouse_pos)
                .then_some(index)
        })
    }

    fn hovered_drag_handle(&self) -> Option<usize> {
        (0..self.blocks.len()).find(|&index| {
            self.block_drag_handle_rect(index)
                .is_some_and(|rect| rect.contains(self.last_mouse_pos))
        })
    }

    fn draw_block_drop_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        color: vello::peniko::Color,
        drag: BlockDrag,
    ) {
        let Some((group, boundary)) = self.group_move_boundary(drag.source, drag.target) else {
            return;
        };
        let after = boundary > group.start;
        let index = if after {
            boundary.saturating_sub(1)
        } else {
            boundary
        };
        let Some(rect) = self.block_paint_rect(index) else {
            return;
        };
        let y = if after { rect.y1 } else { rect.y0 };
        // Moving a parent past another subtree keeps the parent's depth. The
        // destination's last child supplies the vertical boundary, not indent.
        let x0 = self
            .block_paint_rect(group.start)
            .map_or(rect.x0, |source| source.x0);
        let insertion = Rect::new(x0 - 4.0, y - 1.0, rect.x1, y + 1.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &insertion,
        );
    }

    fn open_block_type_menu(&mut self, index: usize) -> bool {
        let Some(anchor_rect) = self.block_drag_handle_rect(index) else {
            return false;
        };
        self.focus_block(index, CursorPlacement::Start);
        self.slash_menu.set_anchor(Point::new(
            anchor_rect.x1 + 6.0 * self.text_scale,
            anchor_rect.y0,
        ));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = None;
        self.slash_anchor_caret = None;
        self.dirty = true;
        true
    }

    /// Complete or cancel a handle drag before normal text routing sees its
    /// terminal event. A cancelled drag never changed the projection; a drop
    /// is immediately captured by the semantic action bridge.
    fn handle_block_drag_lifecycle(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let escape = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
                ..
            }
        );
        if (escape || matches!(event, WindowEvent::Focused(false)))
            && self.block_drag.take().is_some()
        {
            *ctx.handled = true;
            self.dirty = true;
            return true;
        }
        let released = matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        );
        let Some(drag) = released.then(|| self.block_drag.take()).flatten() else {
            return false;
        };
        if drag.active {
            let _ = self.move_block_to(drag.source, drag.target);
        } else {
            let _ = self.open_block_type_menu(drag.source);
        }
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    pub fn set_link_pages(&mut self, pages: Vec<PageEntry>) {
        self.link_popover.set_items(pages);
    }

    /// Supply an asynchronous, host-policy-owned image loader. The editor
    /// never performs filesystem or network access itself.
    pub fn set_image_store(&mut self, store: Option<crate::ImageStore>) {
        self.image_store = store;
        self.image_dimensions.clear();
        self.dirty = true;
        self.structure_dirty = true;
    }

    fn set_image_source_mode(&mut self, id: &str, editing: bool) -> bool {
        let Some(image) = self.image_attachments.get_mut(id) else {
            return false;
        };
        image.editing = editing;
        image.edit.set_focus(false);
        image.done.set_focus(false);
        if let Some(index) = self.blocks.iter().position(|block| block.id == id) {
            self.focus_block(index, CursorPlacement::Start);
        }
        self.structure_dirty = true;
        self.dirty = true;
        true
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        reason = "one cohesive image interaction state machine validates the active block before indexing"
    )]
    fn handle_image_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, MouseButton};
        if *ctx.handled {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse_pos =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        for (index, block) in self.blocks.iter().enumerate() {
            if let Some(image) = self.image_attachments.get_mut(&block.id) {
                let hovered = self
                    .image_preview_rects
                    .get(index)
                    .and_then(|rect| *rect)
                    .is_some_and(|rect| rect.contains(self.last_mouse_pos));
                let visible = image.editing
                    || hovered
                    || image.button().is_focused()
                    || (self.is_focused && self.focused == Some(index));
                if visible && let Some(node) = image.node {
                    image
                        .button_mut()
                        .handle_event(event, &mut ctx.reborrow(ctx.path, node));
                    if *ctx.handled {
                        return true;
                    }
                }
            }
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            let hit = self
                .image_preview_rects
                .iter()
                .enumerate()
                .find_map(|(index, rect)| {
                    rect.filter(|rect| rect.contains(self.last_mouse_pos))
                        .map(|_| index)
                });
            if let Some(index) = hit {
                self.focus_block(index, CursorPlacement::Start);
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
                self.dirty = true;
                return true;
            }
        }
        if !self.is_focused {
            return false;
        }
        let Some(index) = self.focused else {
            return false;
        };
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        let id = block.id.clone();
        let Some(image) = self.image_attachments.get(&id) else {
            return false;
        };
        let editing = image.editing;
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if editing {
                if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                    self.set_image_source_mode(&id, false);
                    *ctx.handled = true;
                    return true;
                }
                return false;
            }
            let control = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
            if control && let Key::Character(key) = logical_key {
                if key == "c" {
                    // Browser cut first copies to its event clipboard. Copying
                    // a preview must not expose a replacement text selection.
                    clipboard_copy(self.blocks[index].text());
                    *ctx.handled = true;
                    return true;
                }
                if matches!(key.as_str(), "x" | "v") {
                    let cutting = key == "x";
                    if cutting && !clipboard_copy_accepted(self.blocks[index].text()) {
                        *ctx.handled = true;
                        return true;
                    }
                    let replacement = if cutting {
                        String::new()
                    } else {
                        let Some(text) = clipboard_paste() else {
                            *ctx.handled = true;
                            return true;
                        };
                        text
                    };
                    let end = self.blocks[index].text().len();
                    self.select_document_range(
                        DocumentPosition {
                            block: index,
                            byte: 0,
                        },
                        DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    );
                    self.replace_document_selection(&replacement);
                    *ctx.handled = true;
                    return true;
                }
                *ctx.handled = true;
                return true;
            }
            match logical_key {
                Key::Named(NamedKey::Enter | NamedKey::F2) => {
                    self.set_image_source_mode(&id, true);
                }
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                    self.focus_adjacent_visual_row(index, false);
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                    self.focus_adjacent_visual_row(index, true);
                }
                Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                    let end = self.blocks[index].text().len();
                    self.select_document_range(
                        DocumentPosition {
                            block: index,
                            byte: 0,
                        },
                        DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    );
                    self.replace_document_selection("");
                }
                Key::Named(NamedKey::Tab) => return false,
                _ => {}
            }
            *ctx.handled = true;
            self.dirty = true;
            return true;
        }
        if !editing && matches!(event, WindowEvent::Ime(_)) {
            *ctx.handled = true;
            return true;
        }
        false
    }

    fn refresh_image_attachments(&mut self) {
        let previous_count = self.image_attachments.len();
        let commands = self
            .image_attachments
            .iter()
            .filter_map(|(id, image)| image.command.take().map(|editing| (id.clone(), editing)))
            .collect::<Vec<_>>();
        for (id, editing) in commands {
            self.set_image_source_mode(&id, editing);
        }
        self.image_attachments.retain(|id, image| {
            self.blocks.iter().any(|block| {
                block.id == *id
                    && matches!(block.kind, BlockKind::Paragraph)
                    && (image.editing || standalone_markdown_image(block.text()).is_some())
            })
        });
        for block in &self.blocks {
            if matches!(block.kind, BlockKind::Paragraph)
                && standalone_markdown_image(block.text()).is_some()
            {
                let attachment = self
                    .image_attachments
                    .entry(block.id.clone())
                    .or_insert_with(|| ImageAttachment::new(&block.id, block.text()));
                attachment.source = block.text().into();
            }
        }
        if self.image_attachments.len() != previous_count {
            self.structure_dirty = true;
            self.dirty = true;
        }
    }

    fn refresh_image_dimensions(&mut self) {
        let sources = self
            .image_attachments
            .values()
            .filter_map(|image| standalone_markdown_image(&image.source).map(|(_, source)| source))
            .collect::<std::collections::HashSet<_>>();
        self.image_dimensions
            .retain(|source, _| sources.contains(source.as_str()));
        let Some(store) = self.image_store.as_ref() else {
            return;
        };
        for source in sources {
            if let Some(image) = store.get(source) {
                let dimensions = (image.width.max(1), image.height.max(1));
                if self.image_dimensions.get(source) != Some(&dimensions) {
                    self.image_dimensions.insert(source.to_string(), dimensions);
                    self.structure_dirty = true;
                    self.dirty = true;
                }
            }
        }
    }

    /// Preload currently referenced images for deterministic offscreen shots.
    /// Live UI code must rely on [`Widget::needs_animation_frame`] instead.
    #[doc(hidden)]
    pub fn prewarm_images_for_shots(&self) -> bool {
        let Some(store) = self.image_store.as_ref() else {
            return false;
        };
        let mut sources = Vec::new();
        for (_, source) in self
            .blocks
            .iter()
            .filter_map(|block| standalone_markdown_image(block.text()))
        {
            sources.push(source);
            let _ = store.get(source);
        }
        store.block_until_idle(crate::time::Duration::from_secs(2));
        !sources.is_empty()
            && sources
                .into_iter()
                .all(|source| store.get(source).is_some())
    }

    pub fn link_popover(&self) -> &LinkPopover {
        &self.link_popover
    }

    #[doc(hidden)]
    pub fn link_popover_mut(&mut self) -> &mut LinkPopover {
        &mut self.link_popover
    }

    /// Resolve a link against the exact geometry produced by the live draw.
    /// A stale layout intentionally refuses rather than approximating with a
    /// different theme or font.
    pub fn link_at_point(&self, point: Point) -> Option<LinkTarget> {
        if let Some(id) = self
            .transclusion_rects
            .iter()
            .flatten()
            .find_map(|(rect, id)| rect.contains(point).then_some(*id))
        {
            return Some(LinkTarget::Page(id));
        }
        self.live_link_hits
            .iter()
            .find_map(|(rect, target)| rect.contains(point).then(|| target.clone()))
    }

    pub fn set_transclusion_previews(&mut self, previews: HashMap<PageId, TransclusionPreview>) {
        if self.transclusion_previews != previews {
            self.transclusion_previews = previews;
            self.dirty = true;
            self.structure_dirty = true;
        }
    }

    pub fn take_transclusion_open(&mut self) -> Option<PageId> {
        self.pending_transclusion_opens.pop()
    }

    pub fn take_external_link_open(&mut self) -> Option<String> {
        self.pending_external_link_opens.pop()
    }

    /// Images for legacy hosts which insert after the caret. Canonical hosts
    /// use [`Self::take_image_paste_requests`] to replace the captured selection.
    pub fn take_pasted_images(&mut self) -> Vec<crate::clipboard::ClipboardImage> {
        self.take_image_paste_requests()
            .into_iter()
            .map(|request| request.image)
            .collect()
    }

    /// Drain image intents without changing text. The host validates the image
    /// and applies asset storage and the captured replacement in one action.
    pub fn take_image_paste_requests(&mut self) -> Vec<ImagePasteRequest> {
        std::mem::take(&mut self.pasted_images)
    }

    fn image_paste_target(&self) -> Option<ImagePasteTarget> {
        let index = self.focused?;
        let block = self.blocks.get(index)?;
        let preview = self
            .image_attachments
            .get(&block.id)
            .is_some_and(|image| !image.editing);
        let selection = self
            .document_selection
            .filter(|selection| selection.anchor != selection.cursor)
            .map(DocumentSelection::ordered)
            .or_else(|| {
                let (start, end) = if preview {
                    (0, block.text().len())
                } else {
                    block.line.selection_range()?
                };
                (start != end).then_some((
                    DocumentPosition {
                        block: index,
                        byte: start,
                    },
                    DocumentPosition {
                        block: index,
                        byte: end,
                    },
                ))
            });
        let Some((start, mut end)) = selection else {
            return Some(ImagePasteTarget::After(block.id.clone()));
        };
        if end.byte == 0 && end.block > start.block {
            end.block = end.block.saturating_sub(1);
            end.byte = self.blocks.get(end.block)?.text().len();
        }
        let selected = self.blocks.get(start.block..=end.block)?;
        Some(ImagePasteTarget::Replace {
            blocks: selected.iter().map(Block::content).collect(),
            start_byte: start.byte,
            end_byte: end.byte,
        })
    }

    /// Queue an image intent in prose without deleting any selected content.
    fn paste_image(&mut self) -> bool {
        if self.read_only || self.is_composing() {
            return false;
        }
        let in_prose = self
            .focused
            .and_then(|index| self.blocks.get(index))
            .is_some_and(|block| {
                !matches!(
                    block.kind,
                    BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                )
            });
        if !in_prose {
            return false;
        }
        let Some(image) = crate::clipboard_paste_image() else {
            return false;
        };
        let Some(target) = self.image_paste_target() else {
            return false;
        };
        self.pasted_images.push(ImagePasteRequest { image, target });
        self.dirty = true;
        true
    }

    /// Ctrl+V (Cmd+V) with an image on the clipboard.
    fn try_paste_image_key(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::Key;

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = ctx.modifiers.state();
        let command = mods.control_key() || mods.super_key();
        if !self.is_focused
            || !command
            || mods.alt_key()
            || !key.eq_ignore_ascii_case("v")
            || !self.paste_image()
        {
            return false;
        }
        *ctx.handled = true;
        true
    }

    fn copy_code_run(&mut self, start: usize, end: usize) -> bool {
        if self.fenced_code_run(start) != Some((start, end)) {
            return false;
        }
        let Some(run) = self.blocks.get(start..=end) else {
            return false;
        };
        let Some((first, rest)) = run.split_first() else {
            return false;
        };
        if !matches!(first.kind, BlockKind::CodeBlock { .. })
            || rest
                .iter()
                .any(|block| !matches!(block.kind, BlockKind::CodeBlock { .. }))
        {
            return false;
        }
        clipboard_copy(&run.iter().map(Block::text).collect::<Vec<_>>().join("\n"));
        self.copied_code_start = Some(start);
        self.copied_code_feedback = Some((first.id.clone(), crate::time::now()));
        self.dirty = true;
        true
    }

    fn code_copy_target(block: &Block) -> String {
        match &block.code_group {
            Some(group) => format!("notes.code-copy:group:{group}"),
            None => format!("notes.code-copy:block:{}", block.id),
        }
    }

    fn code_copy_run_target(&self, start: usize, end: usize) -> Option<String> {
        let run = self.blocks.get(start..=end)?;
        let first = run.first()?;
        if let Some(group) = &first.code_group
            && self.blocks.iter().enumerate().any(|(index, block)| {
                !(start..=end).contains(&index) && block.code_group.as_ref() == Some(group)
            })
        {
            // Conversion or a concurrent edit can split an authored fence.
            // Address the visible fragment without rewriting collaborative
            // identities during projection. Both boundaries are witnessed so
            // an old Copy button cannot silently expand after Undo or a merge.
            return Some(format!(
                "notes.code-copy:fragment:{group}:{}:{}",
                first.id,
                run.last()?.id
            ));
        }
        Some(Self::code_copy_target(first))
    }

    fn resolve_code_copy_target(&self, target: &str) -> Option<(usize, usize)> {
        let rest = target.strip_prefix("notes.code-copy:")?;
        let (kind, id) = rest.split_once(':')?;
        if kind == "fragment" {
            let (group, boundaries) = id.split_once(':')?;
            let (first, last) = boundaries.split_once(':')?;
            let index = self.blocks.iter().position(|block| {
                block.id == first && block.code_group.as_deref() == Some(group)
            })?;
            let (start, end) = self.fenced_code_run(index)?;
            return (start == index
                && self.blocks.get(end)?.id == last
                && self.code_copy_run_target(start, end).as_deref() == Some(target))
            .then_some((start, end));
        }
        let index = match kind {
            "group" => self
                .blocks
                .iter()
                .position(|block| block.code_group.as_deref() == Some(id))?,
            "block" => self
                .blocks
                .iter()
                .position(|block| block.id == id && block.code_group.is_none())?,
            _ => return None,
        };
        let (start, end) = self.fenced_code_run(index)?;
        // A conflicted or split group is ambiguous. Do not silently copy only
        // one fragment of an authored fence.
        if kind == "group"
            && self.blocks.iter().enumerate().any(|(i, block)| {
                !(start..=end).contains(&i) && block.code_group.as_deref() == Some(id)
            })
        {
            return None;
        }
        Some((start, end))
    }

    fn refresh_live_link_hits(&mut self, theme: &Theme) {
        self.live_link_hits.clear();
        for block in &self.blocks {
            let line = &block.line;
            for span in line.marks() {
                let target = match &span.mark {
                    Mark::Link(id) => LinkTarget::Page(*id),
                    Mark::ExternalLink(url) => LinkTarget::External(url.clone()),
                    _ => continue,
                };
                for rect in line.mark_rects(theme, span.range.clone()) {
                    self.live_link_hits.push((rect, target.clone()));
                }
            }
        }
    }

    fn commit_link_selection(&mut self, page: &PageEntry) {
        let Some(focused_idx) = self.focused else {
            return;
        };
        let Some(open_byte) = self.link_open_at.take() else {
            return;
        };
        let Some(b) = self.blocks.get_mut(focused_idx) else {
            return;
        };
        let text = b.line.value().to_string();
        let cursor = b.line.cursor_pos().min(text.len());
        let start = open_byte.min(text.len());
        if cursor <= start {
            return;
        }
        let embedded = start > 0 && text.get(start.saturating_sub(1)..start) == Some("!");
        let head_end = if embedded {
            start.saturating_sub(1)
        } else {
            start
        };
        let (Some(head), Some(tail)) = (text.get(..head_end), text.get(cursor..)) else {
            return;
        };
        let mark_start = head_end;
        let title_end = mark_start.saturating_add(page.title.len());
        let mut new_text = String::with_capacity(text.len().saturating_add(page.title.len()));
        new_text.push_str(head);
        new_text.push_str(&page.title);
        new_text.push_str(tail);
        b.line.set_text(&new_text);
        let mark = if embedded {
            Mark::Transclusion(page.id)
        } else {
            Mark::Link(page.id)
        };
        b.line.add_mark(MarkSpan::new(mark_start..title_end, mark));
        b.line.set_cursor_pos(title_end);
        self.dirty = true;
        self.edit_pending = true;
    }

    /// So the host can avoid stealing focus from the popover.
    pub fn slash_menu_visible(&self) -> bool {
        self.slash_menu.is_visible()
    }

    #[doc(hidden)]
    pub fn slash_menu(&self) -> &SlashMenu {
        &self.slash_menu
    }

    /// Mirrors what the `/` keystroke does internally.
    #[doc(hidden)]
    pub fn open_slash_menu_for_test(&mut self) {
        let Some(focused_idx) = self.focused else {
            return;
        };
        let caret = self
            .blocks
            .get(focused_idx)
            .map_or(0, |b| b.line.cursor_pos());
        self.slash_menu
            .set_anchor(self.fallback_popover_anchor(focused_idx));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = Some(caret);
        self.slash_anchor_caret = Some((focused_idx, caret));
    }

    #[doc(hidden)]
    pub fn slash_menu_handle_key_for_test(&mut self, event: &WindowEvent) -> bool {
        let consumed = self.slash_menu.handle_key(event);
        if let Some(new_kind) = self.slash_menu.take_selection() {
            self.commit_slash_selection(new_kind);
        }
        if !self.slash_menu.is_visible() {
            self.slash_open_at = None;
        }
        consumed
    }

    /// No-op for non-Checklist kinds.
    pub fn try_toggle_check(&mut self, idx: usize) -> Option<bool> {
        self.begin_edit();
        let b = self.blocks.get_mut(idx)?;
        if let BlockKind::Checklist { indent, done } = &b.kind {
            let new_done = !*done;
            b.kind = BlockKind::Checklist {
                indent: *indent,
                done: new_done,
            };
            self.dirty = true;
            // Counts as a user edit so the page is dirty + gets persisted.
            self.edit_pending = true;
            Some(new_done)
        } else {
            None
        }
    }
}

/// A UTF-8 byte position in a block document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentPosition {
    pub block: usize,
    pub byte: usize,
}

/// A clipboard image and its original target. Capturing the intent never edits
/// the document; hosts must validate bytes before applying a replacement.
#[derive(Debug, Clone)]
pub struct ImagePasteRequest {
    pub image: crate::clipboard::ClipboardImage,
    pub target: ImagePasteTarget,
}

/// Stable IDs and source snapshots prevent a delayed paste from deleting a
/// different selection or text inserted by a collaborator.
#[derive(Debug, Clone)]
pub enum ImagePasteTarget {
    After(String),
    Replace {
        blocks: Vec<BlockContent>,
        start_byte: usize,
        end_byte: usize,
    },
}

/// Direction-preserving document selection. [`Self::ordered`] provides its
/// normalized start/end pair for editing and serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentSelection {
    pub anchor: DocumentPosition,
    pub cursor: DocumentPosition,
}

impl DocumentSelection {
    pub fn ordered(self) -> (DocumentPosition, DocumentPosition) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CursorPlacement {
    Start,
    End,
    Byte(usize),
}

/// Markdown heading prefix at the start of `text`: `(level, prefix_byte_len)`.
///
/// Shared by `from_markdown`, live `#`/`##`/`###` promotion, and the typed-prefix
/// gate that only fires when the caret sits exactly on that prefix.
fn heading_markdown_prefix(text: &str) -> Option<(u8, usize)> {
    if text.starts_with("### ") {
        Some((3, 4))
    } else if text.starts_with("## ") {
        Some((2, 3))
    } else if text.starts_with("# ") {
        Some((1, 2))
    } else {
        None
    }
}

fn is_heading_prefix(s: &str) -> bool {
    heading_markdown_prefix(s).is_some_and(|(_, len)| len == s.len())
}

#[expect(
    clippy::too_many_lines,
    reason = "the ordered block diff is kept together so move/insert/delete precedence remains explicit"
)]
fn semantic_block_ops(before: &[BlockSnapshot], after: &[BlockSnapshot]) -> Vec<BlockEditOp> {
    let mut ops = Vec::new();
    let old = before
        .iter()
        .map(|b| (b.id.as_str(), b))
        .collect::<HashMap<_, _>>();
    let new = after
        .iter()
        .map(|b| (b.id.as_str(), b))
        .collect::<HashMap<_, _>>();

    for block in before
        .iter()
        .filter(|block| !new.contains_key(block.id.as_str()))
    {
        ops.push(BlockEditOp::Delete {
            block_id: block.id.clone(),
        });
    }
    for (index, block) in after.iter().enumerate() {
        let after_id = index
            .checked_sub(1)
            .and_then(|i| after.get(i))
            .map(|b| b.id.clone());
        let Some(previous) = old.get(block.id.as_str()) else {
            ops.push(BlockEditOp::Insert {
                block_id: block.id.clone(),
                after: after_id,
                kind: block.kind.clone(),
                text: block.text.clone(),
            });
            if block.nesting > 0 {
                ops.push(BlockEditOp::SetNesting {
                    block_id: block.id.clone(),
                    depth: block.nesting,
                });
            }
            if block.code_group.is_some() {
                ops.push(BlockEditOp::SetCodeGroup {
                    block_id: block.id.clone(),
                    group_id: block.code_group.clone(),
                });
            }
            continue;
        };
        if previous.kind != block.kind {
            match (kind_from_str(&previous.kind), kind_from_str(&block.kind)) {
                (
                    BlockKind::Checklist {
                        indent: old_indent, ..
                    },
                    BlockKind::Checklist {
                        indent: new_indent,
                        done,
                    },
                ) if old_indent == new_indent => ops.push(BlockEditOp::SetChecklist {
                    block_id: block.id.clone(),
                    done,
                }),
                _ => ops.push(BlockEditOp::SetKind {
                    block_id: block.id.clone(),
                    kind: block.kind.clone(),
                }),
            }
        }
        if previous.text != block.text {
            let (start, end, text) = semantic_text_splice(&previous.text, &block.text);
            ops.push(BlockEditOp::TextSplice {
                block_id: block.id.clone(),
                start,
                end,
                text,
            });
        }
        if previous.nesting != block.nesting {
            ops.push(BlockEditOp::SetNesting {
                block_id: block.id.clone(),
                depth: block.nesting,
            });
        }
        if previous.code_group != block.code_group {
            ops.push(BlockEditOp::SetCodeGroup {
                block_id: block.id.clone(),
                group_id: block.code_group.clone(),
            });
        }
    }
    // Ordering is expressed with predecessor UUIDs, so it remains meaningful
    // when another peer inserts adjacent blocks concurrently.
    let mut order = before
        .iter()
        .filter(|b| new.contains_key(b.id.as_str()))
        .map(|b| b.id.clone())
        .collect::<Vec<_>>();
    for (index, block) in after.iter().enumerate() {
        if !old.contains_key(block.id.as_str()) {
            continue;
        }
        let expected_after = index
            .checked_sub(1)
            .and_then(|i| after.get(i))
            .map(|b| b.id.clone());
        let current_after = order
            .iter()
            .position(|id| id == &block.id)
            .and_then(|pos| pos.checked_sub(1).and_then(|p| order.get(p).cloned()));
        if current_after != expected_after {
            ops.push(BlockEditOp::Move {
                block_id: block.id.clone(),
                after: expected_after.clone(),
            });
            if let Some(pos) = order.iter().position(|id| id == &block.id) {
                order.remove(pos);
            }
            let destination = expected_after
                .as_ref()
                .and_then(|id| order.iter().position(|current| current == id))
                .map_or(0, |pos| pos.saturating_add(1));
            order.insert(destination, block.id.clone());
        }
    }
    ops
}

#[allow(
    clippy::suspicious_operation_groupings,
    reason = "the suffix scan intentionally compares two independently moving end cursors"
)]
fn semantic_text_splice(before: &str, after: &str) -> (usize, usize, String) {
    let mut start = 0;
    while start < before.len()
        && start < after.len()
        && before.as_bytes().get(start) == after.as_bytes().get(start)
    {
        start = start.saturating_add(1);
    }
    start = before
        .floor_char_boundary(start)
        .min(after.floor_char_boundary(start));
    let mut old_end = before.len();
    let mut new_end = after.len();
    while old_end > start
        && new_end > start
        && (before.as_bytes().get(old_end.saturating_sub(1))
            == after.as_bytes().get(new_end.saturating_sub(1)))
    {
        old_end = old_end.saturating_sub(1);
        new_end = new_end.saturating_sub(1);
    }
    old_end = before.ceil_char_boundary(old_end);
    new_end = after.ceil_char_boundary(new_end);
    (
        start,
        old_end,
        after.get(start..new_end).unwrap_or_default().to_string(),
    )
}

/// Two spaces per indent level (Notion/Obsidian style, not GFM's four);
/// a tab counts as one indent step. Returns `(indent_level, remainder)`.
fn parse_list_indent(line: &str) -> (u8, &str) {
    let mut bytes = 0usize;
    let mut level: u8 = 0;
    let b = line.as_bytes();
    while level < MAX_LIST_INDENT {
        if b.get(bytes..bytes.saturating_add(2)) == Some(&b"  "[..]) {
            bytes = bytes.saturating_add(2);
            level = level.saturating_add(1);
        } else if b.get(bytes) == Some(&b'\t') {
            bytes = bytes.saturating_add(1);
            level = level.saturating_add(1);
        } else {
            break;
        }
    }
    (level, line.get(bytes..).unwrap_or_default())
}

/// The digits are discarded — ordinals are recomputed from list
/// position on render + serialise.
fn strip_ordered_prefix(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i = i.saturating_add(1);
    }
    if i == 0 {
        return None;
    }
    let sep = *bytes.get(i)?;
    let next = *bytes.get(i.saturating_add(1))?;
    if (sep == b'.' || sep == b')') && next == b' ' {
        body.get(i.saturating_add(2)..)
    } else {
        None
    }
}

/// Applies a signed indent delta and clamps to `[0, MAX_LIST_INDENT]`.
/// Widening to `i16` before adding means the sum can never overflow
/// (`u8::MAX + i8::MIN..=i8::MAX` fits comfortably in `i16`), and the
/// post-clamp range always fits back into `u8`.
fn clamp_indent(indent: u8, delta: i8) -> u8 {
    let next = i16::from(indent).saturating_add(i16::from(delta));
    let next = next.clamp(0, i16::from(MAX_LIST_INDENT));
    u8::try_from(next).unwrap_or(0)
}

/// Inverse of [`parse_list_indent`]; tabs re-emit as spaces (the parser
/// also accepts those back).
fn push_indent(out: &mut String, indent: u8) {
    for _ in 0..indent.min(MAX_LIST_INDENT) {
        out.push_str("  ");
    }
}

/// 0 for kinds with no prefix glyph (paragraph, heading, divider);
/// `CodeBlock` reserves gutter for its left-edge bar, drawn in `draw`.
pub(crate) fn prefix_gutter_for(kind: &BlockKind) -> f64 {
    match kind {
        // Every list kind sets its text in one column, so bullets, numbers
        // and checkboxes mixed in a document line up: wide enough for a
        // two-digit ordinal ("12.") without colliding with the text.
        BlockKind::Bullet { .. } | BlockKind::Numbered { .. } | BlockKind::Checklist { .. } => 28.0,
        BlockKind::CodeBlock { .. } => 12.0,
        BlockKind::Quote => 16.0,
        _ => 0.0,
    }
}

pub(crate) const CHECKBOX_SIZE_PX: f64 = 16.0;

/// Geometry of the block drag handle, resolved from `theme.tokens` in
/// `build_layout` (the `Metrics::BASE` / `Spacing::BASE` values until then).
///
/// The handle lives in the editor's handle gutter, left of the text column,
/// so its **width** and the gap to the text are `ui_scale` metrics that make
/// up that gutter; its **height** and the dot cluster
/// follow the accessibility text scale like the line beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HandleMetrics {
    /// Handle column width (`spacing.s4`, 16 px at 1×).
    width: f64,
    /// Handle height at 1× text (`metrics.icon_md`, 20 px), text-scaled.
    height: f64,
    /// Gap between the handle and the text's leading edge
    /// (`metrics.icon_label_gap`, 6 px).
    gap: f64,
    /// `metrics.min_pointer_target`: the hit rect grows to this where the
    /// block is tall enough not to overlap its neighbours' handles.
    min_target: f64,
}

impl Default for HandleMetrics {
    fn default() -> Self {
        Self::from_tokens(&crate::tokens::Spacing::BASE, &crate::tokens::Metrics::BASE)
    }
}

impl HandleMetrics {
    fn from_tokens(spacing: &crate::tokens::Spacing, metrics: &crate::tokens::Metrics) -> Self {
        Self {
            width: f64::from(spacing.s4),
            height: f64::from(metrics.icon_md),
            gap: f64::from(metrics.icon_label_gap),
            min_target: f64::from(metrics.min_pointer_target),
        }
    }
}

fn formula_descriptor(kind: &BlockKind) -> Option<crate::math::FormulaDescriptor> {
    let BlockKind::CodeBlock { lang } = kind else {
        return None;
    };
    crate::math::FormulaDescriptor::from_fence_language(lang.as_deref())
}

fn markdown_table_rows(source: &str) -> Vec<Vec<&str>> {
    markdown_table_ranges(source)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .filter_map(|range| source.get(range))
                .collect()
        })
        .collect()
}

fn markdown_table_data_rows(source: &str) -> Vec<Vec<&str>> {
    markdown_table_rows(source)
        .into_iter()
        .enumerate()
        .filter(|(index, row)| *index != 1 || !markdown_table_separator(row))
        .map(|(_, row)| row)
        .collect()
}

fn markdown_table_ranges(source: &str) -> Vec<Vec<std::ops::Range<usize>>> {
    let mut offset = 0usize;
    let mut rows = Vec::new();
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        let leading = line.len().saturating_sub(line.trim_start().len());
        let mut delimiters = Vec::new();
        let mut escaped = false;
        for (index, ch) in trimmed.char_indices() {
            if ch == '|' && !escaped {
                delimiters.push(index);
            }
            escaped = ch == '\\' && !escaped;
        }
        if !delimiters.is_empty() {
            let start = usize::from(delimiters.first() == Some(&0));
            let end = if delimiters.last().copied() == trimmed.len().checked_sub(1) {
                trimmed.len().saturating_sub(1)
            } else {
                trimmed.len()
            };
            let mut previous = start;
            let mut cells = Vec::new();
            for boundary in delimiters
                .into_iter()
                .filter(|index| *index >= start && *index < end)
                .chain(std::iter::once(end))
            {
                if let Some(cell) = trimmed.get(previous..boundary) {
                    let left =
                        previous.saturating_add(cell.len().saturating_sub(cell.trim_start().len()));
                    let right = left.saturating_add(cell.trim().len());
                    let base = offset.saturating_add(leading);
                    cells.push(base.saturating_add(left)..base.saturating_add(right));
                }
                previous = boundary.saturating_add(1);
            }
            rows.push(cells);
        }
        offset = offset.saturating_add(line.len());
    }
    rows
}

fn decode_table_cell(value: &str) -> String {
    let value = value
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("&#32;", " ")
        .replace("&#9;", "\t")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let mut decoded = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|next| matches!(next, '|' | '\\')) {
            if let Some(next) = chars.next() {
                decoded.push(next);
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

fn encode_table_cell(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let first_content = normalized
        .find(|character| character != ' ')
        .unwrap_or(normalized.len());
    let last_content = normalized
        .rfind(|character| character != ' ')
        .map_or(0, |index| {
            index.saturating_add(
                normalized
                    .get(index..)
                    .and_then(|suffix| suffix.chars().next())
                    .map_or(0, char::len_utf8),
            )
        });
    let mut encoded = String::with_capacity(normalized.len());
    for (index, character) in normalized.char_indices() {
        match character {
            '&' => encoded.push_str("&amp;"),
            '<' => encoded.push_str("&lt;"),
            '>' => encoded.push_str("&gt;"),
            ' ' if index < first_content || index >= last_content => encoded.push_str("&#32;"),
            ' ' => encoded.push(' '),
            '\t' => encoded.push_str("&#9;"),
            '\\' => encoded.push_str("\\\\"),
            '|' => encoded.push_str("\\|"),
            '\n' => encoded.push_str("<br>"),
            other => encoded.push(other),
        }
    }
    encoded
}

fn markdown_table_separator(row: &[&str]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let cell = cell.trim_matches(':').trim();
            cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
        })
}

struct TablePreviewLayout {
    rows: Vec<Vec<String>>,
    row_heights: Vec<f64>,
    columns: usize,
    column_width: f64,
    padding_x: f64,
    stacked_labels: Option<Vec<String>>,
    label_heights: Vec<f64>,
    cell_heights: Vec<Vec<f64>>,
}

struct TableModeControl {
    button: crate::widget::Button,
    requested: std::rc::Rc<std::cell::Cell<bool>>,
    visual: bool,
    add_row: crate::widget::Button,
    add_column: crate::widget::Button,
    structure: std::rc::Rc<std::cell::Cell<Option<bool>>>,
}

impl TableModeControl {
    fn new(id: &str, visual: bool) -> Self {
        let requested = std::rc::Rc::new(std::cell::Cell::new(false));
        let clicked = requested.clone();
        let button = crate::widget::Button::new(
            &format!("notes.table-mode:{id}"),
            if visual {
                "table-edit-source"
            } else {
                "table-edit-visual"
            },
        )
        .on_click(move || clicked.set(true));
        let structure = std::rc::Rc::new(std::cell::Cell::new(None));
        let row_request = structure.clone();
        let column_request = structure.clone();
        let add_row =
            crate::widget::Button::new(&format!("notes.table-add-row:{id}"), "table-add-row")
                .on_click(move || row_request.set(Some(true)));
        let add_column =
            crate::widget::Button::new(&format!("notes.table-add-column:{id}"), "table-add-column")
                .on_click(move || column_request.set(Some(false)));
        Self {
            button,
            requested,
            visual,
            add_row,
            add_column,
            structure,
        }
    }
}

pub(super) fn table_toolbar_height(width: f32, scale: f32) -> f32 {
    let columns = (width / (100.0 * scale)).floor().clamp(1.0, 3.0);
    (3.0 / columns).ceil() * 30.0 * scale + 6.0 * scale
}

impl TablePreviewLayout {
    fn height(&self) -> f64 {
        self.row_heights.iter().sum()
    }

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::cast_precision_loss,
        reason = "bounded table-grid rendering converts validated row and column indices to coordinates"
    )]
    fn editor_rects(&self, rect: Rect, scale: f64) -> Vec<Rect> {
        let source_rows = self.rows.len() + usize::from(self.stacked_labels.is_some());
        let mut rects = vec![Rect::ZERO; source_rows.saturating_mul(self.columns)];
        let mut y = rect.y0;
        for (row, height) in self.row_heights.iter().enumerate() {
            if self.stacked_labels.is_some() {
                let mut field_y = y + 8.0 * scale;
                for column in 0..self.columns {
                    let x = rect.x0 + self.padding_x - 10.0;
                    let right = rect.x1 - self.padding_x + 10.0;
                    let label_height = self.label_heights.get(column).copied().unwrap_or(0.0);
                    if row == 0
                        && let Some(slot) = rects.get_mut(column)
                    {
                        *slot = Rect::new(x, field_y - 8.0, right, field_y + label_height + 8.0);
                    }
                    let cell_height = self
                        .cell_heights
                        .get(row)
                        .and_then(|cells| cells.get(column))
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(slot) = rects.get_mut((row + 1) * self.columns + column) {
                        *slot = Rect::new(
                            x,
                            field_y + label_height + 4.0 * scale - 8.0,
                            right,
                            field_y + cell_height - 4.0 * scale + 8.0,
                        );
                    }
                    field_y += cell_height;
                }
            } else {
                for column in 0..self.columns {
                    let x = rect.x0 + self.column_width * column as f64;
                    if let Some(slot) = rects.get_mut(row * self.columns + column) {
                        *slot = Rect::new(
                            x + self.padding_x - 10.0,
                            y + 8.0 * scale - 8.0,
                            x + self.column_width - self.padding_x + 10.0,
                            y + height,
                        );
                    }
                }
            }
            y += height;
        }
        rects
    }
}

/// Measurement and painting consume the same complete table and text style.
/// The renderer wraps cell text; row height follows its tallest cell.
#[expect(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "table measurement is one cohesive pass and converts bounded viewport dimensions for the text API"
)]
fn markdown_table_layout(
    source: &str,
    width: f32,
    renderer: &mut TextRenderer,
    theme: &Theme,
) -> TablePreviewLayout {
    use crate::text_render::TextWeight;
    let mut rows = markdown_table_data_rows(source);
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let scale = f64::from(theme.text_scale);
    let stacked =
        f64::from(width) / (columns as f64) < 96.0 * scale && columns > 1 && rows.len() > 1;
    let column_width = f64::from(width.max(1.0)) / if stacked { 1.0 } else { columns as f64 };
    let padding_x = (10.0 * scale).min(column_width * 0.15);
    let text_width = (column_width - 2.0 * padding_x).max(1.0) as f32;
    let mut display_text = |cell: &str, weight| {
        let mut display = String::new();
        for word in decode_table_cell(cell).split_inclusive(char::is_whitespace) {
            if renderer
                .measure_styled(word, 14.0, None, weight, false, false)
                .0
                > text_width
            {
                for (index, grapheme) in crate::text_break::graphemes(word).enumerate() {
                    if index > 0 {
                        display.push('\u{200b}');
                    }
                    display.push_str(grapheme);
                }
            } else {
                display.push_str(word);
            }
        }
        display
    };
    let stacked_labels = if stacked && rows.len() > 1 {
        let header = rows.remove(0);
        Some(
            (0..columns)
                .map(|column| {
                    display_text(header.get(column).copied().unwrap_or(""), TextWeight::Bold)
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let label_heights = stacked_labels.as_ref().map_or_else(Vec::new, |labels| {
        labels
            .iter()
            .map(|label| {
                f64::from(
                    renderer
                        .measure_styled(
                            label,
                            14.0,
                            Some(text_width),
                            TextWeight::Bold,
                            false,
                            false,
                        )
                        .1,
                )
            })
            .collect::<Vec<_>>()
    });
    let mut row_heights = Vec::with_capacity(rows.len());
    let mut cell_heights = Vec::with_capacity(rows.len());
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let weight = if index == 0 && stacked_labels.is_none() {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            };
            let mut height = TABLE_ROW_HEIGHT * scale;
            let mut heights = Vec::with_capacity(columns);
            let cells = (0..columns)
                .map(|column| {
                    let cell = row.get(column).copied().unwrap_or("");
                    // Preserve every grapheme of an overlong URL/identifier by adding
                    // display-only break opportunities. The canonical cell is unchanged.
                    let mut display = String::new();
                    for word in decode_table_cell(cell).split_inclusive(char::is_whitespace) {
                        if renderer
                            .measure_styled(word, 14.0, None, weight, false, false)
                            .0
                            > text_width
                        {
                            for (index, grapheme) in crate::text_break::graphemes(word).enumerate()
                            {
                                if index > 0 {
                                    display.push('\u{200b}');
                                }
                                display.push_str(grapheme);
                            }
                        } else {
                            display.push_str(word);
                        }
                    }
                    let (_, text_height) = renderer.measure_styled(
                        &display,
                        14.0,
                        Some(text_width),
                        weight,
                        false,
                        false,
                    );
                    height = height.max(f64::from(text_height) + 16.0 * scale);
                    heights.push(
                        f64::from(text_height)
                            + label_heights.get(column).copied().unwrap_or(0.0)
                            + 16.0 * scale,
                    );
                    display
                })
                .collect();
            if stacked_labels.is_some() {
                height = heights.iter().sum::<f64>() + 16.0 * scale;
            }
            row_heights.push(height);
            cell_heights.push(heights);
            cells
        })
        .collect();
    TablePreviewLayout {
        rows,
        row_heights,
        columns,
        column_width,
        padding_x,
        stacked_labels,
        label_heights,
        cell_heights,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the finite measured pixel height is intentionally narrowed for the f32 layout API"
)]
pub(super) fn measure_table_preview_height(
    source: &str,
    width: f32,
    renderer: &mut TextRenderer,
    theme: &Theme,
) -> f32 {
    markdown_table_layout(source, width, renderer, theme).height() as f32
}

fn standalone_markdown_image(source: &str) -> Option<(&str, &str)> {
    let source = source.trim();
    let body = source.strip_prefix("![")?;
    let (alt, destination) = body.split_once("](")?;
    let destination = destination.strip_suffix(')')?.trim();
    if destination.is_empty() || destination.contains(['\n', '\r']) {
        return None;
    }
    Some((alt.trim(), destination))
}

fn formula_run_end(blocks: &[Block], start: usize) -> usize {
    let Some(descriptor) = blocks
        .get(start)
        .and_then(|block| formula_descriptor(&block.kind))
    else {
        return start;
    };
    let mut end = start;
    while end.saturating_add(1) < blocks.len()
        && blocks
            .get(end.saturating_add(1))
            .and_then(|block| formula_descriptor(&block.kind))
            == Some(descriptor)
        && blocks
            .get(start)
            .zip(blocks.get(end.saturating_add(1)))
            .is_some_and(|(first, next)| first.same_code_group(next))
    {
        end = end.saturating_add(1);
    }
    end
}

/// Per-block "is this the first block of a formula run" flag: the
/// descriptor changes (or there is no previous block) at this index.
/// Shared by `build_layout` and `apply_layout` so the "which index is a
/// formula boundary" logic lives in exactly one panic-free place.
fn compute_is_formula_start(blocks: &[Block]) -> Vec<bool> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            formula_descriptor(&block.kind).is_some()
                && index
                    .checked_sub(1)
                    .and_then(|prev| blocks.get(prev))
                    .is_none_or(|prev_block| !prev_block.same_code_group(block))
        })
        .collect()
}

/// Per-block "is this the last block of a formula run" flag: the
/// descriptor changes (or there is no next block) at this index. See
/// [`compute_is_formula_start`].
fn compute_is_formula_end(blocks: &[Block]) -> Vec<bool> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            formula_descriptor(&block.kind).is_some()
                && blocks
                    .get(index.saturating_add(1))
                    .is_none_or(|next_block| !next_block.same_code_group(block))
        })
        .collect()
}

#[derive(Clone)]
struct FormulaRunPreview {
    end: usize,
    descriptor: crate::math::FormulaDescriptor,
    source: String,
}

#[derive(Clone, Copy, Debug)]
struct FormulaChrome {
    end: usize,
    descriptor: crate::math::FormulaDescriptor,
    syntax_badge: Rect,
    source_button: Rect,
    visual_button: Rect,
    editor_rect: Rect,
}

struct VisualFormulaState {
    source: String,
    syntax: crate::math::FormulaSyntax,
    editor: crate::math::edit::Editor,
    layout: Option<crate::math::Layout>,
}

impl VisualFormulaState {
    fn new(syntax: crate::math::FormulaSyntax, source: &str) -> Self {
        Self {
            source: source.to_string(),
            syntax,
            editor: crate::math::edit::Editor::new(syntax.parse(source).expr),
            layout: None,
        }
    }

    fn sync_source(&mut self, syntax: crate::math::FormulaSyntax, source: &str) {
        if self.syntax != syntax || self.source != source {
            *self = Self::new(syntax, source);
        }
    }
}

fn draw_formula_preview(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    syntax: crate::math::FormulaSyntax,
    source: &str,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    if source.trim().is_empty() {
        text_renderer.draw_text(
            scene,
            "Type a formula above to preview it",
            12.0,
            theme.tokens.colors.text_dim,
            translated(transform, rect.x0 + 8.0, rect.y0 + 8.0),
            None,
        );
        return;
    }

    let parsed = syntax.parse(source);
    let diagnostic_height = if parsed.diagnostics.is_empty() {
        0.0
    } else {
        18.0
    };
    let available_width = (rect.width() - 16.0).max(1.0);
    let available_height = (rect.height() - diagnostic_height - 8.0).max(1.0);
    let mut size = 25.0f32;
    let mut formula = crate::math::layout(&parsed.expr.root, text_renderer, size);
    while size > 13.0 && (formula.width > available_width || formula.height > available_height) {
        size -= 1.5;
        formula = crate::math::layout(&parsed.expr.root, text_renderer, size);
    }
    let origin = Point::new(
        rect.x0 + (rect.width() - formula.width).max(0.0) * 0.5,
        rect.y0 + (available_height - formula.height).max(0.0) * 0.5,
    );
    crate::math::draw_layout(
        scene,
        transform,
        text_renderer,
        &formula,
        origin,
        theme.tokens.colors.text_primary,
    );
    if let Some(diagnostic) = parsed.diagnostics.first() {
        let mut message = format!("Preview: {}", diagnostic.message);
        if message.chars().count() > 90 {
            let mut truncated: String = message.chars().take(89).collect();
            truncated.push('…');
            message = truncated;
        }
        text_renderer.draw_text(
            scene,
            &message,
            11.0,
            theme.tokens.colors.text_dim,
            translated(transform, rect.x0 + 6.0, rect.y1 - 15.0),
            None,
        );
    }
}

fn draw_formula_chrome(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    chrome: FormulaChrome,
) {
    use vello::peniko::Fill;

    let colors = &theme.tokens.colors;
    let syntax = match chrome.descriptor.syntax {
        crate::math::FormulaSyntax::Tex => "TeX",
        crate::math::FormulaSyntax::Typst => "Typst",
    };
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.base,
        None,
        &vello::kurbo::RoundedRect::from_rect(chrome.syntax_badge, 5.0),
    );
    text_renderer.draw_text(
        scene,
        syntax,
        11.0,
        colors.text_primary,
        translated(
            transform,
            chrome.syntax_badge.x0 + 7.0,
            chrome.syntax_badge.y0 + 5.0,
        ),
        None,
    );
    for (rect, label, selected) in [
        (
            chrome.source_button,
            "Source",
            chrome.descriptor.mode == crate::math::FormulaMode::Source,
        ),
        (
            chrome.visual_button,
            "Visual",
            chrome.descriptor.mode == crate::math::FormulaMode::Visual,
        ),
    ] {
        let shape = vello::kurbo::RoundedRect::from_rect(rect, 5.0);
        if selected {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.20),
                None,
                &shape,
            );
        }
        text_renderer.draw_text(
            scene,
            label,
            11.0,
            if selected {
                colors.text_primary
            } else {
                colors.text_dim
            },
            translated(transform, rect.x0 + 7.0, rect.y0 + 5.0),
            None,
        );
    }
}

fn visual_formula_origin(rect: Rect, layout: &crate::math::Layout) -> Point {
    Point::new(
        rect.x0 + (rect.width() - layout.width).max(0.0) * 0.5,
        rect.y0 + (rect.height() - layout.height).max(0.0) * 0.5,
    )
}

fn draw_visual_formula_editor(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    state: &mut VisualFormulaState,
    focused: bool,
) {
    let mut size = 25.0f32;
    let mut layout = crate::math::layout(&state.editor.expr.root, text_renderer, size);
    while size > 13.0 && (layout.width > rect.width() - 16.0 || layout.height > rect.height() - 8.0)
    {
        size -= 1.5;
        layout = crate::math::layout(&state.editor.expr.root, text_renderer, size);
    }
    let origin = visual_formula_origin(rect, &layout);
    if let Some(selection) = state.editor.selection() {
        let row_gaps: Vec<_> = layout
            .gaps
            .iter()
            .filter(|gap| gap.steps == selection.steps)
            .collect();
        let x0 = row_gaps
            .iter()
            .find(|gap| gap.index == selection.lo())
            .map(|gap| gap.x);
        let x1 = row_gaps
            .iter()
            .find(|gap| gap.index == selection.hi())
            .map(|gap| gap.x);
        if let (Some(x0), Some(x1)) = (x0, x1) {
            let top = row_gaps
                .iter()
                .map(|gap| gap.top)
                .fold(f64::INFINITY, f64::min);
            let bottom = row_gaps
                .iter()
                .map(|gap| gap.bottom)
                .fold(f64::NEG_INFINITY, f64::max);
            let band = Rect::new(
                origin.x + x0,
                origin.y + top,
                origin.x + x1,
                origin.y + bottom,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.selection_tint,
                None,
                &band,
            );
        }
    }
    crate::math::draw_layout(
        scene,
        transform,
        text_renderer,
        &layout,
        origin,
        theme.tokens.colors.text_primary,
    );
    if focused
        && let Some(gap) = layout.gaps.iter().find(|gap| {
            gap.steps == state.editor.caret.steps && gap.index == state.editor.caret.offset
        })
    {
        let caret = Rect::new(
            origin.x + gap.x - 1.0,
            origin.y + gap.top,
            origin.x + gap.x + 1.0,
            origin.y + gap.bottom,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.you,
            None,
            &caret,
        );
    }
    state.layout = Some(layout);
}

fn handle_visual_formula_key(
    editor: &mut crate::math::edit::Editor,
    key: &winit::keyboard::Key,
    modifiers: Modifiers,
) -> bool {
    use winit::keyboard::{Key, NamedKey};
    let state = modifiers.state();
    let shift = state.shift_key();
    let ctrl = state.control_key() || state.super_key();
    match key {
        Key::Named(NamedKey::ArrowLeft) => {
            if ctrl {
                editor.move_word_left(shift);
            } else {
                editor.move_left(shift);
            }
            false
        }
        Key::Named(NamedKey::ArrowRight) => {
            if ctrl {
                editor.move_word_right(shift);
            } else {
                editor.move_right(shift);
            }
            false
        }
        Key::Named(NamedKey::ArrowUp) => {
            editor.move_up();
            false
        }
        Key::Named(NamedKey::ArrowDown) => {
            editor.move_down();
            false
        }
        Key::Named(NamedKey::Home) => {
            editor.home(shift);
            false
        }
        Key::Named(NamedKey::End) => {
            editor.end(shift);
            false
        }
        Key::Named(NamedKey::Backspace) => {
            editor.backspace();
            true
        }
        Key::Named(NamedKey::Delete) => {
            editor.delete();
            true
        }
        Key::Character(value) if ctrl => match value.as_str() {
            "a" | "A" => {
                editor.select_all();
                false
            }
            "c" | "C" => {
                if let Some(value) = editor.selected_linear() {
                    clipboard_copy(&value);
                }
                false
            }
            "x" | "X" => {
                if let Some(value) = editor.selected_linear()
                    && clipboard_copy_accepted(&value)
                {
                    editor.delete_selection();
                    return true;
                }
                false
            }
            "v" | "V" => {
                if let Some(value) = clipboard_paste() {
                    editor.paste_linear(
                        &value
                            .chars()
                            .filter(|ch| !ch.is_control())
                            .collect::<String>(),
                    );
                    return true;
                }
                false
            }
            _ => false,
        },
        Key::Character(value) => {
            let mut changed = false;
            for mut ch in value.chars() {
                ch = match ch {
                    '×' => '·',
                    '÷' => '/',
                    '−' => '-',
                    other => other,
                };
                if ch.is_alphanumeric() || "+-*/^(),. %!·π√".contains(ch) {
                    editor.type_char(ch);
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    }
}

/// 1-based ordinal per `Numbered` block, 0 for non-numbered rows.
/// Numbered per contiguous run at each indent level: a deeper level
/// restarts under its parent. A different list kind resets its own level
/// and deeper levels, while preserving the numbering of its ancestors.
pub fn numbered_ordinals(blocks: &[Block]) -> Vec<u32> {
    let mut out = vec![0u32; blocks.len()];
    let mut counters = [0u32; MAX_LIST_INDENT as usize + 1];
    for (i, b) in blocks.iter().enumerate() {
        match b.kind {
            BlockKind::Numbered { indent } => {
                let lvl = (indent as usize).min(MAX_LIST_INDENT as usize);
                if let Some(counter) = counters.get_mut(lvl) {
                    *counter = counter.saturating_add(1);
                    if let Some(o) = out.get_mut(i) {
                        *o = *counter;
                    }
                }
                for c in counters.iter_mut().skip(lvl.saturating_add(1)) {
                    *c = 0;
                }
            }
            BlockKind::Bullet { indent } | BlockKind::Checklist { indent, .. } => {
                for counter in counters.iter_mut().skip(usize::from(indent)) {
                    *counter = 0;
                }
            }
            _ => {
                for counter in counters.iter_mut().skip(usize::from(b.indent())) {
                    *counter = 0;
                }
            }
        }
    }
    out
}

// `BlockKind` is encoded as a compact string in the doc's `kind` field:
// paragraph / heading{1..3} / bullet:{indent} / checklist:{indent}:{0|1}
// / code:{lang} / divider.

/// Keyed by the block's stable CRDT id (never its position), so a peer's
/// caret/selection stays on the same block when blocks are inserted, removed
/// or moved above it on either side.
pub fn block_presence_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
}

/// A document selection uses one transport frame: the start block's stable
/// id and the number of blocks it spans after that block live in the key,
/// the existing selection payload carries the endpoint bytes. Compact enough
/// for the live presence anchor bound (64 bytes with a UUID block id).
pub fn document_selection_presence_key(start_block_id: &str, span: usize) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_sel".to_string()),
            PathSegment::Field(start_block_id.to_string()),
            PathSegment::Index(u32::try_from(span).unwrap_or(u32::MAX)),
        ],
    }
}

/// `(start block id, span)` of a document selection key.
fn document_selection_parts(key: &PresenceKey) -> Option<(&str, usize)> {
    let PresenceKey::Widget { path } = key else {
        return None;
    };
    match path.as_slice() {
        [
            PathSegment::Field(tag),
            PathSegment::Field(start),
            PathSegment::Index(span),
        ] if tag == "notes_sel" => Some((start.as_str(), *span as usize)),
        _ => None,
    }
}

fn kind_to_str(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph => "paragraph".to_string(),
        BlockKind::Quote => "quote".to_string(),
        BlockKind::Heading(n) => format!("heading{n}"),
        BlockKind::Bullet { indent } => format!("bullet:{indent}"),
        BlockKind::Numbered { indent } => format!("numbered:{indent}"),
        BlockKind::Checklist { indent, done } => {
            format!("checklist:{indent}:{}", i32::from(*done))
        }
        BlockKind::CodeBlock { lang } => {
            format!("code:{}", lang.clone().unwrap_or_default())
        }
        BlockKind::Table { visual } => format!("table:{}", i32::from(*visual)),
        BlockKind::Divider => "divider".to_string(),
    }
}

fn accessible_kind_name(kind: &BlockKind) -> &'static str {
    match kind {
        BlockKind::Paragraph => "Paragraph",
        BlockKind::Quote => "Quote",
        BlockKind::Heading(_) => "Heading",
        BlockKind::Bullet { .. } => "Bullet",
        BlockKind::Numbered { .. } => "Numbered item",
        BlockKind::Checklist { .. } => "Checklist item",
        BlockKind::CodeBlock { .. } => "Code block",
        BlockKind::Table { .. } => "Table",
        BlockKind::Divider => "Divider",
    }
}

fn valid_existing_kind(kind: &str) -> bool {
    BlockKind::from_stored(kind).is_some()
}

fn kind_from_str(s: &str) -> BlockKind {
    if s == "paragraph" {
        return BlockKind::Paragraph;
    }
    if s == "quote" {
        return BlockKind::Quote;
    }
    if s == "divider" {
        return BlockKind::Divider;
    }
    if let Some(n) = s.strip_prefix("heading") {
        let lvl = n.parse::<u8>().unwrap_or(1).clamp(1, 3);
        return BlockKind::Heading(lvl);
    }
    if let Some(rest) = s.strip_prefix("bullet:") {
        let indent = rest.parse::<u8>().unwrap_or(0).min(MAX_LIST_INDENT);
        return BlockKind::Bullet { indent };
    }
    if let Some(rest) = s.strip_prefix("numbered:") {
        let indent = rest.parse::<u8>().unwrap_or(0).min(MAX_LIST_INDENT);
        return BlockKind::Numbered { indent };
    }
    if let Some(rest) = s.strip_prefix("checklist:") {
        let mut parts = rest.split(':');
        let indent = parts
            .next()
            .and_then(|p| p.parse::<u8>().ok())
            .unwrap_or(0)
            .min(MAX_LIST_INDENT);
        let done = parts.next().is_some_and(|p| p == "1");
        return BlockKind::Checklist { indent, done };
    }
    if let Some(rest) = s.strip_prefix("code:") {
        let lang = if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        };
        return BlockKind::CodeBlock { lang };
    }
    if let Some(visual) = s.strip_prefix("table:") {
        return BlockKind::Table {
            visual: visual == "1",
        };
    }
    BlockKind::Paragraph
}

/// Text is bound + pulled from the doc afterwards, so we seed with "".
fn block_for_kind(kind: &BlockKind) -> Block {
    match kind {
        BlockKind::Paragraph => Block::paragraph(""),
        BlockKind::Quote => Block::quote(""),
        BlockKind::Heading(n) => Block::heading(*n, ""),
        BlockKind::Bullet { indent } => Block::bullet(*indent, ""),
        BlockKind::Numbered { indent } => Block::numbered(*indent, ""),
        BlockKind::Checklist { indent, done } => Block::checklist(*indent, *done, ""),
        BlockKind::CodeBlock { lang } => Block::code_block(lang.clone(), ""),
        BlockKind::Table { visual } => Block::table("", *visual),
        BlockKind::Divider => Block::divider(),
    }
}

/// Same continuation policy as pressing Enter in a block.
fn continuation_block(kind: &BlockKind, text: &str) -> Block {
    match kind {
        BlockKind::Bullet { indent } => Block::bullet(*indent, text),
        BlockKind::Numbered { indent } => Block::numbered(*indent, text),
        BlockKind::Checklist { indent, .. } => Block::checklist(*indent, false, text),
        BlockKind::CodeBlock { lang } => Block::code_block(lang.clone(), text),
        BlockKind::Table { visual } => Block::table(text, *visual),
        BlockKind::Quote => Block::quote(text),
        _ => Block::paragraph(text),
    }
}

fn list_map(state: &AutoCommit, list: &ObjId, index: usize) -> Option<ObjId> {
    match state.get(list, index) {
        Ok(Some((Value::Object(ObjType::Map), m))) => Some(m),
        _ => None,
    }
}

fn doc_text_obj(state: &AutoCommit, list: &ObjId, index: usize) -> Option<ObjId> {
    let block = list_map(state, list, index)?;
    match state.get(&block, "text") {
        Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
        _ => None,
    }
}

/// Defaults to "paragraph" if the block or field is missing.
fn read_block_kind(state: &AutoCommit, list: &ObjId, index: usize) -> String {
    let Some(block) = list_map(state, list, index) else {
        return "paragraph".to_string();
    };
    match state.get(&block, "kind") {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            automerge::ScalarValue::Str(s) => s.to_string(),
            _ => "paragraph".to_string(),
        },
        _ => "paragraph".to_string(),
    }
}

/// A block's persisted UUID, or the deterministic UUID a block map without
/// one is known by: derived only from replicated content and position, so
/// every replica reads the same value. The owner's action persists it.
fn read_block_id_at(state: &AutoCommit, list: &ObjId, index: usize, kind: &BlockKind) -> String {
    let Some(map) = list_map(state, list, index) else {
        return Uuid::new_v4().to_string();
    };
    if let Some(id) = read_block_id(state, &map) {
        return id;
    }
    let text = doc_text_obj(state, list, index)
        .and_then(|text| state.text(&text).ok())
        .unwrap_or_default();
    legacy_block_id(list, index, &kind_to_str(kind), &text)
}

/// Deterministic stable ID for a legacy block map that has no persisted `id`.
///
/// Action-owned hosts use this read-only projection while capturing an edit;
/// their action handler persists the ID atomically with the first operation.
#[must_use]
pub fn legacy_block_id(list: &ObjId, index: usize, kind: &str, text: &str) -> String {
    let seed = format!("tilo:block:v1:{list:?}:{index}:{kind}:{text}");
    deterministic_block_uuid(&seed).to_string()
}

fn read_block_id(state: &AutoCommit, map: &ObjId) -> Option<String> {
    let Ok(Some((Value::Scalar(value), _))) = state.get(map, "id") else {
        return None;
    };
    let automerge::ScalarValue::Str(id) = value.as_ref() else {
        return None;
    };
    Uuid::parse_str(id).ok().map(|_| id.to_string())
}

fn deterministic_block_uuid(seed: &str) -> Uuid {
    fn fnv(seed: &[u8], initial: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(initial);
        std::hash::Hasher::write(&mut hasher, seed);
        std::hash::Hasher::finish(&hasher)
    }
    let hi = fnv(seed.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv(seed.as_bytes(), 0x8422_2325_cbf2_9ce4);
    let mut raw = (u128::from(hi) << 64) | u128::from(lo);
    // RFC 4122 variant + a deterministic name-based version marker.
    raw = (raw & !(0xf_u128 << 76)) | (5_u128 << 76);
    raw = (raw & !(0x3_u128 << 62)) | (0x2_u128 << 62);
    Uuid::from_u128(raw)
}

fn read_nesting(state: &AutoCommit, map: &ObjId) -> Result<u8, crate::view::ThawError> {
    let invalid = || crate::view::ThawError::State("invalid or conflicted block nesting".into());
    match state
        .get_all(map, "nesting")
        .map_err(|_| invalid())?
        .as_slice()
    {
        [] => Ok(0),
        [(Value::Scalar(value), _)] => value
            .as_u64()
            .filter(|depth| *depth <= u64::from(MAX_LIST_INDENT))
            .and_then(|depth| u8::try_from(depth).ok())
            .ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

fn read_code_group(
    state: &AutoCommit,
    map: &ObjId,
) -> Result<Option<String>, crate::view::ThawError> {
    let invalid = || crate::view::ThawError::State("invalid or conflicted code group".into());
    let values = state.get_all(map, "code_group").map_err(|_| invalid())?;
    match values.as_slice() {
        [] => Ok(None),
        [(Value::Scalar(value), _)] => {
            let raw = value.as_str().ok_or_else(invalid)?;
            let id = Uuid::parse_str(raw).map_err(|_| invalid())?;
            if id.is_nil() {
                return Err(invalid());
            }
            Ok(Some(raw.to_string()))
        }
        _ => Err(invalid()),
    }
}

/// Marks entirely (or partly) before `boundary`, clipped to it — the
/// surviving fragment of a replaced selection's leading edge.
fn marks_before(marks: &[MarkSpan], boundary: usize) -> Vec<MarkSpan> {
    marks
        .iter()
        .filter(|span| span.range.start < boundary)
        .map(|span| {
            MarkSpan::new(
                span.range.start..span.range.end.min(boundary),
                span.mark.clone(),
            )
        })
        .collect()
}

/// Marks entirely (or partly) after `boundary`, re-based to start at 0 —
/// the surviving fragment of a replaced selection's trailing edge, ready
/// to be shifted onto wherever the replacement ends up landing.
fn marks_after(marks: &[MarkSpan], boundary: usize) -> Vec<MarkSpan> {
    marks
        .iter()
        .filter(|span| span.range.end > boundary)
        .map(|span| {
            MarkSpan::new(
                span.range.start.max(boundary).saturating_sub(boundary)
                    ..span.range.end.saturating_sub(boundary),
                span.mark.clone(),
            )
        })
        .collect()
}

/// Collapses clipboard/IME line endings and drops control characters
/// other than `\n`/`\t` before the text is spliced into the document.
fn normalize_replacement(replacement: &str) -> String {
    replacement
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

impl Default for BlockEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockEditor {
    pub fn document_selection(&self) -> Option<DocumentSelection> {
        self.document_selection
    }

    /// Set and paint a document range using UTF-8 byte offsets. Positions are
    /// clamped to existing block/character boundaries.
    pub fn select_document_range(&mut self, anchor: DocumentPosition, cursor: DocumentPosition) {
        self.retained_block_selection = None;
        if self.blocks.is_empty() {
            return;
        }
        let anchor_block = anchor.block.min(self.blocks.len().saturating_sub(1));
        let cursor_block = cursor.block.min(self.blocks.len().saturating_sub(1));
        let Some(anchor_byte) = self
            .blocks
            .get(anchor_block)
            .map(|b| b.line.value().floor_char_boundary(anchor.byte))
        else {
            return;
        };
        let Some(cursor_byte) = self
            .blocks
            .get(cursor_block)
            .map(|b| b.line.value().floor_char_boundary(cursor.byte))
        else {
            return;
        };
        self.focus_block(cursor_block, CursorPlacement::Byte(cursor_byte));
        self.paint_cross_block_selection(anchor_block, anchor_byte, cursor_block, cursor_byte);
    }

    /// Retain the exact batch after a structural command, including empty rows.
    /// Missing or interleaved IDs refuse restoration instead of selecting peers.
    pub fn retain_block_selection(&mut self, ids: Vec<String>, reverse: bool) {
        self.retained_block_selection = Some(RetainedBlockSelection::Active(ids.clone()));
        let Some(range) = self.selected_block_range() else {
            self.reconcile_block_selection();
            return;
        };
        let start = DocumentPosition {
            block: range.start,
            byte: 0,
        };
        let last = range.end.saturating_sub(1);
        let end = DocumentPosition {
            block: last,
            byte: self.blocks[last].text().len(),
        };
        let (anchor, cursor) = if reverse { (end, start) } else { (start, end) };
        self.select_document_range(anchor, cursor);
        self.retained_block_selection = Some(RetainedBlockSelection::Active(ids));
    }

    /// Peer structural edits must not turn the old batch into a text selection
    /// that silently includes new content. Keep a refused marker until the next
    /// explicit caret/selection gesture, so a pending command cannot fall back
    /// to moving just the caret's block.
    fn reconcile_block_selection(&mut self) {
        if self.retained_block_selection.is_some() && self.selected_block_range().is_none() {
            self.clear_document_selection();
            // Presentation may already have discarded the old drag bounds.
            // Collapse every retained line's paint, including nonfocused rows.
            for block in &mut self.blocks {
                if block.line.selection_range().is_some() {
                    let cursor = block.line.cursor_pos();
                    block.line.inner_mut().set_selection(cursor, cursor);
                }
            }
            self.retained_block_selection = Some(RetainedBlockSelection::Refused);
        }
    }

    /// Clear selection and its keyboard/drag anchors without changing text.
    pub fn clear_document_selection(&mut self) {
        self.clear_cross_block_selection();
        self.cross_block_drag_start = None;
        self.kbd_sel_anchor = None;
        if let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index)) {
            let cursor = block.line.cursor_pos();
            block.line.inner_mut().set_selection(cursor, cursor);
        }
        self.dirty = true;
    }

    /// Plain-text serialization uses a newline for every crossed block
    /// boundary, matching browser/contenteditable clipboard behavior.
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.document_selection?;
        let (start, end) = selection.ordered();
        if end.block >= self.blocks.len() {
            return None;
        }
        let mut out = String::new();
        for block_idx in start.block..=end.block {
            if block_idx > start.block {
                out.push('\n');
            }
            let Some(block) = self.blocks.get(block_idx) else {
                continue;
            };
            let text = block.line.value();
            let from = if block_idx == start.block {
                text.floor_char_boundary(start.byte)
            } else {
                0
            };
            let to = if block_idx == end.block {
                text.floor_char_boundary(end.byte)
            } else {
                text.len()
            };
            if from <= to
                && let Some(slice) = text.get(from..to)
            {
                out.push_str(slice);
            }
        }
        Some(out)
    }

    /// Replace a cross-block selection with normalized plain text. All local
    /// mutations happen in this call, so the host commits one Automerge undo
    /// boundary for the originating key/IME event.
    pub fn replace_document_selection(&mut self, replacement: &str) -> bool {
        self.begin_edit();
        let Some(selection) = self.document_selection else {
            return false;
        };
        let (start, end) = selection.ordered();
        if (start == end && replacement.is_empty()) || end.block >= self.blocks.len() {
            return false;
        }
        // Every index below is `start.block`, `end.block`, or derived from
        // them plus `inserted.len()`; the bounds check above proves
        // `start.block <= end.block < self.blocks.len()`, and `drain`/
        // `insert` only ever touch the tail from `start.block` onward, so
        // `start.block` and the later `last_idx` stay valid throughout.
        // Still routed through `.get()`/`.get_mut()` rather than `[]` —
        // this widget must not panic on a text edit even if that invariant
        // is ever violated by a future change.
        let (Some(start_text), Some(end_text)) = (
            self.blocks
                .get(start.block)
                .map(|b| b.line.value().to_string()),
            self.blocks
                .get(end.block)
                .map(|b| b.line.value().to_string()),
        ) else {
            return false;
        };
        let start_byte = start_text.floor_char_boundary(start.byte);
        let end_byte = end_text.floor_char_boundary(end.byte);
        let prefix_marks = self
            .blocks
            .get(start.block)
            .map(|b| marks_before(b.line.marks(), start_byte))
            .unwrap_or_default();
        let suffix_marks = self
            .blocks
            .get(end.block)
            .map(|b| marks_after(b.line.marks(), end_byte))
            .unwrap_or_default();
        let prefix = start_text.get(..start_byte).unwrap_or_default();
        let suffix = end_text.get(end_byte..).unwrap_or_default();
        let cleaned = normalize_replacement(replacement);
        let inserted: Vec<&str> = cleaned.split('\n').collect();

        let Some(first_kind) = self.blocks.get(start.block).map(|b| b.kind.clone()) else {
            return false;
        };
        let first_soft_break = self.blocks.get(start.block).is_some_and(|b| b.soft_break);
        // `str::split` on any string (including empty) always yields at
        // least one item, so `inserted` is never empty.
        let first_body = format!("{prefix}{}", inserted.first().copied().unwrap_or(""));
        if let Some(b) = self.blocks.get_mut(start.block) {
            b.line.set_text(&first_body);
            b.line.set_marks(prefix_marks.clone());
            b.soft_break = first_soft_break;
        }

        if start.block < end.block {
            self.blocks.drain(start.block.saturating_add(1)..=end.block);
        }
        let (caret_block, caret_byte) = if inserted.len() == 1 {
            let caret = first_body.len();
            let mut marks = prefix_marks;
            for span in suffix_marks {
                crate::widget::marks::add_mark_to_range(
                    &mut marks,
                    span.range.start.saturating_add(caret)..span.range.end.saturating_add(caret),
                    span.mark,
                );
            }
            if let Some(b) = self.blocks.get_mut(start.block) {
                b.line.set_text(&format!("{first_body}{suffix}"));
                b.line.set_marks(marks);
            }
            (start.block, caret)
        } else {
            self.insert_replacement_tail(start.block, &first_kind, &inserted, suffix, suffix_marks)
        };

        self.retained_block_selection = None;
        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.structure_dirty = true;
        self.edit_pending = true;
        self.commit_pending = true;
        self.dirty = true;
        self.focus_block(caret_block, CursorPlacement::Byte(caret_byte));
        true
    }

    /// Inserts one new block per remaining line in `inserted` (index 0 was
    /// already folded into the start block by the caller), appending
    /// `suffix` to the last one. Returns `(caret_block, caret_byte)` for
    /// where the cursor should land right after the pasted text.
    #[expect(
        clippy::indexing_slicing,
        reason = "the caller supplies a validated start block and this helper preserves its style"
    )]
    fn insert_replacement_tail(
        &mut self,
        start_block: usize,
        first_kind: &BlockKind,
        inserted: &[&str],
        suffix: &str,
        suffix_marks: Vec<MarkSpan>,
    ) -> (usize, usize) {
        for (offset, line) in inserted.iter().enumerate().skip(1) {
            let body = if offset.saturating_add(1) == inserted.len() {
                format!("{line}{suffix}")
            } else {
                line.to_string()
            };
            let mut block = continuation_block(first_kind, &body);
            self.blocks[start_block]
                .code_group
                .clone_into(&mut block.code_group);
            block.set_indent(self.blocks[start_block].indent());
            block.line.set_local_author(self.local_author.clone());
            self.blocks
                .insert(start_block.saturating_add(offset), block);
        }
        let last_idx = start_block.saturating_add(inserted.len().saturating_sub(1));
        let last_insert_len = inserted.last().map_or(0, |line| line.len());
        let shifted_suffix_marks = suffix_marks
            .into_iter()
            .map(|span| {
                MarkSpan::new(
                    span.range.start.saturating_add(last_insert_len)
                        ..span.range.end.saturating_add(last_insert_len),
                    span.mark,
                )
            })
            .collect();
        if let Some(b) = self.blocks.get_mut(last_idx) {
            b.line.set_marks(shifted_suffix_marks);
        }
        (last_idx, last_insert_len)
    }

    fn handle_document_selection_event(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, NamedKey};

        if self.document_selection.is_none() {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                if ctrl {
                    return self
                        .handle_document_selection_ctrl_key(logical_key)
                        .unwrap_or(false);
                }
                if !modifiers.state().shift_key() {
                    // Guaranteed `Some`: this handler returned early above
                    // if `document_selection` was `None`, and nothing
                    // between there and here clears it when `ctrl` is
                    // false.
                    let Some(selection) = self.document_selection else {
                        return false;
                    };
                    let (start, end) = selection.ordered();
                    let collapse = match logical_key {
                        Key::Named(
                            NamedKey::ArrowLeft
                            | NamedKey::ArrowUp
                            | NamedKey::Home
                            | NamedKey::PageUp,
                        ) => Some(start),
                        Key::Named(
                            NamedKey::ArrowRight
                            | NamedKey::ArrowDown
                            | NamedKey::End
                            | NamedKey::PageDown
                            | NamedKey::Escape,
                        ) => Some(end),
                        _ => None,
                    };
                    if let Some(position) = collapse {
                        self.clear_cross_block_selection();
                        self.focus_block(position.block, CursorPlacement::Byte(position.byte));
                        if *logical_key == Key::Named(NamedKey::Escape) {
                            self.tab_navigation = true;
                        }
                        return true;
                    }
                }
                match logical_key {
                    Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                        self.replace_document_selection("")
                    }
                    Key::Named(NamedKey::Enter) => self.replace_document_selection("\n"),
                    Key::Named(NamedKey::Space) => self.replace_document_selection(" "),
                    Key::Character(c) => {
                        let inserted = text.as_ref().map_or(c.as_str(), |t| t.as_str());
                        if inserted.chars().all(char::is_control) {
                            false
                        } else {
                            self.replace_document_selection(inserted)
                        }
                    }
                    _ => false,
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if text.chars().all(char::is_control) {
                    false
                } else {
                    // This path owns the replacement, so TextInput's normal
                    // Commit handler will not run to retire its preedit.
                    if let Some(index) = self.focused_index()
                        && let Some(block) = self.blocks.get_mut(index)
                    {
                        block.line.inner_mut().finish_preedit();
                    }
                    self.replace_document_selection(text)
                }
            }
            _ => false,
        }
    }

    /// Ctrl/Cmd shortcuts available while a document selection is active:
    /// select-all, copy, cut, paste. `None` when `logical_key` isn't one
    /// of these, so the caller can fall through to its own handling.
    fn handle_document_selection_ctrl_key(
        &mut self,
        logical_key: &winit::keyboard::Key,
    ) -> Option<bool> {
        use winit::keyboard::Key;
        match logical_key {
            Key::Character(c) if c.eq_ignore_ascii_case("a") => {
                let end_block = self.blocks.len().saturating_sub(1);
                let end_byte = self
                    .blocks
                    .get(end_block)
                    .map_or(0, |b| b.line.value().len());
                self.select_document_range(
                    DocumentPosition { block: 0, byte: 0 },
                    DocumentPosition {
                        block: end_block,
                        byte: end_byte,
                    },
                );
                Some(true)
            }
            Key::Character(c) if c.eq_ignore_ascii_case("c") => {
                if let Some(selected) = self.selected_text() {
                    clipboard_copy(&selected);
                }
                Some(true)
            }
            Key::Character(c) if c.eq_ignore_ascii_case("x") => {
                if self
                    .selected_text()
                    .is_some_and(|selected| clipboard_copy_accepted(&selected))
                {
                    self.replace_document_selection("");
                }
                Some(true)
            }
            Key::Character(c) if c.eq_ignore_ascii_case("v") => {
                Some(clipboard_paste().is_none_or(|paste| self.replace_document_selection(&paste)))
            }
            _ => None,
        }
    }

    /// Clamps to the first/last block when `y` is above/below the
    /// editor, so a drag extends cleanly past either edge.
    fn block_at_y(&self, y: f64) -> Option<usize> {
        if self.blocks.is_empty() {
            return None;
        }
        for i in 0..self.blocks.len() {
            let Some(r) = self.block_paint_rect(i) else {
                continue;
            };
            if y <= r.y1 {
                return Some(i);
            }
        }
        if self
            .blocks
            .first()
            .is_some_and(|b| y < b.line.get_rect().y0)
        {
            Some(0)
        } else {
            Some(self.blocks.len().saturating_sub(1))
        }
    }

    /// Blocks previously dragged through but no longer in range get
    /// cleared, so drag-down-then-back-up leaves the right state.
    fn paint_cross_block_selection(
        &mut self,
        start_block: usize,
        start_byte: usize,
        end_block: usize,
        end_byte: usize,
    ) {
        self.retained_block_selection = None;
        self.document_selection =
            ((start_block, start_byte) != (end_block, end_byte)).then_some(DocumentSelection {
                anchor: DocumentPosition {
                    block: start_block,
                    byte: start_byte,
                },
                cursor: DocumentPosition {
                    block: end_block,
                    byte: end_byte,
                },
            });
        let (lo_block, hi_block) = if start_block <= end_block {
            (start_block, end_block)
        } else {
            (end_block, start_block)
        };
        if let Some((prev_lo, prev_hi)) = self.cross_block_drag_touched {
            for i in prev_lo..=prev_hi {
                if (i < lo_block || i > hi_block)
                    && let Some(b) = self.blocks.get_mut(i)
                {
                    let pos = b.line.inner_mut().cursor_pos();
                    b.line.inner_mut().set_selection(pos, pos);
                }
            }
        }
        self.cross_block_drag_touched = Some((lo_block, hi_block));
        if start_block == end_block {
            if let Some(b) = self.blocks.get_mut(start_block) {
                b.line.inner_mut().set_selection(start_byte, end_byte);
            }
            return;
        }
        let forward = start_block < end_block;
        for i in lo_block..=hi_block {
            let Some(len) = self.blocks.get(i).map(|b| b.line.value().len()) else {
                continue;
            };
            let (a, c) = if i == start_block {
                if forward {
                    (start_byte, len)
                } else {
                    (0, start_byte)
                }
            } else if i == end_block {
                if forward {
                    (0, end_byte)
                } else {
                    (end_byte, len)
                }
            } else if forward {
                (0, len)
            } else {
                (len, 0)
            };
            if let Some(b) = self.blocks.get_mut(i) {
                b.line.inner_mut().set_selection(a, c);
            }
        }
    }

    /// `dir` is `-1` up, `+1` down. The active end lands where a plain
    /// arrow would jump to, so the growing selection covers exactly the
    /// "jumped" text. Reuses the mouse drag's band-painting so a
    /// retreating extend clears the blocks it left.
    fn extend_block_selection(&mut self, from_block: usize, caret: usize, dir: i32) {
        let anchor = *self.kbd_sel_anchor.get_or_insert((from_block, caret));
        let target_block = if dir < 0 {
            if from_block == 0 {
                return;
            }
            from_block.saturating_sub(1)
        } else {
            let next = from_block.saturating_add(1);
            if next >= self.blocks.len() {
                return;
            }
            next
        };
        let active_byte = if dir < 0 {
            self.blocks
                .get(target_block)
                .map_or(0, |b| b.line.value().len())
        } else {
            0
        };
        self.focus_block(target_block, CursorPlacement::Byte(active_byte));
        self.paint_cross_block_selection(anchor.0, anchor.1, target_block, active_byte);
        self.dirty = true;
    }

    /// Called on the next fresh click so the new selection doesn't OR
    /// with the old one.
    fn clear_cross_block_selection(&mut self) {
        self.retained_block_selection = None;
        self.document_selection = None;
        if let Some((lo, hi)) = self.cross_block_drag_touched.take() {
            for i in lo..=hi {
                if let Some(b) = self.blocks.get_mut(i) {
                    let pos = b.line.inner_mut().cursor_pos();
                    b.line.inner_mut().set_selection(pos, pos);
                }
            }
        }
    }
}

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Affine composes two 2D transforms via f64 \
              matrix math; it never panics or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "text layout accepts f32 widths; these values are finite viewport-sized geometry, so f64 precision beyond f32 subpixels is immaterial"
)]
fn text_layout_width(width: f64) -> f32 {
    width as f32
}

/// See [`translated`]: `Affine * Point` is the same plain f64 math.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Point maps a point through a 2D transform via \
              f64 matrix math; it never panics or overflows"
)]
fn transformed(transform: Affine, p: Point) -> Point {
    transform * p
}

/// Per-frame drawing context shared by every block in
/// [`BlockEditor::draw`] — bundled into one struct so the per-block
/// helpers below stay well under clippy's argument-count limit.
struct BlockDrawCtx<'a> {
    scene: &'a mut Scene,
    text_renderer: &'a mut TextRenderer,
    transform: Affine,
    theme: &'a Theme,
    presence: &'a PresenceCtx,
    line_rects: &'a [Rect],
    ordinals: &'a [u32],
    table_previews: &'a [Option<Rect>],
    image_previews: &'a [Option<Rect>],
    image_attachments: &'a HashMap<String, ImageAttachment>,
    image_store: Option<&'a crate::ImageStore>,
    /// While `Some`, a table draws each cell's text (each row's card, when
    /// its cells stack) into a scene of its own here, with where it paints,
    /// instead of into `scene`: the editor damages a cell, not the table.
    table_cells: Option<Vec<(Rect, Scene)>>,
    /// While set, the first block of a code run paints the run's ground into
    /// [`Self::code_ground`] with its bounds, instead of into `scene`: a key
    /// in that line damages the line, not the run.
    collect_code_ground: bool,
    code_ground: Option<(Rect, Scene)>,
}

impl BlockDrawCtx<'_> {
    /// Run `draw` into a scene of its own when collecting table cells.
    fn table_cell(&mut self, painted: Rect, draw: impl FnOnce(&mut Self)) {
        if self.table_cells.is_none() {
            draw(self);
            return;
        }
        let outer = std::mem::take(self.scene);
        draw(self);
        let drawn = std::mem::replace(self.scene, outer);
        if let Some(cells) = self.table_cells.as_mut() {
            cells.push((painted, drawn));
        }
    }
}

/// A tracked table block's paint, split where a change can be damaged on
/// its own: [`BlockEditor::draw`] makes each a part.
struct TableParts {
    /// Its source line (a Markdown table being edited as text), frame,
    /// header band and grid.
    ground: Scene,
    /// Each cell's text (each row's card, when the cells stack), with where
    /// it paints.
    cells: Vec<(Rect, Scene)>,
    /// The focused cell's editor.
    focus: Scene,
    /// The mode and add row/column buttons above it.
    controls: Scene,
    /// Where the controls stand.
    controls_paint: Option<Rect>,
}

/// Formula-run state threaded through [`draw_block`]'s `CodeBlock` arm —
/// kept out of [`BlockDrawCtx`] since only that one arm needs it, and
/// `visual_formulas` needs a mutable borrow the rest of `BlockDrawCtx`
/// doesn't.
struct FormulaDrawCtx<'a> {
    runs: &'a [Option<FormulaRunPreview>],
    code_runs: &'a [Option<usize>],
    chrome: &'a [Option<FormulaChrome>],
    preview_rects: &'a [Option<Rect>],
    visual_formulas: &'a mut HashMap<usize, VisualFormulaState>,
    focus: Option<usize>,
}

#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "table painting mirrors the cohesive measurement pass and narrows finite pixel widths for text"
)]
fn draw_markdown_table(ctx: &mut BlockDrawCtx<'_>, rect: Rect, source: &str) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    let layout = markdown_table_layout(source, rect.width() as f32, ctx.text_renderer, ctx.theme);
    if layout.rows.is_empty() {
        return;
    }
    let scale = f64::from(ctx.theme.text_scale);
    if let Some(labels) = &layout.stacked_labels {
        let mut row_y = rect.y0;
        for (row_index, row) in layout.rows.iter().enumerate() {
            let height = layout.row_heights.get(row_index).copied().unwrap_or(0.0);
            let card = Rect::new(rect.x0, row_y, rect.x1, row_y + height - 8.0 * scale);
            // A row is one card: its stroke reaches half a hairline past it.
            ctx.table_cell(card.inflate(scale, scale), |ctx| {
                let shape = vello::kurbo::RoundedRect::from_rect(card, 6.0 * scale);
                ctx.scene.fill(
                    vello::peniko::Fill::NonZero,
                    ctx.transform,
                    ctx.theme.tokens.colors.surfaces.raised,
                    None,
                    &shape,
                );
                ctx.scene.stroke(
                    &vello::kurbo::Stroke::new(scale),
                    ctx.transform,
                    crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.raised),
                    None,
                    &shape,
                );
                let mut y = row_y + 8.0 * scale;
                for (column, value) in row.iter().enumerate() {
                    let x = rect.x0 + layout.padding_x;
                    let width =
                        Some((layout.column_width - 2.0 * layout.padding_x).max(1.0) as f32);
                    if let Some(label) = labels.get(column) {
                        ctx.text_renderer.draw_style(
                            ctx.scene,
                            label,
                            ctx.theme.tokens.colors.text_primary,
                            translated(ctx.transform, x, y),
                            width,
                            &TextStyle::new(14.0).with_weight(crate::text_render::TextWeight::Bold),
                        );
                    }
                    let label_height = layout.label_heights.get(column).copied().unwrap_or(0.0);
                    ctx.text_renderer.draw_style(
                        ctx.scene,
                        value,
                        ctx.theme.tokens.colors.text_primary,
                        translated(ctx.transform, x, y + label_height + 4.0 * scale),
                        width,
                        &TextStyle::new(14.0),
                    );
                    y += layout
                        .cell_heights
                        .get(row_index)
                        .and_then(|heights| heights.get(column))
                        .copied()
                        .unwrap_or(0.0);
                }
            });
            row_y += height;
        }
        return;
    }
    let column_width = layout.column_width;
    let bottom = rect.y0 + layout.height();
    let table_rect = Rect::new(rect.x0, rect.y0, rect.x1, bottom);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.surfaces.raised,
        None,
        &vello::kurbo::RoundedRect::from_rect(table_rect, 6.0 * scale),
    );
    let header = Rect::new(
        table_rect.x0,
        table_rect.y0,
        table_rect.x1,
        table_rect.y0 + layout.row_heights.first().copied().unwrap_or(0.0),
    );
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.lift(0.25),
        None,
        &header,
    );
    let stroke = vello::kurbo::Stroke::new(1.0 * scale);
    for column in 1..layout.columns {
        let x = table_rect.x0 + column_width * column as f64;
        ctx.scene.stroke(
            &stroke,
            ctx.transform,
            crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.base),
            None,
            &vello::kurbo::Line::new((x, table_rect.y0), (x, table_rect.y1)),
        );
    }
    let mut y = table_rect.y0;
    for height in layout
        .row_heights
        .iter()
        .take(layout.rows.len().saturating_sub(1))
    {
        y += height;
        ctx.scene.stroke(
            &stroke,
            ctx.transform,
            crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.base),
            None,
            &vello::kurbo::Line::new((table_rect.x0, y), (table_rect.x1, y)),
        );
    }
    let mut row_y = table_rect.y0;
    for (row_index, (row, height)) in layout.rows.iter().zip(&layout.row_heights).enumerate() {
        for (column_index, value) in row.iter().enumerate() {
            let left = table_rect.x0 + column_width * column_index as f64;
            let x = left + layout.padding_x;
            let y = row_y + 8.0 * scale;
            let cell = Rect::new(left, row_y, left + column_width, row_y + height);
            ctx.table_cell(cell, |ctx| {
                ctx.text_renderer.draw_style(
                    ctx.scene,
                    value,
                    ctx.theme.tokens.colors.text_primary,
                    translated(ctx.transform, x, y),
                    Some((column_width - 2.0 * layout.padding_x).max(1.0) as f32),
                    &TextStyle::new(14.0).with_weight(if row_index == 0 {
                        crate::text_render::TextWeight::Bold
                    } else {
                        crate::text_render::TextWeight::Regular
                    }),
                );
            });
        }
        row_y += height;
    }
}

fn draw_markdown_image(ctx: &mut BlockDrawCtx<'_>, rect: Rect, source: &str) {
    let Some((alt, destination)) = standalone_markdown_image(source) else {
        return;
    };
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    let scale = f64::from(ctx.theme.text_scale);
    let radius = 8.0 * scale;
    let shape = vello::kurbo::RoundedRect::from_rect(rect, radius);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.surfaces.raised,
        None,
        &shape,
    );
    if let Some(image) = ctx.image_store.and_then(|store| store.get(destination)) {
        let ratio = (rect.width() / f64::from(image.width).max(1.0))
            .min(rect.height() / f64::from(image.height).max(1.0));
        let width = f64::from(image.width) * ratio;
        let height = f64::from(image.height) * ratio;
        let fitted = Rect::from_center_size(rect.center(), (width, height));
        crate::image_store::paint_image_cover(ctx.scene, ctx.transform, fitted, &image, 0.0);
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(scale),
            ctx.transform,
            crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.base),
            None,
            &shape,
        );
        return;
    }
    ctx.scene.stroke(
        &vello::kurbo::Stroke::new(scale),
        ctx.transform,
        crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.base),
        None,
        &shape,
    );
    let icon_rect = Rect::new(
        rect.x0 + 18.0 * scale,
        rect.y0 + 18.0 * scale,
        rect.x0 + 46.0 * scale,
        rect.y0 + 46.0 * scale,
    );
    draw_icon(
        ctx.scene,
        ctx.transform,
        icon_rect,
        Icon::Image,
        ctx.theme.tokens.colors.text_dim,
        1.5 * scale,
    );
    let label = if alt.is_empty() { "Image" } else { alt };
    ctx.text_renderer.draw_style(
        ctx.scene,
        label,
        ctx.theme.tokens.colors.text_primary,
        translated(
            ctx.transform,
            rect.x0 + 58.0 * scale,
            rect.y0 + 18.0 * scale,
        ),
        Some(text_layout_width((rect.width() - 76.0 * scale).max(1.0))),
        &TextStyle::new(16.0).with_weight(crate::text_render::TextWeight::Medium),
    );
    ctx.text_renderer.draw_text(
        ctx.scene,
        destination,
        13.0,
        ctx.theme.tokens.colors.text_dim,
        translated(
            ctx.transform,
            rect.x0 + 58.0 * scale,
            rect.y0 + 42.0 * scale,
        ),
        Some(text_layout_width((rect.width() - 76.0 * scale).max(1.0))),
    );
}

/// One block's prefix glyph (bullet/ordinal/checkbox/code bar/divider
/// rule) plus its text line. A free function, not a method — it only
/// ever needs `b`, not `self`, which keeps `draw`'s per-block loop out
/// of the borrow checker's way.
#[expect(
    clippy::too_many_lines,
    reason = "block painting is a cohesive exhaustive dispatch over all block kinds"
)]
fn draw_block(
    b: &mut Block,
    idx: usize,
    ctx: &mut BlockDrawCtx<'_>,
    formulas: &mut FormulaDrawCtx<'_>,
) {
    let line_rect = b.line.get_rect();
    if transclusion_target(b).is_some() {
        return;
    }
    let scale = f64::from(ctx.theme.text_scale);
    let indent_px = f64::from(LIST_INDENT_STEP_PX) * f64::from(b.indent()) * scale;
    let gutter = prefix_gutter_for(&b.kind) * scale;
    let pad = b.line.text_inset(ctx.theme);
    // Every block starts at the one content edge, where a paragraph's text
    // starts: a prefix glyph's box, a quote's or a code block's ground.
    let prefix_x0 = content_edge(line_rect, gutter, pad);

    match &b.kind {
        BlockKind::Paragraph => {
            if let Some(Some(preview)) = ctx.image_previews.get(idx).copied() {
                let image = ctx.image_attachments.get(&b.id);
                let source = image
                    .map_or_else(|| b.text(), |image| image.source.as_str())
                    .to_string();
                let editing = image.is_some_and(|image| image.editing);
                draw_markdown_image(ctx, preview, &source);
                if !editing {
                    return;
                }
            }
        }
        BlockKind::Heading(_) => {}
        BlockKind::Quote => {
            // A quote is a quiet rule at the content edge and the quoted
            // words in secondary ink: no ground, no bright bar.
            let rule_w = f64::from(ctx.theme.tokens.spacing.s1) / 2.0;
            let rule = vello::kurbo::RoundedRect::from_rect(
                Rect::new(prefix_x0, line_rect.y0, prefix_x0 + rule_w, line_rect.y1),
                rule_w / 2.0,
            );
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.content),
                None,
                &rule,
            );
            let mut quiet = ctx.theme.clone();
            quiet.text_primary = quiet.text_dim;
            quiet.tokens.colors.text_primary = quiet.tokens.colors.text_dim;
            b.line.draw(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                &quiet,
                ctx.presence,
            );
            return;
        }

        BlockKind::Bullet { .. } => {
            draw_bullet_prefix(line_rect, prefix_x0, ctx);
            let _ = indent_px;
        }
        BlockKind::Numbered { .. } => {
            let text_rect = Rect::new(line_rect.x0 + pad, line_rect.y0, line_rect.x1, line_rect.y1);
            draw_numbered_prefix(idx, text_rect, prefix_x0, b.kind.font_size(), ctx);
            let _ = indent_px;
        }
        BlockKind::Checklist { done, .. } => {
            draw_checklist_prefix(*done, checklist_box(line_rect, gutter, pad, scale), ctx);
        }
        BlockKind::CodeBlock { .. } => {
            draw_code_block_prefix(idx, line_rect, line_rect.x0 - prefix_x0, ctx, formulas);
            // A block's own line is skipped while its visual formula
            // editor stands in for it — the raw TeX/Typst source
            // behind that editor is still there, just not painted.
            let descriptor = formula_descriptor(&b.kind);
            if descriptor.is_none_or(|value| value.mode == crate::math::FormulaMode::Source) {
                b.line.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text_renderer,
                    ctx.theme,
                    ctx.presence,
                );
            }
            draw_formula_run(idx, ctx, formulas);
            return;
        }
        BlockKind::Table { visual } => {
            if !visual {
                b.line.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text_renderer,
                    ctx.theme,
                    ctx.presence,
                );
            }
            if let Some(Some(preview)) = ctx.table_previews.get(idx).copied() {
                draw_markdown_table(ctx, preview, b.text());
            }
            return;
        }
        BlockKind::Divider => {
            let cy = f64::midpoint(line_rect.y0, line_rect.y1);
            let rule = Rect::new(line_rect.x0, cy - 0.5, line_rect.x1, cy + 0.5);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                crate::surface::edge_on(ctx.theme.tokens.colors.surfaces.base),
                None,
                &rule,
            );
            // Never focusable/editable — no inner line to paint.
            return;
        }
    }
    b.line.draw(
        ctx.scene,
        ctx.transform,
        ctx.text_renderer,
        ctx.theme,
        ctx.presence,
    );
}

/// Whether a change to a block of `kind` can be damaged where the block
/// paints: its row of the editor, the whole ground of the run a code block
/// starts, a table's cells. Not a formula, whose preview and visual editor
/// reach past its source lines.
fn damages_where_it_paints(kind: &BlockKind) -> bool {
    match kind {
        BlockKind::CodeBlock { .. } => formula_descriptor(kind).is_none(),
        _ => true,
    }
}

fn transclusion_target(block: &Block) -> Option<PageId> {
    let text_len = block.text().len();
    block.line.marks().iter().find_map(|span| match &span.mark {
        Mark::Transclusion(id)
            if !id.is_nil() && span.range.start == 0 && span.range.end == text_len =>
        {
            Some(*id)
        }
        _ => None,
    })
}

fn transclusion_action_target(id: PageId) -> String {
    format!("notes.transclusion:{id}")
}

fn page_link_action_target(id: PageId) -> String {
    format!("notes.page-link:{id}")
}

fn external_link_action_target(url: &str) -> String {
    format!("notes.external-link:{url}")
}

fn draw_transclusion_card(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    preview: Option<&TransclusionPreview>,
) {
    let tokens = &theme.tokens;
    let card = vello::kurbo::RoundedRect::from_rect(
        rect.inflate(-1.0, -1.0),
        f64::from(tokens.radii.control),
    );
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        tokens.colors.surfaces.raised,
        None,
        &card,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        crate::surface::edge_on(tokens.colors.surfaces.raised),
        None,
        &card,
    );
    let (title, excerpt) = preview.map_or(
        ("Unavailable note", "The referenced note was removed."),
        |p| (p.title.as_str(), p.excerpt.as_str()),
    );
    // An embedded note is an object you open: a note glyph and its title on
    // the first line with a trailing chevron (the whole card opens it), its
    // excerpt as the secondary line. No "Embedded note · Open" chain.
    let pad = f64::from(tokens.spacing.s4);
    let gap = f64::from(tokens.spacing.s2);
    let icon_side = f64::from(tokens.metrics.icon_sm * theme.text_scale);
    let title_style = theme.text(crate::tokens::TextRole::Subheading);
    let excerpt_style = theme.text(crate::tokens::TextRole::Body);
    let ink = tokens.colors.on_surface(tokens.colors.surfaces.raised);
    let (_, title_h) = text_renderer.measure_style("Ag", None, &title_style);
    let title_h = f64::from(title_h);
    let title_y = rect.y0 + pad;
    let chevron_x = rect.x1 - pad - icon_side;
    let icon_rect = Rect::new(
        rect.x0 + pad,
        title_y + (title_h - icon_side) / 2.0,
        rect.x0 + pad + icon_side,
        title_y + (title_h + icon_side) / 2.0,
    );
    crate::icons::draw_icon(
        scene,
        transform,
        icon_rect,
        crate::icons::Icon::FileText,
        ink.ink_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    crate::icons::draw_icon(
        scene,
        transform,
        Rect::new(chevron_x, icon_rect.y0, chevron_x + icon_side, icon_rect.y1),
        crate::icons::Icon::ChevronRight,
        ink.ink_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    let title_x = icon_rect.x1 + gap;
    let title_w = (chevron_x - gap - title_x).max(1.0);
    let title = text_renderer.elide_to_width_style(title, title_w as f32, &title_style);
    text_renderer.draw_style(
        scene,
        &title,
        ink.ink,
        translated(transform, title_x, title_y),
        None,
        &title_style,
    );
    let excerpt_w = (rect.x1 - pad - title_x).max(1.0);
    let excerpt = text_renderer.elide_to_width_style(excerpt, excerpt_w as f32, &excerpt_style);
    text_renderer.draw_style(
        scene,
        &excerpt,
        ink.ink_dim,
        translated(
            transform,
            title_x,
            title_y + title_h + f64::from(tokens.spacing.s1),
        ),
        None,
        &excerpt_style,
    );
}

/// Prefixes belong to the first visual row, even when a block wraps.
fn first_row_center(rect: Rect, size: f32, scale: f64) -> f64 {
    rect.y0 + (8.0 + f64::from(size) * scale * 0.625).min(rect.height() / 2.0)
}

/// A filled disc, not a "•" glyph, reads the same across fonts.
fn draw_bullet_prefix(line_rect: Rect, prefix_x0: f64, ctx: &mut BlockDrawCtx<'_>) {
    let scale = f64::from(ctx.theme.text_scale);
    let cy = first_row_center(line_rect, 16.0, scale);
    // The disc sits in the same leading glyph box as a checklist's box.
    let cx = prefix_x0 + CHECKBOX_SIZE_PX * scale / 2.0;
    let radius = 3.0 * scale;
    let circle = vello::kurbo::Circle::new((cx, cy), radius);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.text_primary,
        None,
        &circle,
    );
}

/// Right-aligned so single- and double-digit periods line up.
fn draw_numbered_prefix(
    idx: usize,
    line_rect: Rect,
    prefix_x0: f64,
    font_size: f32,
    ctx: &mut BlockDrawCtx<'_>,
) {
    let num = ctx.ordinals.get(idx).copied().unwrap_or(1).max(1);
    let label = format!("{num}.");
    let (label_w, _) = ctx.text_renderer.measure(&label, font_size, None);
    let origin = numbered_prefix_origin(
        line_rect,
        prefix_x0,
        f64::from(label_w),
        font_size,
        ctx.theme.text_scale,
        ctx.theme.tokens.metrics.icon_label_gap,
    );
    ctx.text_renderer.draw_text(
        ctx.scene,
        &label,
        font_size,
        ctx.theme.tokens.colors.text_primary,
        translated(ctx.transform, origin.x, origin.y),
        None,
    );
}

/// `icon_label_gap` is the theme's `metrics.icon_label_gap` — the ordinal
/// keeps the glyph-to-label gap from the item text and, like the prefix
/// gutter it sits in, that gap follows the text scale.
fn numbered_prefix_origin(
    line_rect: Rect,
    prefix_x0: f64,
    label_width: f64,
    font_size: f32,
    text_scale: f32,
    icon_label_gap: f32,
) -> Point {
    // Matches RichTextLine's resolved shaped line height so the ordinal sits
    // on the item text's baseline at every accessibility scale.
    let line_height = f64::from(font_size * text_scale) * 1.25;
    let gap = f64::from(icon_label_gap) * f64::from(text_scale);
    Point::new(
        (line_rect.x0 - gap - label_width).max(prefix_x0),
        line_rect.y0 + (line_rect.height() - line_height) / 2.0,
    )
}

fn draw_checklist_prefix(done: bool, box_rect: Rect, ctx: &mut BlockDrawCtx<'_>) {
    // The toolkit's checkbox face: the inverted fill with its tick when
    // done, the raised box with its derived edge when not. A checklist item
    // is the same control as every other checkbox.
    crate::widget::Checkbox::paint(
        ctx.scene,
        ctx.transform,
        ctx.theme,
        &crate::widget::CheckboxFace::new(box_rect, done),
    );
}

/// The editor's one content edge for a block whose line rect is `line` and
/// whose prefix gutter is `gutter`: where a paragraph's text starts. Prefix
/// glyphs, quote and code grounds, embeds, tables and images all start here.
fn content_edge(line: Rect, gutter: f64, pad: f64) -> f64 {
    line.x0 - gutter + pad
}

/// A checklist item's box: the leading glyph box at the content edge, on
/// the first row's centre line. Paint and hit-testing share it.
fn checklist_box(line: Rect, gutter: f64, pad: f64, scale: f64) -> Rect {
    let side = CHECKBOX_SIZE_PX * scale;
    let x0 = content_edge(line, gutter, pad);
    let cy = first_row_center(line, 16.0, scale);
    Rect::new(x0, cy - side / 2.0, x0 + side, cy + side / 2.0)
}

/// Paint background + accent bar ONCE per contiguous run of code blocks,
/// spanning first to last line, not as per-line boxes.
fn draw_code_block_prefix(
    idx: usize,
    line_rect: Rect,
    gutter: f64,
    ctx: &mut BlockDrawCtx<'_>,
    formulas: &FormulaDrawCtx<'_>,
) {
    // A run breaks whenever the language changes, not just at the
    // code/non-code boundary — two adjacent fences in different
    // languages get their own background rather than merging into one.
    let is_run_start = idx
        .checked_sub(1)
        .is_none_or(|prev| formulas.code_runs.get(prev) != formulas.code_runs.get(idx));
    if !is_run_start {
        return;
    }
    // A formula run spans further than a same-language run alone when
    // its preview sits past the last source line of a shorter language
    // streak; fall back to the language-run end otherwise.
    let end = formulas.runs.get(idx).and_then(Option::as_ref).map_or_else(
        || {
            let mut end = idx;
            while formulas.code_runs.get(end.saturating_add(1)) == formulas.code_runs.get(idx) {
                end = end.saturating_add(1);
            }
            end
        },
        |formula| formula.end,
    );
    let top = formulas.chrome.get(idx).copied().flatten().map_or_else(
        || {
            line_rect.y0
                - CODE_PAD_Y
                - if formulas.runs.get(idx).is_none_or(Option::is_none) {
                    CODE_HEADER_HEIGHT * f64::from(ctx.theme.text_scale)
                } else {
                    0.0
                }
        },
        |chrome| chrome.syntax_badge.y0 - 3.0,
    );
    let bottom = formulas
        .preview_rects
        .get(end)
        .copied()
        .flatten()
        .map_or_else(
            || ctx.line_rects.get(end).map_or(line_rect.y1, |r| r.y1),
            |rect| rect.y1,
        )
        + CODE_PAD_Y;
    // `gutter` is the distance from the content edge to the line rect.
    let bg_rect = Rect::new(line_rect.x0 - gutter, top, line_rect.x1, bottom);
    let outer = ctx.collect_code_ground.then(|| std::mem::take(ctx.scene));
    let shape = vello::kurbo::RoundedRect::from_rect(bg_rect, 4.0);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.surfaces.raised,
        None,
        &shape,
    );
    let bar = Rect::new(bg_rect.x0, bg_rect.y0, bg_rect.x0 + 3.0, bg_rect.y1);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ctx.theme.tokens.colors.intent.inverted,
        None,
        &bar,
    );
    if let Some(outer) = outer {
        let ground = std::mem::replace(ctx.scene, outer);
        ctx.code_ground = Some((bg_rect, ground));
    }
}

/// Draws a code block's paired formula preview/chrome, if this block
/// starts a run [`formula_descriptor`] recognises. Part of
/// [`draw_block`]'s `CodeBlock` arm.
fn draw_formula_run(idx: usize, ctx: &mut BlockDrawCtx<'_>, formulas: &mut FormulaDrawCtx<'_>) {
    let Some(formula) = formulas.runs.get(idx).and_then(Option::as_ref) else {
        return;
    };
    let Some(preview_rect) = formulas.preview_rects.get(formula.end).copied().flatten() else {
        return;
    };
    if let Some(chrome) = formulas.chrome.get(idx).copied().flatten() {
        draw_formula_chrome(
            ctx.scene,
            ctx.transform,
            ctx.text_renderer,
            ctx.theme,
            chrome,
        );
    }
    match formula.descriptor.mode {
        crate::math::FormulaMode::Source => draw_formula_preview(
            ctx.scene,
            ctx.transform,
            ctx.text_renderer,
            ctx.theme,
            preview_rect,
            formula.descriptor.syntax,
            &formula.source,
        ),
        crate::math::FormulaMode::Visual => {
            let visual = formulas.visual_formulas.entry(idx).or_insert_with(|| {
                VisualFormulaState::new(formula.descriptor.syntax, &formula.source)
            });
            if formulas.focus != Some(idx) {
                visual.sync_source(formula.descriptor.syntax, &formula.source);
            }
            draw_visual_formula_editor(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                ctx.theme,
                preview_rect,
                visual,
                formulas.focus == Some(idx),
            );
        }
    }
}

/// How one block sits in the stack: its top margin, and the fixed height or
/// minimum its content height is clamped to.
#[derive(Clone, Copy, Debug)]
struct StackBox {
    margin_top: f32,
    fixed: Option<f32>,
    min: f32,
}

#[derive(Default)]
struct BlockStack {
    tops: Vec<f32>,
    heights: Vec<f32>,
    width: Option<f32>,
    total: f32,
    /// Whether any block's height comes from its content.
    measured: bool,
    /// The text renderer's shaping generation and scale the cached line
    /// heights were measured under; a font or locale swap re-measures all.
    shaping: Option<(std::sync::Arc<()>, u32)>,
}

/// The leaf that stands for the editor's block stack in the layout tree. A
/// separate allocation, so the host never mistakes it for a second editor at
/// the editor's address; it has no identity, focus or semantics of its own
/// and measures through its editor, which points it back at itself whenever
/// it builds the tree (the same lifetime every node-map pointer carries).
struct BlockStackLeaf {
    editor: *mut BlockEditor,
}

impl BlockStackLeaf {
    fn editor(&self) -> Option<&BlockEditor> {
        // SAFETY: set by the owning editor's `build_layout`; the host only
        // measures a tree while the editor that built it stays in place.
        unsafe { self.editor.as_ref() }
    }

    fn editor_mut(&mut self) -> Option<&mut BlockEditor> {
        // SAFETY: as above; measuring holds no other editor borrow.
        unsafe { self.editor.as_mut() }
    }
}

impl Widget for BlockStackLeaf {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        self.editor_mut().map_or(Size::ZERO, |editor| {
            editor.measure_stack(known, available, text_renderer)
        })
    }

    /// The host marks each node it must re-measure; the editor's own node has
    /// children, so its dirtiness alone would leave this leaf's cached size.
    fn layout_needs_measure(&self) -> bool {
        self.editor().is_some_and(Widget::layout_needs_measure)
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn name(&self) -> &'static str {
        "BlockStack"
    }

    fn get_rect(&self) -> Rect {
        self.editor().map_or(Rect::ZERO, |editor| editor.rect)
    }
}

/// Feeds formatted output into a hasher without building the string.
struct HashWriter(std::collections::hash_map::DefaultHasher);

impl std::fmt::Write for HashWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        std::hash::Hasher::write(&mut self.0, text.as_bytes());
        Ok(())
    }
}

impl BlockEditor {
    /// Hand every line one shared theme, replaced only when its content
    /// changes: an unchanged theme then costs each line a pointer compare.
    fn refresh_layout_theme(&mut self, theme: &Theme) {
        use std::fmt::Write as _;
        let mut hasher = HashWriter(std::collections::hash_map::DefaultHasher::new());
        let _ = write!(hasher, "{theme:?}");
        let fingerprint = std::hash::Hasher::finish(&hasher.0);
        if self.layout_theme_fingerprint != Some(fingerprint) {
            self.layout_theme_fingerprint = Some(fingerprint);
            self.layout_theme = std::sync::Arc::new(theme.clone());
        }
    }

    /// Measure the block stack. A query without a definite width asks for an
    /// intrinsic size: answer it from the last stack rather than reshaping
    /// every paragraph at a width it will never be painted at.
    fn measure_stack(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let definite = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            _ => None,
        });
        // 400 is the width a measured line assumes without a definite one.
        let width = match (definite, self.stack.width) {
            (Some(width), _) => width,
            (None, Some(_)) if self.stack.tops.len() == self.blocks.len() => {
                return Size {
                    width: if self.stack.measured { 400.0 } else { 0.0 },
                    height: self.stack.total,
                };
            }
            (None, _) => 400.0,
        };
        self.stack_blocks(width, Some(text_renderer));
        Size {
            width: if self.stack.measured { width } else { 0.0 },
            height: self.stack.total,
        }
    }

    /// Stack every block at `width`, the way a flex column would: each
    /// block's top is the running total plus its margin, and the total grows
    /// by margin plus height. A line whose content and layout inputs are
    /// unchanged reuses its cached height, so the cost of an edit does not
    /// grow with the document. Without a text renderer (a host that solved
    /// the tree without measuring), unmeasured content counts as empty.
    fn stack_blocks(&mut self, width: f32, mut text_renderer: Option<&mut TextRenderer>) {
        let stack = &mut self.stack;
        if let Some(text_renderer) = text_renderer.as_deref() {
            let shaping = (
                std::sync::Arc::clone(text_renderer.layout_epoch()),
                text_renderer.text_scale().to_bits(),
            );
            if stack.shaping.as_ref().is_none_or(|(epoch, scale)| {
                !std::sync::Arc::ptr_eq(epoch, &shaping.0) || *scale != shaping.1
            }) {
                for block in &mut self.blocks {
                    block.line.forget_stacked_height();
                }
                stack.shaping = Some(shaping);
            }
        }
        stack.tops.clear();
        stack.heights.clear();
        stack.measured = false;
        let mut total = 0.0_f32;
        let font_scale = self.font_scale;
        for (block, stack_box) in self.blocks.iter_mut().zip(&self.stack_boxes) {
            // A line the editor made since (a split, a paste) takes the
            // editor's body size too.
            // The kind's role sets the line's weight and leading (a heading
            // is semibold at its role's leading), its size the document's.
            let role = block.kind.text_role();
            if block.line.inner().text_role() != role {
                block.line.set_text_role(role);
            }
            block.line.set_size(block.kind.font_size() * font_scale);
            let content = match (
                block.line.stacked_height(width),
                text_renderer.as_deref_mut(),
            ) {
                (Some(height), _) => height,
                (None, Some(text_renderer)) => {
                    #[cfg(test)]
                    {
                        self.stack_measures = self.stack_measures.saturating_add(1);
                    }
                    block.line.measure_stacked(width, text_renderer)
                }
                (None, None) => 0.0,
            };
            stack.measured |= stack_box.fixed.is_none();
            let height = stack_box.fixed.unwrap_or(content).max(stack_box.min);
            stack.tops.push(total + stack_box.margin_top);
            total += stack_box.margin_top + height;
            stack.heights.push(height);
        }
        stack.total = total;
        stack.width = Some(width);
    }

    /// Each block's rectangle from the last stack, rounded exactly as Taffy
    /// would have rounded one child node per block: the location relative to
    /// the editor, and the height between the rounded absolute edges.
    fn stacked_block_rects(&self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Vec<Rect> {
        let Some(content) = self.content_node else {
            return Vec::new();
        };
        let Ok(content_layout) = taffy.layout(content) else {
            return Vec::new();
        };
        // `layout` hands out the unrounded record itself when rounding is off.
        let rounding = !std::ptr::eq(content_layout, taffy.unrounded_layout(content));
        let mut path = vec![node];
        while let Some(parent) = path.last().and_then(|&child| taffy.parent(child)) {
            path.push(parent);
        }
        let absolute_top = path.iter().rev().fold(0.0_f32, |top, &ancestor| {
            top + taffy.unrounded_layout(ancestor).location.y
        });
        let x0 = origin.x + f64::from(content_layout.location.x);
        let width = f64::from(content_layout.size.width);
        self.stack
            .tops
            .iter()
            .zip(&self.stack.heights)
            .map(|(&top, &height)| {
                let (top, height) = if rounding {
                    let edge = absolute_top + top;
                    (top.round(), (edge + height).round() - edge.round())
                } else {
                    (top, height)
                };
                let y0 = origin.y + f64::from(top);
                Rect::new(x0, y0, x0 + width, y0 + f64::from(height))
            })
            .collect()
    }
}

impl Widget for BlockEditor {
    crate::provides!(dyn crate::presence::capability::PublishesPresence);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, _) = crate::doc::document_ref(state, path);
        self.refresh_image_attachments();
        let Some(list) = self.blocks_list.clone() else {
            // Unbound (examples / tests): local-mode, nothing to sync.
            return;
        };

        // The owner dispatches each edit before this frame's sync; here the
        // editor only binds fresh text objects and adopts structural changes.
        let doc_len = state.length(&list);
        if self.canonical_existing && doc_len == 0 {
            if !self.canonical_empty {
                self.rebuild_from_doc(state, &list, 0);
                self.canonical_empty = true;
            }
            return;
        }
        self.canonical_empty = false;
        if doc_len != self.blocks.len() {
            self.rebuild_from_doc(state, &list, doc_len);
            return;
        }
        let drifted = (0..doc_len).any(|i| {
            let Some(block) = self.blocks.get(i) else {
                return true;
            };
            let Some(map) = list_map(state, &list, i) else {
                return true;
            };
            let id = read_block_id(state, &map);
            let kind = read_block_kind(state, &list, i);
            let text = doc_text_obj(state, &list, i)
                .and_then(|text| state.text(&text).ok())
                .unwrap_or_default();
            id.as_deref() != Some(block.id.as_str())
                || kind != kind_to_str(&block.kind)
                || !read_nesting(state, &map).is_ok_and(|depth| depth == block.nesting)
                || !read_code_group(state, &map).is_ok_and(|group| group == block.code_group)
                || text != block.text()
        });
        if drifted {
            self.rebuild_from_doc(state, &list, doc_len);
            return;
        }
        for (i, block) in self.blocks.iter_mut().enumerate() {
            let kind = kind_from_str(&read_block_kind(state, &list, i));
            block.id = read_block_id_at(state, &list, i, &kind);
            if block.line.bound_obj().is_none()
                && let Some(text) = doc_text_obj(state, &list, i)
            {
                block.line.bind_text(text);
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        reason = "the five-pass widget contract builds one index-aligned layout projection with finite pixel dimensions"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let unit;
        let theme = if self.content_sized {
            unit = theme.at_unit_text_scale();
            &unit
        } else {
            theme
        };
        self.text_scale = f64::from(theme.text_scale);
        self.handle = HandleMetrics::from_tokens(&theme.tokens.spacing, &theme.tokens.metrics);
        self.refresh_image_attachments();
        self.refresh_image_dimensions();
        self.table_editors.retain(|id, _| {
            self.blocks.iter().any(|block| {
                block.id == *id && matches!(block.kind, BlockKind::Table { visual: true })
            })
        });
        self.table_mode_controls.retain(|id, _| {
            self.blocks
                .iter()
                .any(|block| block.id == *id && matches!(block.kind, BlockKind::Table { .. }))
        });
        self.refresh_layout_theme(theme);
        // Lines within one authored fence stay together; distinct adjacent
        // fences need space outside both painted backgrounds.
        let is_code: Vec<bool> = self
            .blocks
            .iter()
            .map(|b| matches!(b.kind, BlockKind::CodeBlock { .. }))
            .collect();
        let is_paragraph: Vec<bool> = self
            .blocks
            .iter()
            .map(|block| matches!(block.kind, BlockKind::Paragraph))
            .collect();
        let continues_code: Vec<bool> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                index
                    .checked_sub(1)
                    .is_some_and(|previous| self.blocks[previous].same_code_group(block))
            })
            .collect();
        let is_formula_end = compute_is_formula_end(&self.blocks);
        let code_headers: Vec<f64> = (0..self.blocks.len())
            .map(|index| code_header_height(&self.blocks, index, self.text_scale))
            .collect();
        self.stack_boxes.clear();
        let embeds: Vec<bool> = self
            .blocks
            .iter()
            .map(|block| {
                transclusion_target(block).is_some()
                    || standalone_markdown_image(block.text()).is_some()
            })
            .collect();
        for (i, b) in self.blocks.iter_mut().enumerate() {
            let image = self.image_attachments.get(&b.id);
            let image_preview = image
                .and_then(|image| standalone_markdown_image(&image.source))
                .map(|(_, source)| self.image_dimensions.get(source).copied());
            b.line.set_image_preview(match image_preview {
                Some(dimensions) => super::rich_text_line::ImagePreview::Present {
                    dimensions,
                    editing: image.is_some_and(|image| image.editing),
                },
                None => super::rich_text_line::ImagePreview::Absent,
            });
            b.line.set_table_preview(match b.kind {
                BlockKind::Table { visual } => Some(visual),
                _ => None,
            });
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bounded on-screen block inset"
            )]
            b.line.set_layout_inset(
                (prefix_gutter_for(&b.kind) * self.text_scale) as f32
                    + LIST_INDENT_STEP_PX * f32::from(b.indent()) * theme.text_scale,
            );
            b.line.set_wrap(
                (self.wrap_lines
                    || image_preview.is_some()
                    || matches!(b.kind, BlockKind::Table { visual: false }))
                    && formula_descriptor(&b.kind).is_none()
                    && !matches!(b.kind, BlockKind::Table { visual: true }),
            );
            // A document paragraph already receives spacing from the block
            // stack. TextInput's normal multiline field padding would add
            // another 8 px above and below every line, making consecutive
            // prose read like separate form fields. Keep that padding for
            // richer controls and use the text line-height for compact prose.
            b.line.set_compact_wrapped(
                self.compact_paragraph_spacing && matches!(b.kind, BlockKind::Paragraph),
            );
            let header_height = code_headers[i] as f32;
            b.line.set_layout_header_height(header_height);
            b.line.prepare_layout(&self.layout_theme);
            let row_height = b.line.row_height(theme.text_scale);
            let mut stack_box = StackBox {
                margin_top: 0.0,
                fixed: (!b.line.measures_height()).then_some(row_height),
                min: row_height,
            };
            if header_height > 0.0 {
                let height = (b.kind.font_size() * theme.text_scale * 1.5)
                    .max(20.0 * theme.text_scale)
                    + header_height;
                if !self.wrap_lines {
                    stack_box.fixed = Some(height);
                }
                stack_box.min = height;
            }
            // Per-block top margin (not a uniform gap) so it can be zeroed
            // for soft-break continuations, the first block, and consecutive
            // code lines (one contiguous fenced unit).
            let continues_code_block = continues_code[i];
            let adjacent_fences =
                is_code[i] && i.checked_sub(1).is_some_and(|previous| is_code[previous]);
            let compact_paragraph_pair = self.compact_paragraph_spacing
                && is_paragraph[i]
                && i.checked_sub(1)
                    .is_some_and(|previous| is_paragraph[previous]);
            // An embed (an embedded note, an image) is an object in the
            // flow: it stands a group gap from the text on either side.
            let around_embed = embeds.get(i).copied().unwrap_or(false)
                || i.checked_sub(1)
                    .and_then(|previous| embeds.get(previous).copied())
                    .unwrap_or(false);
            stack_box.margin_top = if i == 0 || b.soft_break || continues_code_block {
                0.0
            } else if adjacent_fences || around_embed {
                theme.tokens.spacing.s4 * theme.text_scale
            } else if compact_paragraph_pair {
                theme.tokens.spacing.s1 * theme.text_scale
            } else {
                theme.tokens.spacing.s2 * theme.text_scale
            };
            if is_formula_end.get(i).copied().unwrap_or(false) {
                stack_box.min = if formula_descriptor(&b.kind)
                    .is_some_and(|descriptor| descriptor.mode == crate::math::FormulaMode::Visual)
                {
                    FORMULA_VISUAL_MIN_HEIGHT
                } else {
                    FORMULA_BLOCK_MIN_HEIGHT
                };
            }
            if transclusion_target(b).is_some() {
                stack_box.min = TRANSCLUSION_MIN_HEIGHT * theme.text_scale;
            }
            self.stack_boxes.push(stack_box);
        }
        // One measured leaf stands for the whole block stack. The editor
        // places its blocks itself from per-line cached heights, so an edit
        // re-measures only the lines it touched and the layout tree does not
        // grow with the document.
        let content = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        self.stack_leaf.editor = std::ptr::from_mut(self);
        let stack_leaf: &mut dyn Widget = self.stack_leaf.as_mut();
        node_map.insert(content, std::ptr::from_mut(stack_leaf));
        self.content_node = Some(content);
        let mut layout_children = vec![content];
        for block in &self.blocks {
            if let Some(image) = self.image_attachments.get_mut(&block.id) {
                let button_node = image.button_mut().build_layout(taffy, theme, node_map);
                let mut style = taffy.style(button_node).cloned().unwrap_or_default();
                style.position = taffy::Position::Absolute;
                let _ = taffy.set_style(button_node, style);
                image.node = Some(button_node);
                layout_children.push(button_node);
            }
        }
        // The lines' text inset is known once each line has its wrap mode.
        self.text_pad = self
            .blocks
            .first()
            .map_or(0.0, |block| block.line.text_inset(theme));
        let find_node = self.find_field.build_layout(taffy, theme, node_map);
        let mut find_style = taffy.style(find_node).cloned().unwrap_or_default();
        if !self.find_visible {
            find_style.display = taffy::Display::None;
        }
        find_style.position = taffy::Position::Absolute;
        find_style.inset.top = length(8.0_f32 * theme.text_scale);
        find_style.inset.right = length(108.0_f32 * theme.text_scale);
        find_style.size.width = length(248.0_f32 * theme.text_scale);
        find_style.size.height = length(36.0_f32 * theme.text_scale);
        let _ = taffy.set_style(find_node, find_style);
        self.find_node = Some(find_node);
        layout_children.push(find_node);
        if self.replace_enabled {
            self.replace_button
                .set_enabled(self.find_replace_request().is_some());
            let field = self.replace_field.build_layout(taffy, theme, node_map);
            let button = self.replace_button.build_layout(taffy, theme, node_map);
            self.replace_all_button.set_enabled(self.can_replace_all());
            let all = self.replace_all_button.build_layout(taffy, theme, node_map);
            for (node, top, right, width) in [
                (field, 48.0_f32, 108.0_f32, 248.0_f32),
                (button, 48.0_f32, 8.0_f32, 96.0_f32),
                (all, 88.0_f32, 8.0_f32, 120.0_f32),
            ] {
                let mut style = taffy.style(node).cloned().unwrap_or_default();
                if !self.find_visible {
                    style.display = taffy::Display::None;
                }
                style.position = taffy::Position::Absolute;
                style.inset.top = length(top * theme.text_scale);
                style.inset.right = length(right * theme.text_scale);
                style.size.width = length(width * theme.text_scale);
                style.size.height = length(36.0_f32 * theme.text_scale);
                let _ = taffy.set_style(node, style);
                layout_children.push(node);
            }
            self.replace_node = Some(field);
            self.replace_button_node = Some(button);
            self.replace_all_node = Some(all);
        }
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::Stretch),
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            // The drag handles stand in the editor's own leading column,
            // so a host leaves no gutter for them and they are never
            // clipped by the window edge at any text scale.
            padding: taffy::Rect {
                left: length(self.handle_column() as f32),
                right: length(0.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &layout_children, node_map, self)
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        reason = "the five-pass widget contract applies one index-aligned layout projection with finite pixel dimensions"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.table_relayout = false;
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(content) = self.content_node {
            let width = taffy.unrounded_layout(content).size.width;
            if self.stack.tops.len() != self.blocks.len()
                || self.stack.width.map(f32::to_bits) != Some(width.to_bits())
            {
                self.stack_blocks(width, None);
            }
        }
        let block_rects = self.stacked_block_rects(taffy, node, new_origin);
        self.formula_preview_rects = vec![None; self.blocks.len()];
        self.table_preview_rects = vec![None; self.blocks.len()];
        self.image_preview_rects = vec![None; self.blocks.len()];
        self.formula_chrome = vec![None; self.blocks.len()];
        self.transclusion_rects = vec![None; self.blocks.len()];
        let is_formula_start = compute_is_formula_start(&self.blocks);
        let is_formula_end = compute_is_formula_end(&self.blocks);
        let code_headers: Vec<f64> = (0..self.blocks.len())
            .map(|index| code_header_height(&self.blocks, index, self.text_scale))
            .collect();
        for (index, (b, block_rect)) in self.blocks.iter_mut().zip(block_rects).enumerate() {
            b.line.apply_layout_external(block_rect);
            // Where this line's text starts: the content edge every box on
            // the block (a table, an image, an embed) starts at too.
            let pad = b.line.text_inset(&self.layout_theme);
            // Shift prefixed kinds' text rect right of a reserved gutter
            // so the prefix glyph doesn't overlap editable text.
            let gutter = prefix_gutter_for(&b.kind) * self.text_scale;
            let indent_px =
                f64::from(LIST_INDENT_STEP_PX) * f64::from(b.indent()) * self.text_scale;
            // A formula's source line does not wrap, so it has no document
            // inset of its own; move it in by that inset so its card and
            // source start where a wrapping code block's do.
            let edge_px = if self.wrap_lines && formula_descriptor(&b.kind).is_some() {
                (super::text_input::ML_PAD_X - pad).max(0.0)
            } else {
                0.0
            };
            let original = b.line.get_rect();
            let original = Rect::new(
                original.x0 + indent_px + edge_px,
                original.y0,
                original.x1,
                original.y1,
            );
            if let BlockKind::Table { visual } = b.kind {
                let source_bottom = if visual {
                    original.y0
                } else {
                    (original.y0 + f64::from(b.line.table_source_height())).min(original.y1)
                };
                if let Some(slot) = self.table_preview_rects.get_mut(index) {
                    *slot = Some(Rect::new(
                        original.x0 + pad,
                        source_bottom
                            + f64::from(table_toolbar_height(
                                original.width() as f32,
                                self.text_scale as f32,
                            )),
                        original.x1 - pad,
                        original.y1,
                    ));
                }
                b.line.apply_layout_external(Rect::new(
                    original.x0,
                    original.y0,
                    original.x1,
                    source_bottom,
                ));
                continue;
            }
            if let Some(target) = transclusion_target(b)
                && let Some(slot) = self.transclusion_rects.get_mut(index)
            {
                // The card's edge is the content edge, where text starts.
                *slot = Some((
                    Rect::new(
                        original.x0 + pad,
                        original.y0,
                        original.x1 - pad,
                        original.y1,
                    ),
                    target,
                ));
            }
            if let Some(image) = self.image_attachments.get_mut(&b.id) {
                let source_bottom =
                    (original.y0 + f64::from(b.line.image_source_height())).min(original.y1);
                if let Some(slot) = self.image_preview_rects.get_mut(index) {
                    *slot = Some(Rect::new(
                        original.x0 + pad,
                        source_bottom
                            + if image.editing {
                                6.0 * self.text_scale
                            } else {
                                0.0
                            },
                        original.x1 - pad,
                        original.y1,
                    ));
                }
                b.line.apply_layout_external(Rect::new(
                    original.x0,
                    original.y0,
                    original.x1,
                    source_bottom,
                ));
                let width = if image.editing { 88.0 } else { 112.0 } * self.text_scale;
                image.button_mut().apply_layout_external(Rect::new(
                    (original.x1 - width - 8.0 * self.text_scale).max(original.x0),
                    source_bottom + 8.0 * self.text_scale,
                    original.x1 - 8.0 * self.text_scale,
                    source_bottom + 40.0 * self.text_scale,
                ));
                continue;
            }
            let descriptor = formula_descriptor(&b.kind);
            let starts_formula = is_formula_start.get(index).copied().unwrap_or(false);
            let ends_formula = is_formula_end.get(index).copied().unwrap_or(false);
            let source_top = if starts_formula {
                (original.y0 + FORMULA_HEADER_HEIGHT).min(original.y1)
            } else {
                original.y0 + code_headers[index]
            };
            let source_bottom =
                if descriptor.is_some_and(|value| value.mode == crate::math::FormulaMode::Visual) {
                    source_top
                } else if ends_formula {
                    (source_top + FORMULA_SOURCE_HEIGHT).min(original.y1)
                } else {
                    original.y1
                };
            if ends_formula {
                let mode = descriptor.map_or(crate::math::FormulaMode::Source, |value| value.mode);
                let preview = Rect::new(
                    original.x0 + pad + 12.0,
                    (if mode == crate::math::FormulaMode::Source {
                        source_bottom + FORMULA_PREVIEW_GAP
                    } else {
                        source_top + 2.0
                    })
                    .min(original.y1),
                    original.x1 - 8.0,
                    (original.y1 - 6.0).max(source_bottom + FORMULA_PREVIEW_GAP),
                );
                if let Some(slot) = self.formula_preview_rects.get_mut(index) {
                    *slot = Some(preview);
                }
            }
            if gutter > 0.0
                || indent_px > 0.0
                || starts_formula
                || descriptor.is_some_and(|value| value.mode == crate::math::FormulaMode::Visual)
                || ends_formula
            {
                let shifted =
                    Rect::new(original.x0 + gutter, source_top, original.x1, source_bottom);
                b.line.apply_layout_external(shifted);
            }
        }
        if let Some(find_node) = self.find_node {
            let find_origin = Point::new(
                new_origin.x,
                self.tooltip_viewport
                    .map_or(new_origin.y, |viewport| viewport.y0),
            );
            self.find_field.apply_layout(taffy, find_node, find_origin);
            if let Some(node) = self.replace_node {
                self.replace_field.apply_layout(taffy, node, find_origin);
            }
            if let Some(node) = self.replace_button_node {
                self.replace_button.apply_layout(taffy, node, find_origin);
            }
            if let Some(node) = self.replace_all_node {
                self.replace_all_button
                    .apply_layout(taffy, node, find_origin);
            }
            let field = self.find_field.get_rect();
            let size = 30.0 * self.text_scale;
            let gap = 3.0 * self.text_scale;
            let top = field.y0 + (field.height() - size) / 2.0;
            self.find_previous_rect =
                Rect::new(field.x1 + gap, top, field.x1 + gap + size, top + size);
            let advance = size + gap;
            self.find_next_rect = Rect::new(
                self.find_previous_rect.x0 + advance,
                top,
                self.find_previous_rect.x1 + advance,
                top + size,
            );
            self.find_close_rect = Rect::new(
                self.find_next_rect.x0 + advance,
                top,
                self.find_next_rect.x1 + advance,
                top + size,
            );
        }
        self.build_formula_chrome_rects();
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "one cohesive paint pass uses aligned caches and bounded viewport arithmetic"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        #[cfg(test)]
        let setup_probe = crate::profiling::frame_probe::span("block_setup");
        let incoming = std::mem::take(scene);
        let track_paragraphs = !self.blocks.is_empty()
            && !self.has_open_popup()
            && self.block_drag.is_none()
            && self.document_selection.is_none()
            && self.retained_block_selection.is_none()
            && self.blocks.iter().all(|block| {
                damages_where_it_paints(&block.kind) && block.line.selection_range().is_none()
            });
        let mut paragraph_parts = Vec::new();
        let mut paragraph_scene = Scene::new();
        self.line_overhang.clear();
        self.table_focus_paint.clear();
        let unit;
        let theme = if self.content_sized {
            unit = theme.at_unit_text_scale();
            &unit
        } else {
            theme
        };
        self.refresh_image_dimensions();
        let single_block = self.blocks.len() == 1;
        for block in &mut self.blocks {
            let hint = if single_block && matches!(block.kind, BlockKind::Paragraph) {
                self.empty_hint.as_str()
            } else {
                ""
            };
            block.line.inner_mut().set_placeholder(hint);
            // New lines (a split, a paste) take the editor's alignment too.
            if block.line.align() != self.text_align {
                block.line.set_align(self.text_align);
            }
        }

        self.draw_block_drag_affordance(scene, transform, theme);

        // Precompute run geometry so a multi-line fence paints one background
        // and a formula fence paints one preview after its final source line.
        let line_rects: Vec<Rect> = self.blocks.iter().map(|b| b.line.get_rect()).collect();
        let ordinals = numbered_ordinals(&self.blocks);
        let code_languages: Vec<Option<String>> = self
            .blocks
            .iter()
            .map(|block| match &block.kind {
                BlockKind::CodeBlock { lang } => Some(lang.clone().unwrap_or_default()),
                _ => None,
            })
            .collect();
        apply_syntax_colors(&mut self.blocks, &code_languages);
        let mut run_start = 0;
        let code_runs = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                if !matches!(block.kind, BlockKind::CodeBlock { .. }) {
                    return None;
                }
                if index == 0 || !self.blocks[index - 1].same_code_group(block) {
                    run_start = index;
                }
                Some(run_start)
            })
            .collect::<Vec<_>>();
        let mut formula_runs: Vec<Option<FormulaRunPreview>> = vec![None; self.blocks.len()];
        let mut formula_index = 0usize;
        while formula_index < self.blocks.len() {
            let Some(descriptor) = self
                .blocks
                .get(formula_index)
                .and_then(|b| formula_descriptor(&b.kind))
            else {
                formula_index = formula_index.saturating_add(1);
                continue;
            };
            let end = formula_run_end(&self.blocks, formula_index);
            let source = self
                .blocks
                .get(formula_index..=end)
                .map(|run| run.iter().map(Block::text).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            if let Some(slot) = formula_runs.get_mut(formula_index) {
                *slot = Some(FormulaRunPreview {
                    end,
                    descriptor,
                    source,
                });
            }
            formula_index = end.saturating_add(1);
        }

        self.code_copy_hits.clear();
        let mut code_index = 0usize;
        while code_index < code_languages.len() {
            let Some(Some(_)) = code_languages.get(code_index) else {
                code_index = code_index.saturating_add(1);
                continue;
            };
            let mut end = code_index;
            while code_runs.get(end.saturating_add(1)) == code_runs.get(code_index) {
                end = end.saturating_add(1);
            }
            if formula_runs.get(code_index).is_none_or(Option::is_none)
                && let Some(line) = line_rects.get(code_index)
            {
                let size = 22.0 * self.text_scale;
                let rect = Rect::new(
                    line.x1 - size - 4.0 * self.text_scale,
                    line.y0 - CODE_HEADER_HEIGHT * self.text_scale + 2.0 * self.text_scale,
                    line.x1 - 4.0 * self.text_scale,
                    line.y0 - CODE_HEADER_HEIGHT * self.text_scale + 2.0 * self.text_scale + size,
                );
                self.code_copy_hits.push((rect, code_index, end));
            }
            code_index = end.saturating_add(1);
        }

        let paint_rects: Vec<_> = (0..self.blocks.len())
            .map(|index| self.block_paint_rect(index))
            .collect();
        let active_drag = self.block_drag.filter(|drag| drag.active);
        let dragged_group = active_drag.map(|drag| self.movable_group(drag.source));
        // Paint only what the host's viewport can show. The focused block and
        // a dragged group always paint: their geometry feeds the caret, IME
        // and the drag preview even while they are scrolled out of view.
        // One screen of margin on either side keeps the neighbours a keystroke
        // can move the caret into freshly painted, and decorations reaching
        // past a block's own rectangle whole.
        let paint_window = presence
            .viewport
            .get()
            .map(|viewport| viewport.inflate(0.0, viewport.height().max(0.0)));
        let painted: Vec<bool> = paint_rects
            .iter()
            .enumerate()
            .map(|(index, rect)| {
                paint_window.is_none_or(|window| {
                    rect.is_none_or(|rect| rect.y1 >= window.y0 && rect.y0 <= window.y1)
                }) || self.focused == Some(index)
                    || dragged_group
                        .as_ref()
                        .is_some_and(|group| group.contains(&index))
            })
            .collect();
        let mut drag_offset = active_drag
            .map(|drag| self.last_mouse_pos - drag.start)
            .unwrap_or_default();
        let selected_blocks = self
            .retained_block_selection
            .as_ref()
            .and_then(|_| self.selected_block_range());
        let mut ctx = BlockDrawCtx {
            scene,
            text_renderer,
            transform,
            theme,
            presence,
            line_rects: line_rects.as_slice(),
            ordinals: ordinals.as_slice(),
            table_previews: self.table_preview_rects.as_slice(),
            image_previews: self.image_preview_rects.as_slice(),
            image_attachments: &self.image_attachments,
            image_store: self.image_store.as_ref(),
            table_cells: None,
            collect_code_ground: false,
            code_ground: None,
        };
        let mut formulas = FormulaDrawCtx {
            runs: formula_runs.as_slice(),
            code_runs: code_runs.as_slice(),
            chrome: self.formula_chrome.as_slice(),
            preview_rects: self.formula_preview_rects.as_slice(),
            visual_formulas: &mut self.visual_formulas,
            focus: self.visual_formula_focus,
        };

        let outside_prefix = track_paragraphs.then(|| std::mem::take(ctx.scene));
        let mut dragged_scene: Option<(Scene, Rect)> = None;
        #[cfg(test)]
        drop(setup_probe);
        #[cfg(test)]
        let blocks_probe = crate::profiling::frame_probe::span("block_paint");
        for (idx, b) in self.blocks.iter_mut().enumerate() {
            if !painted[idx] {
                continue;
            }
            #[cfg(test)]
            {
                self.painted_blocks = self.painted_blocks.saturating_add(1);
            }
            let preceding = dragged_group
                .as_ref()
                .filter(|group| group.contains(&idx))
                .map(|_| std::mem::take(ctx.scene));
            if b.text().is_empty()
                && selected_blocks
                    .as_ref()
                    .is_some_and(|range| range.contains(&idx))
            {
                ctx.scene.fill(
                    vello::peniko::Fill::NonZero,
                    ctx.transform,
                    ctx.theme.tokens.colors.intent.you.with_alpha_factor(0.35),
                    None,
                    &b.line.get_rect(),
                );
            }
            b.line
                .inner_mut()
                .suppress_caret_paint(preceding.is_some() || self.caret_paint_suppressed);
            if let Some(Some((rect, id))) = self.transclusion_rects.get(idx) {
                draw_transclusion_card(
                    ctx.scene,
                    ctx.transform,
                    ctx.text_renderer,
                    ctx.theme,
                    *rect,
                    self.transclusion_previews.get(id),
                );
            } else {
                // A table's cells become parts of their own.
                ctx.table_cells =
                    (track_paragraphs && matches!(b.kind, BlockKind::Table { .. })).then(Vec::new);
                // A code run's ground too.
                ctx.collect_code_ground = track_paragraphs;
                draw_block(b, idx, &mut ctx, &mut formulas);
                ctx.collect_code_ground = false;
            }
            let code_ground = ctx.code_ground.take();
            b.line.inner_mut().suppress_caret_paint(false);
            if let Some(overhang) = b.line.overhang() {
                self.line_overhang.insert(b.id.clone(), overhang);
            }
            // A tracked table's ground (its source line, its frame and grid)
            // so far; its cells, focused cell and controls follow apart.
            let mut table_parts = ctx.table_cells.take().map(|cells| TableParts {
                ground: std::mem::take(ctx.scene),
                cells,
                focus: Scene::new(),
                controls: Scene::new(),
                controls_paint: None,
            });
            if matches!(b.kind, BlockKind::Table { visual: true })
                && let Some(Some(rect)) = self.table_preview_rects.get(idx)
            {
                let table = self.table_editors.entry(b.id.clone()).or_insert_with(|| {
                    crate::widget::block_editor_table::DocumentTable::new(format!("table-{}", b.id))
                });
                let rows: Vec<Vec<String>> = markdown_table_data_rows(b.text())
                    .into_iter()
                    .map(|row| row.into_iter().map(decode_table_cell).collect())
                    .collect();
                table.project_cells(&rows);
                let layout =
                    markdown_table_layout(b.text(), rect.width() as f32, ctx.text_renderer, theme);
                table.project_geometry(
                    &layout.editor_rects(*rect, f64::from(theme.text_scale)),
                    ctx.text_renderer,
                    theme,
                );
                if preceding.is_none() {
                    table.draw_projected_focus(
                        ctx.scene,
                        ctx.transform,
                        ctx.text_renderer,
                        theme,
                        presence,
                        self.caret_paint_suppressed,
                    );
                    if let Some(painted) = table.projected_focus_paint() {
                        self.table_focus_paint.insert(b.id.clone(), painted);
                    }
                }
            }
            if let Some(parts) = table_parts.as_mut() {
                parts.focus = std::mem::take(ctx.scene);
            }
            if let BlockKind::Table { visual } = b.kind
                && let Some(Some(rect)) = self.table_preview_rects.get(idx)
            {
                let control = self
                    .table_mode_controls
                    .entry(b.id.clone())
                    .or_insert_with(|| TableModeControl::new(&b.id, visual));
                if control.visual != visual {
                    *control = TableModeControl::new(&b.id, visual);
                }
                let columns = (rect.width() / (100.0 * self.text_scale))
                    .floor()
                    .clamp(1.0, 3.0) as usize;
                let top = rect.y0
                    - f64::from(table_toolbar_height(
                        rect.width() as f32,
                        self.text_scale as f32,
                    ));
                for (position, button) in [
                    &mut control.button,
                    &mut control.add_row,
                    &mut control.add_column,
                ]
                .into_iter()
                .enumerate()
                {
                    let x = rect.x0 + (position % columns) as f64 * 100.0 * self.text_scale;
                    let y = top
                        + (position / columns) as f64 * 30.0 * self.text_scale
                        + 3.0 * self.text_scale;
                    button.apply_layout_external(Rect::new(
                        x,
                        y,
                        (x + 96.0 * self.text_scale).min(rect.x1),
                        y + 24.0 * self.text_scale,
                    ));
                }
                control
                    .button
                    .draw(ctx.scene, ctx.transform, ctx.text_renderer, theme, presence);
                if self.focused == Some(idx)
                    || Rect::new(rect.x0, top, rect.x1, rect.y1).contains(self.last_mouse_pos)
                {
                    control.add_row.draw(
                        ctx.scene,
                        ctx.transform,
                        ctx.text_renderer,
                        theme,
                        presence,
                    );
                    control.add_column.draw(
                        ctx.scene,
                        ctx.transform,
                        ctx.text_renderer,
                        theme,
                        presence,
                    );
                }
                if let Some(parts) = table_parts.as_mut() {
                    // Where the controls stand whether or not they show, and
                    // the focus ring a button paints around itself.
                    let reach = f64::from(theme.tokens.spacing.s2);
                    parts.controls_paint = [&control.button, &control.add_row, &control.add_column]
                        .into_iter()
                        .map(|button| button.get_rect().inflate(reach, reach))
                        .reduce(|all, rect| all.union(rect));
                }
            }
            if let Some(parts) = table_parts.as_mut() {
                parts.controls = std::mem::take(ctx.scene);
            }
            if let (Some(parts), Some(painted)) =
                (table_parts, paint_rects.get(idx).copied().flatten())
            {
                // The table's frame where the block paints, across the
                // editor; each cell where it is; the focused cell where it
                // is and was; the controls where they stand.
                let mut ground = vec![
                    Rect::new(self.rect.x0, painted.y0, self.rect.x1, painted.y1)
                        .union(b.line.get_rect()),
                ];
                ground.extend(self.line_overhang.get(&b.id).copied());
                ground.extend(self.drawn_overhang.get(&b.id).copied());
                paragraph_scene.append(&parts.ground, None);
                paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                    b.id.clone(),
                    painted,
                    parts.ground,
                    ground,
                ));
                for (index, (cell, scene)) in parts.cells.into_iter().enumerate() {
                    paragraph_scene.append(&scene, None);
                    paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                        format!("{}:cell:{index}", b.id),
                        cell,
                        scene,
                        vec![cell],
                    ));
                }
                let focus = [
                    self.table_focus_paint.get(&b.id),
                    self.drawn_table_focus.get(&b.id),
                ]
                .into_iter()
                .flatten()
                .copied()
                .collect();
                paragraph_scene.append(&parts.focus, None);
                paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                    format!("{}:focus", b.id),
                    painted,
                    parts.focus,
                    focus,
                ));
                paragraph_scene.append(&parts.controls, None);
                paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                    format!("{}:controls", b.id),
                    painted,
                    parts.controls,
                    parts.controls_paint.into_iter().collect(),
                ));
            } else if track_paragraphs {
                if let Some((ground, scene)) = code_ground {
                    // The run's ground, across the editor where it stands.
                    paragraph_scene.append(&scene, None);
                    paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                        format!("{}:ground", b.id),
                        ground,
                        scene,
                        vec![Rect::new(self.rect.x0, ground.y0, self.rect.x1, ground.y1)],
                    ));
                }
                let drawn = std::mem::take(ctx.scene);
                paragraph_scene.append(&drawn, None);
                let rect = b.line.get_rect();
                // A paragraph paints in its line; a heading, quote, list item
                // or rule also in its prefix gutter, so it damages its row of
                // the editor; a code line paints in its line over the run's
                // ground, a part of its own. Then its correction chip where
                // it is and was.
                let row = match (&b.kind, paint_rects.get(idx).copied().flatten()) {
                    (BlockKind::Paragraph | BlockKind::CodeBlock { .. }, _) | (_, None) => rect,
                    (_, Some(painted)) => {
                        Rect::new(self.rect.x0, painted.y0, self.rect.x1, painted.y1).union(rect)
                    }
                };
                let mut damage = vec![row];
                damage.extend(self.line_overhang.get(&b.id).copied());
                damage.extend(self.drawn_overhang.get(&b.id).copied());
                paragraph_parts.push(crate::widget::paint_damage::PaintPart::new(
                    b.id.clone(),
                    rect,
                    drawn,
                    damage,
                ));
            }
            if let Some(preceding) = preceding {
                let preview = std::mem::replace(ctx.scene, preceding);
                let line_rect = b.line.get_rect();
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "bounded on-screen preview width"
                )]
                let text_width = ctx
                    .text_renderer
                    .measure(b.text(), b.kind.font_size(), Some(line_rect.width() as f32))
                    .0;
                let mut bounds = Rect::new(
                    line_rect.x0 - 24.0,
                    line_rect.y0 - 4.0,
                    (line_rect.x0 + f64::from(text_width) + 24.0).min(line_rect.x1 + 8.0),
                    line_rect.y1 + 4.0,
                );
                if let Some(Some(painted)) = paint_rects.get(idx)
                    && (*painted != line_rect
                        || matches!(b.kind, BlockKind::Quote)
                        || transclusion_target(b).is_some())
                {
                    bounds = painted.inflate(4.0, 4.0);
                }
                ctx.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    0.25,
                    ctx.transform,
                    &bounds,
                );
                ctx.scene.append(&preview, None);
                ctx.scene.pop_layer();
                if let Some((combined, combined_bounds)) = &mut dragged_scene {
                    combined.append(&preview, None);
                    *combined_bounds = combined_bounds.union(bounds);
                } else {
                    dragged_scene = Some((preview, bounds));
                }
            }
        }
        #[cfg(test)]
        drop(blocks_probe);
        #[cfg(test)]
        let suffix_probe = crate::profiling::frame_probe::span("block_suffix");
        if let (Some(_), Some((preview, bounds))) = (active_drag, dragged_scene) {
            // Clamp against the actual preview, not the full-width text row:
            // short paragraphs must still follow horizontal pointer movement.
            // Preserve any existing gutter/padding overhang at drag start.
            // Forcing that decoration inside would make the text jump and pin
            // the preview until the pointer caught up with the artificial shift.
            let min = (self.rect.x0 - bounds.x0).min(0.0);
            let max = (self.rect.x1 - bounds.x1).max(0.0);
            drag_offset.x = if min <= max {
                drag_offset.x.clamp(min, max)
            } else {
                0.0
            };
            let delta = drag_offset;
            let ghost_transform = translated(ctx.transform, delta.x, delta.y);
            let card = vello::kurbo::RoundedRect::from_rect(bounds, 6.0);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ghost_transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &card,
            );
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                ghost_transform,
                crate::surface::edge_on(theme.tokens.colors.surfaces.raised),
                None,
                &card,
            );
            ctx.scene.append(&preview, Some(Affine::translate(delta)));
        }

        // Compact language + copy chrome overlays the upper-right corner of
        // each fenced run, matching established editors while leaving source
        // selection and CRDT text untouched.
        if self
            .copied_code_feedback
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed().as_secs_f32() >= 2.0)
        {
            self.copied_code_feedback = None;
            self.copied_code_start = None;
        }
        for (rect, start, end) in &self.code_copy_hits {
            let dragging_this_group = dragged_group
                .as_ref()
                .is_some_and(|group| group.contains(start));
            let chrome_transform = if dragging_this_group {
                let delta = drag_offset;
                translated(ctx.transform, delta.x, delta.y)
            } else {
                ctx.transform
            };
            let hovered = rect.contains(self.last_mouse_pos);
            let copied = self.copied_code_feedback.as_ref().is_some_and(|(id, _)| {
                self.blocks.get(*start).is_some_and(|block| &block.id == id)
            });
            if hovered || copied {
                ctx.scene.fill(
                    vello::peniko::Fill::NonZero,
                    chrome_transform,
                    ctx.theme.tokens.colors.lift(0.25),
                    None,
                    &vello::kurbo::RoundedRect::from_rect(*rect, 4.0 * self.text_scale),
                );
            }
            draw_icon(
                ctx.scene,
                chrome_transform,
                rect.inflate(-4.0 * self.text_scale, -4.0 * self.text_scale),
                if copied { Icon::Check } else { Icon::Copy },
                ctx.theme.tokens.colors.text_dim,
                1.5 * self.text_scale,
            );
            let language =
                code_languages
                    .get(*start)
                    .and_then(Option::as_ref)
                    .map_or(
                        "Code",
                        |value| if value.is_empty() { "Code" } else { value },
                    );
            let Some((line, inset)) = self
                .blocks
                .get(*start)
                .map(|block| (block.line.get_rect(), block.line.text_inset(ctx.theme)))
            else {
                continue;
            };
            // The language label starts where the code does.
            let left = line.x0 + inset;
            let available = (rect.x0 - left - 8.0 * self.text_scale).max(0.0);
            let active = !dragging_this_group
                && !self.caret_paint_suppressed
                && self.is_focused
                && self
                    .focused
                    .is_some_and(|index| (*start..=*end).contains(&index));
            let hint = crate::i18n!("block-editor-code-exit-hint");
            let compact_hint = crate::i18n!("block-editor-code-exit-compact");
            let (hint_width, _) = ctx.text_renderer.measure(&hint, 11.0, None);
            let hint = if f64::from(hint_width) <= available {
                &hint
            } else {
                &compact_hint
            };
            let (hint_width, _) = ctx.text_renderer.measure(hint, 11.0, None);
            let (language_width, _) = ctx.text_renderer.measure(language, 11.0, None);
            let show_language = !active
                || f64::from(language_width + hint_width) + 20.0 * self.text_scale <= available;
            if show_language {
                let language = ctx
                    .text_renderer
                    .elide_to_width(language, 11.0, available as f32);
                ctx.text_renderer.draw_text(
                    ctx.scene,
                    &language,
                    11.0,
                    ctx.theme.tokens.colors.text_dim,
                    chrome_transform * Affine::translate((left, rect.y0 + 4.0 * self.text_scale)),
                    None,
                );
            }
            if active {
                ctx.text_renderer.draw_text(
                    ctx.scene,
                    hint,
                    11.0,
                    ctx.theme.tokens.colors.text_dim,
                    chrome_transform
                        * Affine::translate((
                            if show_language {
                                rect.x0 - f64::from(hint_width) - 8.0 * self.text_scale
                            } else {
                                left
                            },
                            rect.y0 + 4.0 * self.text_scale,
                        )),
                    Some(available as f32),
                );
            }
        }

        // Runs AFTER the block loop: pixel_x_for_byte needs this frame's
        // fresh `char_x_positions`. Cross-block selections are resolved from
        // their start block id once per frame.
        let document_selections = self.resolve_document_selections(presence);
        for (i, b) in self.blocks.iter().enumerate() {
            if painted[i] {
                self.draw_block_presence(i, b, &document_selections, &mut ctx);
            }
        }
        for (index, block) in self.blocks.iter().enumerate() {
            if !painted[index] {
                continue;
            }
            if let Some(image) = self.image_attachments.get_mut(&block.id) {
                let hovered = self
                    .image_preview_rects
                    .get(index)
                    .and_then(|rect| *rect)
                    .is_some_and(|rect| rect.contains(self.last_mouse_pos));
                if image.editing
                    || hovered
                    || image.button().is_focused()
                    || (self.is_focused && self.focused == Some(index))
                {
                    image
                        .button_mut()
                        .draw(scene, transform, text_renderer, theme, presence);
                }
            }
        }
        // The opaque drag preview can cover its destination. Keep the drop
        // marker above both the preview and its code/header chrome.
        if let Some(drag) = self.block_drag.filter(|drag| drag.active) {
            self.draw_block_drop_indicator(scene, transform, theme.tokens.colors.intent.you, drag);
        }
        self.draw_find_overlay(scene, transform, text_renderer, theme, presence);
        // Link hit geometry depends on the shaped positions produced by this
        // frame's block draw. Refreshing it before that draw made first-frame
        // style, scale, and BiDi changes unclickable.
        #[cfg(test)]
        let links_probe = crate::profiling::frame_probe::span("block_link_hits");
        self.refresh_live_link_hits(theme);
        #[cfg(test)]
        drop(links_probe);
        if let Some(prefix) = outside_prefix {
            let suffix = std::mem::take(scene);
            let mut outside = prefix.clone();
            outside.append(&suffix, None);
            let mut complete = prefix;
            complete.append(&paragraph_scene, None);
            complete.append(&suffix, None);
            *scene = complete;
            self.paint_damage.record(
                self.paint_extent(),
                Some(crate::widget::paint_damage::PaintFrame::new(
                    vec![self.rect, presence.viewport.get().unwrap_or(self.rect)],
                    transform,
                    theme,
                    outside,
                    paragraph_parts,
                )),
            );
        } else {
            self.paint_damage.record(self.paint_extent(), None);
        }
        let own = std::mem::replace(scene, incoming);
        scene.append(&own, None);
        #[cfg(test)]
        drop(suffix_probe);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.read_only {
            return;
        }
        self.begin_edit();
        let typing = self.typing_position(event, *ctx.modifiers);
        self.handle_event_unscoped(event, ctx);
        let scope = match typing {
            Some((index, count, id))
                if self.blocks.len() == count
                    && self.focused == Some(index)
                    && self.document_selection.is_none()
                    && self.blocks.get(index).is_some_and(|block| block.id == id) =>
            {
                EditScope::Blocks(index.saturating_sub(1)..index.saturating_add(2))
            }
            _ => EditScope::Anywhere,
        };
        self.edit_scope.widen(scope);
    }

    fn name(&self) -> &'static str {
        "BlockEditor"
    }

    fn test_id(&self) -> Option<&str> {
        Some("block_editor")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self
            .blocks
            .iter()
            .filter_map(|block| {
                self.image_attachments
                    .get(&block.id)
                    .map(|image| image.button() as &dyn Widget)
            })
            .collect::<Vec<_>>();
        if self.find_visible {
            children.push(&self.find_field);
            if self.replace_enabled {
                children.push(&self.replace_field);
                children.push(&self.replace_button);
                children.push(&self.replace_all_button);
            }
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self
            .image_attachments
            .values_mut()
            .map(|image| image.button_mut() as &mut dyn Widget)
            .collect::<Vec<_>>();
        if self.find_visible {
            children.push(&mut self.find_field);
            if self.replace_enabled {
                children.push(&mut self.replace_field);
                children.push(&mut self.replace_button);
                children.push(&mut self.replace_all_button);
            }
        }
        children
    }

    /// Block lines are not children, so their fields' timers (a settling
    /// squiggle, a suggestion for the word under the caret) are collected
    /// here alongside the children's.
    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        let mut earliest = self
            .blocks
            .iter()
            .filter_map(|block| block.line.next_wake_deadline())
            .min();
        self.for_each_child(&mut |child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest =
                    Some(earliest.map_or(deadline, |old: crate::time::Instant| old.min(deadline)));
            }
        });
        earliest
    }

    fn needs_animation_frame(&self) -> bool {
        self.copied_code_feedback.is_some()
            || self
                .image_store
                .as_ref()
                .is_some_and(|store| !store.is_idle())
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.find_visible {
            if self.find_previous_rect.contains(point)
                || self.find_next_rect.contains(point)
                || self.find_close_rect.contains(point)
            {
                return Some(winit::window::CursorIcon::Pointer);
            }
            if let Some(cursor) = self.find_field.cursor_shape(point) {
                return Some(cursor);
            }
        }
        if self.checkbox_hit_test(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self
            .transclusion_rects
            .iter()
            .flatten()
            .any(|(rect, _)| rect.contains(point))
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self
            .live_link_hits
            .iter()
            .any(|(rect, _)| rect.contains(point))
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.block_drag.is_some() {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        if (0..self.blocks.len()).any(|index| {
            self.block_drag_handle_rect(index)
                .is_some_and(|rect| rect.contains(point))
        }) {
            return Some(winit::window::CursorIcon::Grab);
        }
        if self.formula_chrome.iter().flatten().any(|chrome| {
            chrome.source_button.contains(point) || chrome.visual_button.contains(point)
        }) || self
            .code_copy_hits
            .iter()
            .any(|(rect, _, _)| rect.contains(point))
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn wants_tab_input(&self) -> bool {
        // Escape then Tab is the keyboard route back to surrounding controls.
        !self.read_only && !self.tab_navigation
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.block_drag.is_some()
            || self.slash_menu.is_visible()
            || self.link_popover.is_visible()
        {
            return None;
        }
        if self
            .tooltip_viewport
            .is_some_and(|viewport| !viewport.contains(point))
        {
            return None;
        }
        let visible = |rect: Rect| {
            self.tooltip_viewport
                .map_or(rect, |viewport| rect.intersect(viewport))
        };
        if let Some(rect) = (0..self.blocks.len())
            .filter_map(|index| self.block_drag_handle_rect(index))
            .map(visible)
            .find(|rect| rect.contains(point))
        {
            return Some((rect, crate::i18n!("block-editor-drag-hint")));
        }
        for (rect, start, _) in &self.code_copy_hits {
            let copy_rect = visible(*rect);
            if copy_rect.contains(point) {
                let copied = self.copied_code_feedback.as_ref().is_some_and(|(id, at)| {
                    at.elapsed().as_secs_f32() < 2.0
                        && self.blocks.get(*start).is_some_and(|block| &block.id == id)
                });
                return Some((
                    copy_rect,
                    if copied {
                        crate::i18n!("block-editor-code-copied")
                    } else {
                        crate::i18n!("block-editor-copy-code-hint")
                    },
                ));
            }
            if let Some(line) = self.blocks.get(*start).map(|block| block.line.get_rect()) {
                let header = visible(Rect::new(line.x0, rect.y0, rect.x0, rect.y1));
                if header.contains(point) {
                    return Some((header, crate::i18n!("block-editor-code-keyboard-hint")));
                }
            }
        }
        None
    }

    fn set_focus(&mut self, focused: bool) {
        Self::set_focus(self, focused);
    }

    /// Assistive technology sets the whole text (one paragraph, line breaks
    /// kept) or selects by grapheme offsets into the text, paragraphs joined
    /// by a line break.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if self.read_only {
            return false;
        }
        self.begin_edit();
        match action {
            Action::SetValue(value) => {
                let cleaned: String = value
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .chars()
                    .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
                    .collect();
                let id = self
                    .blocks
                    .first()
                    .map(|block| block.id.clone())
                    .unwrap_or_default();
                let mut block = Block::paragraph(&cleaned);
                if !id.is_empty() {
                    block.id = id;
                }
                self.edit_scope = EditScope::Anywhere;
                self.blocks = vec![block];
                self.retained_block_selection = None;
                self.document_selection = None;
                self.structure_dirty = true;
                self.focus_block(0, CursorPlacement::End);
                self.dirty = true;
                true
            }
            Action::SetSelection { anchor, focus } => {
                self.retained_block_selection = None;
                let text = self
                    .blocks
                    .iter()
                    .map(Block::text)
                    .collect::<Vec<_>>()
                    .join("\n");
                let byte_at = |index: usize| {
                    crate::text_break::grapheme_indices(&text)
                        .nth(index)
                        .map_or(text.len(), |(byte, _)| byte)
                };
                let position = |byte: usize| {
                    let mut offset = 0usize;
                    for (index, block) in self.blocks.iter().enumerate() {
                        let len = block.text().len();
                        if byte <= offset.saturating_add(len) {
                            return DocumentPosition {
                                block: index,
                                byte: byte.saturating_sub(offset),
                            };
                        }
                        offset = offset.saturating_add(len).saturating_add(1);
                    }
                    DocumentPosition {
                        block: self.blocks.len().saturating_sub(1),
                        byte: self.blocks.last().map_or(0, |block| block.text().len()),
                    }
                };
                let (anchor, focus) = (position(byte_at(*anchor)), position(byte_at(*focus)));
                self.focus_block(focus.block, CursorPlacement::Byte(focus.byte));
                if anchor.block == focus.block {
                    self.retained_block_selection = None;
                    self.document_selection = None;
                    if let Some(block) = self.blocks.get_mut(focus.block) {
                        block
                            .line
                            .inner_mut()
                            .set_selection(anchor.byte, focus.byte);
                    }
                } else {
                    self.document_selection = Some(DocumentSelection {
                        anchor,
                        cursor: focus,
                    });
                }
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.find_visible {
            if self.replace_enabled && self.replace_field.is_focused() {
                return Widget::ime_cursor_rect(&self.replace_field);
            }
            return Widget::ime_cursor_rect(&self.find_field);
        }
        if self.link_popover.is_visible() {
            return Widget::ime_cursor_rect(&self.link_popover);
        }
        if self.slash_menu.is_visible() {
            return Widget::ime_cursor_rect(&self.slash_menu);
        }
        let block = self.blocks.get(self.focused?)?;
        if self
            .image_attachments
            .get(&block.id)
            .is_some_and(|image| !image.editing)
        {
            return None;
        }
        if let Some(rect) = self
            .table_editors
            .get(&block.id)
            .and_then(super::block_editor_table::DocumentTable::projected_ime_rect)
        {
            return Some(rect);
        }
        Widget::ime_cursor_rect(&block.line)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        if self.read_only {
            return None;
        }
        self.ime_cursor_rect()?;
        let (scope, field) = if self.find_visible {
            if self.replace_enabled && self.replace_field.is_focused() {
                (
                    b"replace".as_slice(),
                    self.replace_field.ime_edit_fingerprint(key)?,
                )
            } else {
                (
                    b"find".as_slice(),
                    self.find_field.ime_edit_fingerprint(key)?,
                )
            }
        } else if self.link_popover.is_visible() {
            (
                b"link".as_slice(),
                self.link_popover.ime_edit_fingerprint(key)?,
            )
        } else if self.slash_menu.is_visible() {
            (
                b"slash".as_slice(),
                self.slash_menu.ime_edit_fingerprint(key)?,
            )
        } else {
            let block = self.blocks.get(self.focused?)?;
            let field = if let Some(table) = self
                .table_editors
                .get(&block.id)
                .filter(|table| table.projected_ime_rect().is_some())
            {
                table.projected_ime_fingerprint(key)?
            } else {
                block.line.ime_edit_fingerprint(key)?
            };
            (block.id.as_bytes(), field)
        };
        let mut stamp = crate::ime::scoped_fingerprint(scope, field, key);
        if let Some(selection) = self.document_selection {
            for offset in [
                selection.anchor.block,
                selection.anchor.byte,
                selection.cursor.block,
                selection.cursor.byte,
            ] {
                stamp = crate::ime::scoped_fingerprint(&offset.to_le_bytes(), stamp, key);
            }
        }
        match &self.retained_block_selection {
            Some(RetainedBlockSelection::Active(ids)) => {
                stamp = crate::ime::scoped_fingerprint(b"retained", stamp, key);
                for id in ids {
                    stamp = crate::ime::scoped_fingerprint(id.as_bytes(), stamp, key);
                }
            }
            Some(RetainedBlockSelection::Refused) => return None,
            None => {}
        }
        Some(stamp)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn ime_multiline(&self) -> bool {
        self.is_focused
            && !self.read_only
            && !self.find_visible
            && !self.link_popover.is_visible()
            && !self.slash_menu.is_visible()
            && self.visual_formula_focus.is_none()
            && self.ime_cursor_rect().is_some()
            && self
                .focused
                .and_then(|index| self.blocks.get(index))
                .is_some_and(|block| {
                    block.kind.is_editable() && !matches!(block.kind, BlockKind::Table { .. })
                })
    }

    /// Block contents are not surfaced as a `value` — a screen reader
    /// walks the per-block subtree instead.
    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        reason = "the complete resource tree is emitted in one cohesive accessibility snapshot"
    )]
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut children: Vec<AccessNode> = self
            .formula_chrome
            .iter()
            .enumerate()
            .filter_map(|(start, chrome)| {
                let chrome = chrome.as_ref()?;
                let syntax = match chrome.descriptor.syntax {
                    crate::math::FormulaSyntax::Tex => "TeX",
                    crate::math::FormulaSyntax::Typst => "Typst",
                };
                let source = self
                    .blocks
                    .get(start..=chrome.end)?
                    .iter()
                    .map(Block::text)
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(
                    AccessNode::new(Role::Group)
                        .with_name(format!("{syntax} formula"))
                        .with_hashed_key(format!("notes.formula:{}", self.blocks.get(start)?.id))
                        .with_value(source)
                        .with_children(vec![
                            AccessNode::new(Role::ToggleButton)
                                .with_name("Source editor")
                                .with_state(AccessState {
                                    checked: Some(
                                        chrome.descriptor.mode == crate::math::FormulaMode::Source,
                                    ),
                                    ..Default::default()
                                }),
                            AccessNode::new(Role::ToggleButton)
                                .with_name("Visual editor")
                                .with_state(AccessState {
                                    checked: Some(
                                        chrome.descriptor.mode == crate::math::FormulaMode::Visual,
                                    ),
                                    ..Default::default()
                                }),
                        ]),
                )
            })
            .collect();
        children.extend(
            self.code_copy_hits
                .iter()
                .filter_map(|(bounds, start, end)| {
                    let run = self.blocks.get(*start..=*end)?;
                    let language = run.first().and_then(|block| match &block.kind {
                        BlockKind::CodeBlock { lang } => lang.as_deref(),
                        _ => None,
                    });
                    Some(
                        AccessNode::new(Role::Button)
                            .with_name(language.map_or_else(
                                || "Copy code".to_string(),
                                |lang| format!("Copy {lang} code"),
                            ))
                            .with_action_target(self.code_copy_run_target(*start, *end)?)
                            .with_bounds(*bounds)
                            .with_state(AccessState::new().focusable(true)),
                    )
                }),
        );
        if self.find_visible {
            if let Some(search) = self.find_field.accessibility() {
                children.push(
                    search
                        .with_name(crate::i18n!("block-editor-find-placeholder"))
                        .with_action_target("notes.find:query"),
                );
            }
            if self.replace_enabled {
                if let Some(field) = self.replace_field.accessibility() {
                    children.push(
                        field
                            .with_name(crate::i18n!("block-editor-replace-placeholder"))
                            .with_action_target("notes.find:replacement"),
                    );
                }
                if let Some(button) = self.replace_all_button.accessibility() {
                    children.push(
                        button
                            .with_action_target("notes.find:replace-all")
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .disabled(!self.can_replace_all()),
                            ),
                    );
                }
                if let Some(button) = self.replace_button.accessibility() {
                    children.push(
                        button.with_action_target("notes.find:replace").with_state(
                            AccessState::new()
                                .focusable(true)
                                .disabled(self.find_replace_request().is_none()),
                        ),
                    );
                }
            }
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("block-editor-find-previous"))
                    .with_action_target("notes.find:previous")
                    .with_bounds(self.find_previous_rect)
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("block-editor-find-next"))
                    .with_action_target("notes.find:next")
                    .with_bounds(self.find_next_rect)
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("block-editor-find-close"))
                    .with_action_target("notes.find:close")
                    .with_bounds(self.find_close_rect)
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        let total = self.blocks.len();
        let selected_blocks = self
            .retained_block_selection
            .as_ref()
            .and_then(|_| self.selected_block_range());
        children.extend(self.blocks.iter().enumerate().map(|(index, block)| {
            let up_disabled = self.neighboring_group(index, true).is_none();
            let position = index.saturating_add(1);
            let down_disabled = self.neighboring_group(index, false).is_none();
            let bounds = self
                .block_drag_handle_rect(index)
                .unwrap_or_else(|| block.line.get_rect());
            let mut block_children = vec![
                AccessNode::new(Role::Button)
                    .with_name("Move block up")
                    .with_action_target(Self::block_move_target(&block.id, "up"))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(!up_disabled)
                            .disabled(up_disabled),
                    ),
                AccessNode::new(Role::Button)
                    .with_name("Move block down")
                    .with_action_target(Self::block_move_target(&block.id, "down"))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(!down_disabled)
                            .disabled(down_disabled),
                    ),
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n!("block-editor-change-type"))
                    .with_action_target(format!("notes.block-type:{}", block.id))
                    .with_bounds(bounds)
                    .with_state(AccessState::new().focusable(true)),
            ];
            if block.kind.is_editable()
                && block.line.get_rect().height() > 0.0
                && let Some(text) = block.line.accessibility()
            {
                block_children.insert(
                    0,
                    text.with_name(crate::i18n::I18nManager::format("widget-block-text", &[]))
                        .with_action_target(format!("notes.block-text:{}", block.id))
                        .with_bounds(block.line.get_rect()),
                );
            }
            for (direction, label, disabled) in [
                (
                    "in",
                    "widget-block-indent",
                    self.blocks[self.movable_group(index)]
                        .iter()
                        .any(|b| b.indent() >= MAX_LIST_INDENT),
                ),
                ("out", "widget-block-outdent", block.indent() == 0),
            ] {
                block_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(crate::i18n::I18nManager::format(label, &[]))
                        .with_action_target(format!("notes.block-indent:{}:{direction}", block.id))
                        .with_bounds(bounds)
                        .with_state(AccessState::new().focusable(!disabled).disabled(disabled)),
                );
            }
            if let Some(target) = transclusion_target(block) {
                let name = self
                    .transclusion_previews
                    .get(&target)
                    .map_or("embedded note", |preview| preview.title.as_str());
                let card_bounds = self
                    .transclusion_rects
                    .get(index)
                    .and_then(|entry| *entry)
                    .map_or_else(|| block.line.get_rect(), |(rect, _)| rect);
                block_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(format!("Open embedded note: {name}"))
                        .with_description("Live preview of a linked note")
                        .with_action_target(transclusion_action_target(target))
                        .with_bounds(card_bounds)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            if matches!(block.kind, BlockKind::Table { .. }) {
                if let Some(control) = self.table_mode_controls.get(&block.id)
                    && let Some(mut button) = control.button.accessibility()
                {
                    button = button
                        .with_bounds(control.button.get_rect())
                        .with_action_target(format!("notes.table-mode:{}", block.id));
                    block_children.push(button);
                    for (prefix, button) in [
                        ("notes.table-add-row", &control.add_row),
                        ("notes.table-add-column", &control.add_column),
                    ] {
                        if let Some(node) = button.accessibility() {
                            block_children.push(
                                node.with_bounds(button.get_rect())
                                    .with_action_target(format!("{prefix}:{}", block.id)),
                            );
                        }
                    }
                }
                let table_bounds = self
                    .table_preview_rects
                    .get(index)
                    .and_then(|entry| *entry)
                    .unwrap_or_else(|| block.line.get_rect());
                let headers = markdown_table_data_rows(block.text())
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let rows = markdown_table_data_rows(block.text())
                    .into_iter()
                    .enumerate()
                    .map(|(row_index, row)| {
                        let cells = row
                            .into_iter()
                            .enumerate()
                            .map(|(column_index, value)| {
                                let target = format!(
                                    "notes.table-cell:{}:{row_index}:{column_index}",
                                    block.id
                                );
                                let mut input = AccessNode::new(Role::MultilineTextInput)
                                    .with_name(decode_table_cell(
                                        headers.get(column_index).copied().unwrap_or(""),
                                    ))
                                    .with_value(decode_table_cell(value))
                                    .with_action_target(target)
                                    .with_state(AccessState::new().focusable(true));
                                if let Some(rect) = self
                                    .table_editors
                                    .get(&block.id)
                                    .and_then(|table| table.projected_rect(row_index, column_index))
                                {
                                    input = input.with_bounds(rect);
                                }
                                AccessNode::new(Role::Cell)
                                    .with_name(if row_index == 0 {
                                        format!("Column {}", column_index.saturating_add(1))
                                    } else {
                                        format!(
                                            "Cell {}, {}",
                                            row_index,
                                            column_index.saturating_add(1)
                                        )
                                    })
                                    .with_description(decode_table_cell(
                                        headers.get(column_index).copied().unwrap_or(""),
                                    ))
                                    .with_value(decode_table_cell(value))
                                    .with_children(vec![input])
                            })
                            .collect();
                        AccessNode::new(Role::Row)
                            .with_name(if row_index == 0 {
                                "Header row".to_string()
                            } else {
                                format!("Row {row_index}")
                            })
                            .with_children(cells)
                    })
                    .collect();
                block_children.push(
                    AccessNode::new(Role::Table)
                        .with_name("Markdown table")
                        .with_value(block.text())
                        .with_bounds(table_bounds)
                        .with_children(rows),
                );
            }
            if let Some(image) = self.image_attachments.get(&block.id) {
                if let Some(button) = image.button().accessibility() {
                    block_children.push(
                        button
                            .with_bounds(image.button().get_rect())
                            .with_action_target(format!(
                                "notes.image-{}:{}",
                                if image.editing { "done" } else { "edit" },
                                block.id
                            )),
                    );
                }
                if image.editing {
                    block_children.push(
                        AccessNode::new(Role::MultilineTextInput)
                            .with_name(crate::i18n!("notes-image-source"))
                            .with_value(block.text())
                            .with_action_target(format!("notes.image-source:{}", block.id))
                            .with_bounds(block.line.get_rect())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(block.line.is_focused()),
                            ),
                    );
                }
            }
            if let Some((alt, destination)) = self
                .image_attachments
                .get(&block.id)
                .and_then(|image| standalone_markdown_image(&image.source))
                .or_else(|| standalone_markdown_image(block.text()))
            {
                let bounds = self
                    .image_preview_rects
                    .get(index)
                    .and_then(|entry| *entry)
                    .unwrap_or_else(|| block.line.get_rect());
                block_children.push(
                    AccessNode::new(Role::Image)
                        .with_name(if alt.is_empty() { "Image" } else { alt })
                        .with_description(destination)
                        .with_bounds(bounds),
                );
            }
            for span in block.line.marks() {
                let (name, target) = match &span.mark {
                    Mark::Link(id) if !id.is_nil() => {
                        ("Open linked note".to_string(), page_link_action_target(*id))
                    }
                    Mark::ExternalLink(url) => (
                        "Open external link".to_string(),
                        external_link_action_target(url),
                    ),
                    _ => continue,
                };
                block_children.push(
                    AccessNode::new(Role::Link)
                        .with_name(name)
                        .with_value(block.text().get(span.range.clone()).unwrap_or_default())
                        .with_action_target(target)
                        .with_bounds(block.line.get_rect())
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            let level = usize::from(block.indent()).saturating_add(1);
            let group = AccessNode::new(Role::Group)
                .with_name(format!(
                    "{} block {position}",
                    accessible_kind_name(&block.kind)
                ))
                .with_semantic_key(format!("notes.block:{}", block.id))
                .with_state(
                    AccessState::new().selected(
                        selected_blocks
                            .as_ref()
                            .is_some_and(|range| range.contains(&index)),
                    ),
                )
                .with_hashed_key(format!("notes.block:group:{}", block.id))
                .with_position(position, total)
                .with_level(level)
                .with_bounds(block.line.get_rect())
                .with_children(block_children);
            // Also in words, which every screen reader reads: the platform
            // bridge does not carry a group's level.
            if level > 1 {
                group.with_description(format!("Level {level}"))
            } else {
                group
            }
        }));
        if let Some(mut menu) = Widget::accessibility(&self.slash_menu) {
            fn namespace_targets(node: &mut AccessNode) {
                if let Some(target) = node.action_target.take() {
                    node.action_target = Some(format!("notes.slash-menu:{target}"));
                }
                for child in &mut node.children {
                    namespace_targets(child);
                }
            }
            namespace_targets(&mut menu);
            menu.semantic_key = Some("notes.slash-menu".into());
            menu.bounds = Some(self.slash_menu.get_rect().into());
            children.push(menu);
        }
        Some(
            AccessNode::new(Role::Document)
                .with_description(
                    "Drag a block's six-dot handle to reorder it. Alt+Up and Alt+Down move the focused block. Tab nests it under the block above and Shift+Tab moves it back out; press Escape, then Tab, to leave the editor.",
                )
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    multiline: true,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        reason = "one semantic dispatcher keeps every resource action on the same validated target path"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.edit_scope = EditScope::Anywhere;
        if self.find_visible {
            match (target, action) {
                ("notes.find:query", crate::a11y::Action::SetValue(value))
                    if !self.is_composing() =>
                {
                    self.find_field.set_query(value);
                    self.refresh_find_matches(true);
                    self.dirty = true;
                    return true;
                }
                ("notes.find:query", crate::a11y::Action::Focus) if !self.is_composing() => {
                    self.focus_find_input(false);
                    return true;
                }
                ("notes.find:replacement", crate::a11y::Action::SetValue(value))
                    if self.replace_enabled && !self.is_composing() =>
                {
                    self.replace_field.set_text(value);
                    self.dirty = true;
                    return true;
                }
                ("notes.find:replacement", crate::a11y::Action::Focus)
                    if self.replace_enabled && !self.is_composing() =>
                {
                    self.focus_find_input(true);
                    return true;
                }
                ("notes.find:replace-all", crate::a11y::Action::Activate)
                    if self.can_replace_all() =>
                {
                    self.replace_all_requested.set(true);
                    return true;
                }
                ("notes.find:replace", crate::a11y::Action::Activate)
                    if self.find_replace_request().is_some() =>
                {
                    self.replace_requested.set(true);
                    return true;
                }
                _ => {}
            }
        }
        if matches!(action, crate::a11y::Action::Activate) {
            match target {
                "notes.find:previous" if self.find_visible && !self.is_composing() => {
                    self.step_find(true);
                    return true;
                }
                "notes.find:next" if self.find_visible && !self.is_composing() => {
                    self.step_find(false);
                    return true;
                }
                "notes.find:close" if self.find_visible && !self.is_composing() => {
                    self.close_find();
                    return true;
                }
                _ => {}
            }
        }
        if let Some(id) = target.strip_prefix("notes.block-type:")
            && matches!(action, crate::a11y::Action::Activate)
            && let Some(index) = self.blocks.iter().position(|block| block.id == id)
        {
            return self.open_block_type_menu(index);
        }
        if let Some(target) = target.strip_prefix("notes.slash-menu:") {
            let handled = self
                .slash_menu
                .perform_semantic_action(target, action, state, path);
            if let Some(new_kind) = self.slash_menu.take_selection() {
                self.commit_slash_selection(new_kind);
            }
            if !self.slash_menu.is_visible() {
                self.slash_open_at = None;
                self.slash_anchor_caret = None;
            }
            if handled {
                self.dirty = true;
            }
            return handled;
        }
        if let Some(id) = target.strip_prefix("notes.block-text:") {
            let Some(index) = self.blocks.iter().position(|block| {
                block.id == id && block.kind.is_editable() && block.line.get_rect().height() > 0.0
            }) else {
                return false;
            };
            if matches!(action, crate::a11y::Action::SetValue(value) if value == self.blocks[index].text())
            {
                return true;
            }
            if matches!(
                action,
                crate::a11y::Action::Focus | crate::a11y::Action::Activate
            ) {
                let cursor = self.blocks[index].line.cursor_pos();
                self.focus_block(index, CursorPlacement::Byte(cursor));
                return true;
            }
            if !matches!(
                action,
                crate::a11y::Action::SetValue(_) | crate::a11y::Action::SetSelection { .. }
            ) {
                return false;
            }
            if matches!(action, crate::a11y::Action::SetSelection { .. }) {
                let cursor = self.blocks[index].line.cursor_pos();
                self.focus_block(index, CursorPlacement::Byte(cursor));
            }
            self.begin_edit();
            let handled = self.blocks[index]
                .line
                .perform_action(action, &[], state, path);
            if handled {
                self.dirty = true;
                if matches!(action, crate::a11y::Action::SetValue(_)) {
                    self.edit_pending = true;
                    self.commit_pending = true;
                }
            }
            let ops = self.take_edit_ops();
            self.pending_semantic_ops.extend(ops);
            return handled;
        }
        if let Some(id) = target.strip_prefix("notes.image-source:") {
            if !self
                .image_attachments
                .get(id)
                .is_some_and(|image| image.editing)
            {
                return false;
            }
            let Some(index) = self.blocks.iter().position(|block| block.id == id) else {
                return false;
            };
            if let crate::a11y::Action::SetValue(value) = action {
                if self.blocks[index].text() == value {
                    return true;
                }
                self.begin_edit();
                self.blocks[index].line.set_text(value);
                self.edit_pending = true;
                self.commit_pending = true;
                self.structure_dirty = true;
                self.dirty = true;
                let ops = self.take_edit_ops();
                self.pending_semantic_ops.extend(ops);
                return true;
            }
            if matches!(
                action,
                crate::a11y::Action::Focus | crate::a11y::Action::Activate
            ) {
                self.focus_block(index, CursorPlacement::Start);
                return true;
            }
            return false;
        }
        for (prefix, command) in [
            ("notes.table-add-row:", "table-add-row"),
            ("notes.table-add-column:", "table-add-column"),
        ] {
            if let Some(id) = target.strip_prefix(prefix)
                && matches!(action, crate::a11y::Action::Activate)
                && let Some(index) = self.blocks.iter().position(|block| block.id == id)
            {
                let (row, column) = self
                    .table_editors
                    .get(id)
                    .and_then(super::block_editor_table::DocumentTable::projected_focus)
                    .unwrap_or((0, 0));
                self.begin_edit();
                let changed = self.edit_table_structure(index, row, column, command);
                let ops = self.take_edit_ops();
                self.pending_semantic_ops.extend(ops);
                return changed;
            }
        }
        if let Some(target) = target.strip_prefix("notes.table-cell:") {
            let mut parts = target.rsplitn(3, ':');
            if let (Some(column), Some(row), Some(id)) = (parts.next(), parts.next(), parts.next())
                && let (Ok(row), Ok(column)) = (row.parse::<usize>(), column.parse::<usize>())
                && let Some(index) = self.blocks.iter().position(|block| block.id == id)
            {
                match action {
                    crate::a11y::Action::SetValue(value) => {
                        self.begin_edit();
                        let changed = self.apply_table_cell_edit(index, row, column, value);
                        let ops = self.take_edit_ops();
                        self.pending_semantic_ops.extend(ops);
                        return changed;
                    }
                    crate::a11y::Action::Focus | crate::a11y::Action::Activate
                        if self
                            .table_editors
                            .get_mut(id)
                            .is_some_and(|table| table.focus_projected_cell(row, column)) =>
                    {
                        for (other, table) in &mut self.table_editors {
                            if other != id {
                                table.blur_projection();
                            }
                        }
                        self.clear_cross_block_selection();
                        for block in &mut self.blocks {
                            block.line.set_focus(false);
                        }
                        self.focused = Some(index);
                        self.is_focused = true;
                        self.dirty = true;
                        return true;
                    }
                    _ => {}
                }
            }
            return false;
        }
        if let Some(id) = target.strip_prefix("notes.table-mode:")
            && matches!(action, crate::a11y::Action::Activate)
            && let Some(index) = self.blocks.iter().position(|block| block.id == id)
        {
            return self.toggle_table_mode(index, true);
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        if let Some((id, direction)) = target
            .strip_prefix("notes.block-indent:")
            .and_then(|rest| rest.rsplit_once(':'))
        {
            let delta = match direction {
                "in" => 1,
                "out" => -1,
                _ => return false,
            };
            let Some(index) = self.blocks.iter().position(|block| block.id == id) else {
                return false;
            };
            self.begin_edit();
            let changed = self.try_indent(index, delta);
            let ops = self.take_edit_ops();
            self.pending_semantic_ops.extend(ops);
            return changed;
        }
        if let Some(command) = target.strip_prefix("notes.editor:") {
            return self.edit_from_semantics(command);
        }
        if let Some(id) = target.strip_prefix("notes.image-edit:") {
            return self.set_image_source_mode(id, true);
        }
        if let Some(id) = target.strip_prefix("notes.image-done:") {
            return self.set_image_source_mode(id, false);
        }
        if let Some(id) = target
            .strip_prefix("notes.transclusion:")
            .and_then(|id| id.parse::<PageId>().ok())
        {
            self.pending_transclusion_opens.push(id);
            self.dirty = true;
            return true;
        }
        if let Some(id) = target
            .strip_prefix("notes.page-link:")
            .and_then(|id| id.parse::<PageId>().ok())
        {
            self.pending_transclusion_opens.push(id);
            self.dirty = true;
            return true;
        }
        if let Some(url) = target.strip_prefix("notes.external-link:") {
            self.pending_external_link_opens.push(url.to_string());
            self.dirty = true;
            return true;
        }
        if let Some((start, end)) = self.resolve_code_copy_target(target) {
            return self.copy_code_run(start, end);
        }
        self.move_block_from_semantics(target)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.visual_formula_focus.is_some()
    }

    fn layout_needs_measure(&self) -> bool {
        self.structure_dirty
            || self.dirty
            || self
                .blocks
                .iter()
                .any(|block| block.line.layout_needs_measure())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.paint_extent(), regions);
        }
    }

    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if self.paint_damage.is_recorded() {
            self.drawn_overhang.clone_from(&self.line_overhang);
            self.drawn_table_focus.clone_from(&self.table_focus_paint);
        }
        self.paint_damage.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        let was = self.structure_dirty;
        self.structure_dirty = false;
        was
    }

    /// The editor is a column of one taffy node per line; its own margins
    /// and formula minimums are styles set at build time (a structural
    /// change), so its size can only move when a line's does. Forwarded per
    /// line rather than through `children()`, which the editor does not
    /// expose, so an idle document re-measures nothing.
    fn is_layout_dirty(&self) -> bool {
        self.table_relayout || self.blocks.iter().any(|block| block.line.is_layout_dirty())
    }
}

impl crate::presence::capability::PublishesPresence for BlockEditor {
    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        use crate::collaboration::PresencePayload;
        let idx = self.focused?;
        let block = self.blocks.get(idx)?;
        if !block.line.is_focused() {
            return None;
        }
        if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            if !self.presence_keyed_by_model() {
                let start_id = self.blocks.get(start.block)?.id.as_str();
                return Some((
                    document_selection_presence_key(
                        start_id,
                        end.block.saturating_sub(start.block),
                    ),
                    PresencePayload::Selection {
                        start: start.byte,
                        end: end.byte,
                    },
                ));
            }
            // Keyed by the owning model's ids, a selection is reported as its
            // part in the caret's block, which is where a peer's caret lands.
            let from = if start.block == idx { start.byte } else { 0 };
            let to = if end.block == idx {
                end.byte
            } else {
                block.text().len()
            };
            return Some((
                self.presence_key_for(&block.id),
                PresencePayload::Selection {
                    start: from.min(to),
                    end: to.max(from),
                },
            ));
        }
        let key = self.presence_key_for(&block.id);
        // A selection takes priority over the bare caret.
        if let Some((start, end)) = block.line.selection_range() {
            return Some((key, PresencePayload::Selection { start, end }));
        }
        Some((
            key,
            PresencePayload::Caret {
                pos: block.line.cursor_pos(),
            },
        ))
    }
}

impl BlockEditor {
    /// Rebuilds `self.formula_chrome` (the syntax badge + Source/Visual
    /// button rects, keyed by each formula run's first block) from the
    /// just-laid-out `self.formula_preview_rects`. Split out of
    /// `apply_layout` to keep that under the line cap.
    fn build_formula_chrome_rects(&mut self) {
        let mut start = 0usize;
        while start < self.blocks.len() {
            let Some(descriptor) = self
                .blocks
                .get(start)
                .and_then(|b| formula_descriptor(&b.kind))
            else {
                start = start.saturating_add(1);
                continue;
            };
            let end = formula_run_end(&self.blocks, start);
            if let Some(Some(editor_rect)) = self.formula_preview_rects.get(end).copied()
                && let Some(first) = self.blocks.get(start).map(|b| b.line.get_rect())
                && let Some(last) = self.blocks.get(end).map(|b| b.line.get_rect())
            {
                let header_y = first.y0 - FORMULA_HEADER_HEIGHT;
                let badge = Rect::new(first.x0, header_y + 3.0, first.x0 + 40.0, header_y + 25.0);
                let visual_button =
                    Rect::new(last.x1 - 60.0, header_y + 3.0, last.x1, header_y + 25.0);
                let source_button = Rect::new(
                    visual_button.x0 - 68.0,
                    header_y + 3.0,
                    visual_button.x0 - 4.0,
                    header_y + 25.0,
                );
                if let Some(slot) = self.formula_chrome.get_mut(start) {
                    *slot = Some(FormulaChrome {
                        end,
                        descriptor,
                        syntax_badge: badge,
                        source_button,
                        visual_button,
                        editor_rect,
                    });
                }
            }
            start = end.saturating_add(1);
        }
    }

    /// Peer authorship marker, live-typing glow, carets and selection
    /// bands for one block — collaboration overlays drawn after every
    /// block's own content so they land on top.
    /// `(start index, end index, user, byte range)` of every remote document
    /// selection whose start block exists here; the span is clamped to the
    /// last block.
    fn resolve_document_selections<'p>(
        &self,
        presence: &'p PresenceCtx,
    ) -> Vec<(
        usize,
        usize,
        &'p crate::permissions::UserId,
        crate::presence::SelectionRange,
    )> {
        let mut out = Vec::new();
        for (key, list) in &presence.selections {
            let Some((start_id, span)) = document_selection_parts(key) else {
                continue;
            };
            let Some(start) = self.blocks.iter().position(|block| block.id == start_id) else {
                continue;
            };
            let end = start
                .saturating_add(span)
                .min(self.blocks.len().saturating_sub(1));
            for (user, range) in list {
                out.push((start, end, user, *range));
            }
        }
        out
    }

    fn draw_block_presence(
        &self,
        i: usize,
        b: &Block,
        document_selections: &[(
            usize,
            usize,
            &crate::permissions::UserId,
            crate::presence::SelectionRange,
        )],
        ctx: &mut BlockDrawCtx<'_>,
    ) {
        let key = self.presence_key_for(&b.id);
        let line = &b.line;
        let brect = line.get_rect();
        let _tr_rect = {
            let p0 = transformed(ctx.transform, Point::new(brect.x0, brect.y0));
            let p1 = transformed(ctx.transform, Point::new(brect.x1, brect.y1));
            Rect::new(p0.x, p0.y, p1.x, p1.y)
        };
        // Drawn first, under caret/selection: the runs each foreign
        // author wrote, marker-style in their color.
        for (bs, be, author) in line.author_runs() {
            if self.local_author.as_deref() == Some(author.as_str()) {
                continue;
            }
            let x0 = line.pixel_x_for_byte(ctx.theme, *bs);
            let x1 = line.pixel_x_for_byte(ctx.theme, *be);
            let p0 = transformed(ctx.transform, Point::new(x0, brect.y0));
            let p1 = transformed(ctx.transform, Point::new(x1, brect.y1));
            let user = crate::permissions::UserId(author.clone());
            ctx.presence.draw_peer_authorship(
                ctx.scene,
                ctx.theme,
                &user,
                &[Rect::new(p0.x, p0.y, p1.x, p1.y)],
            );
        }
        // A peer's recently-inserted run glows and decays over
        // TYPING_FADE. Drawn over authorship, under caret/selection.
        let now = crate::time::now();
        for (user, range, alpha) in ctx.presence.typing_marks_for_key(&key, now) {
            if self.local_author.as_deref() == Some(user.0.as_str()) {
                continue;
            }
            let rect = line.selection_rect(ctx.theme, range.start, range.end);
            let p0 = transformed(ctx.transform, Point::new(rect.x0, rect.y0));
            let p1 = transformed(ctx.transform, Point::new(rect.x1, rect.y1));
            ctx.presence.draw_peer_typing(
                ctx.scene,
                ctx.theme,
                &user,
                &[(Rect::new(p0.x, p0.y, p1.x, p1.y), alpha)],
            );
        }
        if let Some(list) = ctx.presence.carets.get(&key) {
            for (user, byte_pos) in list {
                let name = ctx.presence.display_name(user);
                ctx.presence.draw_peer_caret_labeled(
                    ctx.scene,
                    ctx.theme,
                    ctx.text_renderer,
                    ctx.transform,
                    user,
                    &name,
                    line.caret_rect(ctx.theme, *byte_pos),
                );
            }
        }
        if let Some(list) = ctx.presence.selections.get(&key) {
            for (user, range) in list {
                ctx.presence.draw_peer_selection(
                    ctx.scene,
                    ctx.theme,
                    ctx.transform,
                    user,
                    line.selection_rect(ctx.theme, range.start, range.end),
                );
            }
        }
        // Cross-block selection presence is sent once under a document
        // key, then expanded into the same per-line geometry locally.
        for &(start_block, end_block, user, range) in document_selections {
            if i < start_block || i > end_block {
                continue;
            }
            let start = if i == start_block { range.start } else { 0 };
            let end = if i == end_block {
                range.end
            } else {
                line.value().len()
            };
            ctx.presence.draw_peer_selection(
                ctx.scene,
                ctx.theme,
                ctx.transform,
                user,
                line.selection_rect(ctx.theme, start, end),
            );
        }
    }
}

impl BlockEditor {
    /// Intercepts keystrokes while the link popover is open, before the
    /// focused line ever sees them. Returns `true` when the caller
    /// should stop routing this event any further.
    fn handle_link_popover_key(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.link_popover.is_visible()
            || !matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return false;
        }
        let consumed = self.link_popover.handle_key(event);
        if let Some(page) = self.link_popover.take_selection() {
            self.commit_link_selection(&page);
        }
        if !self.link_popover.is_visible() {
            self.link_open_at = None;
            self.link_anchor_caret = None;
        }
        if consumed {
            *ctx.handled = true;
            self.dirty = true;
        }
        consumed
    }

    /// The slash menu owns arrow nav / Enter / Escape / typing-into-filter
    /// while it's visible. Returns `true` when the caller should stop
    /// routing this event any further.
    fn handle_slash_menu_key(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.slash_menu.is_visible()
            || !matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return false;
        }
        let consumed = self.slash_menu.handle_key(event);
        // Drain the pending selection regardless of `consumed` — Enter may have set it.
        if let Some(new_kind) = self.slash_menu.take_selection() {
            self.commit_slash_selection(new_kind);
        }
        if !self.slash_menu.is_visible() {
            self.slash_open_at = None;
            self.slash_anchor_caret = None;
        }
        if consumed {
            *ctx.handled = true;
            self.dirty = true;
        }
        consumed
    }

    /// Select-all is document-scoped, like a browser editing host, rather
    /// than stopping at the focused paragraph's `TextInput`. Returns
    /// `true` when the caller should stop routing this event any further.
    #[expect(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "validated non-empty block and code-run endpoints define the selection bounds"
    )]
    fn try_select_all_shortcut(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::Key;

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if !(ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
            || !c.eq_ignore_ascii_case("a")
            || self.blocks.is_empty()
        {
            return false;
        }
        let whole_note = self.document_selection.is_some_and(|selection| {
            let (first, last) = selection.ordered();
            first.block == 0
                && first.byte == 0
                && last.block + 1 == self.blocks.len()
                && self
                    .blocks
                    .last()
                    .is_some_and(|block| last.byte == block.text().len())
        });
        if !whole_note
            && let Some(index) = self.focused
            && let Some((start, end)) = self.ordinary_code_run(index)
        {
            let anchor = DocumentPosition {
                block: start,
                byte: 0,
            };
            let cursor = DocumentPosition {
                block: end,
                byte: self.blocks[end].text().len(),
            };
            if self.document_selection.map(DocumentSelection::ordered) != Some((anchor, cursor)) {
                self.select_document_range(anchor, cursor);
                *ctx.handled = true;
                return true;
            }
        }
        let end_block = self.blocks.len().saturating_sub(1);
        let end_byte = self
            .blocks
            .get(end_block)
            .map_or(0, |b| b.line.value().len());
        self.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition {
                block: end_block,
                byte: end_byte,
            },
        );
        *ctx.handled = true;
        true
    }

    /// Reset the keyboard-selection anchor on anything but a vertical
    /// Shift+Arrow extend, so the next extend latches a fresh one.
    fn maybe_reset_kbd_sel_anchor(&mut self, event: &WindowEvent, modifiers: Modifiers) {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, NamedKey};

        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let m = modifiers.state();
                let is_extend = m.shift_key()
                    && matches!(
                        logical_key,
                        Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown)
                    );
                if !is_extend {
                    self.kbd_sel_anchor = None;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                self.kbd_sel_anchor = None;
            }
            _ => {}
        }
    }

    /// Give the topmost overlay first refusal of a pointer press. It closes on
    /// an outside click but deliberately lets that click continue into the
    /// editor, matching normal popover focus behavior.
    fn handle_popover_pointer_press(&mut self, ctx: &mut EventCtx<'_>) -> bool {
        if self.slash_menu.is_visible() {
            if let Some(new_kind) = self.slash_menu.click_at(self.last_mouse_pos) {
                self.commit_slash_selection(new_kind);
                self.slash_open_at = None;
                self.slash_anchor_caret = None;
                *ctx.handled = true;
                self.dirty = true;
                return true;
            }
            self.slash_menu.set_visible(false);
            self.slash_open_at = None;
            self.slash_anchor_caret = None;
            self.dirty = true;
        }
        if self.link_popover.is_visible() {
            if let Some(page) = self.link_popover.click_at(self.last_mouse_pos) {
                self.commit_link_selection(&page);
                self.link_open_at = None;
                self.link_anchor_caret = None;
                *ctx.handled = true;
                self.dirty = true;
                return true;
            }
            self.link_popover.set_visible(false);
            self.link_open_at = None;
            self.link_anchor_caret = None;
            self.dirty = true;
        }
        false
    }

    fn begin_block_drag_at_handle(&mut self, ctx: &mut EventCtx<'_>) -> bool {
        let Some(source) = (0..self.blocks.len()).find(|&index| {
            self.block_drag_handle_rect(index)
                .is_some_and(|rect| rect.contains(self.last_mouse_pos))
        }) else {
            return false;
        };
        self.clear_cross_block_selection();
        self.visual_formula_focus = None;
        self.focus_block(source, CursorPlacement::Start);
        self.block_drag = Some(BlockDrag {
            source,
            target: source,
            start: self.last_mouse_pos,
            active: false,
        });
        *ctx.focus_target = Some(ctx.node_id);
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    /// A left click on the slash-menu, a checklist checkbox, or empty
    /// block space (which arms a cross-block drag). Runs BEFORE event
    /// routing so the click doesn't fall through to `TextInput` (which
    /// would place the caret at byte 0). Returns `true` when the caller
    /// should stop routing this event any further.
    fn handle_mouse_press_gestures(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, MouseButton};

        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        if let Some((_, start, end)) = self
            .code_copy_hits
            .iter()
            .copied()
            .find(|(rect, _, _)| rect.contains(self.last_mouse_pos))
            && self.copy_code_run(start, end)
        {
            *ctx.handled = true;
            return true;
        }
        if self.handle_popover_pointer_press(ctx) || self.begin_block_drag_at_handle(ctx) {
            return true;
        }
        // A click on a formula's source/visual toggle, or inside its
        // visual editor (to place a caret there). Checked before the
        // checkbox/cross-block-drag gestures below since the chrome
        // paints on top of the block it belongs to.
        if let Some((start, chrome)) = self
            .formula_chrome
            .iter()
            .enumerate()
            .find_map(|(index, chrome)| chrome.map(|value| (index, value)))
            .filter(|(_, chrome)| {
                chrome.source_button.contains(self.last_mouse_pos)
                    || chrome.visual_button.contains(self.last_mouse_pos)
                    || (chrome.descriptor.mode == crate::math::FormulaMode::Visual
                        && chrome.editor_rect.contains(self.last_mouse_pos))
            })
        {
            if chrome.source_button.contains(self.last_mouse_pos) {
                self.set_formula_mode(start, crate::math::FormulaMode::Source);
            } else if chrome.visual_button.contains(self.last_mouse_pos) {
                self.set_formula_mode(start, crate::math::FormulaMode::Visual);
            } else if let Some(visual) = self.visual_formulas.get_mut(&start)
                && let Some(layout) = &visual.layout
            {
                let origin = visual_formula_origin(chrome.editor_rect, layout);
                if let Some(gap) = crate::math::nearest_gap(
                    &layout.gaps,
                    self.last_mouse_pos.x - origin.x,
                    self.last_mouse_pos.y - origin.y,
                ) {
                    visual.editor.caret.steps.clone_from(&gap.steps);
                    visual.editor.caret.offset = gap.index;
                    visual.editor.anchor = None;
                }
                self.visual_formula_focus = Some(start);
                self.focused = None;
            }
            self.is_focused = true;
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
            self.dirty = true;
            return true;
        }
        self.visual_formula_focus = None;
        if let Some(hit_idx) = self.checkbox_hit_test_last_mouse() {
            let _ = self.try_toggle_check(hit_idx);
            *ctx.handled = true;
            return true;
        }
        // The inner TextInput's own press handler also runs and starts
        // a single-block drag (X within its own rect); this
        // coordinator picks up the slack across block boundaries.
        self.clear_cross_block_selection();
        if let Some(block_idx) = self.block_at_y(self.last_mouse_pos.y)
            && let Some(b) = self.blocks.get_mut(block_idx)
        {
            let byte = b.line.inner().byte_at_point(self.last_mouse_pos);
            self.cross_block_drag_start = Some((block_idx, byte));
        }
        false
    }

    /// Tracks the pointer for slash-menu hover and cross-block drag
    /// painting.
    fn handle_cursor_moved(&mut self, event: &WindowEvent, scale_factor: f64) {
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            if self.block_drag.is_none() {
                self.last_mouse_pos = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
            }
            self.dirty = true;
            return;
        }
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
        let previous_hover = self.hovered_block();
        let previous_handle = self.hovered_drag_handle();
        self.last_mouse_pos = logical;
        if previous_hover != self.hovered_block() || previous_handle != self.hovered_drag_handle() {
            self.dirty = true;
        }

        // Hovering a slash-menu row moves the highlight to it, so
        // mouse and keyboard drive the same selection cursor.
        if self.slash_menu.is_visible() && self.slash_menu.hover_at(logical) {
            self.dirty = true;
        }
        if self.link_popover.is_visible() && self.link_popover.hover_at(logical) {
            self.dirty = true;
        }

        if let Some(mut drag) = self.block_drag {
            self.dirty = true;
            let dx = logical.x - drag.start.x;
            let dy = logical.y - drag.start.y;
            if !drag.active && dx.hypot(dy) >= 4.0 {
                drag.active = true;
            }
            if drag.active
                && let Some(target) = self.block_at_y(logical.y)
                && target != drag.target
            {
                drag.target = target;
                self.dirty = true;
            }
            self.block_drag = Some(drag);
            return;
        }

        // Per-block drag stays under the start TextInput's control
        // inside its own rect; this only adds a band on the OTHER
        // blocks the pointer crossed into.
        if let Some((start_block, start_byte)) = self.cross_block_drag_start {
            let any_dragging = self.blocks.iter().any(|b| b.line.inner().is_dragging());
            if any_dragging && let Some(end_block) = self.block_at_y(logical.y) {
                let end_byte = self
                    .blocks
                    .get_mut(end_block)
                    .map_or(0, |b| b.line.inner().byte_at_point(logical));
                self.paint_cross_block_selection(start_block, start_byte, end_block, end_byte);
            }
        }
    }

    /// `TextInput` does NOT handle Tab, but intercept anyway so future
    /// `TextInput` changes don't break the contract. Returns `true` when
    /// the caller should stop routing this event any further.
    fn try_tab_indent(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, NamedKey};

        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let Some(focused_idx) = self.focused else {
            return false;
        };
        if !self.is_focused || self.tab_navigation || ctx.modifiers.state().alt_key() {
            return false;
        }
        let command = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        let delta = match logical_key {
            Key::Character(key) if command && key == "]" => 1,
            Key::Character(key) if command && key == "[" => -1,
            Key::Named(NamedKey::Tab)
                if !command
                    && (self.document_selection.is_some()
                        || self.blocks.get(focused_idx).is_some_and(|b| {
                            !matches!(
                                b.kind,
                                BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                            )
                        })) =>
            {
                if ctx.modifiers.state().shift_key() {
                    -1
                } else {
                    1
                }
            }
            _ => return false,
        };
        self.try_indent_selection(delta);
        *ctx.handled = true;
        true
    }

    /// The leading `/` still falls through to `TextInput` (visible
    /// feedback); `slash_open_at` remembers where to strip it on commit.
    /// Code blocks excluded — `/` there is meaningful text.
    fn try_open_slash_menu(&mut self, event: &WindowEvent) {
        use winit::event::{ElementState, Ime, KeyEvent};
        use winit::keyboard::Key;

        if self.slash_menu.is_visible() {
            return;
        }
        let character = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => c.as_str(),
            // Browser beforeinput delivers ordinary text as a commit. Preedit
            // and larger insertions remain text, not command activation.
            WindowEvent::Ime(Ime::Commit(text)) => text.as_str(),
            _ => return,
        };
        if character != "/" {
            return;
        }
        let Some(focused_idx) = self.focused else {
            return;
        };
        let can_open = self.blocks.get(focused_idx).is_some_and(|b| {
            !matches!(b.kind, BlockKind::CodeBlock { .. }) && b.kind.is_editable()
        });
        if !can_open {
            return;
        }
        // Typing replaces a local selection at its normalized start, which
        // can precede the caret for a forward selection.
        let trigger_byte = self.blocks.get(focused_idx).map_or(0, |block| {
            block
                .line
                .selection_range()
                .map_or(block.line.cursor_pos(), |(start, _)| start)
        });
        self.slash_menu
            .set_anchor(self.fallback_popover_anchor(focused_idx));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = Some(trigger_byte);
        self.slash_anchor_caret = Some((focused_idx, trigger_byte));
    }

    /// Focus whichever editable block a left click lands on first, so the
    /// later `TextInput` click handler sets the caret there.
    fn focus_block_under_mouse(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        use winit::event::{ElementState, MouseButton};

        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        let hit = self
            .blocks
            .iter()
            .position(|b| b.kind.is_editable() && b.line.get_rect().contains(self.last_mouse_pos));
        let Some(idx) = hit else {
            return;
        };
        if self.focused == Some(idx) {
            // A programmatic focus (binding a page places the caret) leaves
            // the block and its line focused without the host keyboard owner
            // following. A click on it still names this editor the owner;
            // re-claiming an already owned focus is a host no-op.
            *ctx.focus_target = Some(ctx.node_id);
            return;
        }
        if let Some(prev) = self.focused
            && let Some(b) = self.blocks.get_mut(prev)
        {
            b.line.set_focus(false);
        }
        self.focused = Some(idx);
        self.is_focused = true;
        if let Some(b) = self.blocks.get_mut(idx) {
            b.line.set_focus(true);
            // Only the previously focused line saw CursorMoved. Forward the
            // current pointer before the new line handles this same press;
            // otherwise its hover gate rejects it or uses a stale position.
            b.line
                .inner_mut()
                .update_pointer_position(self.last_mouse_pos);
        }
        *ctx.focus_target = Some(ctx.node_id);
    }

    /// Routes the event into the focused block's own `TextInput`, drains
    /// any structural signal it emitted, and promotes `# `/`## `/`### `
    /// paragraphs to headings on the keystroke that completes the prefix.
    #[expect(
        clippy::indexing_slicing,
        reason = "the focused index is validated before it is used"
    )]
    fn route_event_to_focused_block(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        ) && !ctx.modifiers.state().shift_key()
            && !ctx.modifiers.state().control_key()
            && !ctx.modifiers.state().super_key()
            && !ctx.modifiers.state().alt_key()
            && self.document_selection.is_none()
            && let Some(index) = self.focused
            && self.blocks.get(index).is_some_and(|block| {
                matches!(block.kind, BlockKind::CodeBlock { .. })
                    && block.text().trim() == "```"
                    && formula_descriptor(&block.kind).is_none()
                    && block.line.cursor_pos() == block.text().len()
                    && block.line.selection_range().is_none()
            })
        {
            self.try_convert_to(index, BlockKind::Paragraph);
            self.blocks[index].line.set_text("");
            self.blocks[index].line.set_cursor_pos(0);
            self.edit_pending = true;
            self.commit_pending = true;
            *ctx.handled = true;
            return;
        }
        if self.handle_visual_arrow(event, ctx) {
            return;
        }
        let Some(focused_idx) = self.focused else {
            return;
        };
        // Snapshot the text to distinguish an actual content change
        // (typing) from a mere caret move (a click). Markdown shortcuts
        // fire only on content changes; bullets also accept task markers.
        let before_text = self
            .blocks
            .get(focused_idx)
            .filter(|b| matches!(b.kind, BlockKind::Paragraph | BlockKind::Bullet { .. }))
            .map(|b| b.line.value().to_string());
        if let Some(b) = self.blocks.get_mut(focused_idx)
            && b.kind.is_editable()
        {
            b.line.handle_event(event, ctx);
        }
        if let Some(sig) = self
            .blocks
            .get_mut(focused_idx)
            .and_then(|b| b.line.take_signal())
        {
            self.apply_signal(focused_idx, &sig);
        }
        // Promote only when the text changed and everything before the
        // caret is a complete marker. Clicking after an existing marker
        // or typing a marker in the middle of prose does not promote.
        if let Some(before) = before_text
            && let Some(b) = self.blocks.get(focused_idx)
        {
            let now = b.line.value();
            if now != before {
                let caret = b.line.cursor_pos().min(now.len());
                if let Some((kind, prefix_len)) = typed_list_prefix(&b.kind, now, caret) {
                    self.try_convert_to(focused_idx, kind);
                    let line = &mut self.blocks[focused_idx].line;
                    Self::remove_line_range(line, 0..prefix_len);
                    self.edit_pending = true;
                    self.commit_pending = true;
                } else if now.get(..caret).is_some_and(is_heading_prefix) {
                    let _ = self.try_promote_to_heading(focused_idx);
                } else if now.get(..caret) == Some("> ") {
                    let _ = self.try_promote_to_quote(focused_idx);
                } else if matches!(b.kind, BlockKind::Paragraph)
                    && caret == now.len()
                    && let Some(language) = now.strip_prefix("```")
                    && language.chars().all(is_fence_language_character)
                {
                    let lang = (!language.is_empty()).then(|| language.to_string());
                    let block = &mut self.blocks[focused_idx];
                    let bound = block.line.bound_obj();
                    block.kind = BlockKind::CodeBlock { lang: lang.clone() };
                    block.code_group = Some(block.id.clone());
                    block.line = RichTextLine::new().with_size(block.kind.font_size());
                    if let Some(bound) = bound {
                        block.line.bind_text(bound);
                    }
                    block.line.set_mono(true);
                    block.line.set_inline_formatting(false);
                    block.line.set_focus(true);
                    self.opening_code_fence = Some((block.id.clone(), lang));
                    self.structure_dirty = true;
                    self.dirty = true;
                    self.edit_pending = true;
                    self.commit_pending = true;
                }
            }
        }
    }
}

fn is_fence_language_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+' | '#' | '.')
}

/// Recognize only a complete marker before the caret. A bullet can become a
/// task after its `- ` marker has already promoted, preserving its indentation.
fn typed_list_prefix(kind: &BlockKind, text: &str, caret: usize) -> Option<(BlockKind, usize)> {
    let prefix = text.get(..caret)?;
    let indent = kind.indent();
    let task = match prefix {
        "[ ] " => Some(false),
        "[x] " | "[X] " => Some(true),
        _ => None,
    };
    if matches!(kind, BlockKind::Paragraph | BlockKind::Bullet { .. })
        && let Some(done) = task
    {
        return Some((BlockKind::Checklist { indent, done }, prefix.len()));
    }
    if !matches!(kind, BlockKind::Paragraph) {
        return None;
    }
    let new_kind = match prefix {
        "- " | "* " | "+ " => BlockKind::Bullet { indent: 0 },
        "1. " | "1) " => BlockKind::Numbered { indent: 0 },
        _ => return None,
    };
    Some((new_kind, prefix.len()))
}

fn code_header_height(blocks: &[Block], index: usize, scale: f64) -> f64 {
    let Some(block) = blocks.get(index) else {
        return 0.0;
    };
    if !matches!(block.kind, BlockKind::CodeBlock { .. })
        || formula_descriptor(&block.kind).is_some()
    {
        return 0.0;
    }
    if index
        .checked_sub(1)
        .and_then(|previous| blocks.get(previous))
        .is_some_and(|previous| previous.same_code_group(block))
    {
        return 0.0;
    }
    CODE_HEADER_HEIGHT * scale
}

#[path = "block_editor_present.rs"]
mod present;
pub use present::{BlockContent, EditScope, PresenceKeyFn};

#[cfg(test)]
#[path = "block_editor_fence_tests.rs"]
mod fence_typing_tests;

#[cfg(test)]
#[path = "block_editor_affordance_tests.rs"]
mod affordance_tests;

#[cfg(test)]
#[path = "block_editor_code_keyboard_tests.rs"]
mod code_keyboard_tests;

#[cfg(test)]
#[path = "block_editor_retained_layout_tests.rs"]
mod retained_layout_tests;

#[cfg(test)]
#[path = "block_editor_outline_tests.rs"]
mod outline_tests;

#[cfg(test)]
#[path = "block_editor_image_paste_tests.rs"]
mod image_paste_tests;

impl BlockEditor {
    /// The current representation uses one stable block per source line.
    /// Keyboard commands operate on one authored fence, across its source rows.
    fn ordinary_code_run(&self, index: usize) -> Option<(usize, usize)> {
        let kind = &self.blocks.get(index)?.kind;
        if !matches!(kind, BlockKind::CodeBlock { .. }) || formula_descriptor(kind).is_some() {
            return None;
        }
        self.fenced_code_run(index)
    }

    #[expect(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "the seed index is validated and both scans maintain in-bounds neighboring indices"
    )]
    fn fenced_code_run(&self, index: usize) -> Option<(usize, usize)> {
        let kind = &self.blocks.get(index)?.kind;
        if !matches!(kind, BlockKind::CodeBlock { .. }) {
            return None;
        }
        let mut start = index;
        let mut end = index;
        while start > 0 && self.blocks[start - 1].same_code_group(&self.blocks[index]) {
            start -= 1;
        }
        while end + 1 < self.blocks.len()
            && self.blocks[end + 1].same_code_group(&self.blocks[index])
        {
            end += 1;
        }
        Some((start, end))
    }

    #[expect(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "ordinary_code_run validates the complete inclusive run and its focused seed"
    )]
    fn exit_code_run(&mut self, index: usize, before: bool) {
        let Some((start, end)) = self.ordinary_code_run(index) else {
            return;
        };
        self.clear_cross_block_selection();
        self.opening_code_fence = None;
        let adjacent = if before {
            start.checked_sub(1)
        } else {
            Some(end + 1)
        };
        if let Some(target) = adjacent.filter(|&target| {
            self.blocks.get(target).is_some_and(|block| {
                block.kind == BlockKind::Paragraph && block.indent() == self.blocks[index].indent()
            })
        }) {
            self.focus_block(
                target,
                if before {
                    CursorPlacement::End
                } else {
                    CursorPlacement::Start
                },
            );
            return;
        }
        let target = if before { start } else { end + 1 };
        let mut paragraph = Block::paragraph("");
        paragraph.set_indent(self.blocks[index].indent());
        self.blocks.insert(target, paragraph);
        self.code_keyboard_edited();
        self.focus_block(target, CursorPlacement::Start);
    }

    fn code_keyboard_edited(&mut self) {
        self.edit_pending = true;
        self.commit_pending = true;
        self.structure_dirty = true;
        self.dirty = true;
    }

    #[expect(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "one cohesive keyboard transaction operates on an already validated inclusive code run"
    )]
    fn handle_code_keyboard(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        if !self.is_focused || event.state != ElementState::Pressed {
            return false;
        }
        let Some(index) = self.focused else {
            return false;
        };
        let Some((start, end)) = self.ordinary_code_run(index) else {
            return false;
        };
        let modifiers = ctx.modifiers.state();
        if modifiers.alt_key() {
            return false;
        }
        let command = modifiers.control_key() || modifiers.super_key();
        if command
            && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("v"))
        {
            if let Some(text) = clipboard_paste() {
                if self.document_selection.is_none() {
                    let line = &self.blocks[index].line;
                    let (start, end) = line
                        .selection_range()
                        .unwrap_or_else(|| (line.cursor_pos(), line.cursor_pos()));
                    self.document_selection = Some(DocumentSelection {
                        anchor: DocumentPosition {
                            block: index,
                            byte: start,
                        },
                        cursor: DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    });
                }
                self.replace_document_selection(&text);
            }
            *ctx.handled = true;
            return true;
        }
        if command
            && modifiers.shift_key()
            && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("c"))
        {
            self.copy_code_run(start, end);
            *ctx.handled = true;
            return true;
        }
        if event.logical_key == Key::Named(NamedKey::Enter) && command {
            self.exit_code_run(index, modifiers.shift_key());
            *ctx.handled = true;
            return true;
        }
        if command {
            return false;
        }
        if event.logical_key == Key::Named(NamedKey::Tab) {
            let selection = self.document_selection;
            let local_selection = self.blocks[index].line.selection_range();
            let (first, last) = selection.map_or((index, index), |selection| {
                let (first, last) = selection.ordered();
                // An endpoint at column zero does not select that final line.
                (
                    first.block,
                    if last.byte == 0 && last.block > first.block {
                        last.block - 1
                    } else {
                        last.block
                    },
                )
            });
            if first < start || last > end {
                return false;
            }
            let whole_lines =
                selection.is_some() || local_selection.is_some() || modifiers.shift_key();
            let mut changes = Vec::new();
            for line_index in first..=last {
                let line = &mut self.blocks[line_index].line;
                let old_cursor = line.cursor_pos();
                let mut text = line.value().to_owned();
                let (at, removed, inserted) = if modifiers.shift_key() {
                    let removed = if text.starts_with('\t') {
                        1
                    } else {
                        text.bytes()
                            .take(4)
                            .take_while(|byte| *byte == b' ')
                            .count()
                    };
                    text.drain(..removed);
                    (0, removed, 0)
                } else {
                    let at = if whole_lines {
                        0
                    } else {
                        text.floor_char_boundary(old_cursor)
                    };
                    text.insert_str(at, "    ");
                    (at, 0, 4)
                };
                if removed > 0 || inserted > 0 {
                    line.set_text(&text);
                    line.set_cursor_pos(if old_cursor >= at {
                        old_cursor.saturating_sub(removed) + inserted
                    } else {
                        old_cursor
                    });
                }
                changes.push((line_index, removed, inserted));
            }
            let adjust = |position: DocumentPosition| {
                let byte = changes
                    .iter()
                    .find(|(line, _, _)| *line == position.block)
                    .map_or(position.byte, |(_, removed, inserted)| {
                        position.byte.saturating_sub(*removed) + inserted
                    });
                DocumentPosition { byte, ..position }
            };
            if let Some(selection) = selection {
                let anchor = adjust(selection.anchor);
                let cursor = adjust(selection.cursor);
                self.paint_cross_block_selection(
                    anchor.block,
                    anchor.byte,
                    cursor.block,
                    cursor.byte,
                );
                self.kbd_sel_anchor = Some((anchor.block, anchor.byte));
            } else if let Some((a, b)) = local_selection {
                let a = adjust(DocumentPosition {
                    block: index,
                    byte: a,
                })
                .byte;
                let b = adjust(DocumentPosition {
                    block: index,
                    byte: b,
                })
                .byte;
                self.blocks[index].line.inner_mut().set_selection(a, b);
            }
            if changes
                .iter()
                .any(|(_, removed, inserted)| removed + inserted > 0)
            {
                self.code_keyboard_edited();
            }
            *ctx.handled = true;
            return true;
        }
        if self.document_selection.is_none() && self.blocks[index].line.selection_range().is_none()
        {
            if event.logical_key == Key::Named(NamedKey::Enter)
                && !modifiers.shift_key()
                && index == end
                && self.blocks[index].text().trim().is_empty()
            {
                self.try_convert_to(index, BlockKind::Paragraph);
                self.blocks[index].line.set_text("");
                self.blocks[index].line.set_cursor_pos(0);
                self.code_keyboard_edited();
                *ctx.handled = true;
                return true;
            }
            if event.logical_key == Key::Named(NamedKey::Backspace)
                && index == start
                && self.blocks[index].line.cursor_pos() == 0
            {
                if self.blocks[index].text().is_empty() {
                    self.try_convert_to(index, BlockKind::Paragraph);
                    self.code_keyboard_edited();
                }
                *ctx.handled = true;
                return true;
            }
        }
        false
    }

    fn handle_opening_code_fence(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.is_focused {
            self.opening_code_fence = None;
            return false;
        }
        let Some((id, expected_language)) = self.opening_code_fence.clone() else {
            return false;
        };
        let Some(index) = self.focused.filter(|index| {
            self.blocks.get(*index).is_some_and(|block| {
                block.id == id
                    && block.text().is_empty()
                    && block.kind
                        == (BlockKind::CodeBlock {
                            lang: expected_language.clone(),
                        })
            })
        }) else {
            self.opening_code_fence = None;
            return false;
        };
        let mut language = expected_language.unwrap_or_default();
        match event {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if matches!(
                    event.logical_key,
                    Key::Named(
                        NamedKey::Shift
                            | NamedKey::Control
                            | NamedKey::Alt
                            | NamedKey::Super
                            | NamedKey::CapsLock
                    )
                ) {
                    return false;
                }
                if ctx.modifiers.state().control_key()
                    || ctx.modifiers.state().super_key()
                    || ctx.modifiers.state().alt_key()
                {
                    self.opening_code_fence = None;
                    return false;
                }
                match &event.logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                        self.opening_code_fence = None;
                        *ctx.handled = true;
                        return true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if language.pop().is_none() {
                            self.try_convert_to(index, BlockKind::Paragraph);
                            let Some(block) = self.blocks.get_mut(index) else {
                                return false;
                            };
                            block.line.set_text("``");
                            block.line.set_cursor_pos(2);
                            self.opening_code_fence = None;
                            self.edit_pending = true;
                            self.commit_pending = true;
                            *ctx.handled = true;
                            return true;
                        }
                    }
                    Key::Character(text) if text.chars().all(is_fence_language_character) => {
                        language.push_str(text);
                    }
                    _ => {
                        self.opening_code_fence = None;
                        return false;
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text))
                if text.chars().all(is_fence_language_character) =>
            {
                language.push_str(text);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
            | WindowEvent::Focused(false) => {
                self.opening_code_fence = None;
                return false;
            }
            _ => return false,
        }
        let lang = (!language.is_empty()).then_some(language);
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        block.kind = BlockKind::CodeBlock { lang: lang.clone() };
        self.opening_code_fence = Some((id, lang));
        self.structure_dirty = true;
        self.dirty = true;
        self.edit_pending = true;
        self.commit_pending = true;
        *ctx.handled = true;
        true
    }
}

impl BlockEditor {
    fn edit_table_structure(
        &mut self,
        index: usize,
        row: usize,
        column: usize,
        command: &str,
    ) -> bool {
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        if !matches!(block.kind, BlockKind::Table { .. }) {
            return false;
        }
        let source = block.text().to_owned();
        let mut rows: Vec<Vec<String>> = markdown_table_data_rows(&source)
            .into_iter()
            .map(|row| row.into_iter().map(str::to_owned).collect())
            .collect();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if row >= rows.len() || column >= columns {
            return false;
        }
        for row in &mut rows {
            row.resize(columns, String::new());
        }
        let mut divider: Vec<String> = markdown_table_rows(&source)
            .get(1)
            .filter(|row| markdown_table_separator(row))
            .map_or_else(
                || vec!["---".to_owned(); columns],
                |row| row.iter().map(|value| (*value).to_owned()).collect(),
            );
        divider.resize(columns, "---".to_owned());
        let mut focus_row = row;
        let mut focus_column = column;
        match command {
            "table-add-row" => {
                focus_row = row.saturating_add(1);
                rows.insert(focus_row, vec![String::new(); columns]);
            }
            "table-add-column" => {
                focus_column = column.saturating_add(1);
                for row in &mut rows {
                    row.insert(focus_column, String::new());
                }
                divider.insert(focus_column, "---".into());
            }
            "table-delete-row" if rows.len() > 1 => {
                rows.remove(row);
                focus_row = row.min(rows.len().saturating_sub(1));
            }
            "table-delete-column" if columns > 1 => {
                for row in &mut rows {
                    row.remove(column);
                }
                divider.remove(column);
                focus_column = column.min(columns.saturating_sub(2));
            }
            _ => return false,
        }
        let mut lines = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            lines.push(format!("| {} |", row.join(" | ")));
            if index == 0 {
                lines.push(format!("| {} |", divider.join(" | ")));
            }
        }
        let mut updated = lines.join("\n");
        if source.ends_with('\n') {
            updated.push('\n');
        }
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        block.line.set_text(&updated);
        let block_id = block.id.clone();
        let visual = matches!(block.kind, BlockKind::Table { visual: true });
        for table in self.table_editors.values_mut() {
            table.blur_projection();
        }
        let projected = if visual {
            let table = self
                .table_editors
                .entry(block_id.clone())
                .or_insert_with(|| {
                    crate::widget::block_editor_table::DocumentTable::new(format!(
                        "table-{block_id}"
                    ))
                });
            let decoded: Vec<Vec<String>> = rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| decode_table_cell(&value))
                        .collect()
                })
                .collect();
            table.project_cells(&decoded);
            table.focus_projected_cell(focus_row, focus_column)
        } else {
            false
        };
        self.clear_cross_block_selection();
        for (block_index, block) in self.blocks.iter_mut().enumerate() {
            block.line.set_focus(!projected && block_index == index);
        }
        self.focused = Some(index);
        self.is_focused = true;
        self.edit_pending = true;
        self.commit_pending = true;
        self.structure_dirty = true;
        self.dirty = true;
        true
    }

    fn apply_table_cell_edit(
        &mut self,
        index: usize,
        row: usize,
        column: usize,
        value: &str,
    ) -> bool {
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        if !matches!(block.kind, BlockKind::Table { .. }) {
            return false;
        }
        let mut source = block.text().to_owned();
        let rows: Vec<_> = markdown_table_ranges(&source)
            .into_iter()
            .enumerate()
            .filter(|(index, row)| {
                let values: Vec<_> = row
                    .iter()
                    .filter_map(|range| source.get(range.clone()))
                    .collect();
                *index != 1 || !markdown_table_separator(&values)
            })
            .map(|(_, row)| row)
            .collect();
        if column >= rows.iter().map(Vec::len).max().unwrap_or(0) {
            return false;
        }
        let Some(ranges) = rows.get(row) else {
            return false;
        };
        if let Some(current) = ranges
            .get(column)
            .and_then(|range| source.get(range.clone()))
            && decode_table_cell(current) == value
        {
            return false;
        }
        let encoded = encode_table_cell(value);
        if let Some(range) = ranges.get(column) {
            source.replace_range(range.clone(), &encoded);
        } else {
            let (Some(first), Some(last)) = (ranges.first(), ranges.last()) else {
                return false;
            };
            let mut cells: Vec<_> = ranges
                .iter()
                .filter_map(|range| source.get(range.clone()).map(str::to_owned))
                .collect();
            cells.resize(column.saturating_add(1), String::new());
            if let Some(cell) = cells.get_mut(column) {
                *cell = encoded;
            }
            source.replace_range(first.start..last.end, &cells.join(" | "));
        }
        if self
            .blocks
            .get(index)
            .is_some_and(|block| block.text() == source)
        {
            return false;
        }
        if let Some(block) = self.blocks.get_mut(index) {
            // The line measures the table from its text, so a cell that now
            // wraps relayouts the editor; no widget came or went, so the
            // tree is not rebuilt (which would repaint the window per key).
            block.line.set_text(&source);
            block.line.forget_stacked_height();
        }
        self.table_relayout = true;
        self.edit_pending = true;
        self.commit_pending = true;
        self.dirty = true;
        true
    }

    fn handle_visual_table_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let mut toggle = None;
        let mut structural = None;
        for (id, control) in &mut self.table_mode_controls {
            control.button.handle_event(event, ctx);
            if control.requested.replace(false) {
                toggle = Some(id.clone());
            }
            control.add_row.handle_event(event, ctx);
            control.add_column.handle_event(event, ctx);
            if let Some(row) = control.structure.take() {
                structural = Some((id.clone(), row));
            }
        }
        if let Some((id, add_row)) = structural
            && let Some(index) = self.blocks.iter().position(|block| block.id == id)
        {
            let (row, column) = self
                .table_editors
                .get(&id)
                .and_then(super::block_editor_table::DocumentTable::projected_focus)
                .unwrap_or((0, 0));
            self.edit_table_structure(
                index,
                row,
                column,
                if add_row {
                    "table-add-row"
                } else {
                    "table-add-column"
                },
            );
            return true;
        }
        if let Some(id) = toggle
            && let Some(index) = self.blocks.iter().position(|block| block.id == id)
        {
            self.toggle_table_mode(index, false);
            return true;
        }
        if *ctx.handled {
            return true;
        }
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        let mut change = None;
        let mut focused = None;
        let mut exit = None;
        for (index, block) in self.blocks.iter().enumerate() {
            if let Some(table) = self.table_editors.get_mut(&block.id) {
                if let Some((row, column, value)) = table.handle_projected_event(event, ctx) {
                    change = Some((index, row, column, value));
                }
                if table.projected_focus().is_some() {
                    focused = Some(index);
                }
                if let Some(down) = table.take_projection_exit() {
                    exit = Some((index, down));
                }
            }
        }
        if let Some(index) = focused {
            self.clear_cross_block_selection();
            self.focused = Some(index);
            self.is_focused = true;
            for block in &mut self.blocks {
                block.line.set_focus(false);
            }
            self.dirty = true;
        }
        if let Some((index, row, column, value)) = change {
            self.apply_table_cell_edit(index, row, column, &value);
        }
        if let Some((index, down)) = exit {
            let at_boundary = if down {
                index.saturating_add(1) == self.blocks.len()
            } else {
                index == 0
            };
            if at_boundary {
                self.focused = None;
                self.is_focused = false;
                if matches!(event, WindowEvent::KeyboardInput { event, .. } if matches!(event.logical_key, Key::Named(NamedKey::Tab)))
                {
                    *ctx.handled = false;
                }
            } else {
                self.focus_adjacent_visual_row(index, down);
            }
        }
        exit.is_some() || *ctx.handled
    }

    fn toggle_table_mode(&mut self, index: usize, semantic: bool) -> bool {
        let Some(BlockKind::Table { visual }) =
            self.blocks.get(index).map(|block| block.kind.clone())
        else {
            return false;
        };
        if semantic {
            self.begin_edit();
        }
        let changed = self.try_convert_to(index, BlockKind::Table { visual: !visual });
        if semantic {
            let ops = self.take_edit_ops();
            self.pending_semantic_ops.extend(ops);
        }
        if visual {
            self.focus_block(index, CursorPlacement::Start);
        }
        changed
    }

    /// A wrapped editor owns one selection and preferred column across all
    /// paragraphs. Child inputs must not independently extend a stale band.
    fn handle_visual_arrow(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        use winit::event::ElementState;
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        let modifiers = ctx.modifiers.state();
        if !self.wrap_lines
            || !self.is_focused
            || event.state != ElementState::Pressed
            || modifiers.control_key()
            || modifiers.super_key()
            || modifiers.alt_key()
        {
            return false;
        }
        let down = match event.logical_key {
            Key::Named(NamedKey::ArrowDown) => true,
            Key::Named(NamedKey::ArrowUp) => false,
            _ => return false,
        };
        let Some(source) = self.focused else {
            return false;
        };
        let Some(block) = self.blocks.get(source) else {
            return false;
        };
        let input = block.line.inner();
        let caret = input.caret_position();
        let goal = input.vertical_goal_x();
        let local_anchor = block
            .line
            .selection_range()
            .map_or(
                caret.byte,
                |(start, end)| {
                    if caret.byte == start { end } else { start }
                },
            );
        let anchor = self
            .document_selection
            .map(|selection| (selection.anchor.block, selection.anchor.byte))
            .or(self.kbd_sel_anchor)
            .unwrap_or((source, local_anchor));
        let within = input.has_visual_row(down);
        if within {
            if let Some(block) = self.blocks.get_mut(source) {
                block.line.inner_mut().move_visual_row(down);
            }
        } else {
            self.focus_adjacent_visual_row(source, down);
        }
        let target = self.focused.unwrap_or(source);
        let Some(position) = self
            .blocks
            .get(target)
            .map(|block| block.line.inner().caret_position())
        else {
            return false;
        };
        if modifiers.shift_key() {
            self.kbd_sel_anchor = Some(anchor);
            self.paint_cross_block_selection(anchor.0, anchor.1, target, position.byte);
            if let Some(block) = self.blocks.get_mut(target) {
                let local_anchor = match target.cmp(&anchor.0) {
                    std::cmp::Ordering::Equal => anchor.1,
                    std::cmp::Ordering::Greater => 0,
                    std::cmp::Ordering::Less => block.line.value().len(),
                };
                block
                    .line
                    .inner_mut()
                    .set_selection_at(local_anchor, position);
            }
        } else {
            self.kbd_sel_anchor = None;
        }
        if let Some(block) = self.blocks.get_mut(target) {
            block.line.inner_mut().set_vertical_goal_x(goal);
        }
        self.dirty = true;
        *ctx.handled = true;
        true
    }

    fn apply_signal(&mut self, idx: usize, sig: &LineSignal) {
        match sig {
            LineSignal::Enter { split_at } => {
                if self.split_code_with_indent(idx, *split_at) {
                    return;
                }
                if self.exit_empty_outline_block(idx) {
                    return;
                }
                if !self.insert_table_newline(idx, *split_at) {
                    self.split_at_cursor(idx, *split_at);
                }
            }
            LineSignal::SoftNewline { split_at } => {
                if self.split_code_with_indent(idx, *split_at) {
                    return;
                }
                if self.insert_table_newline(idx, *split_at) {
                    return;
                }
                // Same as Enter, but marks the new block soft_break.
                self.split_at_cursor(idx, *split_at);
                let new_idx = idx.saturating_add(1);
                if let Some(b) = self.blocks.get_mut(new_idx) {
                    b.soft_break = true;
                }
            }
            LineSignal::BackspaceAtStart => {
                if self.blocks.get(idx).is_some_and(|block| block.indent() > 0) {
                    self.try_indent(idx, -1);
                    return;
                }
                if self.blocks.get(idx).is_some_and(|block| {
                    matches!(block.kind, BlockKind::Heading(_) | BlockKind::Quote)
                        || block.kind.supports_indent()
                }) {
                    self.try_convert_to(idx, BlockKind::Paragraph);
                    self.edit_pending = true;
                    self.commit_pending = true;
                    return;
                }
                if idx == 0 {
                    return;
                }
                let block_empty = self
                    .blocks
                    .get(idx)
                    .is_some_and(|b| b.line.value().is_empty());
                if block_empty {
                    self.delete_empty(idx);
                } else {
                    self.merge_with_previous(idx);
                }
            }
            LineSignal::ArrowUp => {
                self.kbd_sel_anchor = None;
                if idx > 0 {
                    self.focus_adjacent_visual_row(idx, false);
                }
            }
            LineSignal::ArrowDown => {
                self.kbd_sel_anchor = None;
                let next_idx = idx.saturating_add(1);
                if next_idx < self.blocks.len() {
                    self.focus_adjacent_visual_row(idx, true);
                }
            }
            LineSignal::SelectBlockUp { caret } => {
                self.extend_block_selection(idx, *caret, -1);
            }
            LineSignal::SelectBlockDown { caret } => {
                self.extend_block_selection(idx, *caret, 1);
            }
            LineSignal::LinkOpen { open_at } => {
                self.link_popover
                    .set_anchor(self.fallback_popover_anchor(idx));
                self.link_popover.set_filter("");
                self.link_popover.set_visible(true);
                self.link_open_at = Some(*open_at);
                let caret = self
                    .blocks
                    .get(idx)
                    .map_or(*open_at, |block| block.line.cursor_pos());
                self.link_anchor_caret = Some((idx, caret));
            }
        }
    }

    fn split_code_with_indent(&mut self, index: usize, split: usize) -> bool {
        if self.ordinary_code_run(index).is_none() {
            return false;
        }
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        let text = block.text();
        let split = text.floor_char_boundary(split);
        let indent: String = text
            .get(..split)
            .unwrap_or_default()
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect();
        self.split_at_cursor(index, split);
        let Some(line) = self
            .blocks
            .get_mut(index.saturating_add(1))
            .map(|block| &mut block.line)
        else {
            return false;
        };
        let text = format!("{indent}{}", line.value());
        line.set_text(&text);
        line.set_cursor_pos(indent.len());
        self.code_keyboard_edited();
        true
    }

    fn focus_adjacent_visual_row(&mut self, source: usize, down: bool) {
        let target = if down {
            (source.saturating_add(1)..self.blocks.len()).find(|&index| {
                self.blocks
                    .get(index)
                    .is_some_and(|block| block.kind.is_editable())
            })
        } else {
            (0..source).rev().find(|&index| {
                self.blocks
                    .get(index)
                    .is_some_and(|block| block.kind.is_editable())
            })
        };
        let Some(target) = target else {
            return;
        };
        if self
            .blocks
            .get(target)
            .is_some_and(|block| matches!(block.kind, BlockKind::Table { visual: true }))
        {
            self.focus_block(
                target,
                if down {
                    CursorPlacement::Start
                } else {
                    CursorPlacement::End
                },
            );
            return;
        }
        if self.wrap_lines {
            let Some(block) = self.blocks.get(source) else {
                return;
            };
            let x = block.line.inner().vertical_goal_x();
            let Some(block) = self.blocks.get(target) else {
                return;
            };
            let rect = block.line.get_rect();
            let point = Point::new(x, if down { rect.y0 + 9.0 } else { rect.y1 - 9.0 });
            let position = block.line.inner().position_at_point(point);
            self.focus_block(target, CursorPlacement::Byte(position.byte));
            if let Some(block) = self.blocks.get_mut(target) {
                block.line.inner_mut().set_caret_position(position);
                block.line.inner_mut().set_vertical_goal_x(x);
            }
        } else {
            self.focus_block(
                target,
                if down {
                    CursorPlacement::Start
                } else {
                    CursorPlacement::End
                },
            );
        }
    }

    fn insert_table_newline(&mut self, idx: usize, split_at: usize) -> bool {
        let Some(block) = self.blocks.get_mut(idx) else {
            return false;
        };
        if !matches!(block.kind, BlockKind::Table { visual: false }) {
            return false;
        }
        let source = block.text();
        let split_at = source.floor_char_boundary(split_at);
        let Some((before, after)) = source.split_at_checked(split_at) else {
            return false;
        };
        let mut updated = String::with_capacity(source.len().saturating_add(1));
        updated.push_str(before);
        updated.push('\n');
        updated.push_str(after);
        block.line.set_text(&updated);
        block.line.set_cursor_pos(split_at.saturating_add(1));
        self.dirty = true;
        self.edit_pending = true;
        true
    }

    fn exit_empty_outline_block(&mut self, idx: usize) -> bool {
        let Some(block) = self.blocks.get(idx) else {
            return false;
        };
        if !block.text().is_empty() {
            return false;
        }
        if matches!(block.kind, BlockKind::Paragraph) && block.indent() > 0 {
            return self.try_indent(idx, -1);
        }
        let next = match &block.kind {
            BlockKind::Bullet { indent } if *indent > 0 => BlockKind::Bullet {
                indent: indent.saturating_sub(1),
            },
            BlockKind::Numbered { indent } if *indent > 0 => BlockKind::Numbered {
                indent: indent.saturating_sub(1),
            },
            BlockKind::Checklist { indent, .. } if *indent > 0 => BlockKind::Checklist {
                indent: indent.saturating_sub(1),
                done: false,
            },
            BlockKind::Bullet { .. }
            | BlockKind::Numbered { .. }
            | BlockKind::Checklist { .. }
            | BlockKind::Quote => BlockKind::Paragraph,
            _ => return false,
        };
        self.try_convert_to(idx, next)
    }
}

#[cfg(test)]
#[path = "block_editor_tests.rs"]
mod tests;

impl BlockEditor {
    /// For an event that can only type into the focused paragraph (a
    /// character, an IME commit, Backspace or Delete, with no selection
    /// across paragraphs and no find bar), the paragraph's index, the block
    /// count and its id: what must still hold afterwards for the edit to
    /// have stayed in that paragraph.
    fn typing_position(
        &self,
        event: &WindowEvent,
        modifiers: Modifiers,
    ) -> Option<(usize, usize, String)> {
        use winit::keyboard::NamedKey;
        let plain = match event {
            WindowEvent::Ime(_) => true,
            WindowEvent::KeyboardInput { event, .. } => {
                let held = modifiers.state();
                !held.control_key()
                    && !held.alt_key()
                    && !held.super_key()
                    && matches!(
                        &event.logical_key,
                        Key::Character(_)
                            | Key::Named(NamedKey::Backspace | NamedKey::Delete | NamedKey::Space)
                    )
            }
            _ => false,
        };
        if !plain || self.find_visible || self.document_selection.is_some() || !self.is_focused {
            return None;
        }
        let index = self.focused?;
        let block = self.blocks.get(index)?;
        Some((index, self.blocks.len(), block.id.clone()))
    }

    fn handle_event_unscoped(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        use winit::event::{ElementState, MouseButton};

        // The IME owns candidate-selection keys until it commits/cancels.
        // Do not let Enter, Tab, or Backspace become structural document edits.
        if self.is_focused
            && matches!(event, WindowEvent::KeyboardInput { .. })
            && self.is_composing()
        {
            *ctx.handled = true;
            return;
        }

        let changes_selection = match event {
            WindowEvent::Ime(winit::event::Ime::Commit(_)) => true,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(
                        NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::Home
                        | NamedKey::End
                        | NamedKey::PageUp
                        | NamedKey::PageDown
                        | NamedKey::Backspace
                        | NamedKey::Delete
                        | NamedKey::Enter
                        | NamedKey::Space,
                    ) => true,
                    Key::Character(_) => {
                        let held = ctx.modifiers.state();
                        !held.control_key() && !held.super_key() && !held.alt_key()
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if changes_selection {
            self.retained_block_selection = None;
        }

        if self.handle_find_event(event, ctx) {
            return;
        }

        if self.try_paste_image_key(event, ctx) {
            return;
        }

        if self.handle_opening_code_fence(event, ctx) {
            return;
        }

        // While visible, intercept keystrokes BEFORE the editable line
        // sees them. Same pattern as the slash menu below.
        if self.handle_link_popover_key(event, ctx) {
            return;
        }

        // The menu owns arrow nav / Enter / Escape / typing-into-filter.
        if self.handle_slash_menu_key(event, ctx) {
            return;
        }

        if let Some(start) = self.visual_formula_focus
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.visual_formula_focus = None;
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            if !matches!(logical_key, Key::Named(NamedKey::Tab)) {
                let chrome = self.formula_chrome.get(start).and_then(|value| *value);
                let changed = self.visual_formulas.get_mut(&start).is_some_and(|visual| {
                    handle_visual_formula_key(&mut visual.editor, logical_key, *ctx.modifiers)
                });
                if changed && let Some(chrome) = chrome {
                    self.commit_visual_formula_edit(start, chrome.end, chrome.descriptor.syntax);
                }
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
        }

        // A focused table cell owns editing shortcuts before the document.
        if self.handle_visual_table_event(event, ctx) {
            return;
        }
        // Select-all is document-scoped, like a browser editing host, rather
        // than stopping at the focused paragraph's TextInput.
        if self.try_select_all_shortcut(event, ctx) {
            return;
        }

        // Code indentation owns whole selected lines before generic replacement.
        if self.handle_code_keyboard(event, ctx) {
            return;
        }

        // Paragraph commits belong to the document transaction, even when
        // the original selection is entirely inside one single-line child.
        if let WindowEvent::Ime(winit::event::Ime::Commit(text)) = event
            && self.ime_multiline()
            && text.contains('\n')
            && text.chars().all(|c| c == '\n' || !c.is_control())
        {
            if let Some(index) = self.focused
                && let Some(block) = self.blocks.get_mut(index)
            {
                let (start, end) = block
                    .line
                    .selection_range()
                    .unwrap_or_else(|| (block.line.cursor_pos(), block.line.cursor_pos()));
                block.line.handle_event(
                    &WindowEvent::Ime(winit::event::Ime::Preedit(String::new(), None)),
                    ctx,
                );
                if self.document_selection.is_none() {
                    self.document_selection = Some(DocumentSelection {
                        anchor: DocumentPosition {
                            block: index,
                            byte: start,
                        },
                        cursor: DocumentPosition {
                            block: index,
                            byte: end,
                        },
                    });
                }
            }
            if self.replace_document_selection(text) {
                *ctx.handled = true;
                return;
            }
        }

        // A cross-block selection belongs to the document, not any one child
        // TextInput. Consume clipboard/replacement input before line routing.
        if self.handle_document_selection_event(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        if self.try_move_focused_block_shortcut(event, ctx) {
            return;
        }

        if self.handle_block_drag_lifecycle(event, ctx) {
            return;
        }

        if self.handle_image_event(event, ctx) {
            return;
        }

        // Reset the anchor on anything but a Ctrl+Shift+Arrow extend, so
        // the next extend latches a fresh one.
        self.maybe_reset_kbd_sel_anchor(event, *ctx.modifiers);

        // Runs BEFORE event routing so the click doesn't fall through to
        // TextInput (which would place the caret at byte 0).
        if self.handle_mouse_press_gestures(event, ctx) {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.cross_block_drag_start = None;
        }
        self.handle_cursor_moved(event, ctx.scale_factor);

        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && self.is_focused
        {
            if event.logical_key == Key::Named(NamedKey::Escape) {
                self.clear_document_selection();
                self.tab_navigation = true;
                *ctx.handled = true;
                return;
            }
            if !matches!(
                event.logical_key,
                Key::Named(
                    NamedKey::Tab
                        | NamedKey::Shift
                        | NamedKey::Control
                        | NamedKey::Alt
                        | NamedKey::AltGraph
                        | NamedKey::Super
                        | NamedKey::Meta
                )
            ) {
                self.tab_navigation = false;
            }
        }

        // TextInput does NOT handle Tab, but intercept anyway so future
        // TextInput changes don't break the contract.
        if self.try_tab_indent(event, ctx) {
            return;
        }

        // The leading `/` still falls through to TextInput (visible
        // feedback); `slash_open_at` remembers where to strip it on
        // commit. Code blocks excluded — `/` there is meaningful text.
        self.try_open_slash_menu(event);

        // Focus whichever editable block the press lands on first, so the
        // routing below's TextInput click handler sets the caret there.
        self.focus_block_under_mouse(event, ctx);

        self.route_event_to_focused_block(event, ctx);
        // Consumed input can move the caret or selection without editing text.
        // Repaint that change once; focus alone is not an animation source.
        self.dirty |= *ctx.handled;
    }
}
