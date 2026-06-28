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

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::presence::{PathSegment, PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::link_popover::{LinkPopover, PageEntry};
use crate::widget::marks::{Mark, MarkSpan};
use crate::widget::rich_text_line::{LineSignal, RichTextLine};
use crate::widget::slash_menu::SlashMenu;
use crate::widget::Widget;

/// Maximum indent level for `Bullet` / `Checklist` blocks. Beyond
/// this the visual hierarchy stops reading as nested and just shifts
/// off the right edge of the panel — so we clamp aggressively.
pub const MAX_LIST_INDENT: u8 = 5;
/// Horizontal indent per level, in CSS-like pixels. Matches the
/// `28px` indent step Notion uses in its dense block view.
pub const LIST_INDENT_STEP_PX: f32 = 28.0;

/// Block kinds supported by the Phase 3 block editor.
///
/// Each variant carries the per-block state the editor needs to
/// render and serialise it:
/// - `Bullet { indent }` / `Checklist { indent, done }` carry the
///   nesting depth (clamped to [0, MAX_LIST_INDENT]).
/// - `CodeBlock { lang }` carries the optional language tag for the
///   fenced markdown serialisation. **v0.1 limitation**: code blocks
///   are single-line per block — Enter inside a code block creates a
///   *new* code block below, not a literal newline. Multi-line code
///   editing is a Phase 3+ follow-up; the existing `RichTextLine`
///   substrate is single-line only.
/// - `Divider` is non-editable, non-focusable. It serialises to
///   `---` on a line by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 1, 2, or 3. Any other value is invalid; the constructor
    /// helpers in this module clamp to [1, 3].
    Heading(u8),
    /// Unordered list item. `indent` is the nesting depth in
    /// [0, MAX_LIST_INDENT].
    Bullet { indent: u8 },
    /// Checklist item. `indent` is the nesting depth; `done` flips
    /// when the user clicks the checkbox glyph.
    Checklist { indent: u8, done: bool },
    /// Fenced code block. `lang` is the language tag after the
    /// opening fence (e.g. `rust`), or `None` for a bare ` ``` ` fence.
    /// Single-line per block — see the type doc-comment.
    CodeBlock { lang: Option<String> },
    /// Horizontal rule. Non-editable, non-focusable.
    Divider,
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
            BlockKind::Bullet { .. } => 16.0,
            BlockKind::Checklist { .. } => 16.0,
            BlockKind::CodeBlock { .. } => 14.0,
            // Dividers don't render text — height comes from the
            // editor's row scaffolding, not the font.
            BlockKind::Divider => 16.0,
        }
    }

    /// True iff this block kind supports indent / outdent via
    /// Tab / Shift-Tab.
    pub fn supports_indent(&self) -> bool {
        matches!(self, BlockKind::Bullet { .. } | BlockKind::Checklist { .. })
    }

    /// True iff this block has an editable text body. Dividers and
    /// (visually, but not structurally) future placeholder kinds
    /// return false. Used to skip event routing to non-editable rows.
    pub fn is_editable(&self) -> bool {
        !matches!(self, BlockKind::Divider)
    }

    /// Current indent level (0 for non-list kinds).
    pub fn indent(&self) -> u8 {
        match self {
            BlockKind::Bullet { indent } => *indent,
            BlockKind::Checklist { indent, .. } => *indent,
            _ => 0,
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
            kind: kind.clone(),
            line: RichTextLine::new().with_text(text).with_size(kind.font_size()),
        }
    }

    /// Bullet list item with the given indent + text. Indent clamped
    /// to [0, MAX_LIST_INDENT].
    pub fn bullet(indent: u8, text: &str) -> Self {
        let kind = BlockKind::Bullet { indent: indent.min(MAX_LIST_INDENT) };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new().with_text(text).with_size(kind.font_size()),
        }
    }

    /// Checklist item with the given indent + done flag + text.
    pub fn checklist(indent: u8, done: bool, text: &str) -> Self {
        let kind = BlockKind::Checklist { indent: indent.min(MAX_LIST_INDENT), done };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new().with_text(text).with_size(kind.font_size()),
        }
    }

    /// Fenced code block. `lang` is `None` for a bare ` ``` ` fence.
    ///
    /// **v0.1 limitation**: single-line per block — see `BlockKind`
    /// doc-comment. Multi-line code editing is a follow-up.
    pub fn code_block(lang: Option<String>, text: &str) -> Self {
        let kind = BlockKind::CodeBlock { lang };
        Self {
            kind: kind.clone(),
            line: RichTextLine::new().with_text(text).with_size(kind.font_size()),
        }
    }

    /// Horizontal rule. Carries an empty inner line so the layout
    /// machinery keeps a uniform shape; the line is never focused or
    /// edited.
    pub fn divider() -> Self {
        Self {
            kind: BlockKind::Divider,
            line: RichTextLine::new().with_size(BlockKind::Divider.font_size()),
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
    /// Last mouse position in logical coords, cached from
    /// `CursorMoved` events so click-handling code can hit-test the
    /// checkbox glyphs without re-reading the winit event.
    last_mouse_pos: Point,
    /// Slash menu popover. Owned by the editor so it shares lifecycle
    /// with the surrounding block list; rendered out-of-band via
    /// [`BlockEditor::draw_overlay`].
    slash_menu: SlashMenu,
    /// Byte offset (in the focused block) where the `/` keystroke
    /// that opened the slash menu landed. Used to splice the menu's
    /// `/<filter>` text back out of the block when a kind is committed
    /// (so the block ends up with just the existing content).
    slash_open_at: Option<usize>,
    /// Page-link popover. Opens when the user types `[[` inside any
    /// editable block; closes on Enter / Escape / no-match-backspace.
    link_popover: LinkPopover,
    /// Byte offset of the leading `[[` that opened the popover. Used
    /// to splice the `[[<filter>` back out of the block once a page
    /// is selected.
    link_open_at: Option<usize>,
    /// When `Some`, the editor is backed by this Automerge `List` of
    /// block Maps (`pages[id].blocks`). Each block's `Text` is bound to
    /// the matching line so edits sync. `None` = local-only editor (the
    /// pre-collaboration behaviour, kept for examples/tests).
    blocks_list: Option<ObjId>,
    /// Set by structural edits (split / merge / delete) so the next
    /// `sync` mirrors the new block structure into the doc List,
    /// preserving existing block `Text` objects by identity. Distinct
    /// from `structure_dirty`, which drives taffy rebuilds on the host.
    doc_structure_dirty: bool,
    /// The local user's id, threaded into the focused block's line so its
    /// edits stamp authorship marks. `None` = no authorship attribution.
    local_author: Option<String>,
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
            last_mouse_pos: Point::ZERO,
            slash_menu: SlashMenu::new(),
            slash_open_at: None,
            link_popover: LinkPopover::new(),
            link_open_at: None,
            blocks_list: None,
            doc_structure_dirty: false,
            local_author: None,
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
            last_mouse_pos: Point::ZERO,
            slash_menu: SlashMenu::new(),
            slash_open_at: None,
            link_popover: LinkPopover::new(),
            link_open_at: None,
            blocks_list: None,
            doc_structure_dirty: false,
            local_author: None,
        };
        let mut lines = md.split('\n').peekable();
        while let Some(line) = lines.next() {
            // Skip pure blank lines — they serve as block separators
            // but don't produce a block themselves. (A trailing
            // newline at EOF is the common case.)
            if line.is_empty() {
                continue;
            }
            // Fenced code block: parse the optional language tag, then
            // consume lines until the closing fence. Single-line in v0.1
            // means we expect ``` -> one body line -> ``` and serialise
            // back the same. If the body spans multiple lines we join
            // them with `\n` into the single RichTextLine — the user
            // sees the literal newlines as characters (a known v0.1
            // limitation flagged in `BlockKind::CodeBlock` docs).
            if let Some(rest) = line.strip_prefix("```") {
                let lang = if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                };
                let mut body: Vec<&str> = Vec::new();
                while let Some(next) = lines.peek() {
                    if *next == "```" {
                        // Consume the closing fence.
                        lines.next();
                        break;
                    }
                    body.push(lines.next().unwrap());
                }
                editor.blocks.push(Block::code_block(lang, &body.join("\n")));
                continue;
            }
            // Divider: a line of exactly `---`. Three or more hyphens
            // is the markdown convention but we only emit exactly
            // three so round-trip is byte-stable.
            if line == "---" {
                editor.blocks.push(Block::divider());
                continue;
            }
            // Checklist comes BEFORE bullet because `- [ ]` matches the
            // bullet prefix `- ` too. Indent: count leading 4-space
            // groups (matches Notion / GFM nesting). Tabs are treated
            // as one indent step.
            let (indent, body) = parse_list_indent(line);
            if let Some(check_body) = body
                .strip_prefix("- [ ] ")
                .or_else(|| body.strip_prefix("* [ ] "))
            {
                editor.blocks.push(Block::checklist(indent, false, check_body));
                continue;
            }
            if let Some(check_body) = body
                .strip_prefix("- [x] ")
                .or_else(|| body.strip_prefix("* [x] "))
                .or_else(|| body.strip_prefix("- [X] "))
                .or_else(|| body.strip_prefix("* [X] "))
            {
                editor.blocks.push(Block::checklist(indent, true, check_body));
                continue;
            }
            if let Some(bullet_body) = body
                .strip_prefix("- ")
                .or_else(|| body.strip_prefix("* "))
            {
                editor.blocks.push(Block::bullet(indent, bullet_body));
                continue;
            }
            // Headings are NOT indented in our grammar — they live at
            // column 0 always. So we use the raw `line`, not `body`.
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
                BlockKind::Heading(3) => {
                    out.push_str("### ");
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

    /// Replace the entire block list with `blocks`. Pairs with the
    /// notes app's markdown parser, which produces a richer
    /// representation (carrying inline marks) than this widget's
    /// own from_markdown can express.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.blocks = blocks;
        if self.blocks.is_empty() {
            self.blocks.push(Block::paragraph(""));
        }
        self.focused = None;
        self.is_focused = false;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
    }

    /// Set the local user's id for authorship attribution. Applied to
    /// every block's line so edits stamp marks regardless of which block
    /// is focused.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author = author.clone();
        for b in &mut self.blocks {
            b.line.set_local_author(author.clone());
        }
    }

    /// Back this editor with an Automerge `List` of block Maps
    /// (`pages[id].blocks`). After binding, every edit syncs through the
    /// CRDT and remote edits appear via [`sync`].
    ///
    /// - If the doc List already has blocks (an existing or remote
    ///   page), the editor **adopts** them, discarding the locally
    ///   seeded blocks.
    /// - If the doc List is empty (a brand-new page), the editor
    ///   **seeds** it from its current local blocks.
    pub fn bind_blocks(&mut self, state: &mut AutoCommit, list: ObjId) {
        self.blocks_list = Some(list.clone());
        let doc_len = state.length(&list);
        if doc_len == 0 {
            // Seed the doc from our current local blocks.
            self.doc_structure_dirty = true;
            self.reconcile_structure_to_doc(state, &list);
            self.doc_structure_dirty = false;
        } else {
            // Adopt the doc's blocks.
            self.rebuild_from_doc(state, &list, doc_len);
        }
    }

    /// Mirror the local block structure into the doc `List`, preserving
    /// each existing block's `Text` object by identity (Automerge list
    /// insert/delete keep other elements' object ids stable, so a
    /// mid-list split doesn't disturb its neighbours' history). New
    /// blocks get a fresh `Text` seeded from their local content;
    /// blocks whose `Text` is bound but whose local text drifted (after
    /// a programmatic `set_text` during split/merge) are reconciled with
    /// a minimal splice.
    fn reconcile_structure_to_doc(&mut self, state: &mut AutoCommit, list: &ObjId) {
        let mut doc_idx = 0usize;
        let len = self.blocks.len();
        for i in 0..len {
            let kind_str = kind_to_str(&self.blocks[i].kind);
            let local_text = self.blocks[i].line.value().to_string();
            match self.blocks[i].line.bound_obj() {
                Some(bound) => {
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
                        doc_idx += 1;
                    } else {
                        // Bound object vanished from the doc — re-insert.
                        let t = insert_doc_block(state, list, doc_idx, &kind_str, &local_text);
                        self.blocks[i].line.bind_text(t);
                        doc_idx += 1;
                    }
                }
                None => {
                    // New block (e.g. the suffix of a split): insert a
                    // fresh doc block and bind it.
                    let t = insert_doc_block(state, list, doc_idx, &kind_str, &local_text);
                    self.blocks[i].line.bind_text(t);
                    doc_idx += 1;
                }
            }
        }
        // Drop any trailing doc blocks beyond what we have locally.
        while state.length(list) > doc_idx {
            let last = state.length(list) - 1;
            let _ = state.delete(list, last);
        }
    }

    /// Rebuild the local block list from the doc `List`, binding each
    /// line to its `Text`. Preserves the focused block index + caret
    /// offset across the rebuild so a remote edit elsewhere doesn't
    /// yank the local caret.
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
        self.structure_dirty = true;

        if let Some(fi) = saved_focus {
            let fi = fi.min(self.blocks.len().saturating_sub(1));
            self.focused = Some(fi);
            self.is_focused = true;
            self.blocks[fi].line.set_focus(true);
            if let Some(c) = saved_caret {
                self.blocks[fi].line.set_cursor_pos(c);
            }
        }
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
        self.doc_structure_dirty = true;
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
        let cur_kind = block.kind.clone();
        let text = block.line.value().to_string();
        let split = split_at.min(text.len());
        // Char-boundary safety: round down to the nearest char start.
        let split = clamp_char_boundary(&text, split);
        let (prefix, suffix) = text.split_at(split);
        // Update the current block's text to the prefix; preserve kind.
        block.line.set_text(prefix);
        // Suffix's kind depends on the current block's kind:
        // - Bullet/Checklist: new item of the SAME kind + indent. This
        //   matches every Notion-style editor — pressing Enter on a
        //   list item continues the list.
        // - CodeBlock: new code block (Enter inside a code block does
        //   NOT insert a literal newline in v0.1 — see BlockKind doc).
        // - Anything else: a fresh paragraph.
        let suffix_block = match cur_kind {
            BlockKind::Bullet { indent } => Block::bullet(indent, suffix),
            BlockKind::Checklist { indent, .. } => {
                // The new item is unchecked even if the source was
                // checked — matches the user's mental model of "add a
                // new todo".
                Block::checklist(indent, false, suffix)
            }
            BlockKind::CodeBlock { ref lang } => Block::code_block(lang.clone(), suffix),
            _ => Block::paragraph(suffix),
        };
        self.blocks.insert(idx + 1, suffix_block);
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
        self.doc_structure_dirty = true;
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
            self.doc_structure_dirty = true;
        self.doc_structure_dirty = true;
            self.dirty = true;
            return;
        }
        let target = if idx == 0 { 0 } else { idx - 1 };
        self.blocks.remove(idx);
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
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
        let mut new_line = RichTextLine::new()
            .with_text(&prefix)
            .with_size(font_size);
        new_line.set_focus(true);
        // Park the caret at the start of the stripped text — that's
        // where the user expects to keep typing.
        new_line.set_cursor_pos(0);
        b.line = new_line;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
        true
    }

    /// Convert the block at `idx` to `new_kind`, preserving the
    /// current text content. Used by the SlashMenu Enter handler:
    /// the menu picks the target kind and this applies the conversion.
    ///
    /// Behaviour:
    /// - Paragraph ↔ Heading(n): text body is preserved, font size
    ///   changes. No-op if the block is already that kind.
    /// - Paragraph ↔ Bullet/Checklist: indent resets to 0, done = false.
    /// - Any → Divider: replaces the block entirely (no text).
    /// - Any → CodeBlock: keeps text, drops marks, sets lang = None.
    ///
    /// Returns `true` if a conversion happened. After a successful
    /// conversion, focus stays on the same block index with the caret
    /// at end of the preserved text.
    pub fn try_convert_to(&mut self, idx: usize, new_kind: BlockKind) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        if b.kind == new_kind {
            return false;
        }
        let text = b.line.value().to_string();
        let font_size = new_kind.font_size();
        b.kind = new_kind.clone();
        // Replace the inner line so the new font size lands cleanly
        // (`set_font_size` propagates, but rebuilding the input also
        // resets selection / dirty state — a clean cut for what is
        // visually a hard conversion). For dividers we keep an empty
        // line to satisfy layout but it's never rendered or focused.
        let body = if matches!(new_kind, BlockKind::Divider) { "" } else { &text };
        let was_focused = b.line.is_focused();
        let mut new_line = RichTextLine::new()
            .with_text(body)
            .with_size(font_size);
        // After conversion, keep the caret at the end of the preserved
        // body so the user can keep typing in the new kind. Focus
        // restoration honours the editor's current focused-block state.
        new_line.set_cursor_pos(body.len());
        new_line.set_focus(was_focused && !matches!(new_kind, BlockKind::Divider));
        b.line = new_line;
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
        // Dividers are not focusable — push focus to the next block
        // (or fall back to a paragraph we splice in if we're at the end).
        if matches!(new_kind, BlockKind::Divider) {
            if self.focused == Some(idx) {
                if idx + 1 < self.blocks.len() {
                    self.focus_block(idx + 1, CursorPlacement::Start);
                } else {
                    // Add a trailing empty paragraph so the user can
                    // keep typing below the divider. Common Notion
                    // pattern: inserting a divider lands the caret on
                    // the line after it.
                    self.blocks.push(Block::paragraph(""));
                    self.focus_block(idx + 1, CursorPlacement::Start);
                }
            }
        }
        true
    }

    /// Adjust the indent of the block at `idx` by `delta` (signed).
    /// Only Bullet / Checklist blocks have an indent — for any other
    /// kind this is a no-op and returns `false`. Indent is clamped to
    /// [0, MAX_LIST_INDENT].
    ///
    /// Returns `true` if the indent actually changed.
    pub fn try_indent(&mut self, idx: usize, delta: i8) -> bool {
        let Some(b) = self.blocks.get_mut(idx) else {
            return false;
        };
        let (new_kind, changed) = match &b.kind {
            BlockKind::Bullet { indent } => {
                let next = (*indent as i16 + delta as i16)
                    .clamp(0, MAX_LIST_INDENT as i16) as u8;
                let changed = next != *indent;
                (BlockKind::Bullet { indent: next }, changed)
            }
            BlockKind::Checklist { indent, done } => {
                let next = (*indent as i16 + delta as i16)
                    .clamp(0, MAX_LIST_INDENT as i16) as u8;
                let changed = next != *indent;
                (BlockKind::Checklist { indent: next, done: *done }, changed)
            }
            _ => return false,
        };
        if !changed {
            return false;
        }
        b.kind = new_kind;
        // Mark dirty so the renderer re-positions the bullet glyph at
        // the new indent. Structure doesn't actually change (still one
        // block at the same index), but the row's horizontal layout
        // does — flag both so MainPanel rebuilds taffy if needed.
        self.structure_dirty = true;
        self.doc_structure_dirty = true;
        self.dirty = true;
        true
    }

    /// Hit-test the cached last mouse position against the checkbox
    /// rect of every Checklist block. Returns the block index whose
    /// checkbox was clicked, or `None`. Used by `handle_event` to
    /// route checkbox clicks to `try_toggle_check`.
    pub(crate) fn checkbox_hit_test_last_mouse(&self) -> Option<usize> {
        let p = self.last_mouse_pos;
        for (idx, b) in self.blocks.iter().enumerate() {
            if !matches!(b.kind, BlockKind::Checklist { .. }) {
                continue;
            }
            let line_rect = b.line.get_rect();
            let gutter = prefix_gutter_for(&b.kind);
            // The drawn checkbox is centered in the gutter to the
            // left of line_rect.
            let cy = (line_rect.y0 + line_rect.y1) / 2.0;
            let cx = line_rect.x0 - gutter + gutter / 2.0;
            let r = Rect::new(
                cx - CHECKBOX_SIZE_PX / 2.0,
                cy - CHECKBOX_SIZE_PX / 2.0,
                cx + CHECKBOX_SIZE_PX / 2.0,
                cy + CHECKBOX_SIZE_PX / 2.0,
            );
            // Slight slop margin (~4 px) so users don't have to land
            // pixel-perfect.
            let slop = 4.0;
            let slopped = Rect::new(
                r.x0 - slop,
                r.y0 - slop,
                r.x1 + slop,
                r.y1 + slop,
            );
            if slopped.contains(p) {
                return Some(idx);
            }
        }
        None
    }

    /// Apply a slash-menu commit: strip the `/<filter>` text from the
    /// focused block (between `slash_open_at` and current caret), then
    /// convert the block to `new_kind`. Run by handle_event after the
    /// menu fires its Enter. Idempotent — if the focused block has
    /// changed since the menu was opened we still apply the conversion
    /// and leave the text alone.
    fn commit_slash_selection(&mut self, new_kind: BlockKind) {
        let Some(focused_idx) = self.focused else {
            return;
        };
        if let Some(open_byte) = self.slash_open_at {
            if let Some(b) = self.blocks.get_mut(focused_idx) {
                let text = b.line.value().to_string();
                let cursor = b.line.cursor_pos().min(text.len());
                let start = open_byte.min(text.len());
                if cursor > start {
                    // Drop bytes [start, cursor) — the `/<filter>` slice.
                    let mut new_text = String::with_capacity(text.len());
                    new_text.push_str(&text[..start]);
                    new_text.push_str(&text[cursor..]);
                    b.line.set_text(&new_text);
                    b.line.set_cursor_pos(start);
                }
            }
        }
        self.slash_open_at = None;
        // Apply the conversion. Skip if block already matches (e.g.
        // user picked Paragraph on a Paragraph block).
        let _ = self.try_convert_to(focused_idx, new_kind);
    }

    /// Paint the slash menu popover on top of the editor's own
    /// scene. The host (`MainPanel` / `NotesApp`) calls this AFTER
    /// the main draw pass — popovers must paint last so they land
    /// above every other widget. See [`SlashMenu`] for the contract.
    pub fn draw_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.slash_menu.draw_overlay(scene, transform, text_renderer, theme);
        self.link_popover.draw_overlay(scene, transform, text_renderer, theme);
    }

    /// Seed the link popover's page list (call whenever the host
    /// workspace changes — e.g. on `NotesApp::sync_from_workspace`).
    pub fn set_link_pages(&mut self, pages: Vec<PageEntry>) {
        self.link_popover.set_items(pages);
    }

    /// Read-only access to the link popover (for tests + the host
    /// app that wants to peek at the current filter / visibility).
    pub fn link_popover(&self) -> &LinkPopover {
        &self.link_popover
    }

    /// Mutable access to the link popover (test hook).
    #[doc(hidden)]
    pub fn link_popover_mut(&mut self) -> &mut LinkPopover {
        &mut self.link_popover
    }

    /// Apply a link-popover selection: strip the `[[<filter>` slice
    /// out of the focused block, then insert the chosen page's title
    /// covered by a `Mark::Link(page_id)` span.
    fn commit_link_selection(&mut self, page: PageEntry) {
        let Some(focused_idx) = self.focused else { return };
        let Some(open_byte) = self.link_open_at.take() else { return };
        let Some(b) = self.blocks.get_mut(focused_idx) else { return };
        let text = b.line.value().to_string();
        let cursor = b.line.cursor_pos().min(text.len());
        let start = open_byte.min(text.len());
        if cursor <= start {
            return;
        }
        // Splice: replace [start, cursor) with the page title.
        let mut new_text = String::with_capacity(text.len() + page.title.len());
        new_text.push_str(&text[..start]);
        new_text.push_str(&page.title);
        new_text.push_str(&text[cursor..]);
        b.line.set_text(&new_text);
        // Add a Mark::Link covering the inserted title.
        let mark_range = start..(start + page.title.len());
        b.line.add_mark(MarkSpan::new(mark_range.clone(), Mark::Link(page.id)));
        b.line.set_cursor_pos(start + page.title.len());
        self.dirty = true;
        self.edit_pending = true;
    }

    /// True iff the slash menu is currently displayed (so the host
    /// can avoid stealing focus from the popover).
    pub fn slash_menu_visible(&self) -> bool {
        self.slash_menu.is_visible()
    }

    /// Test/integration hook: read-only access to the slash menu so
    /// tests can assert on its filter / highlighted kind without
    /// going through `pub(crate)` indirection.
    #[doc(hidden)]
    pub fn slash_menu(&self) -> &SlashMenu {
        &self.slash_menu
    }

    /// Test/integration hook: mutable slash-menu access. Used by the
    /// integration test that pre-positions the menu rather than
    /// driving the `/` keystroke through the full event chain.
    #[doc(hidden)]
    pub fn slash_menu_mut(&mut self) -> &mut SlashMenu {
        &mut self.slash_menu
    }

    /// Test hook: open the slash menu programmatically on the focused
    /// block, anchoring it under the line's bottom-left. Mirrors what
    /// the `/` keystroke does internally.
    #[doc(hidden)]
    pub fn open_slash_menu_for_test(&mut self) {
        let Some(focused_idx) = self.focused else { return };
        let caret = self
            .blocks
            .get(focused_idx)
            .map(|b| b.line.cursor_pos())
            .unwrap_or(0);
        let line_rect = self
            .blocks
            .get(focused_idx)
            .map(|b| b.line.get_rect())
            .unwrap_or(Rect::ZERO);
        self.slash_menu
            .set_anchor(Point::new(line_rect.x0, line_rect.y1 + 2.0));
        self.slash_menu.set_filter("");
        self.slash_menu.set_visible(true);
        self.slash_open_at = Some(caret);
    }

    /// Test hook: route a key event into the slash menu and, if the
    /// menu fires an Enter, apply the commit. Returns whether the menu
    /// consumed the key.
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

    /// Flip the `done` flag on a Checklist block. No-op for other
    /// kinds. Returns the new `done` value, or `None` if the block
    /// isn't a Checklist.
    pub fn try_toggle_check(&mut self, idx: usize) -> Option<bool> {
        let Some(b) = self.blocks.get_mut(idx) else {
            return None;
        };
        if let BlockKind::Checklist { indent, done } = &b.kind {
            let new_done = !*done;
            b.kind = BlockKind::Checklist { indent: *indent, done: new_done };
            self.dirty = true;
            // Toggling check counts as a user edit so the page is
            // dirty + gets persisted.
            self.edit_pending = true;
            Some(new_done)
        } else {
            None
        }
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

/// Parse the leading indent of a list-item line. Returns `(indent_level,
/// remainder)` where `remainder` is the line with the indent stripped.
///
/// Convention (matches GFM nested lists): each indent level is **two
/// spaces**. We use 2 here rather than 4 to match what Notion / Obsidian
/// emit by default for nested bullets. A single tab character counts
/// as one indent step.
fn parse_list_indent(line: &str) -> (u8, &str) {
    let mut bytes = 0usize;
    let mut level: u8 = 0;
    let b = line.as_bytes();
    while level < MAX_LIST_INDENT {
        if b.len() >= bytes + 2 && &b[bytes..bytes + 2] == b"  " {
            bytes += 2;
            level += 1;
        } else if b.len() > bytes && b[bytes] == b'\t' {
            bytes += 1;
            level += 1;
        } else {
            break;
        }
    }
    (level, &line[bytes..])
}

/// Append `indent` levels of 2-space indent to `out`. Inverse of
/// [`parse_list_indent`] when the input contained spaces (tabs are
/// re-emitted as spaces on the round-trip, which is fine — the parser
/// accepts both).
fn push_indent(out: &mut String, indent: u8) {
    for _ in 0..indent.min(MAX_LIST_INDENT) {
        out.push_str("  ");
    }
}

/// Width in CSS-like pixels of the reserved gutter to the left of the
/// editable line where a kind-specific prefix glyph (bullet point,
/// checkbox, code-fence marker) is drawn.
///
/// Returns 0 for kinds with no prefix (paragraph, heading, divider,
/// code block — the latter draws its prefix as a left-edge bar in
/// `draw`, not inline glyphs).
pub(crate) fn prefix_gutter_for(kind: &BlockKind) -> f64 {
    match kind {
        BlockKind::Bullet { .. } => 20.0,
        BlockKind::Checklist { .. } => 24.0,
        BlockKind::CodeBlock { .. } => 12.0,
        _ => 0.0,
    }
}

/// Width of the clickable checkbox square in a checklist row. Slightly
/// smaller than the gutter so there's visual breathing room either side.
pub(crate) const CHECKBOX_SIZE_PX: f64 = 16.0;

// ---------------------------------------------------------------------
// CRDT block-list helpers (used by sync / reconcile / rebuild).
//
// `BlockKind` is encoded as a compact string in the doc's `kind` field
// so the full block model round-trips: paragraph / heading{1..3} /
// bullet:{indent} / checklist:{indent}:{0|1} / code:{lang} / divider.
// ---------------------------------------------------------------------

/// Presence key addressing block `index` within the editor, so a peer's
/// caret/selection in that block can be looked up. Both peers derive the
/// same key from the block's position in the shared `blocks` list, so the
/// caret renders on the matching block on every peer.
pub(crate) fn block_presence_key(index: usize) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_block".to_string()),
            PathSegment::Index(index as u32),
        ],
    }
}

fn kind_to_str(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph => "paragraph".to_string(),
        BlockKind::Heading(n) => format!("heading{n}"),
        BlockKind::Bullet { indent } => format!("bullet:{indent}"),
        BlockKind::Checklist { indent, done } => {
            format!("checklist:{indent}:{}", if *done { 1 } else { 0 })
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
    if let Some(rest) = s.strip_prefix("checklist:") {
        let mut parts = rest.split(':');
        let indent = parts
            .next()
            .and_then(|p| p.parse::<u8>().ok())
            .unwrap_or(0)
            .min(MAX_LIST_INDENT);
        let done = parts.next().map(|p| p == "1").unwrap_or(false);
        return BlockKind::Checklist { indent, done };
    }
    if let Some(rest) = s.strip_prefix("code:") {
        let lang = if rest.is_empty() { None } else { Some(rest.to_string()) };
        return BlockKind::CodeBlock { lang };
    }
    BlockKind::Paragraph
}

/// Build an empty `Block` of the given kind. Text is bound + pulled from
/// the doc afterwards, so we seed with "".
fn block_for_kind(kind: &BlockKind) -> Block {
    match kind {
        BlockKind::Paragraph => Block::paragraph(""),
        BlockKind::Heading(n) => Block::heading(*n, ""),
        BlockKind::Bullet { indent } => Block::bullet(*indent, ""),
        BlockKind::Checklist { indent, done } => Block::checklist(*indent, *done, ""),
        BlockKind::CodeBlock { lang } => Block::code_block(lang.clone(), ""),
        BlockKind::Divider => Block::divider(),
    }
}

/// The block Map at `index` in a `blocks` List, if present.
fn list_map(state: &AutoCommit, list: &ObjId, index: usize) -> Option<ObjId> {
    match state.get(list, index) {
        Ok(Some((Value::Object(ObjType::Map), m))) => Some(m),
        _ => None,
    }
}

/// The `Text` ObjId for block `index`, if present.
fn doc_text_obj(state: &AutoCommit, list: &ObjId, index: usize) -> Option<ObjId> {
    let block = list_map(state, list, index)?;
    match state.get(&block, "text") {
        Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
        _ => None,
    }
}

/// The `kind` string for block `index`, defaulting to "paragraph".
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

/// Insert a new block Map `{ kind, text }` at `index` in the List and
/// return the new `Text` ObjId, seeded with `text`.
fn insert_doc_block(
    state: &mut AutoCommit,
    list: &ObjId,
    index: usize,
    kind: &str,
    text: &str,
) -> ObjId {
    let block = state
        .insert_object(list, index, ObjType::Map)
        .expect("insert block map never fails");
    let _ = state.put(&block, "kind", kind);
    let text_id = state
        .put_object(&block, "text", ObjType::Text)
        .expect("put_object block text never fails");
    if !text.is_empty() {
        let _ = state.splice_text(&text_id, 0, 0, text);
    }
    text_id
}

/// Reconcile a doc `Text` object's content toward `local` with a single
/// minimal splice (the structural ops produce one contiguous change).
fn push_text_diff(state: &mut AutoCommit, text_obj: &ObjId, local: &str) {
    let cur = state.text(text_obj).unwrap_or_default();
    if cur == local {
        return;
    }
    let (at_byte, inserted, removed) =
        crate::widget::rich_text_line::diff_splice(&cur, local);
    let char_at = cur[..at_byte.min(cur.len())].chars().count();
    let removed_chars = cur[at_byte..(at_byte + removed).min(cur.len())]
        .chars()
        .count();
    let inserted_str = &local[at_byte..(at_byte + inserted).min(local.len())];
    let _ = state.splice_text(text_obj, char_at, removed_chars as isize, inserted_str);
}

impl Default for BlockEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for BlockEditor {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        let Some(list) = self.blocks_list.clone() else {
            // Unbound (examples / tests): local-mode, nothing to sync.
            return;
        };

        // 1. Push any pending local structural edit into the doc first,
        //    preserving existing block Text objects by identity.
        if self.doc_structure_dirty {
            self.reconcile_structure_to_doc(state, &list);
            self.doc_structure_dirty = false;
        }

        // 2. If the doc now has a different block count than we do, a
        //    remote peer changed the structure — adopt it (caret-safe).
        let doc_len = state.length(&list);
        if doc_len != self.blocks.len() {
            self.rebuild_from_doc(state, &list, doc_len);
            return;
        }

        // 3. Same structure: refresh each block's kind + text from the
        //    doc. RichTextLine::sync pulls bound text without disturbing
        //    the local caret.
        for i in 0..self.blocks.len() {
            let k = kind_from_str(&read_block_kind(state, &list, i));
            if self.blocks[i].kind != k {
                self.blocks[i].kind = k;
                self.structure_dirty = true;
            }
            if self.blocks[i].line.bound_obj().is_none() {
                if let Some(t) = doc_text_obj(state, &list, i) {
                    self.blocks[i].line.bind_text(t);
                }
            }
            self.blocks[i].line.set_local_author(self.local_author.clone());
            self.blocks[i].line.sync(state, &list);
        }
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
            // For prefixed kinds (bullets, checklists, code blocks) we
            // narrow the line's rect on the left so the prefix glyph
            // (bullet point, checkbox, etc.) lands in a reserved
            // gutter that doesn't overlap the editable text. The
            // gutter width is fixed per kind so the caret-x math
            // inside TextInput stays consistent.
            let gutter = prefix_gutter_for(&b.kind);
            let indent_px =
                LIST_INDENT_STEP_PX as f64 * b.kind.indent() as f64;
            let original = b.line.get_rect();
            if gutter > 0.0 || indent_px > 0.0 {
                let shifted = Rect::new(
                    original.x0 + indent_px + gutter,
                    original.y0,
                    original.x1,
                    original.y1,
                );
                b.line.apply_layout_external(shifted);
            }
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

        // Empty-state hint: when the whole editor consists of one
        // empty Paragraph block, draw a dim placeholder next to the
        // caret so a brand-new page doesn't look like a void. Painted
        // BEFORE the block content so the live caret (drawn by the
        // inner TextInput) stays on top of the hint glyphs. We bail
        // when the line is non-empty (or there's more than one block)
        // because the hint would compete with real content.
        let is_lone_empty_paragraph = self.blocks.len() == 1
            && matches!(self.blocks[0].kind, BlockKind::Paragraph)
            && self.blocks[0].line.value().is_empty();
        if is_lone_empty_paragraph {
            let line_rect = self.blocks[0].line.get_rect();
            // Same vertical centring as the inner TextInput uses so
            // the hint reads as the placeholder for THIS line, not as
            // a floating label above/below it.
            let size = self.blocks[0].line.font_size();
            let line_height = (size as f64) * 1.25;
            let text_y = line_rect.y0 + (line_rect.height() - line_height) / 2.0;
            // Offset 2 px right of the caret so the hint doesn't
            // collide with the caret bar itself.
            let caret_w = 2.0;
            let x = line_rect.x0 + caret_w + 4.0;
            text_renderer.draw_text(
                scene,
                "Start typing — / for blocks, [[ to link a page",
                size,
                tokens.colors.text_dim,
                transform * Affine::translate((x, text_y)),
                None,
            );
        }

        for (idx, b) in self.blocks.iter_mut().enumerate() {
            let line_rect = b.line.get_rect();
            let indent_px =
                LIST_INDENT_STEP_PX as f64 * b.kind.indent() as f64;
            let gutter = prefix_gutter_for(&b.kind);
            // x0 of the prefix region (before the gutter shift).
            let prefix_x0 = line_rect.x0 - gutter;
            // Editor-column left edge — where the block "row" starts,
            // before any indent shift or kind-specific prefix gutter.
            // Used by the focused-block accent bar so the bar lines up
            // across every kind regardless of indent level.
            let row_x0 = prefix_x0 - indent_px;

            // Focused-block indicator: a 2 px accent-tinted vertical
            // bar at the leftmost edge of the focused block's row.
            // Drawn at ~50% alpha so it reads as a subtle cue rather
            // than competing with the caret. Skipped for Dividers
            // since they don't get focus.
            if Some(idx) == self.focused && self.is_focused
                && !matches!(b.kind, BlockKind::Divider)
            {
                use crate::color_compat::ColorCompat;
                let bar_color =
                    theme.accent_primary.with_alpha_factor(0.5);
                let bar = Rect::new(
                    row_x0,
                    line_rect.y0 + 2.0,
                    row_x0 + 2.0,
                    line_rect.y1 - 2.0,
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    bar_color,
                    None,
                    &bar,
                );
            }

            match &b.kind {
                BlockKind::Paragraph | BlockKind::Heading(_) => {
                    b.line.draw(scene, transform, text_renderer, theme, presence);
                }
                BlockKind::Bullet { .. } => {
                    // Bullet glyph (•) centered vertically against the
                    // line band. We draw a small filled disc instead of
                    // a "•" character so the dot reads the same across
                    // fonts.
                    let cy = (line_rect.y0 + line_rect.y1) / 2.0;
                    let cx = prefix_x0 + gutter / 2.0;
                    let radius = 3.0;
                    let circle = vello::kurbo::Circle::new((cx, cy), radius);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.text_primary,
                        None,
                        &circle,
                    );
                    b.line.draw(scene, transform, text_renderer, theme, presence);
                    let _ = indent_px;
                }
                BlockKind::Checklist { done, .. } => {
                    // Clickable checkbox square. Filled + with a check
                    // glyph when done; empty stroke when not.
                    let cy = (line_rect.y0 + line_rect.y1) / 2.0;
                    let cx = prefix_x0 + gutter / 2.0;
                    let box_rect = Rect::new(
                        cx - CHECKBOX_SIZE_PX / 2.0,
                        cy - CHECKBOX_SIZE_PX / 2.0,
                        cx + CHECKBOX_SIZE_PX / 2.0,
                        cy + CHECKBOX_SIZE_PX / 2.0,
                    );
                    let shape = vello::kurbo::RoundedRect::from_rect(box_rect, 3.0);
                    if *done {
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            tokens.colors.accent[5],
                            None,
                            &shape,
                        );
                        // Simple check mark — two lines from lower-left
                        // through center to upper-right.
                        let mut path = vello::kurbo::BezPath::new();
                        let pad = CHECKBOX_SIZE_PX * 0.22;
                        path.move_to((box_rect.x0 + pad, box_rect.y0 + CHECKBOX_SIZE_PX * 0.55));
                        path.line_to((box_rect.x0 + CHECKBOX_SIZE_PX * 0.42, box_rect.y1 - pad));
                        path.line_to((box_rect.x1 - pad, box_rect.y0 + pad));
                        scene.stroke(
                            &vello::kurbo::Stroke::new(2.0),
                            transform,
                            tokens.colors.text_primary,
                            None,
                            &path,
                        );
                    } else {
                        scene.stroke(
                            &vello::kurbo::Stroke::new(1.5),
                            transform,
                            tokens.colors.border_standard,
                            None,
                            &shape,
                        );
                    }
                    b.line.draw(scene, transform, text_renderer, theme, presence);
                }
                BlockKind::CodeBlock { .. } => {
                    // Subtle monospace-style chrome: a soft surface
                    // background spanning the line's full width + a
                    // thin accent bar on the left edge.
                    let bg_rect = Rect::new(
                        line_rect.x0 - gutter,
                        line_rect.y0,
                        line_rect.x1,
                        line_rect.y1,
                    );
                    let shape =
                        vello::kurbo::RoundedRect::from_rect(bg_rect, 4.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.surfaces.raised,
                        None,
                        &shape,
                    );
                    let bar = Rect::new(
                        bg_rect.x0,
                        bg_rect.y0,
                        bg_rect.x0 + 3.0,
                        bg_rect.y1,
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.accent[5],
                        None,
                        &bar,
                    );
                    b.line.draw(scene, transform, text_renderer, theme, presence);
                }
                BlockKind::Divider => {
                    // Horizontal rule centered vertically in the row.
                    let cy = (line_rect.y0 + line_rect.y1) / 2.0;
                    let rule = Rect::new(
                        line_rect.x0,
                        cy - 0.5,
                        line_rect.x1,
                        cy + 0.5,
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.border_standard,
                        None,
                        &rule,
                    );
                }
            }
        }

        // Remote presence: draw each peer's caret + selection on the
        // block it's in, using real glyph metrics. This runs AFTER the
        // block loop so every line's `char_x_positions` are fresh this
        // frame (pixel_x_for_byte reads them). Positions are mapped
        // through `transform` to match the scaled scene.
        for i in 0..self.blocks.len() {
            let key = block_presence_key(i);
            let line = &self.blocks[i].line;
            let brect = line.get_rect();
            // The block's rect mapped into scaled scene space, reused for
            // the per-peer focus outline ("highlighted UI element").
            let tr_rect = {
                let p0 = transform * Point::new(brect.x0, brect.y0);
                let p1 = transform * Point::new(brect.x1, brect.y1);
                Rect::new(p0.x, p0.y, p1.x, p1.y)
            };
            // Authorship highlight (drawn first, under caret/selection):
            // the runs each foreign author wrote, marker-style in their
            // color. Skip runs the local user authored.
            for (bs, be, author) in line.author_runs() {
                if self.local_author.as_deref() == Some(author.as_str()) {
                    continue;
                }
                let x0 = line.pixel_x_for_byte(theme, *bs);
                let x1 = line.pixel_x_for_byte(theme, *be);
                let p0 = transform * Point::new(x0, brect.y0);
                let p1 = transform * Point::new(x1, brect.y1);
                let user = crate::permissions::UserId(author.clone());
                presence.draw_peer_authorship(
                    scene,
                    theme,
                    &user,
                    &[Rect::new(p0.x, p0.y, p1.x, p1.y)],
                );
            }
            if let Some(list) = presence.carets.get(&key) {
                for (user, byte_pos) in list {
                    // Highlight the block this peer is editing.
                    presence.draw_peer_focus(scene, theme, user, tr_rect);
                    let x = line.pixel_x_for_byte(theme, *byte_pos);
                    let p = transform * Point::new(x, brect.y0);
                    presence.draw_peer_caret(
                        scene,
                        theme,
                        user,
                        (p.x as f32, p.y as f32),
                    );
                }
            }
            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list {
                    let x0 = line.pixel_x_for_byte(theme, range.start);
                    let x1 = line.pixel_x_for_byte(theme, range.end);
                    let p0 = transform * Point::new(x0, brect.y0);
                    let p1 = transform * Point::new(x1, brect.y1);
                    presence.draw_peer_selection(
                        scene,
                        theme,
                        user,
                        range,
                        &[Rect::new(p0.x, p0.y, p1.x, p1.y)],
                    );
                }
            }
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
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        // Link popover: while visible, intercept keystrokes BEFORE
        // the editable line sees them. Same pattern as the slash
        // menu below. Enter commits the highlighted page; Escape
        // closes.
        if self.link_popover.is_visible() {
            if let WindowEvent::KeyboardInput { .. } = event {
                let consumed = self.link_popover.handle_key(event);
                if let Some(page) = self.link_popover.take_selection() {
                    self.commit_link_selection(page);
                }
                if !self.link_popover.is_visible() {
                    self.link_open_at = None;
                }
                if consumed {
                    *handled = true;
                    self.dirty = true;
                    return;
                }
            }
        }

        // Slash menu: while visible, intercept keystrokes BEFORE the
        // editable line sees them. The menu owns arrow nav / Enter /
        // Escape / typing-into-filter. If the menu signals Enter
        // (pending_selection becomes Some), we splice the `/<filter>`
        // out of the block and apply the conversion.
        if self.slash_menu.is_visible() {
            if let WindowEvent::KeyboardInput { .. } = event {
                let consumed = self.slash_menu.handle_key(event);
                // Whether or not the menu consumed the key, an Enter
                // press may have set pending_selection — drain it.
                if let Some(new_kind) = self.slash_menu.take_selected_kind() {
                    self.commit_slash_selection(new_kind);
                }
                // If the menu went invisible without committing (Escape
                // or empty-filter Backspace), close it without altering
                // the block.
                if !self.slash_menu.is_visible() {
                    self.slash_open_at = None;
                }
                if consumed {
                    *handled = true;
                    self.dirty = true;
                    return;
                }
            }
        }

        // Mouse: checkbox-click hit-test on Checklist rows. Runs BEFORE
        // event routing so the click doesn't fall through to TextInput
        // (which would just place the caret at byte 0).
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if let Some(hit_idx) = self.checkbox_hit_test_last_mouse() {
                let _ = self.try_toggle_check(hit_idx);
                *handled = true;
                return;
            }
            // A click anywhere closes an open slash menu (matches
            // every popover convention).
            if self.slash_menu.is_visible() {
                self.slash_menu.set_visible(false);
                self.slash_open_at = None;
            }
        }
        // Cache the last mouse position so the next MouseInput knows
        // where the click landed. Same shape as TextInput.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let logical =
                Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_mouse_pos = logical;
        }

        // Tab / Shift-Tab on a list block adjusts indent BEFORE the
        // inner line sees it (TextInput does NOT handle Tab, but we
        // intercept anyway so future TextInput changes don't break
        // the contract).
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Tab),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if let Some(focused_idx) = self.focused {
                let supports = self
                    .blocks
                    .get(focused_idx)
                    .map(|b| b.kind.supports_indent())
                    .unwrap_or(false);
                if supports {
                    let delta = if modifiers.state().shift_key() { -1 } else { 1 };
                    self.try_indent(focused_idx, delta);
                    *handled = true;
                    return;
                }
            }
        }

        // `/` keystroke: if pressed in an editable block while the
        // menu is NOT already open, open the menu anchored under the
        // current line. The leading `/` STILL falls through to
        // TextInput (so the user sees it as feedback that they're
        // typing a slash command); we remember `slash_open_at` so we
        // can remove it on commit. Code blocks don't get the slash
        // menu (typing `/` in code is meaningful).
        if !self.slash_menu.is_visible() {
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            {
                if c.as_str() == "/" {
                    if let Some(focused_idx) = self.focused {
                        let can_open = self
                            .blocks
                            .get(focused_idx)
                            .map(|b| {
                                !matches!(b.kind, BlockKind::CodeBlock { .. })
                                    && b.kind.is_editable()
                            })
                            .unwrap_or(false);
                        if can_open {
                            let caret_byte = self
                                .blocks
                                .get(focused_idx)
                                .map(|b| b.line.cursor_pos())
                                .unwrap_or(0);
                            // Anchor the popover just below the line's
                            // left edge — close enough to the caret
                            // for the user to associate the popover
                            // with their `/`.
                            let line_rect = self
                                .blocks
                                .get(focused_idx)
                                .map(|b| b.line.get_rect())
                                .unwrap_or(Rect::ZERO);
                            self.slash_menu.set_anchor(Point::new(
                                line_rect.x0,
                                line_rect.y1 + 2.0,
                            ));
                            self.slash_menu.set_filter("");
                            self.slash_menu.set_visible(true);
                            self.slash_open_at = Some(caret_byte);
                            // Let the `/` keystroke ALSO fall through
                            // to TextInput so it appears in the line —
                            // commit_slash_selection will strip it.
                        }
                    }
                }
            }
        }

        // Click hit-test: a left-press places the caret in whichever
        // editable block it lands on — not only the already-focused one.
        // Focus that block first so the inner TextInput's click handler
        // (in the routing below) sets the caret at the click position.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let hit = self.blocks.iter().position(|b| {
                b.kind.is_editable()
                    && b.line.get_rect().contains(self.last_mouse_pos)
            });
            if let Some(idx) = hit {
                if self.focused != Some(idx) {
                    if let Some(prev) = self.focused {
                        if let Some(b) = self.blocks.get_mut(prev) {
                            b.line.set_focus(false);
                        }
                    }
                    self.focused = Some(idx);
                    self.is_focused = true;
                    if let Some(b) = self.blocks.get_mut(idx) {
                        b.line.set_focus(true);
                    }
                    *focus_target = Some(node_id);
                }
            }
        }

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
                if b.kind.is_editable() {
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

    fn presence_payload(
        &self,
    ) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        use crate::collaboration::PresencePayload;
        let idx = self.focused?;
        let block = self.blocks.get(idx)?;
        if !block.line.is_focused() {
            return None;
        }
        let key = block_presence_key(idx);
        // A selection takes priority over the bare caret.
        if let Some((start, end)) = block.line.selection_range() {
            return Some((key, PresencePayload::Selection { start, end }));
        }
        Some((key, PresencePayload::Caret { pos: block.line.cursor_pos() }))
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
            LineSignal::LinkOpen { open_at } => {
                // Anchor the popover under the line's bottom-left
                // (the same convention SlashMenu uses). A future
                // polish pass can switch to caret-precise anchoring.
                let line_rect = self
                    .blocks
                    .get(idx)
                    .map(|b| b.line.get_rect())
                    .unwrap_or(Rect::ZERO);
                self.link_popover.set_anchor(Point::new(line_rect.x0, line_rect.y1 + 2.0));
                self.link_popover.set_filter("");
                self.link_popover.set_visible(true);
                self.link_open_at = Some(open_at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CRDT backing (sync / bind / converge) -----------------------

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
        // Peer A seeds a one-block document.
        let mut a_doc = AutoCommit::new();
        let a_list = a_doc.put_object(&ROOT, "blocks", ObjType::List).unwrap();
        let mut a = BlockEditor::new();
        a.set_blocks(vec![Block::paragraph("from-A")]);
        a.bind_blocks(&mut a_doc, a_list);

        // Peer B joins from A's full state (the late-join handshake).
        let mut b_doc = AutoCommit::load(&a_doc.save()).unwrap();
        let b_list = match b_doc.get(&ROOT, "blocks") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => panic!("blocks list should exist in B's adopted doc"),
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

        // Split into "abc" | "def" locally, then sync to mirror to doc.
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

    // ---- Phase 3: convert / indent / toggle ----------------------------

    #[test]
    fn try_convert_paragraph_to_h1() {
        let mut e = BlockEditor::new();
        e.blocks[0].line.set_text("hello");
        assert!(e.try_convert_to(0, BlockKind::Heading(1)));
        assert!(matches!(e.blocks()[0].kind, BlockKind::Heading(1)));
        // Text preserved.
        assert_eq!(e.blocks()[0].text(), "hello");
    }

    #[test]
    fn try_convert_h1_to_bullet_preserves_text_resets_indent() {
        let mut e = BlockEditor::new();
        e.blocks[0] = Block::heading(1, "task");
        assert!(e.try_convert_to(0, BlockKind::Bullet { indent: 0 }));
        match &e.blocks()[0].kind {
            BlockKind::Bullet { indent } => assert_eq!(*indent, 0),
            k => panic!("expected Bullet, got {:?}", k),
        }
        assert_eq!(e.blocks()[0].text(), "task");
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
        // Paragraph→Paragraph
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
            k => panic!("expected Checklist, got {:?}", k),
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
            k => panic!("expected Bullet at indent 2, got {:?}", k),
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
            k => panic!("expected Checklist, got {:?}", k),
        }
    }

    #[test]
    fn supports_indent_predicate() {
        assert!(BlockKind::Bullet { indent: 0 }.supports_indent());
        assert!(BlockKind::Checklist { indent: 0, done: false }.supports_indent());
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
