//! Presenting blocks an external document model owns.
//!
//! A host that keeps the document elsewhere — a rich-text resource the
//! framework reads, a record a collaborator edits — shows it here with
//! [`BlockEditor::present_blocks`]. Blocks keep their widgets across
//! presentations by id, so the caret, focus and selection of the person
//! typing survive a collaborator's edit, an undo or the host's own
//! read-back of an edit it just applied.

use std::ops::Range;
use std::rc::Rc;

use crate::presence::PresenceKey;
use crate::widget::marks::MarkSpan;
use crate::widget::rich_text_line::RichTextLine;

use super::{Block, BlockEditor, BlockKind, DocumentPosition, DocumentSelection};

/// What one block shows, as the owning model says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockContent {
    /// The block's stable id in the owning model.
    pub id: String,
    pub kind: BlockKind,
    pub text: String,
    /// Inline marks over byte ranges of `text`.
    pub marks: Vec<MarkSpan>,
    /// Outline depth of a block that is not a list item.
    pub nesting: u8,
    /// The fence a code line belongs to.
    pub code_group: Option<String>,
    /// Who wrote which run of `text`: byte ranges and the writer's id, shown
    /// as authorship; empty when nobody is attributed.
    pub authors: Vec<(Range<usize>, String)>,
}

impl BlockContent {
    /// A paragraph of plain text.
    #[must_use]
    pub fn paragraph(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: BlockKind::Paragraph,
            text: text.into(),
            marks: Vec::new(),
            nesting: 0,
            code_group: None,
            authors: Vec::new(),
        }
    }
}

/// Whether `line` shows the authored runs `content` names.
fn shows_authors(content: &BlockContent, line: &RichTextLine) -> bool {
    let shown = line.author_runs();
    shown.len() == content.authors.len()
        && shown
            .iter()
            .zip(&content.authors)
            .all(|((start, end, author), (range, named))| {
                *start == range.start && *end == range.end && author == named
            })
}

/// Whether `block` shows `content` as it is.
fn shows(block: &Block, content: &BlockContent) -> bool {
    block.id == content.id
        && block.kind == content.kind
        && block.text() == content.text
        && block.line.marks() == content.marks.as_slice()
        && block.nesting == content.nesting
        && block.code_group == content.code_group
        && shows_authors(content, &block.line)
}

/// The runs a line shows as authored, as `content` names them.
fn author_runs(content: &BlockContent) -> Vec<(usize, usize, String)> {
    content
        .authors
        .iter()
        .map(|(range, author)| (range.start, range.end, author.clone()))
        .collect()
}

/// Which blocks the person may have changed, as
/// [`BlockEditor::take_edit_scope`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditScope {
    /// No block.
    Nothing,
    /// Only blocks at these indices.
    Blocks(Range<usize>),
    /// Any block, and the block list itself.
    Anywhere,
}

impl EditScope {
    /// Also cover what `other` covers.
    pub fn widen(&mut self, other: Self) {
        *self = match (std::mem::replace(self, Self::Nothing), other) {
            (Self::Anywhere, _) | (_, Self::Anywhere) => Self::Anywhere,
            (Self::Nothing, scope) | (scope, Self::Nothing) => scope,
            (Self::Blocks(a), Self::Blocks(b)) => {
                Self::Blocks(a.start.min(b.start)..a.end.max(b.end))
            }
        };
    }
}

/// Maps a block id to the presence key collaborators address it by.
pub type PresenceKeyFn = Rc<dyn Fn(&str) -> PresenceKey>;

/// Where `old` text's byte `at` lands in `new` text: after the common prefix
/// a caret moves with the text following it.
fn carry_byte(old: &str, new: &str, at: usize) -> usize {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(old.len().min(new.len()).saturating_sub(prefix))
        .take_while(|(a, b)| a == b)
        .count();
    let carried = if at <= prefix {
        at
    } else if at >= old.len().saturating_sub(suffix) {
        new.len().saturating_sub(old.len().saturating_sub(at))
    } else {
        new.len().saturating_sub(suffix)
    };
    new.floor_char_boundary(carried)
}

fn line_for(kind: &BlockKind, text: &str) -> RichTextLine {
    let mut line = RichTextLine::new()
        .with_text(text)
        .with_size(kind.font_size());
    match kind {
        BlockKind::CodeBlock { .. } => {
            line.set_mono(true);
            line.set_inline_formatting(false);
        }
        BlockKind::Table { .. } => line.set_mono(true),
        _ => {}
    }
    line
}

impl Block {
    /// A block showing `content`.
    #[must_use]
    pub fn from_content(content: BlockContent) -> Self {
        let text = if matches!(content.kind, BlockKind::Divider) {
            ""
        } else {
            content.text.as_str()
        };
        let mut line = line_for(&content.kind, text);
        let authors = author_runs(&content);
        line.set_marks(content.marks);
        line.set_author_runs(authors);
        Self {
            id: content.id,
            kind: content.kind,
            code_group: content.code_group,
            nesting: content.nesting,
            line,
            soft_break: false,
        }
    }

    /// What this block shows, as [`BlockEditor::present_blocks`] takes it.
    #[must_use]
    pub fn content(&self) -> BlockContent {
        BlockContent {
            id: self.id.clone(),
            kind: self.kind.clone(),
            text: self.text().to_owned(),
            marks: self.line.marks().to_vec(),
            nesting: self.nesting,
            code_group: self.code_group.clone(),
            authors: self
                .line
                .author_runs()
                .iter()
                .map(|(start, end, author)| (*start..*end, author.clone()))
                .collect(),
        }
    }

    /// Show `content` in this block's widget, keeping its caret where the
    /// text around it went. Returns whether anything changed and whether
    /// the change reshapes the block (kind, depth or fence).
    fn present(&mut self, content: &BlockContent) -> (bool, bool) {
        let mut changed = false;
        let mut reshaped = false;
        if self.kind != content.kind {
            let focused = self.line.is_focused();
            let caret = self.line.cursor_pos();
            let marks = self.line.marks().to_vec();
            let authors = self.line.author_runs().to_vec();
            self.line = line_for(&content.kind, self.text());
            self.line.set_marks(marks);
            self.line.set_author_runs(authors);
            self.line.set_cursor_pos(caret);
            self.line.set_focus(focused);
            self.kind = content.kind.clone();
            changed = true;
            reshaped = true;
        }
        if self.text() != content.text {
            let caret = self.line.cursor_pos();
            let carried = carry_byte(self.text(), &content.text, caret);
            self.line.set_text(&content.text);
            self.line.set_cursor_pos(carried);
            changed = true;
        }
        if self.line.marks() != content.marks.as_slice() {
            self.line.set_marks(content.marks.clone());
            changed = true;
        }
        if !shows_authors(content, &self.line) {
            self.line.set_author_runs(author_runs(content));
            changed = true;
        }
        if self.nesting != content.nesting || self.code_group != content.code_group {
            self.nesting = content.nesting;
            self.code_group.clone_from(&content.code_group);
            changed = true;
            reshaped = true;
        }
        (changed, reshaped)
    }
}

impl BlockEditor {
    /// Show `blocks` as the owning model has them now: after a
    /// collaborator's edit, an undo, or the host's read-back of an edit it
    /// applied. A block whose id stays keeps its widget, with the caret
    /// carried past text inserted or removed before it; focus and a
    /// selection follow their block's id. Returns whether anything changed.
    ///
    /// Edits the person makes still arrive as the editor's own blocks
    /// ([`Self::blocks`]); a host that owns the document compares them with
    /// what it presented, applies the difference to its model, and presents
    /// the result.
    pub fn present_blocks(&mut self, blocks: &[BlockContent]) -> bool {
        let same_blocks = blocks.len() == self.blocks.len()
            && blocks
                .iter()
                .zip(&self.blocks)
                .all(|(next, current)| next.id == current.id);
        if same_blocks {
            return self.present_in_place(blocks.iter().enumerate());
        }
        let old_ids: Vec<String> = self.blocks.iter().map(|block| block.id.clone()).collect();
        let focused_id = self.focused.and_then(|index| old_ids.get(index)).cloned();
        let mut retained: std::collections::HashMap<String, Block> =
            std::mem::take(&mut self.blocks)
                .into_iter()
                .map(|block| (block.id.clone(), block))
                .collect();
        let mut reshaped = blocks.len() != old_ids.len()
            || blocks
                .iter()
                .zip(&old_ids)
                .any(|(next, old)| &next.id != old);
        let mut next_blocks = Vec::with_capacity(blocks.len());
        for content in blocks {
            match retained.remove(&content.id) {
                Some(mut block) => {
                    let (_, block_reshaped) = block.present(content);
                    reshaped |= block_reshaped;
                    next_blocks.push(block);
                }
                None => next_blocks.push(Block::from_content(content.clone())),
            }
        }
        if next_blocks.is_empty() {
            next_blocks.push(Block::paragraph(""));
            reshaped = true;
        }
        self.blocks = next_blocks;
        let local_author = self.local_author.clone();
        if local_author.is_some() {
            for block in &mut self.blocks {
                block.line.set_local_author(local_author.clone());
            }
        }
        if let Some(paint) = self.mention_paint().cloned() {
            for block in &mut self.blocks {
                block.line.set_mention_paint(Some(paint.clone()));
            }
        }
        let old_focus = self.focused;
        self.focused = focused_id
            .and_then(|id| self.blocks.iter().position(|block| block.id == id))
            .or_else(|| old_focus.map(|index| index.min(self.blocks.len().saturating_sub(1))));
        // A focused table cell stays focused, clamped into the table's new
        // shape; its hidden source line never takes the caret.
        for block in &self.blocks {
            if !matches!(block.kind, BlockKind::Table { visual: true }) {
                continue;
            }
            if let Some(table) = self.table_editors.get_mut(&block.id) {
                let rows: Vec<Vec<String>> = super::markdown_table_data_rows(block.text())
                    .into_iter()
                    .map(|row| row.into_iter().map(super::decode_table_cell).collect())
                    .collect();
                table.project_cells(&rows);
            }
        }
        let cell_focused = self.blocks.iter().any(|block| {
            matches!(block.kind, BlockKind::Table { visual: true })
                && self
                    .table_editors
                    .get(&block.id)
                    .is_some_and(|table| table.projected_focus().is_some())
        });
        for (index, block) in self.blocks.iter_mut().enumerate() {
            let focus = self.is_focused && self.focused == Some(index) && !cell_focused;
            if block.line.is_focused() != focus {
                block.line.set_focus(focus);
            }
        }
        let blocks = &self.blocks;
        let remap = |position: DocumentPosition| {
            let id = old_ids.get(position.block)?;
            let index = blocks.iter().position(|block| &block.id == id)?;
            Some(DocumentPosition {
                block: index,
                byte: blocks.get(index)?.text().floor_char_boundary(position.byte),
            })
        };
        self.document_selection = self.document_selection.and_then(|selection| {
            Some(DocumentSelection {
                anchor: remap(selection.anchor)?,
                cursor: remap(selection.cursor)?,
            })
        });
        if reshaped {
            self.cross_block_drag_start = None;
            self.cross_block_drag_touched = None;
            self.kbd_sel_anchor = None;
            self.formula_preview_rects.clear();
            self.table_preview_rects.clear();
            self.formula_chrome.clear();
            self.visual_formulas.clear();
            self.visual_formula_focus = None;
            self.restack();
        }
        self.reconcile_block_selection();
        self.dirty = true;
        true
    }

    /// Show new content for some blocks, the rest as they are: `changed`
    /// pairs a block's index with what it shows now, and each index must
    /// still hold the block with that content's id. For an owning model
    /// that knows which paragraphs an edit touched, so a keystroke's
    /// read-back costs the blocks it changed, not the document. Returns
    /// `None`, presenting nothing, when a pair names another block (the
    /// order changed: present everything with [`Self::present_blocks`]),
    /// otherwise whether anything changed.
    pub fn present_changed(&mut self, changed: &[(usize, &BlockContent)]) -> Option<bool> {
        let fits = changed.iter().all(|(index, content)| {
            self.blocks
                .get(*index)
                .is_some_and(|block| block.id == content.id)
        });
        fits.then(|| {
            self.present_in_place(changed.iter().map(|(index, content)| (*index, *content)))
        })
    }

    /// Present new content for blocks at their indices, which hold blocks
    /// of the same ids: only the blocks whose content changed are touched.
    fn present_in_place<'a>(
        &mut self,
        blocks: impl IntoIterator<Item = (usize, &'a BlockContent)>,
    ) -> bool {
        let mut touched = false;
        let mut reshaped = false;
        for (index, content) in blocks {
            let Some(block) = self.blocks.get_mut(index) else {
                continue;
            };
            if shows(block, content) {
                continue;
            }
            let kind_changed = block.kind != content.kind;
            let (_, block_reshaped) = block.present(content);
            touched = true;
            reshaped |= block_reshaped;
            if kind_changed && self.local_author.is_some() {
                block.line.set_local_author(self.local_author.clone());
            }
            if kind_changed && self.mention_paint.is_some() {
                block.line.set_mention_paint(self.mention_paint.clone());
            }
            if matches!(block.kind, BlockKind::Table { visual: true })
                && let Some(table) = self.table_editors.get_mut(&block.id)
            {
                let rows: Vec<Vec<String>> = super::markdown_table_data_rows(block.text())
                    .into_iter()
                    .map(|row| row.into_iter().map(super::decode_table_cell).collect())
                    .collect();
                table.project_cells(&rows);
            }
        }
        if !touched {
            return false;
        }
        // A focused table cell keeps the caret from its hidden source line.
        if let Some(block) = self.focused.and_then(|index| self.blocks.get_mut(index))
            && matches!(block.kind, BlockKind::Table { visual: true })
            && self
                .table_editors
                .get(&block.id)
                .is_some_and(|table| table.projected_focus().is_some())
            && block.line.is_focused()
        {
            block.line.set_focus(false);
        }
        let blocks = &self.blocks;
        let clamp = |position: DocumentPosition| DocumentPosition {
            byte: blocks.get(position.block).map_or(position.byte, |block| {
                block.text().floor_char_boundary(position.byte)
            }),
            ..position
        };
        self.document_selection = self.document_selection.map(|selection| DocumentSelection {
            anchor: clamp(selection.anchor),
            cursor: clamp(selection.cursor),
        });
        if reshaped {
            self.cross_block_drag_start = None;
            self.cross_block_drag_touched = None;
            self.kbd_sel_anchor = None;
            self.formula_preview_rects.clear();
            self.table_preview_rects.clear();
            self.formula_chrome.clear();
            self.visual_formulas.clear();
            self.visual_formula_focus = None;
            self.restack();
        }
        self.reconcile_block_selection();
        self.dirty = true;
        true
    }

    /// Give block `from` the id `to`: the owning model named a block the
    /// editor created (the paragraph an Enter split off) when it applied
    /// the edit. Returns whether a block was renamed.
    pub fn rename_block(&mut self, from: &str, to: &str) -> bool {
        if from == to || self.blocks.iter().any(|block| block.id == to) {
            return false;
        }
        match self.blocks.iter_mut().find(|block| block.id == from) {
            Some(block) => {
                to.clone_into(&mut block.id);
                true
            }
            None => false,
        }
    }

    /// Address blocks in presence by the owning model's keys (a resource
    /// selection, say) rather than the editor's own, so a collaborator's
    /// caret lands in the same paragraph of the same resource.
    pub fn set_presence_key(&mut self, key: PresenceKeyFn) {
        self.presence_key = Some(key);
    }

    /// The presence key of the block with id `block`.
    pub(super) fn presence_key_for(&self, block: &str) -> PresenceKey {
        match &self.presence_key {
            Some(key) => key(block),
            None => super::block_presence_key(block),
        }
    }

    /// Whether presence is keyed by the owning model's keys.
    pub(super) fn presence_keyed_by_model(&self) -> bool {
        self.presence_key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_with(blocks: Vec<BlockContent>) -> BlockEditor {
        let mut editor = BlockEditor::new();
        editor.present_blocks(&blocks);
        editor
    }

    #[test]
    fn a_collaborators_insert_before_the_caret_carries_it() {
        let mut editor = editor_with(vec![BlockContent::paragraph("a", "hello world")]);
        editor.focus_block(0, super::super::CursorPlacement::Byte(6));
        assert!(editor.present_blocks(&[BlockContent::paragraph("a", "oh, hello world")]));
        assert_eq!(editor.blocks()[0].line.cursor_pos(), 10);
        assert_eq!(editor.focused_index(), Some(0));
    }

    #[test]
    fn focus_follows_its_block_when_another_is_inserted_before_it() {
        let mut editor = editor_with(vec![
            BlockContent::paragraph("a", "first"),
            BlockContent::paragraph("b", "second"),
        ]);
        editor.focus_block(1, super::super::CursorPlacement::Byte(3));
        editor.present_blocks(&[
            BlockContent::paragraph("z", "new"),
            BlockContent::paragraph("a", "first"),
            BlockContent::paragraph("b", "second"),
        ]);
        assert_eq!(editor.focused_index(), Some(2));
        assert_eq!(editor.blocks()[2].line.cursor_pos(), 3);
    }

    #[test]
    fn presenting_the_same_blocks_changes_nothing() {
        let blocks = vec![BlockContent::paragraph("a", "same")];
        let mut editor = editor_with(blocks.clone());
        assert!(!editor.present_blocks(&blocks));
    }

    #[test]
    fn who_wrote_a_run_is_shown_and_read_back() {
        let mut editor = editor_with(vec![BlockContent::paragraph("a", "hello world")]);
        let mut authored = BlockContent::paragraph("a", "hello world");
        authored.authors = vec![(0..5, "ada".into()), (6..11, "grace".into())];
        assert!(editor.present_blocks(std::slice::from_ref(&authored)));
        assert_eq!(
            editor.blocks()[0].line.author_runs(),
            [(0, 5, "ada".to_owned()), (6, 11, "grace".to_owned())]
        );
        assert_eq!(editor.blocks()[0].content(), authored);
        assert!(!editor.present_blocks(&[authored]));
    }

    #[test]
    fn a_new_kind_keeps_the_text_and_caret() {
        let mut editor = editor_with(vec![BlockContent::paragraph("a", "Title")]);
        editor.focus_block(0, super::super::CursorPlacement::Byte(2));
        let mut heading = BlockContent::paragraph("a", "Title");
        heading.kind = BlockKind::Heading(1);
        editor.present_blocks(&[heading]);
        assert_eq!(editor.blocks()[0].kind, BlockKind::Heading(1));
        assert_eq!(editor.blocks()[0].line.cursor_pos(), 2);
    }

    #[test]
    fn renaming_keeps_the_widget() {
        let mut editor = editor_with(vec![BlockContent::paragraph("local", "x")]);
        assert!(editor.rename_block("local", "minted"));
        assert_eq!(editor.blocks()[0].id, "minted");
        assert!(!editor.rename_block("missing", "other"));
    }

    #[test]
    fn carry_moves_with_the_text_after_the_edit() {
        assert_eq!(carry_byte("abcdef", "abXcdef", 4), 5);
        // Text a collaborator inserts right at the caret lands after it.
        assert_eq!(carry_byte("abcdef", "abcdef!", 6), 6);
        assert_eq!(carry_byte("abcdef", "adef", 3), 1);
        assert_eq!(carry_byte("abcdef", "abcdef", 2), 2);
    }
}
