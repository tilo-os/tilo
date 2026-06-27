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
}

impl Default for WordProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessor {
    pub fn new() -> Self {
        Self { containers: None, ids: None, caret: Caret::default() }
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

    /// Insert `s` at the caret and advance.
    pub fn insert_str(&mut self, state: &mut AutoCommit, s: &str) {
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
}
