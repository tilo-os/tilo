//! `BlockEditor`: a stack of [`RichTextLine`]-backed blocks (paragraph,
//! heading, lists, code, divider) with split-on-Enter, merge-on-Backspace,
//! arrow-key navigation, and markdown-shortcut promotion (`# ` -> H1, etc).
//!
//! No demotion once a block is a heading. Promotion only fires on an
//! *empty* paragraph, matching the slash-menu convention.

use std::collections::HashMap;
use std::fmt::Write as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::presence::{PathSegment, PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::text_selection::{clipboard_copy, clipboard_paste};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::link_popover::{LinkPopover, PageEntry};
use crate::widget::marks::{Mark, MarkSpan};
use crate::widget::rich_text_line::{LineSignal, RichTextLine};
use crate::widget::slash_menu::SlashMenu;

/// Deeper nesting runs off the panel's right edge.
pub const MAX_LIST_INDENT: u8 = 5;
/// Matches the 28px indent step Notion uses in its dense block view.
pub const LIST_INDENT_STEP_PX: f32 = 28.0;

/// `CodeBlock` is single-line per block: Enter creates a new code block
/// below rather than a newline, since `RichTextLine` is single-line only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
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
    /// Non-editable, non-focusable.
    Divider,
}

impl BlockKind {
    pub fn font_size(&self) -> f32 {
        match self {
            BlockKind::Paragraph
            | BlockKind::Bullet { .. }
            | BlockKind::Numbered { .. }
            | BlockKind::Checklist { .. }
            | BlockKind::Divider => 16.0,
            BlockKind::Heading(1) => 28.0,
            BlockKind::Heading(2) => 22.0,
            BlockKind::Heading(_) => 18.0,
            BlockKind::CodeBlock { .. } => 14.0,
        }
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
}

pub struct Block {
    pub kind: BlockKind,
    pub line: RichTextLine,
    /// Set by Shift+Enter: renders with no inter-block top spacing so
    /// the two lines read as one paragraph.
    pub soft_break: bool,
}

impl Block {
    pub fn paragraph(text: &str) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            line: RichTextLine::new()
                .with_text(text)
                .with_size(BlockKind::Paragraph.font_size()),
            soft_break: false,
        }
    }

    pub fn heading(level: u8, text: &str) -> Self {
        let kind = BlockKind::Heading(level.clamp(1, 3));
        Self {
            kind: kind.clone(),
            line: RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
            soft_break: false,
        }
    }

    pub fn bullet(indent: u8, text: &str) -> Self {
        let kind = BlockKind::Bullet {
            indent: indent.min(MAX_LIST_INDENT),
        };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
            soft_break: false,
        }
    }

    pub fn numbered(indent: u8, text: &str) -> Self {
        let kind = BlockKind::Numbered {
            indent: indent.min(MAX_LIST_INDENT),
        };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
            soft_break: false,
        }
    }

    pub fn checklist(indent: u8, done: bool, text: &str) -> Self {
        let kind = BlockKind::Checklist {
            indent: indent.min(MAX_LIST_INDENT),
            done,
        };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size()),
            soft_break: false,
        }
    }

    pub fn code_block(lang: Option<String>, text: &str) -> Self {
        let kind = BlockKind::CodeBlock { lang };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new()
                .with_text(text)
                .with_size(kind.font_size())
                .with_mono(),
            soft_break: false,
        }
    }

    /// Empty inner line keeps a uniform shape; never focused or edited.
    pub fn divider() -> Self {
        Self {
            kind: BlockKind::Divider,
            line: RichTextLine::new().with_size(BlockKind::Divider.font_size()),
            soft_break: false,
        }
    }

    pub fn text(&self) -> &str {
        self.line.value()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent, orthogonal piece of transient \
              widget state (focus, paint-dirty, taffy-structure-dirty, \
              edit-in-flight, commit-pending, doc-structure-dirty); merging \
              them into a bitflags/enum would not change behavior and this \
              struct is private to the module"
)]
pub struct BlockEditor {
    blocks: Vec<Block>,
    focused: Option<usize>,
    /// Decoupled from `focused` so the index survives blur/refocus.
    is_focused: bool,
    rect: Rect,
    child_nodes: Vec<NodeId>,
    dirty: bool,
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
    slash_menu: SlashMenu,
    /// Byte offset where `/` was typed, so `/<filter>` can be spliced
    /// back out on commit.
    slash_open_at: Option<usize>,
    link_popover: LinkPopover,
    /// Byte offset of the leading `[[`, spliced back out once a page
    /// is selected.
    link_open_at: Option<usize>,
    /// `Some`: backed by this Automerge block-Map list, bound per-line
    /// so edits sync. `None`: local-only (examples/tests).
    blocks_list: Option<ObjId>,
    /// Set by split/merge/delete so the next `sync` mirrors structure
    /// into the doc, preserving block `Text` identity. Distinct from
    /// `structure_dirty` (which drives taffy).
    doc_structure_dirty: bool,
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
    formula_chrome: Vec<Option<FormulaChrome>>,
    visual_formulas: HashMap<usize, VisualFormulaState>,
    visual_formula_focus: Option<usize>,
    preferred_formula_mode: crate::math::FormulaMode,
}

const DEFAULT_EMPTY_HINT: &str = "Start typing — / for blocks, [[ to link a page";
const FORMULA_BLOCK_MIN_HEIGHT: f32 = 164.0;
const FORMULA_VISUAL_MIN_HEIGHT: f32 = 132.0;
const FORMULA_HEADER_HEIGHT: f64 = 28.0;
const FORMULA_SOURCE_HEIGHT: f64 = 24.0;
const FORMULA_PREVIEW_GAP: f64 = 6.0;

impl BlockEditor {
    /// Starts with one empty paragraph — there must be a block to focus.
    pub fn new() -> Self {
        let mut s = Self {
            blocks: Vec::new(),
            focused: None,
            is_focused: false,
            rect: Rect::ZERO,
            child_nodes: Vec::new(),
            dirty: true,
            structure_dirty: true,
            edit_pending: false,
            commit_pending: false,
            last_mouse_pos: Point::ZERO,
            cross_block_drag_start: None,
            cross_block_drag_touched: None,
            document_selection: None,
            slash_menu: SlashMenu::new(),
            slash_open_at: None,
            link_popover: LinkPopover::new(),
            link_open_at: None,
            blocks_list: None,
            doc_structure_dirty: false,
            local_author: None,
            kbd_sel_anchor: None,
            empty_hint: DEFAULT_EMPTY_HINT.to_string(),
            formula_preview_rects: Vec::new(),
            formula_chrome: Vec::new(),
            visual_formulas: HashMap::new(),
            visual_formula_focus: None,
            preferred_formula_mode: crate::math::FormulaMode::Visual,
        };
        s.blocks.push(Block::paragraph(""));
        s
    }

    #[must_use]
    pub fn with_empty_hint(mut self, hint: &str) -> Self {
        self.empty_hint = hint.to_string();
        self
    }

    pub fn set_empty_hint(&mut self, hint: &str) {
        self.empty_hint = hint.to_string();
    }

    pub fn empty_hint(&self) -> &str {
        &self.empty_hint
    }

    /// Minimal markdown parser; empty input still produces one empty
    /// paragraph.
    pub fn from_markdown(md: &str) -> Self {
        let mut editor = Self {
            blocks: Vec::new(),
            focused: None,
            is_focused: false,
            rect: Rect::ZERO,
            child_nodes: Vec::new(),
            dirty: true,
            structure_dirty: true,
            edit_pending: false,
            commit_pending: false,
            last_mouse_pos: Point::ZERO,
            cross_block_drag_start: None,
            cross_block_drag_touched: None,
            document_selection: None,
            slash_menu: SlashMenu::new(),
            slash_open_at: None,
            link_popover: LinkPopover::new(),
            link_open_at: None,
            blocks_list: None,
            doc_structure_dirty: false,
            local_author: None,
            kbd_sel_anchor: None,
            empty_hint: DEFAULT_EMPTY_HINT.to_string(),
            formula_preview_rects: Vec::new(),
            formula_chrome: Vec::new(),
            visual_formulas: HashMap::new(),
            visual_formula_focus: None,
            preferred_formula_mode: crate::math::FormulaMode::Visual,
        };
        let mut lines = md.split('\n');
        while let Some(line) = lines.next() {
            if line.is_empty() {
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
                editor
                    .blocks
                    .push(Block::code_block(lang, &body.join("\n")));
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
            if let Some(rest) = line.strip_prefix("### ") {
                editor.blocks.push(Block::heading(3, rest));
            } else if let Some(rest) = line.strip_prefix("## ") {
                editor.blocks.push(Block::heading(2, rest));
            } else if let Some(rest) = line.strip_prefix("# ") {
                editor.blocks.push(Block::heading(1, rest));
            } else {
                editor.blocks.push(Block::paragraph(line));
            }
        }
        if editor.blocks.is_empty() {
            editor.blocks.push(Block::paragraph(""));
        }
        editor
    }

    /// Inverse of [`from_markdown`]; round-trip is byte-stable.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let ordinals = numbered_ordinals(&self.blocks);
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n");
            }
            match &b.kind {
                BlockKind::Paragraph => out.push_str(b.text()),
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

    pub fn set_focus(&mut self, focused: bool) {
        if focused == self.is_focused {
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
        self.blocks
            .iter()
            .map(|block| block.line.get_rect().y1)
            .chain(
                self.formula_preview_rects
                    .iter()
                    .filter_map(|rect| rect.map(|rect| rect.y1)),
            )
            .fold(self.rect.y0, f64::max)
    }

    /// For tests + notes-app persistence to seed blocks directly;
    /// production code should prefer the public mutators.
    pub fn blocks_mut(&mut self) -> &mut Vec<Block> {
        &mut self.blocks
    }

    /// For the notes app's markdown parser, which produces a richer
    /// representation (inline marks) than this widget's `from_markdown`.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.blocks = blocks;
        if self.blocks.is_empty() {
            self.blocks.push(Block::paragraph(""));
        }
        self.focused = None;
        self.is_focused = false;
        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.formula_preview_rects.clear();
        self.formula_chrome.clear();
        self.visual_formulas.clear();
        self.visual_formula_focus = None;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        for b in &mut self.blocks {
            b.line.set_local_author(author.clone());
        }
        self.local_author = author;
    }

    /// If the doc list already has blocks, adopts them (discarding the
    /// locally seeded ones); if empty, seeds it from the local blocks.
    pub fn bind_blocks(&mut self, state: &mut AutoCommit, list: ObjId) {
        let doc_len = state.length(&list);
        if doc_len == 0 {
            self.doc_structure_dirty = true;
            self.reconcile_structure_to_doc(state, &list);
            self.doc_structure_dirty = false;
        } else {
            self.rebuild_from_doc(state, &list, doc_len);
        }
        self.blocks_list = Some(list);
    }

    /// Preserves each existing block's `Text` object by identity
    /// (Automerge list insert/delete keeps other elements' object ids
    /// stable); text drifted by a local split/merge is reconciled with
    /// a minimal splice.
    fn reconcile_structure_to_doc(&mut self, state: &mut AutoCommit, list: &ObjId) {
        let mut doc_idx = 0usize;
        for block in &mut self.blocks {
            let kind_str = kind_to_str(&block.kind);
            let local_text = block.line.value().to_string();
            if let Some(bound) = block.line.bound_obj() {
                // Delete any doc blocks before the bound one — they
                // were removed locally.
                while doc_idx < state.length(list)
                    && doc_text_obj(state, list, doc_idx).as_ref() != Some(&bound)
                {
                    let _ = state.delete(list, doc_idx);
                }
                if doc_idx < state.length(list) {
                    let block_map = list_map(state, list, doc_idx);
                    if let Some(bm) = block_map {
                        let _ = state.put(&bm, "kind", kind_str.as_str());
                    }
                    push_text_diff(state, &bound, &local_text);
                    doc_idx = doc_idx.saturating_add(1);
                } else {
                    // Bound object vanished from the doc — re-insert. A
                    // `None` here means the doc's `list` itself was
                    // concurrently removed; leave the block unbound rather
                    // than desync `doc_idx` from a write that never landed.
                    if let Some(t) = insert_doc_block(state, list, doc_idx, &kind_str, &local_text)
                    {
                        block.line.bind_text(t);
                        doc_idx = doc_idx.saturating_add(1);
                    }
                }
            } else {
                // New block (e.g. the suffix of a split): insert a
                // fresh doc block and bind it.
                if let Some(t) = insert_doc_block(state, list, doc_idx, &kind_str, &local_text) {
                    block.line.bind_text(t);
                    doc_idx = doc_idx.saturating_add(1);
                }
            }
        }
        while state.length(list) > doc_idx {
            let last_idx = state.length(list).saturating_sub(1);
            let _ = state.delete(list, last_idx);
        }
    }

    /// Preserves focus + caret across the rebuild so a remote edit
    /// elsewhere doesn't yank the local caret.
    fn rebuild_from_doc(&mut self, state: &mut AutoCommit, list: &ObjId, doc_len: usize) {
        let saved_focus = self.focused;
        let saved_caret = self
            .focused
            .and_then(|i| self.blocks.get(i))
            .map(|b| b.line.cursor_pos());

        let mut new_blocks = Vec::with_capacity(doc_len);
        for i in 0..doc_len {
            let kind = kind_from_str(&read_block_kind(state, list, i));
            let mut b = block_for_kind(&kind);
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
        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.structure_dirty = true;

        if let Some(fi) = saved_focus {
            let fi = fi.min(self.blocks.len().saturating_sub(1));
            self.focused = Some(fi);
            self.is_focused = true;
            if let Some(b) = self.blocks.get_mut(fi) {
                b.line.set_focus(true);
                if let Some(c) = saved_caret {
                    b.line.set_cursor_pos(c);
                }
            }
        }
    }

    /// `RichTextLine::set_text` deliberately doesn't flip dirty (it's
    /// not a user edit); tests use this to simulate one.
    pub fn force_dirty_for_test(&mut self) {
        self.edit_pending = true;
        self.commit_pending = true;
        self.dirty = true;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
    }

    pub fn focused_index(&self) -> Option<usize> {
        self.focused
    }

    pub fn focus_block(&mut self, idx: usize, cursor_at: CursorPlacement) {
        let target = idx.min(self.blocks.len().saturating_sub(1));
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
        let split = clamp_char_boundary(&text, split);
        let (prefix, suffix) = text.split_at(split);
        block.line.set_text(prefix);
        let suffix_block = match cur_kind {
            BlockKind::Bullet { indent } => Block::bullet(indent, suffix),
            BlockKind::Numbered { indent } => Block::numbered(indent, suffix),
            BlockKind::Checklist { indent, .. } => {
                // Unchecked even if the source was checked — "add a new todo".
                Block::checklist(indent, false, suffix)
            }
            BlockKind::CodeBlock { ref lang } => Block::code_block(lang.clone(), suffix),
            _ => Block::paragraph(suffix),
        };
        let next_idx = idx.saturating_add(1);
        self.blocks.insert(next_idx, suffix_block);
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
        if let Some(b) = self.blocks.get_mut(prev_idx) {
            b.line.set_text(&merged);
        }
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
            self.doc_structure_dirty = true;
            self.doc_structure_dirty = true;
            self.dirty = true;
            return;
        }
        let target = idx.saturating_sub(1);
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
        let (level, prefix) = if let Some(rest) = text.strip_prefix("### ") {
            (3u8, rest.to_string())
        } else if let Some(rest) = text.strip_prefix("## ") {
            (2u8, rest.to_string())
        } else if let Some(rest) = text.strip_prefix("# ") {
            (1u8, rest.to_string())
        } else {
            return false;
        };
        let kind = BlockKind::Heading(level);
        let font_size = kind.font_size();
        b.kind = kind;
        let mut new_line = RichTextLine::new().with_text(&prefix).with_size(font_size);
        new_line.set_focus(true);
        new_line.set_cursor_pos(0);
        b.line = new_line;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
        true
    }

    /// Preserves text, except converting to Divider drops it. Resets
    /// indent to 0 and done to false.
    pub fn try_convert_to(&mut self, idx: usize, new_kind: BlockKind) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        if b.kind == new_kind {
            return false;
        }
        let text = b.line.value().to_string();
        let font_size = new_kind.font_size();
        // Rebuilt (not just resized) so selection/dirty state resets too.
        let is_divider = matches!(new_kind, BlockKind::Divider);
        let body = if is_divider { "" } else { &text };
        let was_focused = b.line.is_focused();
        let mut new_line = RichTextLine::new().with_text(body).with_size(font_size);
        if matches!(new_kind, BlockKind::CodeBlock { .. }) {
            new_line.set_mono(true);
        }
        new_line.set_cursor_pos(body.len());
        new_line.set_focus(was_focused && !is_divider);
        b.line = new_line;
        b.kind = new_kind;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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

    /// No-op (returns `false`) for kinds without an indent. Clamped to
    /// [0, `MAX_LIST_INDENT`].
    pub fn try_indent(&mut self, idx: usize, delta: i8) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        let (new_kind, changed) = match &b.kind {
            BlockKind::Bullet { indent } => {
                let next = clamp_indent(*indent, delta);
                let changed = next != *indent;
                (BlockKind::Bullet { indent: next }, changed)
            }
            BlockKind::Numbered { indent } => {
                let next = clamp_indent(*indent, delta);
                let changed = next != *indent;
                (BlockKind::Numbered { indent: next }, changed)
            }
            BlockKind::Checklist { indent, done } => {
                let next = clamp_indent(*indent, delta);
                let changed = next != *indent;
                (
                    BlockKind::Checklist {
                        indent: next,
                        done: *done,
                    },
                    changed,
                )
            }
            _ => return false,
        };
        if !changed {
            return false;
        }
        b.kind = new_kind;
        // Structure (block count) doesn't change, but the row's layout
        // does — flag both so MainPanel rebuilds taffy if needed.
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
        true
    }

    /// Checkbox rect is centered in the gutter to the left of
    /// `line_rect`, matching where it's drawn.
    pub(crate) fn checkbox_hit_test_last_mouse(&self) -> Option<usize> {
        let p = self.last_mouse_pos;
        for (idx, b) in self.blocks.iter().enumerate() {
            if !matches!(b.kind, BlockKind::Checklist { .. }) {
                continue;
            }
            let line_rect = b.line.get_rect();
            let gutter = prefix_gutter_for(&b.kind);
            let cy = f64::midpoint(line_rect.y0, line_rect.y1);
            let cx = line_rect.x0 - gutter + gutter / 2.0;
            let r = Rect::new(
                cx - CHECKBOX_SIZE_PX / 2.0,
                cy - CHECKBOX_SIZE_PX / 2.0,
                cx + CHECKBOX_SIZE_PX / 2.0,
                cy + CHECKBOX_SIZE_PX / 2.0,
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
            if cursor > start
                && let (Some(head), Some(tail)) = (text.get(..start), text.get(cursor..))
            {
                // Drop bytes [start, cursor) — the `/<filter>` slice.
                let mut new_text = String::with_capacity(text.len());
                new_text.push_str(head);
                new_text.push_str(tail);
                b.line.set_text(&new_text);
                b.line.set_cursor_pos(start);
            }
        }
        self.slash_open_at = None;
        if let Some(mut descriptor) = formula_descriptor(&new_kind) {
            descriptor.mode = self.preferred_formula_mode;
            new_kind = BlockKind::CodeBlock {
                lang: Some(descriptor.fence_language()),
            };
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
        let source = self.blocks[start..=end]
            .iter()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n");
        descriptor.mode = mode;
        let language = descriptor.fence_language();
        for block in &mut self.blocks[start..=end] {
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
        self.doc_structure_dirty = true;
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
        for block in self.blocks.iter_mut().take(end + 1).skip(start + 1) {
            block.line.set_text("");
        }
        self.doc_structure_dirty = true;
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
        self.slash_menu
            .draw_overlay(scene, transform, text_renderer, theme);
        self.link_popover
            .draw_overlay(scene, transform, text_renderer, theme);
    }

    pub fn set_link_pages(&mut self, pages: Vec<PageEntry>) {
        self.link_popover.set_items(pages);
    }

    pub fn link_popover(&self) -> &LinkPopover {
        &self.link_popover
    }

    #[doc(hidden)]
    pub fn link_popover_mut(&mut self) -> &mut LinkPopover {
        &mut self.link_popover
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
        let (Some(head), Some(tail)) = (text.get(..start), text.get(cursor..)) else {
            return;
        };
        let title_end = start.saturating_add(page.title.len());
        let mut new_text = String::with_capacity(text.len().saturating_add(page.title.len()));
        new_text.push_str(head);
        new_text.push_str(&page.title);
        new_text.push_str(tail);
        b.line.set_text(&new_text);
        let mark_range = start..title_end;
        b.line
            .add_mark(MarkSpan::new(mark_range, Mark::Link(page.id)));
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

    /// Used by the integration test that pre-positions the menu rather
    /// than driving the `/` keystroke through the full event chain.
    #[doc(hidden)]
    pub fn slash_menu_mut(&mut self) -> &mut SlashMenu {
        &mut self.slash_menu
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
        let line_rect = self
            .blocks
            .get(focused_idx)
            .map_or(Rect::ZERO, |b| b.line.get_rect());
        self.slash_menu
            .set_anchor(Point::new(line_rect.x0, line_rect.y1 + 2.0));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = Some(caret);
    }

    #[doc(hidden)]
    pub fn slash_menu_handle_key_for_test(&mut self, event: &WindowEvent) -> bool {
        let consumed = self.slash_menu.handle_key(event);
        if let Some(new_kind) = self.slash_menu.take_selected_kind() {
            self.commit_slash_selection(new_kind);
        }
        if !self.slash_menu.is_visible() {
            self.slash_open_at = None;
        }
        consumed
    }

    /// No-op for non-Checklist kinds.
    pub fn try_toggle_check(&mut self, idx: usize) -> Option<bool> {
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

fn is_heading_prefix(s: &str) -> bool {
    matches!(s, "# " | "## " | "### ")
}

fn clamp_char_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b = b.saturating_sub(1);
    }
    b
}

/// Rounds a byte offset *up* to the next char boundary — the mirror of
/// [`clamp_char_boundary`]'s round-down. Used where growing a matched
/// region (rather than shrinking it) is what keeps a replacement exact.
fn ceil_char_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b < text.len() && !text.is_char_boundary(b) {
        b = b.saturating_add(1);
    }
    b
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
        BlockKind::Bullet { .. } => 20.0,
        // Wide enough for a two-digit ordinal ("12.") without colliding
        // with the text column.
        BlockKind::Numbered { .. } => 28.0,
        BlockKind::Checklist { .. } => 24.0,
        BlockKind::CodeBlock { .. } => 12.0,
        _ => 0.0,
    }
}

pub(crate) const CHECKBOX_SIZE_PX: f64 = 16.0;

fn formula_descriptor(kind: &BlockKind) -> Option<crate::math::FormulaDescriptor> {
    let BlockKind::CodeBlock { lang } = kind else {
        return None;
    };
    crate::math::FormulaDescriptor::from_fence_language(lang.as_deref())
}

fn formula_run_end(blocks: &[Block], start: usize) -> usize {
    let Some(descriptor) = blocks
        .get(start)
        .and_then(|block| formula_descriptor(&block.kind))
    else {
        return start;
    };
    let mut end = start;
    while end + 1 < blocks.len()
        && blocks
            .get(end + 1)
            .and_then(|block| formula_descriptor(&block.kind))
            == Some(descriptor)
    {
        end += 1;
    }
    end
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
            transform * Affine::translate((rect.x0 + 8.0, rect.y0 + 8.0)),
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
            message = message.chars().take(89).collect::<String>() + "…";
        }
        text_renderer.draw_text(
            scene,
            &message,
            11.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((rect.x0 + 6.0, rect.y1 - 15.0)),
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
        transform * Affine::translate((chrome.syntax_badge.x0 + 7.0, chrome.syntax_badge.y0 + 5.0)),
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
                colors.accent[5].with_alpha_factor(0.20),
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
            transform * Affine::translate((rect.x0 + 7.0, rect.y0 + 5.0)),
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
                theme.tokens.colors.accent[5].with_alpha_factor(0.25),
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
            theme.tokens.colors.accent[5],
            None,
            &caret,
        );
    }
    state.layout = Some(layout);
}

fn handle_visual_formula_key(
    editor: &mut crate::math::edit::Editor,
    key: &winit::keyboard::Key,
    modifiers: &Modifiers,
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
                if let Some(value) = editor.selected_linear() {
                    clipboard_copy(&value);
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
/// restarts under its parent, and ANY non-numbered block resets every
/// counter.
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
            _ => {
                counters = [0u32; MAX_LIST_INDENT as usize + 1];
            }
        }
    }
    out
}

// `BlockKind` is encoded as a compact string in the doc's `kind` field:
// paragraph / heading{1..3} / bullet:{indent} / checklist:{indent}:{0|1}
// / code:{lang} / divider.

/// Derived from block position, so a peer's caret/selection renders on
/// the matching block everywhere.
pub fn block_presence_key(index: usize) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_block".to_string()),
            PathSegment::Index(u32::try_from(index).unwrap_or(u32::MAX)),
        ],
    }
}

/// A document selection uses one transport frame; block endpoints live in
/// the key while the existing selection payload carries endpoint bytes.
pub fn document_selection_presence_key(start_block: usize, end_block: usize) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_document_selection".to_string()),
            PathSegment::Index(u32::try_from(start_block).unwrap_or(u32::MAX)),
            PathSegment::Index(u32::try_from(end_block).unwrap_or(u32::MAX)),
        ],
    }
}

fn document_selection_blocks(key: &PresenceKey) -> Option<(usize, usize)> {
    let PresenceKey::Widget { path } = key else {
        return None;
    };
    match path.as_slice() {
        [
            PathSegment::Field(tag),
            PathSegment::Index(start),
            PathSegment::Index(end),
        ] if tag == "notes_document_selection" => Some((*start as usize, *end as usize)),
        _ => None,
    }
}

fn kind_to_str(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph => "paragraph".to_string(),
        BlockKind::Heading(n) => format!("heading{n}"),
        BlockKind::Bullet { indent } => format!("bullet:{indent}"),
        BlockKind::Numbered { indent } => format!("numbered:{indent}"),
        BlockKind::Checklist { indent, done } => {
            format!("checklist:{indent}:{}", i32::from(*done))
        }
        BlockKind::CodeBlock { lang } => {
            format!("code:{}", lang.clone().unwrap_or_default())
        }
        BlockKind::Divider => "divider".to_string(),
    }
}

fn kind_from_str(s: &str) -> BlockKind {
    if s == "paragraph" {
        return BlockKind::Paragraph;
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
    BlockKind::Paragraph
}

/// Text is bound + pulled from the doc afterwards, so we seed with "".
fn block_for_kind(kind: &BlockKind) -> Block {
    match kind {
        BlockKind::Paragraph => Block::paragraph(""),
        BlockKind::Heading(n) => Block::heading(*n, ""),
        BlockKind::Bullet { indent } => Block::bullet(*indent, ""),
        BlockKind::Numbered { indent } => Block::numbered(*indent, ""),
        BlockKind::Checklist { indent, done } => Block::checklist(*indent, *done, ""),
        BlockKind::CodeBlock { lang } => Block::code_block(lang.clone(), ""),
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

/// Returns `None` if `list` was concurrently removed from the document —
/// the only way `insert_object`/`put_object` can fail here — leaving the
/// caller's local block unbound rather than panicking on a legitimate CRDT
/// race.
fn insert_doc_block(
    state: &mut AutoCommit,
    list: &ObjId,
    index: usize,
    kind: &str,
    text: &str,
) -> Option<ObjId> {
    let block = state.insert_object(list, index, ObjType::Map).ok()?;
    let _ = state.put(&block, "kind", kind);
    let text_id = state.put_object(&block, "text", ObjType::Text).ok()?;
    if !text.is_empty() {
        let _ = state.splice_text(&text_id, 0, 0, text);
    }
    Some(text_id)
}

/// A single minimal splice, since the structural ops that call this
/// produce one contiguous change.
fn push_text_diff(state: &mut AutoCommit, text_obj: &ObjId, local: &str) {
    let cur = state.text(text_obj).unwrap_or_default();
    if cur == local {
        return;
    }
    let (at_byte, inserted, removed) = crate::widget::rich_text_line::diff_splice(&cur, local);
    let at_byte = at_byte.min(cur.len()).min(local.len());
    let raw_removed_end = at_byte.saturating_add(removed).min(cur.len());
    let raw_inserted_end = at_byte.saturating_add(inserted).min(local.len());

    // `diff_splice` compares raw bytes, so none of `at_byte`,
    // `raw_removed_end` or `raw_inserted_end` is guaranteed to land on a
    // UTF-8 char boundary in `cur`/`local`. Bytes before `at_byte` are
    // byte-identical between the two strings (that's their shared
    // prefix), and likewise for the bytes from each raw end to the end
    // of its string (their shared suffix) — so moving the start earlier,
    // or an end later, by the same byte count in both strings never
    // changes what the resulting splice produces, only how many
    // already-identical bytes it needlessly re-touches. Without this, a
    // diff that happens to split a multi-byte character (e.g. between
    // "café" and "cafe\u{301}") would panic on the slice below.
    let start = clamp_char_boundary(&cur, at_byte);
    let removed_end = ceil_char_boundary(&cur, raw_removed_end);
    let shift = removed_end.saturating_sub(raw_removed_end);
    let inserted_end = raw_inserted_end.saturating_add(shift).min(local.len());

    let (Some(prefix), Some(removed_slice), Some(inserted_str)) = (
        cur.get(..start),
        cur.get(start..removed_end),
        local.get(start..inserted_end),
    ) else {
        return;
    };
    let char_at = prefix.chars().count();
    let removed_chars = isize::try_from(removed_slice.chars().count()).unwrap_or(isize::MAX);
    let _ = state.splice_text(text_obj, char_at, removed_chars, inserted_str);
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
        if self.blocks.is_empty() {
            return;
        }
        let anchor_block = anchor.block.min(self.blocks.len().saturating_sub(1));
        let cursor_block = cursor.block.min(self.blocks.len().saturating_sub(1));
        let Some(anchor_byte) = self
            .blocks
            .get(anchor_block)
            .map(|b| clamp_char_boundary(b.line.value(), anchor.byte))
        else {
            return;
        };
        let Some(cursor_byte) = self
            .blocks
            .get(cursor_block)
            .map(|b| clamp_char_boundary(b.line.value(), cursor.byte))
        else {
            return;
        };
        self.focus_block(cursor_block, CursorPlacement::Byte(cursor_byte));
        self.paint_cross_block_selection(anchor_block, anchor_byte, cursor_block, cursor_byte);
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
                clamp_char_boundary(text, start.byte)
            } else {
                0
            };
            let to = if block_idx == end.block {
                clamp_char_boundary(text, end.byte)
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
        let Some(selection) = self.document_selection else {
            return false;
        };
        let (start, end) = selection.ordered();
        if start >= end || end.block >= self.blocks.len() {
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
        let start_byte = clamp_char_boundary(&start_text, start.byte);
        let end_byte = clamp_char_boundary(&end_text, end.byte);
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

        self.document_selection = None;
        self.cross_block_drag_start = None;
        self.cross_block_drag_touched = None;
        self.kbd_sel_anchor = None;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
                (*line).to_string()
            };
            let mut block = continuation_block(first_kind, &body);
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
                if let Some(selected) = self.selected_text() {
                    clipboard_copy(&selected);
                }
                Some(self.replace_document_selection(""))
            }
            Key::Character(c) if c.eq_ignore_ascii_case("v") => {
                let paste = clipboard_paste().unwrap_or_default();
                Some(self.replace_document_selection(&paste))
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
        for (i, b) in self.blocks.iter().enumerate() {
            let r = b.line.get_rect();
            if y >= r.y0 && y <= r.y1 {
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

/// See [`translated`]: `Affine * Point` is the same plain f64 math.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Point maps a point through a 2D transform via \
              f64 matrix math; it never panics or overflows"
)]
fn transformed(transform: Affine, p: Point) -> Point {
    transform * p
}

/// Bundles [`Widget::handle_event`]'s pass-through parameters so the
/// per-gesture helpers in that method stay well under clippy's
/// argument-count limit instead of repeating this whole list on each one.
struct EventCtx<'a> {
    scale_factor: f64,
    modifiers: &'a Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    handled: &'a mut bool,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
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
}

/// Formula-run state threaded through [`draw_block`]'s `CodeBlock` arm —
/// kept out of [`BlockDrawCtx`] since only that one arm needs it, and
/// `visual_formulas` needs a mutable borrow the rest of `BlockDrawCtx`
/// doesn't.
struct FormulaDrawCtx<'a> {
    runs: &'a [Option<FormulaRunPreview>],
    code_languages: &'a [Option<String>],
    chrome: &'a [Option<FormulaChrome>],
    preview_rects: &'a [Option<Rect>],
    visual_formulas: &'a mut HashMap<usize, VisualFormulaState>,
    focus: Option<usize>,
}

/// One block's prefix glyph (bullet/ordinal/checkbox/code bar/divider
/// rule) plus its text line. A free function, not a method — it only
/// ever needs `b`, not `self`, which keeps `draw`'s per-block loop out
/// of the borrow checker's way.
fn draw_block(
    b: &mut Block,
    idx: usize,
    ctx: &mut BlockDrawCtx<'_>,
    formulas: &mut FormulaDrawCtx<'_>,
) {
    let line_rect = b.line.get_rect();
    let indent_px = f64::from(LIST_INDENT_STEP_PX) * f64::from(b.kind.indent());
    let gutter = prefix_gutter_for(&b.kind);
    let prefix_x0 = line_rect.x0 - gutter;
    let row_x0 = prefix_x0 - indent_px;
    let _ = row_x0;

    match &b.kind {
        BlockKind::Paragraph | BlockKind::Heading(_) => {}
        BlockKind::Bullet { .. } => {
            draw_bullet_prefix(line_rect, prefix_x0, gutter, ctx);
            let _ = indent_px;
        }
        BlockKind::Numbered { .. } => {
            draw_numbered_prefix(idx, line_rect, prefix_x0, b.kind.font_size(), ctx);
            let _ = indent_px;
        }
        BlockKind::Checklist { done, .. } => {
            draw_checklist_prefix(*done, line_rect, prefix_x0, gutter, ctx);
        }
        BlockKind::CodeBlock { .. } => {
            draw_code_block_prefix(idx, line_rect, gutter, ctx, formulas);
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
        BlockKind::Divider => {
            let cy = f64::midpoint(line_rect.y0, line_rect.y1);
            let rule = Rect::new(line_rect.x0, cy - 0.5, line_rect.x1, cy + 0.5);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                ctx.theme.tokens.colors.border_standard,
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

/// A filled disc, not a "•" glyph, reads the same across fonts.
fn draw_bullet_prefix(line_rect: Rect, prefix_x0: f64, gutter: f64, ctx: &mut BlockDrawCtx<'_>) {
    let cy = f64::midpoint(line_rect.y0, line_rect.y1);
    let cx = prefix_x0 + gutter / 2.0;
    let radius = 3.0;
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
    // Matches RichTextLine's line height so the ordinal
    // sits on the item text's baseline.
    let line_height = f64::from(font_size) * 1.25;
    let text_y = line_rect.y0 + (line_rect.height() - line_height) / 2.0;
    let x = (line_rect.x0 - 6.0 - f64::from(label_w)).max(prefix_x0);
    ctx.text_renderer.draw_text(
        ctx.scene,
        &label,
        font_size,
        ctx.theme.tokens.colors.text_primary,
        translated(ctx.transform, x, text_y),
        None,
    );
}

fn draw_checklist_prefix(
    done: bool,
    line_rect: Rect,
    prefix_x0: f64,
    gutter: f64,
    ctx: &mut BlockDrawCtx<'_>,
) {
    let cy = f64::midpoint(line_rect.y0, line_rect.y1);
    let cx = prefix_x0 + gutter / 2.0;
    let box_rect = Rect::new(
        cx - CHECKBOX_SIZE_PX / 2.0,
        cy - CHECKBOX_SIZE_PX / 2.0,
        cx + CHECKBOX_SIZE_PX / 2.0,
        cy + CHECKBOX_SIZE_PX / 2.0,
    );
    let shape = vello::kurbo::RoundedRect::from_rect(box_rect, 3.0);
    if done {
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            ctx.theme.tokens.colors.accent[5],
            None,
            &shape,
        );
        // Two lines from lower-left through center to upper-right.
        let mut path = vello::kurbo::BezPath::new();
        let pad = CHECKBOX_SIZE_PX * 0.22;
        path.move_to((box_rect.x0 + pad, box_rect.y0 + CHECKBOX_SIZE_PX * 0.55));
        path.line_to((box_rect.x0 + CHECKBOX_SIZE_PX * 0.42, box_rect.y1 - pad));
        path.line_to((box_rect.x1 - pad, box_rect.y0 + pad));
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            ctx.transform,
            ctx.theme.tokens.colors.text_primary,
            None,
            &path,
        );
    } else {
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            ctx.transform,
            ctx.theme.tokens.colors.border_standard,
            None,
            &shape,
        );
    }
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
    // 4px: stays inside the inter-block gap, never overlaps neighbours.
    const CODE_PAD_Y: f64 = 4.0;
    // A run breaks whenever the language changes, not just at the
    // code/non-code boundary — two adjacent fences in different
    // languages get their own background rather than merging into one.
    let is_run_start = idx
        .checked_sub(1)
        .is_none_or(|prev| formulas.code_languages.get(prev) != formulas.code_languages.get(idx));
    if !is_run_start {
        return;
    }
    // A formula run spans further than a same-language run alone when
    // its preview sits past the last source line of a shorter language
    // streak; fall back to the language-run end otherwise.
    let end = formulas.runs.get(idx).and_then(Option::as_ref).map_or_else(
        || {
            let mut end = idx;
            while formulas.code_languages.get(end.saturating_add(1))
                == formulas.code_languages.get(idx)
            {
                end = end.saturating_add(1);
            }
            end
        },
        |formula| formula.end,
    );
    let top = formulas
        .chrome
        .get(idx)
        .copied()
        .flatten()
        .map_or(line_rect.y0 - CODE_PAD_Y, |chrome| {
            chrome.syntax_badge.y0 - 3.0
        });
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
    let bg_rect = Rect::new(line_rect.x0 - gutter, top, line_rect.x1, bottom);
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
        ctx.theme.tokens.colors.accent[5],
        None,
        &bar,
    );
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

impl Widget for BlockEditor {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        let Some(list) = self.blocks_list.clone() else {
            // Unbound (examples / tests): local-mode, nothing to sync.
            return;
        };

        if self.doc_structure_dirty {
            self.reconcile_structure_to_doc(state, &list);
            self.doc_structure_dirty = false;
        }

        // A different block count now means a remote peer changed the
        // structure — adopt it.
        let doc_len = state.length(&list);
        if doc_len != self.blocks.len() {
            self.rebuild_from_doc(state, &list, doc_len);
            return;
        }

        // Same structure: refresh kind + text (RichTextLine::sync pulls
        // bound text without disturbing the local caret).
        for (i, block) in self.blocks.iter_mut().enumerate() {
            let k = kind_from_str(&read_block_kind(state, &list, i));
            if block.kind != k {
                block.kind = k;
                self.structure_dirty = true;
            }
            if block.line.bound_obj().is_none()
                && let Some(t) = doc_text_obj(state, &list, i)
            {
                block.line.bind_text(t);
            }
            block.line.set_local_author(self.local_author.clone());
            block.line.sync(state, &list);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.child_nodes.clear();
        // Consecutive code lines butt together (no inter-line gap) so
        // they read as one fenced block.
        let is_code: Vec<bool> = self
            .blocks
            .iter()
            .map(|b| matches!(b.kind, BlockKind::CodeBlock { .. }))
            .collect();
        let is_formula_end: Vec<bool> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                formula_descriptor(&block.kind).is_some()
                    && (index + 1 == self.blocks.len()
                        || formula_descriptor(&self.blocks[index + 1].kind)
                            != formula_descriptor(&block.kind))
            })
            .collect();
        for (i, b) in self.blocks.iter_mut().enumerate() {
            let n = b.line.build_layout(taffy, theme, node_map);
            // Per-block top margin (not the parent's `gap`) so it can be
            // zeroed for soft-break continuations, the first block, and
            // consecutive code lines (one contiguous fenced unit).
            let continues_code_block = is_code.get(i).copied().unwrap_or(false)
                && i.checked_sub(1)
                    .and_then(|p| is_code.get(p))
                    .copied()
                    .unwrap_or(false);
            let top_margin: f32 = if i == 0 || b.soft_break || continues_code_block {
                0.0
            } else {
                8.0
            };
            if top_margin > 0.0 || is_formula_end[i] {
                let existing = taffy.style(n).cloned().unwrap_or_default();
                let mut s = existing;
                s.margin = taffy::Rect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(top_margin),
                    bottom: length(0.0_f32),
                };
                if is_formula_end[i] {
                    let min_height = if formula_descriptor(&b.kind).is_some_and(|descriptor| {
                        descriptor.mode == crate::math::FormulaMode::Visual
                    }) {
                        FORMULA_VISUAL_MIN_HEIGHT
                    } else {
                        FORMULA_BLOCK_MIN_HEIGHT
                    };
                    s.min_size.height = length(min_height);
                }
                let _ = taffy.set_style(n, s);
            }
            self.child_nodes.push(n);
        }
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::Stretch),
                gap: taffy::Size {
                    width: length(0.0_f32),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &self.child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        // Taffy's child list is authoritative (not `self.child_nodes`)
        // so any internal re-ordering taffy applies is tolerated.
        let children = taffy.children(node).unwrap_or_default();
        self.formula_preview_rects = vec![None; self.blocks.len()];
        self.formula_chrome = vec![None; self.blocks.len()];
        let is_formula_start: Vec<bool> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                formula_descriptor(&block.kind).is_some()
                    && (index == 0
                        || formula_descriptor(&self.blocks[index - 1].kind)
                            != formula_descriptor(&block.kind))
            })
            .collect();
        let is_formula_end: Vec<bool> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                formula_descriptor(&block.kind).is_some()
                    && (index + 1 == self.blocks.len()
                        || formula_descriptor(&self.blocks[index + 1].kind)
                            != formula_descriptor(&block.kind))
            })
            .collect();
        for (index, (b, child_node)) in self.blocks.iter_mut().zip(children.iter()).enumerate() {
            b.line.apply_layout(taffy, *child_node, new_origin);
            // Shift prefixed kinds' text rect right of a reserved gutter
            // so the prefix glyph doesn't overlap editable text.
            let gutter = prefix_gutter_for(&b.kind);
            let indent_px = f64::from(LIST_INDENT_STEP_PX) * f64::from(b.kind.indent());
            let original = b.line.get_rect();
            let descriptor = formula_descriptor(&b.kind);
            let source_top = if is_formula_start[index] {
                (original.y0 + FORMULA_HEADER_HEIGHT).min(original.y1)
            } else {
                original.y0
            };
            let source_bottom =
                if descriptor.is_some_and(|value| value.mode == crate::math::FormulaMode::Visual) {
                    source_top
                } else if is_formula_end[index] {
                    (source_top + FORMULA_SOURCE_HEIGHT).min(original.y1)
                } else {
                    original.y1
                };
            if is_formula_end[index] {
                let mode = descriptor.map_or(crate::math::FormulaMode::Source, |value| value.mode);
                let preview = Rect::new(
                    original.x0 + 12.0,
                    (if mode == crate::math::FormulaMode::Source {
                        source_bottom + FORMULA_PREVIEW_GAP
                    } else {
                        source_top + 2.0
                    })
                    .min(original.y1),
                    original.x1 - 8.0,
                    (original.y1 - 6.0).max(source_bottom + FORMULA_PREVIEW_GAP),
                );
                self.formula_preview_rects[index] = Some(preview);
            }
            if gutter > 0.0
                || indent_px > 0.0
                || is_formula_start[index]
                || descriptor.is_some_and(|value| value.mode == crate::math::FormulaMode::Visual)
                || is_formula_end[index]
            {
                let shifted = Rect::new(
                    original.x0 + indent_px + gutter,
                    source_top,
                    original.x1,
                    source_bottom,
                );
                b.line.apply_layout_external(shifted);
            }
        }
        let mut start = 0usize;
        while start < self.blocks.len() {
            let Some(descriptor) = formula_descriptor(&self.blocks[start].kind) else {
                start += 1;
                continue;
            };
            let end = formula_run_end(&self.blocks, start);
            if let Some(editor_rect) = self.formula_preview_rects[end] {
                let first = self.blocks[start].line.get_rect();
                let last = self.blocks[end].line.get_rect();
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
                self.formula_chrome[start] = Some(FormulaChrome {
                    end,
                    descriptor,
                    syntax_badge: badge,
                    source_button,
                    visual_button,
                    editor_rect,
                });
            }
            start = end + 1;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;

        // Painted BEFORE block content so the live caret stays on top.
        if self.blocks.len() == 1
            && let Some(first) = self.blocks.first()
            && matches!(first.kind, BlockKind::Paragraph)
            && first.line.value().is_empty()
        {
            let line_rect = first.line.get_rect();
            // Same vertical centring as the inner TextInput so the hint
            // reads as this line's placeholder, not a floating label.
            let size = first.line.font_size();
            let line_height = f64::from(size) * 1.25;
            let text_y = line_rect.y0 + (line_rect.height() - line_height) / 2.0;
            // 2px right of the caret so it doesn't collide with the bar.
            let caret_w = 2.0;
            let x = line_rect.x0 + caret_w + 4.0;
            text_renderer.draw_text(
                scene,
                &self.empty_hint,
                size,
                tokens.colors.text_dim,
                translated(transform, x, text_y),
                None,
            );
        }

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
        let mut formula_runs: Vec<Option<FormulaRunPreview>> = vec![None; self.blocks.len()];
        let mut formula_index = 0usize;
        while formula_index < self.blocks.len() {
            let Some(descriptor) = formula_descriptor(&self.blocks[formula_index].kind) else {
                formula_index += 1;
                continue;
            };
            let end = formula_run_end(&self.blocks, formula_index);
            let source = self.blocks[formula_index..=end]
                .iter()
                .map(Block::text)
                .collect::<Vec<_>>()
                .join("\n");
            formula_runs[formula_index] = Some(FormulaRunPreview {
                end,
                descriptor,
                source,
            });
            formula_index = end + 1;
        }

        let mut ctx = BlockDrawCtx {
            scene,
            text_renderer,
            transform,
            theme,
            presence,
            line_rects: line_rects.as_slice(),
            ordinals: ordinals.as_slice(),
        };
        let mut formulas = FormulaDrawCtx {
            runs: formula_runs.as_slice(),
            code_languages: code_languages.as_slice(),
            chrome: self.formula_chrome.as_slice(),
            preview_rects: self.formula_preview_rects.as_slice(),
            visual_formulas: &mut self.visual_formulas,
            focus: self.visual_formula_focus,
        };

        for (idx, b) in self.blocks.iter_mut().enumerate() {
            draw_block(b, idx, &mut ctx, &mut formulas);
        }

        // Runs AFTER the block loop: pixel_x_for_byte needs this frame's
        // fresh `char_x_positions`.
        for (i, b) in self.blocks.iter().enumerate() {
            self.draw_block_presence(i, b, &mut ctx);
        }
    }
}

impl BlockEditor {
    /// Peer authorship marker, live-typing glow, carets and selection
    /// bands for one block — collaboration overlays drawn after every
    /// block's own content so they land on top.
    fn draw_block_presence(&self, i: usize, b: &Block, ctx: &mut BlockDrawCtx<'_>) {
        let key = block_presence_key(i);
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
        let now = std::time::Instant::now();
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
        for (document_key, list) in &ctx.presence.selections {
            let Some((start_block, end_block)) = document_selection_blocks(document_key) else {
                continue;
            };
            if i < start_block || i > end_block {
                continue;
            }
            for (user, range) in list {
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
}

impl Widget for BlockEditor {
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, MouseButton};

        let mut ctx = EventCtx {
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        };

        // While visible, intercept keystrokes BEFORE the editable line
        // sees them. Same pattern as the slash menu below.
        if self.handle_link_popover_key(event, &mut ctx) {
            return;
        }

        // The menu owns arrow nav / Enter / Escape / typing-into-filter.
        if self.handle_slash_menu_key(event, &mut ctx) {
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
                *handled = true;
                return;
            }
            if !matches!(logical_key, Key::Named(NamedKey::Tab)) {
                let chrome = self.formula_chrome.get(start).and_then(|value| *value);
                let changed = self.visual_formulas.get_mut(&start).is_some_and(|visual| {
                    handle_visual_formula_key(&mut visual.editor, logical_key, modifiers)
                });
                if changed && let Some(chrome) = chrome {
                    self.commit_visual_formula_edit(start, chrome.end, chrome.descriptor.syntax);
                }
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // Select-all is document-scoped, like a browser editing host, rather
        // than stopping at the focused paragraph's TextInput.
        if self.try_select_all_shortcut(event, &mut ctx) {
            return;
        }

        // A cross-block selection belongs to the document, not any one child
        // TextInput. Consume clipboard/replacement input before line routing.
        if self.handle_document_selection_event(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        // Reset the anchor on anything but a Ctrl+Shift+Arrow extend, so
        // the next extend latches a fresh one.
        self.maybe_reset_kbd_sel_anchor(event, ctx.modifiers);

        // Runs BEFORE event routing so the click doesn't fall through to
        // TextInput (which would place the caret at byte 0).
        if self.handle_mouse_press_gestures(event, &mut ctx) {
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

        // TextInput does NOT handle Tab, but intercept anyway so future
        // TextInput changes don't break the contract.
        if self.try_tab_indent(event, &mut ctx) {
            return;
        }

        // The leading `/` still falls through to TextInput (visible
        // feedback); `slash_open_at` remembers where to strip it on
        // commit. Code blocks excluded — `/` there is meaningful text.
        self.try_open_slash_menu(event);

        // Focus whichever editable block the press lands on first, so the
        // routing below's TextInput click handler sets the caret there.
        self.focus_block_under_mouse(event, &mut ctx);

        self.route_event_to_focused_block(event, &mut ctx);
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
        // Drain pending_selection regardless of `consumed` — Enter may have set it.
        if let Some(new_kind) = self.slash_menu.take_selected_kind() {
            self.commit_slash_selection(new_kind);
        }
        if !self.slash_menu.is_visible() {
            self.slash_open_at = None;
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

    /// Reset the keyboard-selection anchor on anything but a
    /// Ctrl+Shift+Arrow extend, so the next extend latches a fresh one.
    fn maybe_reset_kbd_sel_anchor(&mut self, event: &WindowEvent, modifiers: &Modifiers) {
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
                let is_extend = (m.control_key() || m.super_key())
                    && m.shift_key()
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
        // A click on a slash-menu row selects that kind (strips the
        // trigger, like Enter); elsewhere dismisses it. Checked first
        // since the popover paints on top.
        if self.slash_menu.is_visible() {
            if let Some(new_kind) = self.slash_menu.click_at(self.last_mouse_pos) {
                self.commit_slash_selection(new_kind);
                self.slash_open_at = None;
                *ctx.handled = true;
                self.dirty = true;
                return true;
            }
            self.slash_menu.set_visible(false);
            self.slash_open_at = None;
            self.dirty = true;
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
            let byte = b.line.inner_mut().byte_at_x(self.last_mouse_pos.x);
            self.cross_block_drag_start = Some((block_idx, byte));
        }
        false
    }

    /// Tracks the pointer for slash-menu hover and cross-block drag
    /// painting.
    fn handle_cursor_moved(&mut self, event: &WindowEvent, scale_factor: f64) {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_mouse_pos = logical;

        // Hovering a slash-menu row moves the highlight to it, so
        // mouse and keyboard drive the same selection cursor.
        if self.slash_menu.is_visible() && self.slash_menu.hover_at(logical) {
            self.dirty = true;
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
                    .map_or(0, |b| b.line.inner_mut().byte_at_x(logical.x));
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
                    logical_key: Key::Named(NamedKey::Tab),
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
        let supports = self
            .blocks
            .get(focused_idx)
            .is_some_and(|b| b.kind.supports_indent());
        if !supports {
            return false;
        }
        let delta = if ctx.modifiers.state().shift_key() {
            -1
        } else {
            1
        };
        self.try_indent(focused_idx, delta);
        *ctx.handled = true;
        true
    }

    /// The leading `/` still falls through to `TextInput` (visible
    /// feedback); `slash_open_at` remembers where to strip it on commit.
    /// Code blocks excluded — `/` there is meaningful text.
    fn try_open_slash_menu(&mut self, event: &WindowEvent) {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::Key;

        if self.slash_menu.is_visible() {
            return;
        }
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
            return;
        };
        if c.as_str() != "/" {
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
        let caret_byte = self
            .blocks
            .get(focused_idx)
            .map_or(0, |b| b.line.cursor_pos());
        let line_rect = self
            .blocks
            .get(focused_idx)
            .map_or(Rect::ZERO, |b| b.line.get_rect());
        self.slash_menu
            .set_anchor(Point::new(line_rect.x0, line_rect.y1 + 2.0));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = Some(caret_byte);
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
        }
        *ctx.focus_target = Some(ctx.node_id);
    }

    /// Routes the event into the focused block's own `TextInput`, drains
    /// any structural signal it emitted, and promotes `# `/`## `/`### `
    /// paragraphs to headings on the keystroke that completes the prefix.
    fn route_event_to_focused_block(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(focused_idx) = self.focused else {
            return;
        };
        // Snapshot the text to distinguish an actual content change
        // (typing) from a mere caret move (a click) — the heading
        // shortcut fires only on the former. `None` for non-paragraph
        // blocks skips the check.
        let before_text = self
            .blocks
            .get(focused_idx)
            .filter(|b| matches!(b.kind, BlockKind::Paragraph))
            .map(|b| b.line.value().to_string());
        if let Some(b) = self.blocks.get_mut(focused_idx)
            && b.kind.is_editable()
        {
            b.line.handle_event(
                event,
                ctx.scale_factor,
                ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.handled,
                ctx.focus_target,
                ctx.node_id,
                ctx.taffy,
            );
        }
        if let Some(sig) = self
            .blocks
            .get_mut(focused_idx)
            .and_then(|b| b.line.take_signal())
        {
            self.apply_signal(focused_idx, &sig);
        }
        // Promote only when the text just changed AND everything
        // before the caret is exactly a heading prefix — a click that
        // parks the caret after an existing "# " doesn't promote,
        // nor does a space typed mid-line.
        if let Some(before) = before_text
            && let Some(b) = self.blocks.get(focused_idx)
        {
            let now = b.line.value();
            if now != before {
                let caret = b.line.cursor_pos().min(now.len());
                if now.get(..caret).is_some_and(is_heading_prefix) {
                    let _ = self.try_promote_to_heading(focused_idx);
                }
            }
        }
    }
}

impl Widget for BlockEditor {
    fn name(&self) -> &'static str {
        "BlockEditor"
    }

    fn test_id(&self) -> Option<&str> {
        Some("block_editor")
    }

    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        use crate::collaboration::PresencePayload;
        let idx = self.focused?;
        let block = self.blocks.get(idx)?;
        if !block.line.is_focused() {
            return None;
        }
        if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            return Some((
                document_selection_presence_key(start.block, end.block),
                PresencePayload::Selection {
                    start: start.byte,
                    end: end.byte,
                },
            ));
        }
        let key = block_presence_key(idx);
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

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.formula_chrome.iter().flatten().any(|chrome| {
            chrome.source_button.contains(point) || chrome.visual_button.contains(point)
        }) {
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

    fn set_focus(&mut self, focused: bool) {
        Self::set_focus(self, focused);
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.link_popover.is_visible() {
            return Widget::ime_cursor_rect(&self.link_popover);
        }
        if self.slash_menu.is_visible() {
            return Widget::ime_cursor_rect(&self.slash_menu);
        }
        let block = self.blocks.get(self.focused?)?;
        Widget::ime_cursor_rect(&block.line)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    /// Block contents are not surfaced as a `value` — a screen reader
    /// walks the per-block subtree instead.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let formulas = self
            .formula_chrome
            .iter()
            .enumerate()
            .filter_map(|(start, chrome)| {
                let chrome = chrome.as_ref()?;
                let syntax = match chrome.descriptor.syntax {
                    crate::math::FormulaSyntax::Tex => "TeX",
                    crate::math::FormulaSyntax::Typst => "Typst",
                };
                let source = self.blocks[start..=chrome.end]
                    .iter()
                    .map(Block::text)
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(
                    AccessNode::new(Role::Group)
                        .with_name(format!("{syntax} formula"))
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
        Some(
            AccessNode::new(Role::Document)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    multiline: true,
                    ..Default::default()
                })
                .with_children(formulas),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.visual_formula_focus.is_some()
            || self.blocks.iter().any(|b| b.line.is_focused())
    }

    fn needs_commit(&self) -> bool {
        self.commit_pending
            || self
                .blocks
                .iter()
                .any(|block| Widget::needs_commit(block.line.inner()))
    }

    fn on_commit(&mut self) {
        self.commit_pending = false;
        for block in &mut self.blocks {
            Widget::on_commit(block.line.inner_mut());
        }
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn take_structure_dirty(&mut self) -> bool {
        let was = self.structure_dirty;
        self.structure_dirty = false;
        was
    }
}

impl BlockEditor {
    fn apply_signal(&mut self, idx: usize, sig: &LineSignal) {
        match sig {
            LineSignal::Enter { split_at } => {
                self.split_at_cursor(idx, *split_at);
            }
            LineSignal::SoftNewline { split_at } => {
                // Same as Enter, but marks the new block soft_break.
                self.split_at_cursor(idx, *split_at);
                let new_idx = idx.saturating_add(1);
                if let Some(b) = self.blocks.get_mut(new_idx) {
                    b.soft_break = true;
                }
            }
            LineSignal::BackspaceAtStart => {
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
                    self.focus_block(idx.saturating_sub(1), CursorPlacement::End);
                }
            }
            LineSignal::ArrowDown => {
                self.kbd_sel_anchor = None;
                let next_idx = idx.saturating_add(1);
                if next_idx < self.blocks.len() {
                    self.focus_block(next_idx, CursorPlacement::Start);
                }
            }
            LineSignal::SelectBlockUp { caret } => {
                self.extend_block_selection(idx, *caret, -1);
            }
            LineSignal::SelectBlockDown { caret } => {
                self.extend_block_selection(idx, *caret, 1);
            }
            LineSignal::LinkOpen { open_at } => {
                // Anchored at the line's bottom-left, like SlashMenu.
                let line_rect = self
                    .blocks
                    .get(idx)
                    .map_or(Rect::ZERO, |b| b.line.get_rect());
                self.link_popover
                    .set_anchor(Point::new(line_rect.x0, line_rect.y1 + 2.0));
                self.link_popover.set_filter("");
                self.link_popover.set_visible(true);
                self.link_open_at = Some(*open_at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessibility_reports_multiline_document_role() {
        use crate::a11y::Role;
        let ed = BlockEditor::new();
        let node = Widget::accessibility(&ed).expect("editor is a document");
        assert_eq!(node.role, Role::Document);
        assert!(node.state.multiline, "a document is multiline");
        assert!(node.state.focusable);
        assert!(!node.state.focused, "fresh editor is not focused");
        assert_eq!(
            node.value, None,
            "the document text is not dumped into value"
        );
    }

    #[test]
    fn accessibility_focus_tracks_editor_focus() {
        let mut ed = BlockEditor::new();
        Widget::set_focus(&mut ed, true);
        let node = Widget::accessibility(&ed).unwrap();
        assert!(node.state.focused);
    }

    #[test]
    fn slash_menu_owns_ime_without_committing_filter_text_into_the_document() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("/")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.open_slash_menu_for_test();

        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy = TaffyTree::<()>::new();
        let mut send = |editor: &mut BlockEditor, event: &WindowEvent| {
            let mut handled = false;
            let mut focus_target = None;
            editor.handle_event(
                event,
                1.0,
                &Modifiers::default(),
                &mut state,
                &root,
                &mut handled,
                &mut focus_target,
                NodeId::new(0),
                &taffy,
            );
            assert!(handled);
        };

        send(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Preedit("h2".into(), Some((1, 1)))),
        );
        assert_eq!(editor.blocks()[0].text(), "/");
        assert_eq!(editor.slash_menu().filter(), "");

        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        editor.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
        assert!(Widget::ime_cursor_rect(&editor).is_some());

        send(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Commit("h2".into())),
        );
        assert_eq!(editor.blocks()[0].text(), "/");
        assert_eq!(editor.slash_menu().filter(), "h2");
        assert_eq!(editor.slash_menu().visible_len(), 1);
    }

    #[test]
    fn editor_adopts_blocks_from_doc() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = AutoCommit::new();
        let list = doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();
        let b0 = doc.insert_object(&list, 0, ObjType::Map).unwrap();
        doc.put(&b0, "kind", "heading1").unwrap();
        let t0 = doc.put_object(&b0, "text", ObjType::Text).unwrap();
        doc.splice_text(&t0, 0, 0, "Title").unwrap();

        let mut ed = BlockEditor::new();
        ed.bind_blocks(&mut doc, list);
        // Non-empty doc -> adopt it.
        assert_eq!(ed.blocks().len(), 1);
        assert_eq!(ed.blocks()[0].text(), "Title");
        assert!(matches!(ed.blocks()[0].kind, BlockKind::Heading(1)));
    }

    #[test]
    fn editor_seeds_empty_doc_from_local_blocks() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = AutoCommit::new();
        let list = doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();

        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("hello")]);
        ed.bind_blocks(&mut doc, list.clone());

        // Empty doc -> seeded from local blocks.
        assert_eq!(doc.length(&list), 1);
        let t = doc_text_obj(&doc, &list, 0).unwrap();
        assert_eq!(doc.text(&t).unwrap(), "hello");
    }

    #[test]
    fn two_editors_converge_on_block_text() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut a_doc = AutoCommit::new();
        let a_list = a_doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();
        let mut a = BlockEditor::new();
        a.set_blocks(vec![Block::paragraph("from-A")]);
        a.bind_blocks(&mut a_doc, a_list);

        // Peer B joins from A's full state (the late-join handshake).
        let mut b_doc = AutoCommit::load(&a_doc.save()).unwrap();
        let Ok(Some((Value::Object(ObjType::List), b_list))) = b_doc.get(&ROOT, "blocks") else {
            panic!("blocks list should exist in B's adopted doc");
        };
        let mut b = BlockEditor::new();
        b.bind_blocks(&mut b_doc, b_list);

        assert_eq!(b.blocks()[0].text(), "from-A", "B adopts A's block text");
    }

    #[test]
    fn split_then_sync_mirrors_structure_into_doc() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = AutoCommit::new();
        let list = doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();
        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("abcdef")]);
        ed.bind_blocks(&mut doc, list.clone());
        assert_eq!(doc.length(&list), 1);

        ed.split_at_cursor(0, 3);
        ed.sync(&mut doc, &ROOT);

        assert_eq!(doc.length(&list), 2, "doc gained the split block");
        let t0 = doc_text_obj(&doc, &list, 0).unwrap();
        let t1 = doc_text_obj(&doc, &list, 1).unwrap();
        assert_eq!(doc.text(&t0).unwrap(), "abc");
        assert_eq!(doc.text(&t1).unwrap(), "def");
    }

    #[test]
    fn presence_payload_reports_focused_block_caret() {
        use crate::collaboration::PresencePayload;
        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("hello world")]);
        ed.focus_block(0, CursorPlacement::Byte(3));
        match ed.presence_payload() {
            Some((key, PresencePayload::Caret { pos })) => {
                assert_eq!(key, block_presence_key(0));
                assert_eq!(pos, 3);
            }
            other => panic!("expected caret payload at byte 3, got {other:?}"),
        }
    }

    #[test]
    fn presence_payload_carries_one_ordered_cross_block_selection() {
        use crate::collaboration::PresencePayload;

        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::paragraph("middle"),
            Block::paragraph("omega"),
        ]);
        ed.select_document_range(
            DocumentPosition { block: 2, byte: 3 },
            DocumentPosition { block: 0, byte: 2 },
        );

        assert_eq!(
            ed.presence_payload(),
            Some((
                document_selection_presence_key(0, 2),
                PresencePayload::Selection { start: 2, end: 3 },
            ))
        );
    }

    #[test]
    fn presence_payload_none_when_unfocused() {
        let mut ed = BlockEditor::new();
        ed.set_blocks(vec![Block::paragraph("hi")]);
        // set_blocks leaves the editor unfocused.
        assert!(ed.presence_payload().is_none());
    }

    #[test]
    fn click_focuses_the_block_under_the_cursor() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

        let mut e = BlockEditor::new();
        e.set_blocks(vec![Block::paragraph("first"), Block::paragraph("second")]);
        // Stack two rows: block 0 at y[0,20], block 1 at y[24,44].
        e.blocks[0]
            .line
            .apply_layout_external(Rect::new(0.0, 0.0, 200.0, 20.0));
        e.blocks[1]
            .line
            .apply_layout_external(Rect::new(0.0, 24.0, 200.0, 44.0));
        e.focus_block(0, CursorPlacement::End);
        assert_eq!(e.focused, Some(0));

        let dev: DeviceId = unsafe { std::mem::zeroed() };
        let moved = WindowEvent::CursorMoved {
            device_id: dev,
            position: PhysicalPosition::new(50.0, 30.0), // inside block 1
        };
        let click = WindowEvent::MouseInput {
            device_id: dev,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let mods = Modifiers::default();
        for ev in [&moved, &click] {
            let mut handled = false;
            let mut focus_target = None;
            e.handle_event(
                ev,
                1.0,
                &mods,
                &mut state,
                &root,
                &mut handled,
                &mut focus_target,
                NodeId::new(0),
                &taffy,
            );
        }
        assert_eq!(
            e.focused,
            Some(1),
            "clicking inside block 1 must move focus (and the caret) there"
        );
    }

    #[test]
    fn cross_block_selection_serializes_plain_text_in_document_order() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::heading(2, "βeta"),
            Block::paragraph("omega"),
        ]);

        // A backwards drag still copies in reading order. The endpoints are
        // UTF-8 byte offsets: after β in block 1, after "om" in block 2.
        e.paint_cross_block_selection(2, 2, 0, 2);
        assert_eq!(
            e.document_selection(),
            Some(DocumentSelection {
                anchor: DocumentPosition { block: 2, byte: 2 },
                cursor: DocumentPosition { block: 0, byte: 2 },
            })
        );
        assert_eq!(e.selected_text().as_deref(), Some("pha\nβeta\nom"));
    }

    #[test]
    fn deleting_cross_block_selection_merges_prefix_and_suffix() {
        let mut e = BlockEditor::new();
        let mut first = Block::heading(1, "Hello world");
        first.line.set_marks(vec![MarkSpan::new(0..5, Mark::Bold)]);
        let mut last = Block::paragraph("goodbye");
        last.line.set_marks(vec![MarkSpan::new(4..7, Mark::Italic)]);
        e.set_blocks(vec![first, Block::paragraph("middle"), last]);
        e.paint_cross_block_selection(0, 6, 2, 4);

        assert!(e.replace_document_selection(""));
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "Hello bye");
        assert_eq!(e.blocks()[0].kind, BlockKind::Heading(1));
        assert_eq!(
            e.blocks()[0].line.marks(),
            &[
                MarkSpan::new(0..5, Mark::Bold),
                MarkSpan::new(6..9, Mark::Italic),
            ],
            "unselected inline formatting survives the merge"
        );
        assert_eq!(e.focused_index(), Some(0));
        assert_eq!(e.blocks()[0].line.cursor_pos(), 6);
        assert_eq!(e.document_selection(), None);
        assert!(
            Widget::needs_commit(&e),
            "replacement arms one undo boundary"
        );
        Widget::on_commit(&mut e);
        assert!(!Widget::needs_commit(&e), "the boundary is rising-edge");
    }

    #[test]
    fn multiline_paste_replaces_range_and_uses_enter_continuation_kinds() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::bullet(1, "before selected"),
            Block::paragraph("discard me"),
            Block::paragraph("selected after"),
        ]);
        e.paint_cross_block_selection(0, 7, 2, 8);

        assert!(e.replace_document_selection("one\r\ntwo\rthree\u{0}four"));
        assert_eq!(
            e.blocks().iter().map(Block::text).collect::<Vec<_>>(),
            vec!["before one", "two", "threefour after"]
        );
        assert_eq!(e.blocks()[0].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.blocks()[1].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.blocks()[2].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(e.focused_index(), Some(2));
        assert_eq!(e.blocks()[2].line.cursor_pos(), "threefour".len());
    }

    #[test]
    fn ctrl_a_then_typing_replaces_the_document_through_the_real_event_path() {
        use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
        use winit::keyboard::{Key, KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

        fn character(key: &str, text: Option<&str>) -> WindowEvent {
            let mut event: KeyEvent = unsafe { std::mem::zeroed() };
            event.logical_key = Key::Character(key.into());
            event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
            event.location = KeyLocation::Standard;
            event.state = ElementState::Pressed;
            event.repeat = false;
            event.text = text.map(Into::into);
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event,
                is_synthetic: false,
            }
        }

        fn dispatch(
            editor: &mut BlockEditor,
            state: &mut AutoCommit,
            taffy: &TaffyTree<()>,
            event: &WindowEvent,
            modifiers: Modifiers,
        ) {
            let mut handled = false;
            let mut focus_target = None;
            editor.handle_event(
                event,
                1.0,
                &modifiers,
                state,
                &automerge::ROOT,
                &mut handled,
                &mut focus_target,
                NodeId::new(0),
                taffy,
            );
            assert!(handled);
        }

        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("alpha"),
            Block::heading(2, "middle"),
            Block::paragraph("omega"),
        ]);
        editor.focus_block(1, CursorPlacement::Byte(3));
        let mut state = AutoCommit::new();
        let taffy = TaffyTree::new();
        dispatch(
            &mut editor,
            &mut state,
            &taffy,
            &character("a", None),
            Modifiers::from(ModifiersState::CONTROL),
        );
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("alpha\nmiddle\nomega")
        );

        dispatch(
            &mut editor,
            &mut state,
            &taffy,
            &character("X", Some("X")),
            Modifiers::default(),
        );
        assert_eq!(editor.blocks().len(), 1);
        assert_eq!(editor.blocks()[0].text(), "X");
        assert_eq!(editor.blocks()[0].kind, BlockKind::Paragraph);
        assert!(Widget::needs_commit(&editor));
    }

    #[test]
    fn cross_block_replacement_syncs_as_one_coherent_document_state() {
        use automerge::{ObjType, ROOT, transaction::Transactable};

        let mut doc = AutoCommit::new();
        let list = doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();
        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::paragraph("left OLD"),
            Block::paragraph("remove"),
            Block::paragraph("OLD right"),
        ]);
        e.bind_blocks(&mut doc, list.clone());
        e.paint_cross_block_selection(0, 5, 2, 3);

        assert!(e.replace_document_selection("new\nline"));
        e.sync(&mut doc, &ROOT);

        assert_eq!(doc.length(&list), 2);
        let first = doc_text_obj(&doc, &list, 0).unwrap();
        let second = doc_text_obj(&doc, &list, 1).unwrap();
        assert_eq!(doc.text(&first).unwrap(), "left new");
        assert_eq!(doc.text(&second).unwrap(), "line right");
    }

    #[test]
    fn ctrl_shift_arrow_extends_selection_across_blocks() {
        use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
        use winit::keyboard::{
            Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey,
        };

        fn arrow(named: NamedKey) -> WindowEvent {
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = Key::Named(named);
            ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
            ev.location = KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            }
        }

        let mut e = BlockEditor::new();
        e.set_blocks(vec![
            Block::paragraph("hello"),
            Block::paragraph("world"),
            Block::paragraph("foo"),
        ]);
        e.focus_block(0, CursorPlacement::Byte(2)); // caret after "he"

        let ctrl_shift = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);

        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let mut send = |e: &mut BlockEditor, ev: &WindowEvent| {
            let mut handled = false;
            let mut ft = None;
            e.handle_event(
                ev,
                1.0,
                &ctrl_shift,
                &mut state,
                &root,
                &mut handled,
                &mut ft,
                NodeId::new(0),
                &taffy,
            );
        };

        // Down once: selects the tail of block 0 ("llo"), caret into block 1.
        send(&mut e, &arrow(NamedKey::ArrowDown));
        assert_eq!(e.focused_index(), Some(1), "active end walked into block 1");
        assert_eq!(
            e.blocks()[0].line.selection_range(),
            Some((2, 5)),
            "the jumped tail of block 0 is now selected (real key path)"
        );

        // Down again: block 1 fully selected too.
        send(&mut e, &arrow(NamedKey::ArrowDown));
        assert_eq!(e.focused_index(), Some(2));
        assert_eq!(e.blocks()[0].line.selection_range(), Some((2, 5)));
        assert_eq!(
            e.blocks()[1].line.selection_range(),
            Some((0, 5)),
            "extending over block 1 selects it whole"
        );

        // Retreat up: block 2 cleared, block 1 selected to its end.
        send(&mut e, &arrow(NamedKey::ArrowUp));
        assert_eq!(e.focused_index(), Some(1));
        assert_eq!(
            e.blocks()[2].line.selection_range(),
            None,
            "retreating clears the block we left"
        );
        assert_eq!(e.blocks()[1].line.selection_range(), Some((0, 5)));
    }

    #[test]
    fn plain_arrow_does_not_start_a_selection() {
        let mut e = BlockEditor::new();
        e.set_blocks(vec![Block::paragraph("hello"), Block::paragraph("world")]);
        e.focus_block(0, CursorPlacement::Byte(2));
        e.apply_signal(0, &LineSignal::ArrowDown);
        assert_eq!(e.focused_index(), Some(1));
        assert_eq!(e.blocks()[0].line.selection_range(), None);
        assert_eq!(e.blocks()[1].line.selection_range(), None);
    }

    #[test]
    fn code_block_cells_render_monospace() {
        let b = Block::code_block(Some("rust".into()), "let x = 1;");
        assert!(
            b.line.inner().is_mono(),
            "code-block cells must shape their text in the monospace face"
        );
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("plain");
        e.focused = Some(0);
        assert!(e.try_convert_to(0, BlockKind::CodeBlock { lang: None }));
        assert!(
            e.blocks()[0].line.inner().is_mono(),
            "converting to a code block switches the cell to monospace"
        );
        assert!(!Block::paragraph("hi").line.inner().is_mono());
    }

    #[test]
    fn formula_fence_reserves_preview_below_editable_source() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::code_block(Some("tex".into()), r"\frac{x^2+1}{\sqrt{y}}"),
            Block::paragraph("following paragraph"),
        ]);
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = editor.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        editor.apply_layout(&taffy, node, Point::ZERO);

        let preview = editor.formula_preview_rects[0].expect("TeX preview geometry");
        assert!(preview.height() > 70.0);
        assert!(preview.y0 >= editor.blocks[0].line.get_rect().y1);
        assert!(editor.blocks[1].line.get_rect().y0 > preview.y1);
        assert!(editor.content_bottom() >= editor.blocks[1].line.get_rect().y1);
    }

    #[test]
    fn formula_mode_is_persisted_and_visual_edits_emit_native_source() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::code_block(
            Some("typst".into()),
            "vec(alpha, beta, gamma)",
        )]);
        let original = editor.blocks[0].text().to_string();
        editor.set_formula_mode(0, crate::math::FormulaMode::Visual);
        assert!(matches!(
            &editor.blocks[0].kind,
            BlockKind::CodeBlock { lang: Some(language) } if language == "typst visual"
        ));
        assert_eq!(editor.blocks[0].text(), original);
        editor.set_formula_mode(0, crate::math::FormulaMode::Source);
        assert_eq!(editor.blocks[0].text(), original);
        editor.set_formula_mode(0, crate::math::FormulaMode::Visual);

        let source = editor.blocks[0].text().to_string();
        assert_eq!(source, original);
        editor
            .visual_formulas
            .get_mut(&0)
            .unwrap()
            .editor
            .type_char('+');
        editor.commit_visual_formula_edit(0, 0, crate::math::FormulaSyntax::Typst);
        assert!(editor.blocks[0].text().starts_with("vec("));
        assert!(editor.blocks[0].text().ends_with('+'));
        assert!(Widget::needs_commit(&editor));
    }

    #[test]
    fn new_has_one_empty_paragraph() {
        let e = BlockEditor::new();
        assert_eq!(e.blocks().len(), 1);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "");
    }

    #[test]
    fn split_inside_text_produces_two_blocks() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abcdefg");
        e.focused = Some(0);
        e.split_at_cursor(0, 3);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "abc");
        assert_eq!(e.blocks()[1].text(), "defg");
        assert!(matches!(e.blocks()[1].kind, BlockKind::Paragraph));
        assert_eq!(e.focused_index(), Some(1));
        assert!(e.blocks()[1].line.cursor_at_start());
    }

    #[test]
    fn split_at_end_creates_empty_block_below() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "hello");
        assert_eq!(e.blocks()[1].text(), "");
        assert_eq!(e.focused_index(), Some(1));
    }

    #[test]
    fn merge_joins_and_lands_cursor_at_join() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abc");
        e.blocks.push(Block::paragraph("def"));
        e.focused = Some(1);
        e.merge_with_previous(1);
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "abcdef");
        // Caret lands at byte 3 — the join point.
        assert_eq!(e.blocks()[0].line.cursor_pos(), 3);
        assert_eq!(e.focused_index(), Some(0));
    }

    #[test]
    fn merge_at_index_zero_is_noop() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("abc");
        e.focused = Some(0);
        e.merge_with_previous(0);
        assert_eq!(e.blocks().len(), 1);
        assert_eq!(e.blocks()[0].text(), "abc");
    }

    #[test]
    fn delete_empty_mid_list_shifts_focus_back() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("first");
        e.blocks.push(Block::paragraph(""));
        e.blocks.push(Block::paragraph("third"));
        e.focused = Some(1);
        e.delete_empty(1);
        assert_eq!(e.blocks().len(), 2);
        assert_eq!(e.blocks()[0].text(), "first");
        assert_eq!(e.blocks()[1].text(), "third");
        // Focus walks back to the previous block.
        assert_eq!(e.focused_index(), Some(0));
        // ...with the caret at the end of "first".
        assert_eq!(e.blocks()[0].line.cursor_pos(), 5);
    }

    #[test]
    fn delete_empty_first_block_keeps_at_least_one() {
        let mut e = BlockEditor::new();
        e.focused = Some(0);
        e.delete_empty(0);
        assert_eq!(e.blocks().len(), 1);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "");
    }

    #[test]
    fn promote_to_h1_from_hash_space() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("# x");
        e.focused = Some(0);
        let promoted = e.try_promote_to_heading(0);
        assert!(promoted);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(1)));
        assert_eq!(e.blocks()[0].text(), "x");
        assert_eq!(e.blocks()[0].line.cursor_pos(), 0);
    }

    #[test]
    fn promote_to_h2_and_h3() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("## title");
        e.focused = Some(0);
        e.try_promote_to_heading(0);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(2)));
        assert_eq!(e.blocks()[0].text(), "title");

        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("### sub");
        e.try_promote_to_heading(0);
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(3)));
        assert_eq!(e.blocks()[0].text(), "sub");
    }

    #[test]
    fn promote_does_nothing_without_prefix() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        assert!(!e.try_promote_to_heading(0));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
    }

    #[test]
    fn promote_does_nothing_on_existing_heading() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::heading(1, "# hi");
        // Even though the text starts with "# ", we don't double-
        // promote a heading.
        assert!(!e.try_promote_to_heading(0));
    }

    #[test]
    fn focus_block_clamps_to_range() {
        let mut e = BlockEditor::new();
        e.blocks.push(Block::paragraph("second"));
        e.focus_block(99, CursorPlacement::End);
        assert_eq!(e.focused_index(), Some(1));
    }

    #[test]
    fn font_size_per_kind() {
        assert!(BlockKind::Heading(1).font_size() > BlockKind::Heading(2).font_size());
        assert!(BlockKind::Heading(2).font_size() > BlockKind::Heading(3).font_size());
        assert!(BlockKind::Heading(3).font_size() > BlockKind::Paragraph.font_size());
    }

    #[test]
    fn try_convert_paragraph_to_h1() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        assert!(e.try_convert_to(0, BlockKind::Heading(1)));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(1)));
        assert_eq!(e.blocks()[0].text(), "hello");
    }

    #[test]
    fn try_convert_h1_to_bullet_preserves_text_resets_indent() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::heading(1, "task");
        assert!(e.try_convert_to(0, BlockKind::Bullet { indent: 0 }));
        match &e.blocks()[0].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 0),
            k => panic!("expected Bullet, got {k:?}"),
        }
        assert_eq!(e.blocks()[0].text(), "task");
    }

    #[test]
    fn numbered_ordinals_count_within_run() {
        let blocks = vec![
            Block::numbered(0, "a"),
            Block::numbered(0, "b"),
            Block::numbered(0, "c"),
        ];
        assert_eq!(numbered_ordinals(&blocks), vec![1, 2, 3]);
    }

    #[test]
    fn numbered_ordinals_reset_after_interruption() {
        let blocks = vec![
            Block::numbered(0, "a"),
            Block::numbered(0, "b"),
            Block::paragraph("break"),
            Block::numbered(0, "c"),
        ];
        // The paragraph breaks the run; the third item restarts at 1.
        assert_eq!(numbered_ordinals(&blocks), vec![1, 2, 0, 1]);
    }

    #[test]
    fn numbered_ordinals_nested_restart_and_resume() {
        let blocks = vec![
            Block::numbered(0, "one"),
            Block::numbered(1, "inner a"),
            Block::numbered(1, "inner b"),
            Block::numbered(0, "two"),
        ];
        // Outer: 1 … 2; inner level restarts at 1,2; outer resumes at 2.
        assert_eq!(numbered_ordinals(&blocks), vec![1, 1, 2, 2]);
    }

    #[test]
    fn enter_continues_a_numbered_list() {
        // Pressing Enter mid-numbered-item continues the list: the suffix
        // becomes a NEW numbered item at the same indent (the renderer
        // then numbers it 2).
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::numbered(0, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5); // caret at end of "first"
        assert_eq!(e.blocks().len(), 2);
        assert!(matches!(
            e.blocks()[1].kind,
            BlockKind::Numbered { indent: 0 }
        ));
        assert_eq!(numbered_ordinals(e.blocks()), vec![1, 2]);
    }

    #[test]
    fn try_convert_paragraph_to_numbered() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("step");
        assert!(e.try_convert_to(0, BlockKind::Numbered { indent: 0 }));
        assert!(matches!(
            e.blocks()[0].kind,
            BlockKind::Numbered { indent: 0 }
        ));
        assert_eq!(e.blocks()[0].text(), "step");
    }

    #[test]
    fn try_indent_numbered_steps_up_and_down() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::numbered(0, "a");
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
        assert!(e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 0);
    }

    #[test]
    fn try_convert_bullet_to_paragraph() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(2, "item");
        assert!(e.try_convert_to(0, BlockKind::Paragraph));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
        assert_eq!(e.blocks()[0].text(), "item");
    }

    #[test]
    fn try_convert_to_same_kind_is_noop() {
        let mut e = BlockEditor::new();
        assert!(!e.try_convert_to(0, BlockKind::Paragraph));
    }

    #[test]
    fn try_convert_to_divider_appends_trailing_paragraph_when_focused() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("body");
        e.focused = Some(0);
        e.blocks[0].line.set_focus(true);
        e.is_focused = true;
        assert!(e.try_convert_to(0, BlockKind::Divider));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Divider));
        // Focus moved to the new paragraph spliced after the divider.
        assert_eq!(e.blocks().len(), 2);
        assert!(matches!(e.blocks()[1].kind, BlockKind::Paragraph));
        assert_eq!(e.focused_index(), Some(1));
    }

    #[test]
    fn try_indent_bullet_steps_up_and_down() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(0, "a");
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
        assert!(e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), 2);
        assert!(e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 1);
    }

    #[test]
    fn try_indent_clamps_at_max_and_zero() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(MAX_LIST_INDENT, "deep");
        // +1 is clamped, returns false (no change).
        assert!(!e.try_indent(0, 1));
        assert_eq!(e.blocks()[0].kind.indent(), MAX_LIST_INDENT);

        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(0, "top");
        assert!(!e.try_indent(0, -1));
        assert_eq!(e.blocks()[0].kind.indent(), 0);
    }

    #[test]
    fn try_indent_on_paragraph_is_noop() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("body");
        assert!(!e.try_indent(0, 1));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Paragraph));
    }

    #[test]
    fn try_indent_on_checklist_preserves_done() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(0, true, "task");
        assert!(e.try_indent(0, 1));
        match &e.blocks()[0].kind {
            BlockKind::Checklist { indent, done } => {
                assert_eq!(*indent, 1);
                assert!(*done, "toggling indent must not change done");
            }
            k => panic!("expected Checklist, got {k:?}"),
        }
    }

    #[test]
    fn try_toggle_check_flips_done_flag() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(0, false, "task");
        assert_eq!(e.try_toggle_check(0), Some(true));
        match &e.blocks()[0].kind {
            BlockKind::Checklist { done, .. } => assert!(*done),
            _ => panic!("expected Checklist"),
        }
        assert_eq!(e.try_toggle_check(0), Some(false));
    }

    #[test]
    fn try_toggle_check_on_paragraph_is_none() {
        let mut e = BlockEditor::new();
        assert_eq!(e.try_toggle_check(0), None);
    }

    #[test]
    fn split_bullet_continues_list_at_same_indent() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::bullet(2, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        match &e.blocks()[1].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 2),
            k => panic!("expected Bullet at indent 2, got {k:?}"),
        }
    }

    #[test]
    fn split_checklist_creates_unchecked_followup() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::checklist(1, true, "first");
        e.focused = Some(0);
        e.split_at_cursor(0, 5);
        assert_eq!(e.blocks().len(), 2);
        match &e.blocks()[1].kind {
            BlockKind::Checklist { indent, done } => {
                assert_eq!(*indent, 1);
                assert!(!*done, "new checklist item should default to unchecked");
            }
            k => panic!("expected Checklist, got {k:?}"),
        }
    }

    #[test]
    fn supports_indent_predicate() {
        assert!(BlockKind::Bullet { indent: 0 }.supports_indent());
        assert!(
            BlockKind::Checklist {
                indent: 0,
                done: false
            }
            .supports_indent()
        );
        assert!(!BlockKind::Paragraph.supports_indent());
        assert!(!BlockKind::Heading(1).supports_indent());
        assert!(!BlockKind::CodeBlock { lang: None }.supports_indent());
        assert!(!BlockKind::Divider.supports_indent());
    }

    #[test]
    fn divider_is_not_editable() {
        assert!(!BlockKind::Divider.is_editable());
        assert!(BlockKind::Paragraph.is_editable());
        assert!(BlockKind::CodeBlock { lang: None }.is_editable());
    }

    #[test]
    fn parse_indent_two_spaces_per_level() {
        let (l, rest) = parse_list_indent("    item");
        assert_eq!(l, 2);
        assert_eq!(rest, "item");
        let (l, rest) = parse_list_indent("\t\titem");
        assert_eq!(l, 2);
        assert_eq!(rest, "item");
        let (l, rest) = parse_list_indent("no indent");
        assert_eq!(l, 0);
        assert_eq!(rest, "no indent");
    }
}
