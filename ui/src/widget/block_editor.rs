//! `BlockEditor` — a vertical stack of [`RichTextLine`]-backed blocks
//! (paragraphs + headings) wired up with split-on-Enter, merge-on-
//! Backspace, arrow-key block navigation, and markdown-shortcut
//! promotion (`# ` → H1, `## ` → H2, `### ` → H3).
//!
//! Phase 2 supports two block kinds:
//! - **Paragraph** — body text.
//! - **Heading(1..=3)** — H1 (28 pt), H2 (22 pt), H3 (18 pt).
//!
//! No demotion: once a block is a heading the user must explicitly
//! convert (Phase 3+). Promotion only fires when the user types the
//! markdown prefix into an *empty* paragraph — typing `#` inside an
//! already-non-empty paragraph leaves it as a paragraph (matches
//! Notion / Obsidian's `/` slash menu mental model: a markdown
//! shortcut acts only on the leading position of an empty line).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::rich_text_line::{LineSignal, RichTextLine};
use crate::widget::Widget;

/// Heading levels supported by the Phase 2 block editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 1, 2, or 3. Any other value is invalid; the constructor
    /// helpers in this module clamp to [1, 3].
    Heading(u8),
}

impl BlockKind {
    /// Render-time font size in CSS-like pixels. Matches what
    /// `RichTextLine::with_size` expects.
    pub fn font_size(&self) -> f32 {
        match self {
            BlockKind::Paragraph => 16.0,
            BlockKind::Heading(1) => 28.0,
            BlockKind::Heading(2) => 22.0,
            BlockKind::Heading(3) => 18.0,
            // Future-proof: unknown heading level falls back to H3.
            BlockKind::Heading(_) => 18.0,
        }
    }
}

/// One row in the editor: a kind + the editable line.
pub struct Block {
    pub kind: BlockKind,
    pub line: RichTextLine,
}

impl Block {
    pub fn paragraph(text: &str) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            line: RichTextLine::new()
                .with_text(text)
                .with_size(BlockKind::Paragraph.font_size()),
        }
    }

    pub fn heading(level: u8, text: &str) -> Self {
        let kind = BlockKind::Heading(level.clamp(1, 3));
        Self {
            kind,
            line: RichTextLine::new().with_text(text).with_size(kind.font_size()),
        }
    }

    /// Convenience read-through for tests + markdown serializer.
    pub fn text(&self) -> &str {
        self.line.value()
    }
}

/// Vertical column of paragraph + heading blocks.
pub struct BlockEditor {
    blocks: Vec<Block>,
    /// Currently focused block, if any. Used to route key events and
    /// place the caret after split/merge. `None` when the whole
    /// editor is unfocused.
    focused: Option<usize>,
    /// True iff the editor as a whole holds App-level focus. Decoupled
    /// from `focused` so we can preserve the focused-block index
    /// across blur/refocus cycles.
    is_focused: bool,
    rect: Rect,
    /// Per-block taffy node ids, filled in `build_layout`.
    child_nodes: Vec<NodeId>,
    /// True until the next `mark_drawn`. Flipped on any structural
    /// or content edit.
    dirty: bool,
    /// Set whenever the block list grows or shrinks. Surfaced via
    /// `take_structure_dirty` so the host rebuilds taffy.
    structure_dirty: bool,
    /// Internal: set to true when any block reported a user edit
    /// since the last `is_dirty()` call. Drained by `is_dirty`.
    edit_pending: bool,
}

impl BlockEditor {
    /// New editor with a single empty paragraph. Mirrors what every
    /// rich text editor does on first open — there has to be at least
    /// one block to receive focus.
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
        };
        s.blocks.push(Block::paragraph(""));
        s
    }

    /// Parse a markdown string into a fresh editor. Uses the
    /// line-based dumb parser in `apps/notes/src/markdown.rs`'s
    /// counterpart logic — here we re-implement the minimum:
    ///
    /// - Lines starting with `# `, `## `, `### ` become Heading(1..=3).
    /// - Blank lines are block separators (consecutive blank lines
    ///   collapse).
    /// - Anything else is a Paragraph.
    ///
    /// Empty input → an editor with one empty paragraph (so the user
    /// always has a place to start typing).
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
        };
        for line in md.split('\n') {
            // Skip pure blank lines — they serve as block separators
            // but don't produce a block themselves. (A trailing
            // newline at EOF is the common case.)
            if line.is_empty() {
                continue;
            }
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

    /// Inverse of [`from_markdown`]. Round-trip is byte-stable for
    /// any document this editor can produce: each block becomes one
    /// line; blocks are joined by a single blank line; the output
    /// ends in a trailing newline. So:
    ///
    /// ```text
    /// # Title\n\nFirst paragraph.\n\nSecond paragraph.\n
    /// ```
    ///
    /// round-trips identically.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for (i, b) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n");
            }
            match b.kind {
                BlockKind::Paragraph => out.push_str(b.text()),
                BlockKind::Heading(1) => {
                    out.push_str("# ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(2) => {
                    out.push_str("## ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(3) => {
                    out.push_str("### ");
                    out.push_str(b.text());
                }
                BlockKind::Heading(_) => {
                    out.push_str("### ");
                    out.push_str(b.text());
                }
            }
        }
        // Always trailing-newline. Round-trip stability: the parser
        // skips empty lines, so adding a single \n at the end does
        // not introduce a phantom block on re-parse.
        out.push('\n');
        out
    }

    /// True iff any block reported a user edit since the last call.
    /// Drains the edit flag.
    pub fn is_dirty(&mut self) -> bool {
        // Drain per-block dirty flags so they don't accumulate.
        for b in &mut self.blocks {
            if b.line.take_dirty().is_some() {
                self.edit_pending = true;
            }
        }
        let was = self.edit_pending || self.structure_dirty;
        // structure_dirty stays set until `take_structure_dirty` —
        // is_dirty merely peeks at it. But edit_pending is consumed.
        self.edit_pending = false;
        was
    }

    /// True iff at least one block is currently focused.
    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// Set editor-wide focus. When focus arrives, route it to
    /// `focused` (or block 0 if no prior focus); when focus leaves,
    /// blur the focused block but remember the index for next time.
    pub fn set_focus(&mut self, focused: bool) {
        if focused == self.is_focused {
            return;
        }
        self.is_focused = focused;
        if focused {
            if self.focused.is_none() {
                self.focused = Some(0);
            }
            if let Some(idx) = self.focused {
                if let Some(b) = self.blocks.get_mut(idx) {
                    b.line.set_focus(true);
                }
            }
        } else {
            if let Some(idx) = self.focused {
                if let Some(b) = self.blocks.get_mut(idx) {
                    b.line.set_focus(false);
                }
            }
        }
        self.dirty = true;
    }

    /// Read-only block-list view, for tests + the markdown serializer.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Mutable block-list view. Used by integration tests + the
    /// notes app's persistence path that needs to seed blocks
    /// programmatically (bypassing the event loop). Production code
    /// should prefer the public mutators (`split_at_cursor` etc.).
    pub fn blocks_mut(&mut self) -> &mut Vec<Block> {
        &mut self.blocks
    }

    /// Test/integration hook: mark the edit-pending flag so the
    /// next `is_dirty()` returns true. Necessary because programmatic
    /// `set_text` on the inner line deliberately doesn't flip dirty
    /// (it's not a user edit). Tests use this to simulate the user
    /// having edited.
    pub fn force_dirty_for_test(&mut self) {
        self.edit_pending = true;
        self.dirty = true;
        self.structure_dirty = true;
    }

    /// Currently focused block index, if any.
    pub fn focused_index(&self) -> Option<usize> {
        self.focused
    }

    /// Move focus to `idx`. Clamps to the valid block range. Lands
    /// the caret at byte 0 of the target block (used after a split
    /// to land in the new block).
    pub fn focus_block(&mut self, idx: usize, cursor_at: CursorPlacement) {
        let target = idx.min(self.blocks.len().saturating_sub(1));
        if let Some(prev) = self.focused {
            if prev != target {
                if let Some(b) = self.blocks.get_mut(prev) {
                    b.line.set_focus(false);
                }
            }
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

    // -------- internal mutators (also exposed for tests) -------------

    /// Split the block at `idx` into two blocks at byte `split_at`.
    /// The prefix stays in place (as the same kind); the suffix
    /// becomes a NEW paragraph below. After the split, focus lands
    /// at byte 0 of the new (suffix) block.
    ///
    /// Splitting at byte 0 of an empty block: still produces two
    /// blocks (the original empty one + a new empty paragraph below).
    /// This is intentional — it's how the user gets a blank line by
    /// hitting Enter twice.
    pub fn split_at_cursor(&mut self, idx: usize, split_at: usize) {
        let Some(block) = self.blocks.get_mut(idx) else {
            return;
        };
        let text = block.line.value().to_string();
        let split = split_at.min(text.len());
        // Char-boundary safety: round down to the nearest char start.
        let split = clamp_char_boundary(&text, split);
        let (prefix, suffix) = text.split_at(split);
        // Update the current block's text to the prefix; preserve kind.
        block.line.set_text(prefix);
        let suffix_block = Block::paragraph(suffix);
        self.blocks.insert(idx + 1, suffix_block);
        self.structure_dirty = true;
        self.dirty = true;
        self.focus_block(idx + 1, CursorPlacement::Start);
    }

    /// Merge the block at `idx` with the previous block: append the
    /// current block's text to the previous block's, drop the current
    /// block, and re-focus the previous block at the join byte. No-op
    /// if `idx == 0` (there's no previous block to merge into).
    ///
    /// Kind handling: the surviving block keeps the *previous* block's
    /// kind. This matches every editor — backspacing the start of a
    /// paragraph into a heading folds the paragraph text into the
    /// heading, not the other way around.
    pub fn merge_with_previous(&mut self, idx: usize) {
        if idx == 0 || idx >= self.blocks.len() {
            return;
        }
        let cur_text = self.blocks[idx].line.value().to_string();
        let prev_text = self.blocks[idx - 1].line.value().to_string();
        let join_at = prev_text.len();
        let merged = format!("{}{}", prev_text, cur_text);
        self.blocks[idx - 1].line.set_text(&merged);
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.dirty = true;
        self.focus_block(idx - 1, CursorPlacement::Byte(join_at));
    }

    /// Remove the block at `idx`, shifting focus to the previous
    /// block (or the next one if `idx == 0`). Never deletes the last
    /// block — the editor always retains at least one paragraph to
    /// receive focus. Called when the user backspaces an empty
    /// paragraph.
    pub fn delete_empty(&mut self, idx: usize) {
        if self.blocks.len() <= 1 {
            // Don't let the editor go to zero blocks. Reset the kind
            // to Paragraph if a lone heading was being deleted — the
            // user's intent is "start over with a clean slate".
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
        let target = if idx == 0 { 0 } else { idx - 1 };
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.dirty = true;
        // After removal the target index is correct only if we
        // removed something *after* it.
        let target = target.min(self.blocks.len().saturating_sub(1));
        self.focus_block(target, CursorPlacement::End);
    }

    /// Inspect the leading characters of the block at `idx`. If the
    /// block is a paragraph and its content begins with one of the
    /// markdown heading prefixes (`# `, `## `, `### `), promote the
    /// block to the corresponding heading kind and strip the prefix.
    /// Returns true if a promotion happened.
    ///
    /// Used after every user edit to honour the markdown shortcut.
    /// The caller is responsible for restricting *when* this runs —
    /// it must only fire on the user's very next keystroke after an
    /// empty paragraph, otherwise typing `#` mid-paragraph would
    /// promote the wrong way around. See `handle_event` for the
    /// gating logic.
    pub fn try_promote_to_heading(&mut self, idx: usize) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        if b.kind != BlockKind::Paragraph {
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
        b.kind = kind;
        let mut new_line = RichTextLine::new()
            .with_text(&prefix)
            .with_size(kind.font_size());
        new_line.set_focus(true);
        // Park the caret at the start of the stripped text — that's
        // where the user expects to keep typing.
        new_line.set_cursor_pos(0);
        b.line = new_line;
        self.structure_dirty = true;
        self.dirty = true;
        true
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CursorPlacement {
    Start,
    End,
    Byte(usize),
}

/// Round `byte` down to the nearest UTF-8 char boundary inside `text`.
fn clamp_char_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}

impl Default for BlockEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for BlockEditor {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Local-mode children; nothing to sync from the CRDT.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.child_nodes.clear();
        for b in &mut self.blocks {
            let n = b.line.build_layout(taffy, theme, node_map);
            self.child_nodes.push(n);
        }
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    align_items: Some(taffy::AlignItems::Stretch),
                    gap: taffy::Size {
                        width: length(0.0),
                        height: length(8.0),
                    },
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &self.child_nodes,
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        // Taffy's child list is the authoritative ordering — use it
        // instead of `self.child_nodes` so we tolerate any internal
        // re-ordering taffy might apply for justification.
        let children = taffy.children(node).unwrap();
        for (b, child_node) in self.blocks.iter_mut().zip(children.iter()) {
            b.line.apply_layout(taffy, *child_node, new_origin);
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
        for b in &mut self.blocks {
            b.line.draw(scene, transform, text_renderer, theme, presence);
        }
    }

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
        // Route the event to the focused block (if any). Each block
        // re-reads `*handled` so a previously-handled event isn't
        // re-processed.
        if let Some(focused_idx) = self.focused {
            let was_empty_paragraph = self
                .blocks
                .get(focused_idx)
                .map(|b| {
                    matches!(b.kind, BlockKind::Paragraph) && b.line.value().is_empty()
                })
                .unwrap_or(false);
            if let Some(b) = self.blocks.get_mut(focused_idx) {
                b.line.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            // Drain the signal from the line and act on it.
            if let Some(sig) = self
                .blocks
                .get_mut(focused_idx)
                .and_then(|b| b.line.take_signal())
            {
                self.apply_signal(focused_idx, sig);
            }
            // Markdown promotion: only fires when the focused block
            // was an empty paragraph entering this event. The user's
            // single keystroke must have produced an exact `# `,
            // `## ` or `### ` prefix.
            if was_empty_paragraph {
                let _ = self.try_promote_to_heading(focused_idx);
            }
        }
    }

    fn name(&self) -> &str {
        "BlockEditor"
    }

    fn test_id(&self) -> Option<&str> {
        Some("block_editor")
    }

    fn get_rect(&self) -> Rect {
        self.rect
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

    fn needs_redraw(&self) -> bool {
        self.dirty || self.blocks.iter().any(|b| b.line.is_focused())
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
    fn apply_signal(&mut self, idx: usize, sig: LineSignal) {
        match sig {
            LineSignal::Enter { split_at } => {
                self.split_at_cursor(idx, split_at);
            }
            LineSignal::BackspaceAtStart => {
                if idx == 0 {
                    // First block — Backspace at the very top is a no-op.
                    return;
                }
                let block_empty = self
                    .blocks
                    .get(idx)
                    .map(|b| b.line.value().is_empty())
                    .unwrap_or(false);
                if block_empty {
                    // Empty paragraph: collapse into the previous block.
                    self.delete_empty(idx);
                } else {
                    // Non-empty: merge content into previous.
                    self.merge_with_previous(idx);
                }
            }
            LineSignal::ArrowUp => {
                if idx > 0 {
                    self.focus_block(idx - 1, CursorPlacement::End);
                }
            }
            LineSignal::ArrowDown => {
                if idx + 1 < self.blocks.len() {
                    self.focus_block(idx + 1, CursorPlacement::Start);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Focus lands on the new (suffix) block at byte 0.
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
}
