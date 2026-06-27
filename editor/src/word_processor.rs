//! `WordProcessor` — a rich-text editor over a `qdoc` document living in a host
//! `AutoCommit` (e.g. `ui::State.doc`). This module holds the **editing model**:
//! a caret `(block_id, char_offset)` and the keyboard edit operations
//! (insert / Enter-split / Backspace-merge / caret motion) expressed as
//! mutations of the canonical document. Rendering + the `ui::Widget` impl live
//! in `widget.rs`; click-to-caret needs layout and is wired there.

use automerge::{AutoCommit, ObjId};
use qdoc::{Containers, DocMut, IdGen};

/// Caret position: a block id plus a character offset within that block's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub block: String,
    pub offset: usize,
}

/// The editing core. `bind` resolves/initialises the document; the edit ops
/// mutate the host `AutoCommit` and keep the caret consistent.
pub struct WordProcessor {
    pub(crate) containers: Option<Containers>,
    pub(crate) ids: Option<IdGen>,
    pub(crate) caret: Caret,
    /// Selection anchor; a selection exists when this is set and differs from
    /// the caret (within the same block, for v1).
    pub(crate) selection_anchor: Option<Caret>,
}

impl Default for WordProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessor {
    pub fn new() -> Self {
        Self { containers: None, ids: None, caret: Caret::default(), selection_anchor: None }
    }

    // ---- selection ---------------------------------------------------------

    /// Begin (or keep) a selection anchored at the current caret.
    pub fn set_anchor(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.caret.clone());
        }
    }

    pub fn clear_anchor(&mut self) {
        self.selection_anchor = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection_anchor.as_ref().map(|a| a != &self.caret).unwrap_or(false)
    }

    /// Selection endpoints in document order, if non-empty.
    pub fn selection_span(&self, state: &AutoCommit) -> Option<(Caret, Caret)> {
        let anchor = self.selection_anchor.clone()?;
        if anchor == self.caret {
            return None;
        }
        let blocks = self.ordered_blocks(state);
        let pos = |c: &Caret| blocks.iter().position(|b| b == &c.block);
        match (pos(&anchor), pos(&self.caret)) {
            (Some(pa), Some(pc)) => {
                if (pa, anchor.offset) <= (pc, self.caret.offset) {
                    Some((anchor, self.caret.clone()))
                } else {
                    Some((self.caret.clone(), anchor))
                }
            }
            _ => None,
        }
    }

    /// Per-block selected ranges `(block, start, end)` spanning the selection.
    pub fn selection_blocks(&self, state: &AutoCommit) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span(state) else {
            return vec![];
        };
        let blocks = self.ordered_blocks(state);
        let (Some(si), Some(ei)) = (
            blocks.iter().position(|b| b == &start.block),
            blocks.iter().position(|b| b == &end.block),
        ) else {
            return vec![];
        };
        (si..=ei)
            .map(|i| {
                let b = &blocks[i];
                let blen = self.block_len(state, b);
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei { end.offset } else { blen };
                (b.clone(), s, e)
            })
            .collect()
    }

    /// The selected text, joining blocks with newlines.
    pub fn selection_text(&self, state: &AutoCommit) -> String {
        let Some(containers) = self.containers.as_ref() else {
            return String::new();
        };
        self.selection_blocks(state)
            .into_iter()
            .map(|(b, s, e)| {
                qdoc::model::block_text(state, containers, &b)
                    .unwrap_or_default()
                    .chars()
                    .skip(s)
                    .take(e.saturating_sub(s))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn selected_block_ids(&self, state: &AutoCommit) -> Vec<String> {
        let sel = self.selection_blocks(state);
        if sel.is_empty() {
            vec![self.caret.block.clone()]
        } else {
            sel.into_iter().map(|(b, _, _)| b).collect()
        }
    }

    /// Delete the current selection (no-op if none); returns whether it deleted.
    pub fn delete_selection(&mut self, state: &mut AutoCommit) -> bool {
        let Some((start, end)) = self.selection_span(state) else {
            return false;
        };
        let containers = self.containers.clone().unwrap();
        if start.block == end.block {
            if let Some(ids) = self.ids.as_mut() {
                let mut dm = DocMut { doc: state, c: &containers, ids };
                dm.splice_text(&start.block, start.offset, (end.offset - start.offset) as isize, "");
            }
        } else {
            // Keep start[..start.offset] + end[end.offset..], drop the rest.
            let end_text = qdoc::model::block_text(state, &containers, &end.block).unwrap_or_default();
            let tail: String = end_text.chars().skip(end.offset).collect();
            let start_len = self.block_len(state, &start.block);
            let blocks = self.ordered_blocks(state);
            let si = blocks.iter().position(|b| b == &start.block).unwrap();
            let ei = blocks.iter().position(|b| b == &end.block).unwrap();
            let to_remove: Vec<String> = blocks[si + 1..=ei].to_vec();
            if let Some(ids) = self.ids.as_mut() {
                let mut dm = DocMut { doc: state, c: &containers, ids };
                if start.offset < start_len {
                    dm.splice_text(&start.block, start.offset, (start_len - start.offset) as isize, "");
                }
                if !tail.is_empty() {
                    dm.splice_text(&start.block, start.offset, 0, &tail);
                }
                for b in &to_remove {
                    dm.remove_block(b);
                }
            }
        }
        self.caret = start;
        self.selection_anchor = None;
        true
    }

    /// Set paragraph style on every block in the selection (or the caret block).
    pub fn set_paragraph_style(&mut self, state: &mut AutoCommit, kind: &str, level: Option<i64>, ordered: Option<bool>) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let targets = self.selected_block_ids(state);
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            for block in &targets {
                dm.set_kind(block, kind);
                if let Some(l) = level {
                    dm.set_attr(block, "level", l);
                }
                if let Some(o) = ordered {
                    dm.set_attr(block, "ordered", o);
                }
            }
        }
    }

    /// Set alignment on every block in the selection (or the caret block).
    pub fn set_align(&mut self, state: &mut AutoCommit, align: &str) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let targets = self.selected_block_ids(state);
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            for block in &targets {
                dm.set_attr(block, "align", align);
            }
        }
    }

    /// Set a block-level attribute (e.g. font) on every selected block.
    pub fn set_block_attr(&mut self, state: &mut AutoCommit, key: &str, value: impl Into<automerge::ScalarValue> + Clone) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let targets = self.selected_block_ids(state);
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            for block in &targets {
                dm.set_attr(block, key, value.clone());
            }
        }
    }

    /// Toggle an inline mark over the selection (across blocks).
    pub fn toggle_mark(&mut self, state: &mut AutoCommit, name: &str) {
        let sel = self.selection_blocks(state);
        if sel.is_empty() {
            return;
        }
        let containers = self.containers.clone().unwrap();
        let all_marked = sel
            .iter()
            .filter(|(_, s, e)| s < e)
            .all(|(b, s, e)| qdoc::model::has_mark_over(state, &containers, b, *s, *e, name));
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            for (b, s, e) in &sel {
                if s >= e {
                    continue;
                }
                if all_marked {
                    dm.remove_mark(b, *s, *e, name);
                } else {
                    dm.add_mark(b, *s, *e, name, true);
                }
            }
        }
    }

    /// Insert text, splitting on newlines into new paragraphs (multi-line paste).
    pub fn insert_multiline(&mut self, state: &mut AutoCommit, text: &str) {
        let normalized = text.replace('\r', "");
        let mut first = true;
        for line in normalized.split('\n') {
            if !first {
                self.enter(state);
            }
            if !line.is_empty() {
                self.insert_str(state, line);
            }
            first = false;
        }
    }

    /// Resolve the document model under `root` (initialising the schema if
    /// absent), guarantee at least one block, and keep the caret on a real block.
    pub fn bind(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.containers.is_none() {
            let c = qdoc::model::open(state, root).unwrap_or_else(|_| qdoc::model::init(state, root));
            // Seed past existing block ids so editing a loaded/imported document
            // never re-mints (and overwrites) an existing block.
            self.ids = Some(qdoc::model::seeded_id_gen(state, &c));
            self.containers = Some(c);
        }
        let containers = self.containers.clone().unwrap();
        let blocks = qdoc::model::blocks_in_order(state, &containers);
        if blocks.is_empty() {
            if let Some(ids) = self.ids.as_mut() {
                let mut dm = DocMut { doc: state, c: &containers, ids };
                let id = dm.insert_block(None, "paragraph");
                self.caret = Caret { block: id, offset: 0 };
            }
        } else if !blocks.contains(&self.caret.block) {
            self.caret = Caret { block: blocks[0].clone(), offset: 0 };
        } else {
            // clamp offset to the (possibly remotely-shrunk) block length
            let len = self.block_len(state, &self.caret.block);
            if self.caret.offset > len {
                self.caret.offset = len;
            }
        }
    }

    // ---- read helpers ------------------------------------------------------

    fn block_len(&self, state: &AutoCommit, block: &str) -> usize {
        self.containers
            .as_ref()
            .and_then(|c| qdoc::model::block_text(state, c, block))
            .map(|t| t.chars().count())
            .unwrap_or(0)
    }

    /// Text of the block the caret is in.
    pub fn current_text(&self, state: &AutoCommit) -> String {
        self.containers
            .as_ref()
            .and_then(|c| qdoc::model::block_text(state, c, &self.caret.block))
            .unwrap_or_default()
    }

    /// Ordered block texts (empty blocks render as empty strings).
    pub fn block_texts(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        qdoc::model::blocks_in_order(state, c)
            .iter()
            .map(|id| qdoc::model::block_text(state, c, id).unwrap_or_default())
            .collect()
    }

    fn ordered_blocks(&self, state: &AutoCommit) -> Vec<String> {
        self.containers
            .as_ref()
            .map(|c| qdoc::model::blocks_in_order(state, c))
            .unwrap_or_default()
    }

    // ---- edit operations ---------------------------------------------------

    /// Insert `s` at the caret and advance. Replaces the selection if any.
    pub fn insert_str(&mut self, state: &mut AutoCommit, s: &str) {
        self.delete_selection(state);
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            dm.splice_text(&block, offset, 0, s);
        }
        self.caret.offset = offset + s.chars().count();
    }

    /// Split the current block at the caret into two paragraphs; caret moves to
    /// the start of the new block.
    pub fn enter(&mut self, state: &mut AutoCommit) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        let text = qdoc::model::block_text(state, &containers, &block).unwrap_or_default();
        let char_count = text.chars().count();
        let tail: String = text.chars().skip(offset).collect();

        let new_id = if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            if offset < char_count {
                dm.splice_text(&block, offset, (char_count - offset) as isize, "");
            }
            let id = dm.insert_block(Some(&block), "paragraph");
            if !tail.is_empty() {
                dm.splice_text(&id, 0, 0, &tail);
            }
            id
        } else {
            return;
        };
        self.caret = Caret { block: new_id, offset: 0 };
    }

    /// Delete the character before the caret; at offset 0 merge into the
    /// previous block.
    pub fn backspace(&mut self, state: &mut AutoCommit) {
        if self.delete_selection(state) {
            return;
        }
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let block = self.caret.block.clone();
        let offset = self.caret.offset;

        if offset > 0 {
            if let Some(ids) = self.ids.as_mut() {
                let mut dm = DocMut { doc: state, c: &containers, ids };
                dm.splice_text(&block, offset - 1, 1, "");
            }
            self.caret.offset = offset - 1;
            return;
        }

        // offset == 0: merge into the previous block.
        let blocks = self.ordered_blocks(state);
        let Some(pos) = blocks.iter().position(|b| b == &block) else {
            return;
        };
        if pos == 0 {
            return;
        }
        let prev = blocks[pos - 1].clone();
        let prev_len = self.block_len(state, &prev);
        let cur_text = qdoc::model::block_text(state, &containers, &block).unwrap_or_default();
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            if !cur_text.is_empty() {
                dm.splice_text(&prev, prev_len, 0, &cur_text);
            }
            dm.remove_block(&block);
        }
        self.caret = Caret { block: prev, offset: prev_len };
    }

    /// Delete the character after the caret; at end-of-block merge the next
    /// block in.
    pub fn delete_forward(&mut self, state: &mut AutoCommit) {
        if self.delete_selection(state) {
            return;
        }
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        let len = self.block_len(state, &block);

        if offset < len {
            if let Some(ids) = self.ids.as_mut() {
                let mut dm = DocMut { doc: state, c: &containers, ids };
                dm.splice_text(&block, offset, 1, "");
            }
            return;
        }

        let blocks = self.ordered_blocks(state);
        let Some(pos) = blocks.iter().position(|b| b == &block) else {
            return;
        };
        if pos + 1 >= blocks.len() {
            return;
        }
        let next = blocks[pos + 1].clone();
        let next_text = qdoc::model::block_text(state, &containers, &next).unwrap_or_default();
        if let Some(ids) = self.ids.as_mut() {
            let mut dm = DocMut { doc: state, c: &containers, ids };
            if !next_text.is_empty() {
                dm.splice_text(&block, len, 0, &next_text);
            }
            dm.remove_block(&next);
        }
        // caret stays at the merge point
    }

    // ---- caret motion ------------------------------------------------------

    pub fn move_left(&mut self, state: &AutoCommit) {
        if self.caret.offset > 0 {
            self.caret.offset -= 1;
            return;
        }
        let blocks = self.ordered_blocks(state);
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block) {
            if pos > 0 {
                let prev = blocks[pos - 1].clone();
                let len = self.block_len(state, &prev);
                self.caret = Caret { block: prev, offset: len };
            }
        }
    }

    pub fn move_right(&mut self, state: &AutoCommit) {
        let len = self.block_len(state, &self.caret.block);
        if self.caret.offset < len {
            self.caret.offset += 1;
            return;
        }
        let blocks = self.ordered_blocks(state);
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block) {
            if pos + 1 < blocks.len() {
                self.caret = Caret { block: blocks[pos + 1].clone(), offset: 0 };
            }
        }
    }

    pub fn move_home(&mut self) {
        self.caret.offset = 0;
    }

    pub fn caret(&self) -> &Caret {
        &self.caret
    }

    pub fn set_caret(&mut self, caret: Caret) {
        self.caret = caret;
    }

    pub fn move_end(&mut self, state: &AutoCommit) {
        self.caret.offset = self.block_len(state, &self.caret.block);
    }

    /// Move the caret up/down by `delta` blocks (block-granular Up/Down).
    pub fn move_block_relative(&mut self, state: &AutoCommit, delta: i32) {
        let blocks = self.ordered_blocks(state);
        if blocks.is_empty() {
            return;
        }
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block) {
            let new = (pos as i32 + delta).clamp(0, blocks.len() as i32 - 1) as usize;
            self.move_to_block(state, new);
        }
    }

    /// Move the caret to the block at `index` (clamped), preserving a target
    /// column where possible. Used by Up/Down once layout is available; here it
    /// is the block-granular fallback.
    pub fn move_to_block(&mut self, state: &AutoCommit, index: usize) {
        let blocks = self.ordered_blocks(state);
        if blocks.is_empty() {
            return;
        }
        let i = index.min(blocks.len() - 1);
        let block = blocks[i].clone();
        let len = self.block_len(state, &block);
        let offset = self.caret.offset.min(len);
        self.caret = Caret { block, offset };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};

    fn bound() -> (WordProcessor, AutoCommit) {
        let mut wp = WordProcessor::new();
        let mut state = AutoCommit::new();
        wp.bind(&mut state, &ROOT);
        (wp, state)
    }

    #[test]
    fn bind_creates_initial_paragraph_and_caret() {
        let (wp, state) = bound();
        assert_eq!(wp.block_texts(&state), vec!["".to_string()]);
        assert_eq!(wp.caret.offset, 0);
        assert!(!wp.caret.block.is_empty());
    }

    #[test]
    fn type_text_advances_caret() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "hello");
        assert_eq!(wp.current_text(&state), "hello");
        assert_eq!(wp.caret.offset, 5);
    }

    #[test]
    fn enter_splits_block() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "helloworld");
        // move caret back to between "hello" and "world"
        for _ in 0..5 {
            wp.move_left(&state);
        }
        assert_eq!(wp.caret.offset, 5);
        wp.enter(&mut state);
        assert_eq!(wp.block_texts(&state), vec!["hello".to_string(), "world".to_string()]);
        assert_eq!(wp.caret.offset, 0);
        assert_eq!(wp.current_text(&state), "world");
    }

    #[test]
    fn backspace_within_and_across_blocks() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "ab");
        wp.enter(&mut state);
        wp.insert_str(&mut state, "cd");
        assert_eq!(wp.block_texts(&state), vec!["ab".to_string(), "cd".to_string()]);

        // delete 'd'
        wp.backspace(&mut state);
        assert_eq!(wp.current_text(&state), "c");
        // delete 'c'
        wp.backspace(&mut state);
        assert_eq!(wp.current_text(&state), "");
        // now at offset 0 of second block → merge into first
        wp.backspace(&mut state);
        assert_eq!(wp.block_texts(&state), vec!["ab".to_string()]);
        assert_eq!(wp.caret.offset, 2, "caret lands at the merge point");
    }

    #[test]
    fn delete_forward_merges_next() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "ab");
        wp.enter(&mut state);
        wp.insert_str(&mut state, "cd");
        // caret at end of "cd"; move to end of first block
        wp.move_to_block(&state, 0);
        wp.move_end(&state);
        assert_eq!(wp.current_text(&state), "ab");
        wp.delete_forward(&mut state);
        assert_eq!(wp.block_texts(&state), vec!["abcd".to_string()]);
    }

    #[test]
    fn caret_motion_crosses_blocks() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "ab");
        wp.enter(&mut state);
        wp.insert_str(&mut state, "cd");
        // caret at end of "cd"; move_right at end → no-op (last block)
        wp.move_right(&state);
        assert_eq!(wp.current_text(&state), "cd");
        assert_eq!(wp.caret.offset, 2);
        // move_left from offset 0 of "cd" → end of "ab"
        wp.move_home();
        wp.move_left(&state);
        assert_eq!(wp.current_text(&state), "ab");
        assert_eq!(wp.caret.offset, 2);
    }

    #[test]
    fn selection_delete_and_replace() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "hello world");
        // select "hello" (0..5)
        wp.caret.offset = 0;
        wp.set_anchor();
        wp.caret.offset = 5;
        assert_eq!(wp.selection_blocks(&state).into_iter().map(|(_, s, e)| (s, e)).collect::<Vec<_>>(), vec![(0, 5)]);
        // typing replaces the selection
        wp.insert_str(&mut state, "HI");
        assert_eq!(wp.current_text(&state), "HI world");
        assert_eq!(wp.caret.offset, 2);
        assert!(wp.selection_anchor.is_none());
    }

    #[test]
    fn multi_block_selection_copy_and_delete() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "first");
        wp.enter(&mut state);
        wp.insert_str(&mut state, "second");
        wp.enter(&mut state);
        wp.insert_str(&mut state, "third");
        let blocks = wp.ordered_blocks(&state);
        assert_eq!(blocks.len(), 3);

        // Select from "rst" of block 0 through "thi" of block 2.
        wp.set_caret(Caret { block: blocks[0].clone(), offset: 2 });
        wp.set_anchor();
        wp.set_caret(Caret { block: blocks[2].clone(), offset: 3 });

        // Selected text spans three blocks, joined by newlines.
        assert_eq!(wp.selection_text(&state), "rst\nsecond\nthi");

        // Deleting the selection merges the surviving heads/tails into one block.
        assert!(wp.delete_selection(&mut state));
        assert_eq!(wp.block_texts(&state), vec!["fird".to_string()]);
        assert!(!wp.has_selection());
    }

    #[test]
    fn insert_multiline_splits_into_paragraphs() {
        let (mut wp, mut state) = bound();
        wp.insert_multiline(&mut state, "alpha\nbeta\ngamma");
        assert_eq!(wp.block_texts(&state), vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn toggle_mark_adds_then_removes() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "hello");
        wp.caret.offset = 0;
        wp.set_anchor();
        wp.caret.offset = 5;
        let c = wp.containers.clone().unwrap();
        let block = wp.caret.block.clone();

        wp.toggle_mark(&mut state, "strong");
        assert!(qdoc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"), "mark applied");
        wp.toggle_mark(&mut state, "strong");
        assert!(!qdoc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"), "mark toggled off");
    }

    #[test]
    fn set_align_and_paragraph_style() {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut state, "Title");
        wp.set_align(&mut state, "center");
        wp.set_paragraph_style(&mut state, "heading", Some(2), None);
        let c = wp.containers.clone().unwrap();
        let snap = qdoc::model::materialize(&state, &c);
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&qdoc::AttrVal::Int(2)));
        assert_eq!(snap.blocks[0].attrs.get("align"), Some(&qdoc::AttrVal::Str("center".into())));
    }
}
