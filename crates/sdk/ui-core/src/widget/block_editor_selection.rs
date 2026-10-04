//! Explicit whole-block selection, separate from a text range.
use super::{
    Block, BlockEditor, CursorPlacement, Key, Modifiers, NamedKey, RetainedBlockSelection,
    WindowEvent,
};

impl BlockEditor {
    /// Whether destructive block keys belong to an explicit selection. An
    /// invalidated selection still owns those keys until a fresh gesture.
    pub fn is_block_selection_mode(&self) -> bool {
        matches!(
            self.retained_block_selection,
            Some(RetainedBlockSelection::Whole { .. } | RetainedBlockSelection::RefusedWhole)
        )
    }

    /// Select a complete authored fence or outline subtree without editing it.
    pub fn select_block_group(&mut self, index: usize) -> bool {
        self.select_block_groups(index, index)
    }

    /// Select all blocks, including empty paragraphs.
    pub fn select_all_blocks(&mut self) -> bool {
        self.blocks
            .len()
            .checked_sub(1)
            .is_some_and(|last| self.select_block_groups(0, last))
    }

    fn select_block_groups(&mut self, anchor: usize, cursor: usize) -> bool {
        if self.is_composing() {
            return false;
        }
        let (Some(anchor), Some(cursor)) = (
            self.block_group_range(anchor),
            self.block_group_range(cursor),
        ) else {
            return false;
        };
        let range = anchor.start.min(cursor.start)..anchor.end.max(cursor.end);
        let (Some(first), Some(last), Some(blocks)) = (
            self.blocks.get(anchor.start),
            self.blocks.get(cursor.start),
            self.blocks.get(range),
        ) else {
            return false;
        };
        let (anchor, cursor) = (first.id.clone(), last.id.clone());
        let ids: Vec<String> = blocks.iter().map(|block| block.id.clone()).collect();
        let reverse = blocks.first().is_some_and(|block| block.id != anchor);
        self.retain_block_selection(ids.clone(), reverse);
        self.retained_block_selection = Some(RetainedBlockSelection::Whole {
            ids,
            anchor,
            cursor,
        });
        self.tab_navigation = false;
        self.dirty = true;
        true
    }

    /// Move the active selection by a complete neighboring group. Shift keeps
    /// the original anchor, so reversing direction shrinks the same selection.
    fn block_selection_neighbor(&self, up: bool) -> Option<(usize, usize)> {
        if self.selected_block_range().is_none() || self.is_composing() {
            return None;
        }
        let Some(RetainedBlockSelection::Whole { anchor, cursor, .. }) =
            &self.retained_block_selection
        else {
            return None;
        };
        let (Some(anchor), Some(cursor)) = (
            self.blocks.iter().position(|block| &block.id == anchor),
            self.blocks.iter().position(|block| &block.id == cursor),
        ) else {
            return None;
        };
        let group = self.block_group_range(cursor)?;
        let next = if up {
            let mut previous = group.start.checked_sub(1)?;
            let depth = self.blocks.get(group.start).map(Block::indent)?;
            while previous > 0
                && self
                    .blocks
                    .get(previous)
                    .is_some_and(|block| block.indent() > depth)
            {
                previous = previous.saturating_sub(1);
            }
            previous
        } else {
            group.end
        };
        (next < self.blocks.len()).then_some((anchor, next))
    }

    /// Whether the selected group has a neighbor in this direction.
    pub fn can_navigate_block_selection(&self, up: bool) -> bool {
        self.block_selection_neighbor(up).is_some()
    }

    /// Move the active group; Shift extends or shrinks from its original anchor.
    pub fn navigate_block_selection(&mut self, up: bool, extend: bool) -> bool {
        let Some((anchor, next)) = self.block_selection_neighbor(up) else {
            return false;
        };
        self.select_block_groups(if extend { anchor } else { next }, next)
    }

    /// Return to the active block's caret without replacing selected text.
    pub fn leave_block_selection(&mut self) -> bool {
        if !self.is_block_selection_mode() {
            return false;
        }
        let cursor = match &self.retained_block_selection {
            Some(RetainedBlockSelection::Whole { cursor, .. }) => {
                self.blocks.iter().position(|block| &block.id == cursor)
            }
            _ => self.focused,
        };
        self.clear_document_selection();
        for block in &mut self.blocks {
            if block.line.selection_range().is_some() {
                let cursor = block.line.cursor_pos();
                block.line.inner_mut().set_selection(cursor, cursor);
            }
        }
        if let Some(cursor) = cursor {
            self.focus_block(cursor, CursorPlacement::End);
        }
        self.tab_navigation = false;
        self.dirty = true;
        true
    }

    /// Escape selects a block or clears the current selection, and always
    /// leaves the following Tab available for host focus navigation.
    pub fn toggle_block_selection(&mut self) -> bool {
        if self.is_composing() || self.has_open_popup() {
            return false;
        }
        let handled =
            if self.document_selection.is_some() || self.retained_block_selection.is_some() {
                if !self.leave_block_selection() {
                    self.clear_document_selection();
                }
                true
            } else {
                self.focused
                    .is_some_and(|index| self.select_block_group(index))
            };
        if handled {
            self.tab_navigation = true;
            self.dirty = true;
        }
        handled
    }

    /// Copy the selected blocks as plain text. A cut must stop when the host
    /// rejects this write; the usual in-process clipboard remains supported.
    pub fn copy_selected_blocks(&self) -> bool {
        let Some(range) = self.selected_block_range() else {
            return false;
        };
        let Some(blocks) = self.blocks.get(range) else {
            return false;
        };
        let text = blocks
            .iter()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n");
        crate::text_selection::clipboard_copy_accepted(&text)
    }

    pub(super) fn leave_block_selection_for_input(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
    ) {
        if !self.is_block_selection_mode() {
            return;
        }
        let held = modifiers.state();
        let editing = match event {
            WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) => !text.is_empty(),
            WindowEvent::Ime(winit::event::Ime::Commit(_)) => true,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed =>
            {
                match &event.logical_key {
                    Key::Character(value) => {
                        (!held.control_key() && !held.super_key())
                            || ((held.control_key() || held.super_key())
                                && value.eq_ignore_ascii_case("v"))
                    }
                    Key::Named(
                        NamedKey::Space
                        | NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::Home
                        | NamedKey::End
                        | NamedKey::PageUp
                        | NamedKey::PageDown,
                    ) => true,
                    _ => false,
                }
            }
            _ => false,
        };
        if editing {
            self.leave_block_selection();
        }
    }
}
