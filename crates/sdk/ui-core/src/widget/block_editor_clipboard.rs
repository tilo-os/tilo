//! Cut, copy and paste, owned by the editor whichever way they are asked
//! for: a shortcut, the text menu, assistive technology. A copy offers the
//! selection as formatted text (HTML) and as words; a paste brings in the
//! formatting another editor, a browser or an office suite copied, and
//! Ctrl+Shift+V the words alone.

use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::Key;

use super::html::{blocks_from_html, blocks_to_html};
use super::{
    Block, BlockContent, BlockEditor, BlockKind, CursorPlacement, DocumentPosition,
    DocumentSelection, marks_after, marks_before, normalize_replacement,
};
use crate::text_selection::{clipboard_copy_rich_accepted, clipboard_paste, clipboard_paste_rich};
use crate::widget::EventCtx;
use crate::widget::marks::{MarkSpan, add_mark_to_range};

/// What a paste brings in.
enum Pasted {
    Words(String),
    /// Formatted blocks.
    Blocks(Vec<BlockContent>),
}

impl BlockEditor {
    /// Cut, copy, paste (formatted, or `paste-plain` for the words alone) or
    /// select all (`command`), as the shortcuts and the text menu do, over
    /// the document selection, the focused paragraph's or the focused table
    /// cell's. An edit it makes is left for the owner to take
    /// ([`Self::take_edit_ops`]); within an event, the event's edit scope
    /// covers it. `false` when `command` is none of those.
    pub(super) fn clipboard_command(&mut self, command: &str) -> bool {
        let table_command = if command == "paste-plain" {
            "paste"
        } else {
            command
        };
        if let Some((index, result)) = self.blocks.iter().enumerate().find_map(|(index, block)| {
            self.table_editors
                .get_mut(&block.id)
                .and_then(|table| table.projected_clipboard(table_command))
                .map(|result| (index, result))
        }) {
            if let Some((row, column, value)) = result {
                self.begin_edit();
                self.apply_table_cell_edit(index, row, column, &value);
            }
            self.dirty = true;
            return true;
        }
        if !matches!(
            command,
            "cut" | "copy" | "paste" | "paste-plain" | "select-all"
        ) {
            return false;
        }
        if command == "select-all" {
            self.handle_document_selection_ctrl_key(&Key::Character("a".into()));
            self.dirty = true;
            return true;
        }
        // Words that cannot be edited can still be copied.
        if self.read_only && command != "copy" {
            return true;
        }
        let Some(selection) = self.clipboard_selection() else {
            return true;
        };
        if matches!(command, "copy" | "cut") && selection.anchor == selection.cursor {
            return true;
        }
        if command == "paste" && self.paste_image() {
            return true;
        }
        // An unavailable clipboard must never turn Paste into Delete.
        let pasted = match command {
            "paste" | "paste-plain" => {
                let Some(pasted) = self.clipboard_payload(command == "paste", selection) else {
                    return true;
                };
                pasted
            }
            _ => Pasted::Words(String::new()),
        };
        let prior = self.document_selection;
        self.document_selection = Some(selection);
        if matches!(command, "copy" | "cut") {
            let accepted = self.copy_selection();
            if command == "cut" && !accepted {
                self.document_selection = prior;
                return true;
            }
        }
        if command == "copy" {
            self.document_selection = prior;
        } else {
            self.begin_edit();
            match pasted {
                Pasted::Words(words) => {
                    self.replace_document_selection(&words);
                }
                Pasted::Blocks(blocks) => {
                    self.replace_document_selection_with_blocks(blocks);
                }
            }
            self.set_focus(true);
        }
        self.dirty = true;
        true
    }

    /// What cut, copy and paste act on: the document selection, a picture
    /// block shown as a preview, or the focused line's selection or caret.
    fn clipboard_selection(&self) -> Option<DocumentSelection> {
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
        self.document_selection.or(image_selection).or_else(|| {
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
        })
    }

    /// What the clipboard pastes over `selection`: its formatting when
    /// `rich` and the words land in prose, else its words. `None` when it
    /// holds nothing to paste.
    fn clipboard_payload(&self, rich: bool, selection: DocumentSelection) -> Option<Pasted> {
        let (start, _) = selection.ordered();
        // Code and table source take words: formatting has no place there.
        let prose = self.blocks.get(start.block).is_some_and(|block| {
            !matches!(
                block.kind,
                BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
            )
        });
        if rich
            && prose
            && let Some(offer) = clipboard_paste_rich()
        {
            let mut blocks = blocks_from_html(&offer.html);
            if !blocks.is_empty() {
                if let Some(plain) = offer.plain.as_deref() {
                    keep_edge_spaces(&mut blocks, plain);
                }
                return Some(Pasted::Blocks(blocks));
            }
            // HTML that shows no words falls back to the words copied with
            // it, never to a second read of the clipboard.
            return offer
                .plain
                .filter(|plain| !plain.is_empty())
                .map(Pasted::Words);
        }
        clipboard_paste().map(Pasted::Words)
    }

    /// Put the document selection on the clipboard, formatted and as
    /// words. Whether the clipboard took it.
    fn copy_selection(&self) -> bool {
        let (Some(plain), Some(blocks)) = (self.selected_text(), self.selected_content()) else {
            return false;
        };
        clipboard_copy_rich_accepted(&blocks_to_html(&blocks), &plain)
    }

    /// The document selection as blocks: the blocks it spans, the first and
    /// last cut to where it starts and ends, each with its marks.
    #[must_use]
    pub fn selected_content(&self) -> Option<Vec<BlockContent>> {
        let selection = self.document_selection?;
        let (start, end) = selection.ordered();
        if end.block >= self.blocks.len() {
            return None;
        }
        let mut out = Vec::new();
        for index in start.block..=end.block {
            let block = self.blocks.get(index)?;
            let text = block.text();
            let from = if index == start.block {
                text.floor_char_boundary(start.byte)
            } else {
                0
            };
            let to = if index == end.block {
                text.floor_char_boundary(end.byte)
            } else {
                text.len()
            };
            let mut content = block.content();
            text.get(from..to.max(from))
                .unwrap_or_default()
                .clone_into(&mut content.text);
            content.marks = marks_after(&marks_before(block.line.marks(), to), from);
            content.authors.clear();
            out.push(content);
        }
        Some(out)
    }

    /// Replace the document selection with formatted `pasted` blocks: the
    /// first joins the words before the selection, the last the words after
    /// it, and the ones between stand as blocks of their own kinds. One
    /// edit, as [`Self::replace_document_selection`] is.
    pub fn replace_document_selection_with_blocks(&mut self, pasted: Vec<BlockContent>) -> bool {
        let mut pasted: Vec<BlockContent> = pasted
            .into_iter()
            .map(|mut block| {
                // A block is one line; control characters go.
                let text = normalize_replacement(&block.text).replace('\n', " ");
                if text.len() != block.text.len() {
                    block.marks.retain(|span| span.range.end <= text.len());
                }
                block.text = text;
                block
            })
            .collect();
        if pasted.is_empty() {
            return false;
        }
        self.begin_edit();
        let Some(selection) = self.document_selection else {
            return false;
        };
        let (start, end) = selection.ordered();
        if end.block >= self.blocks.len() {
            return false;
        }
        let (Some(start_text), Some(end_text)) = (
            self.blocks.get(start.block).map(|b| b.text().to_owned()),
            self.blocks.get(end.block).map(|b| b.text().to_owned()),
        ) else {
            return false;
        };
        let start_byte = start_text.floor_char_boundary(start.byte);
        let end_byte = end_text.floor_char_boundary(end.byte);
        let prefix = start_text.get(..start_byte).unwrap_or_default().to_owned();
        let suffix = end_text.get(end_byte..).unwrap_or_default().to_owned();
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

        // The first pasted block joins the words before the selection; on
        // an empty line it brings its own kind (a heading, a list item).
        let first = pasted.remove(0);
        let mut first_text = prefix.clone();
        first_text.push_str(&first.text);
        let mut first_marks = prefix_marks;
        shift_into(&mut first_marks, &first.marks, prefix.len());
        let local_author = self.local_author.clone();
        if let Some(block) = self.blocks.get_mut(start.block) {
            if prefix.is_empty()
                && first.kind != block.kind
                && !matches!(first.kind, BlockKind::Paragraph | BlockKind::Divider)
            {
                block.retype(&first.kind);
                block.code_group.clone_from(&first.code_group);
                block.line.set_local_author(local_author.clone());
            }
            block.line.set_text(&first_text);
            block.line.set_marks(first_marks.clone());
        }
        if start.block < end.block {
            self.blocks.drain(start.block.saturating_add(1)..=end.block);
        }

        let (caret_block, caret_byte) = if pasted.is_empty() {
            let caret = first_text.len();
            let mut marks = first_marks;
            shift_into(&mut marks, &suffix_marks, caret);
            if let Some(block) = self.blocks.get_mut(start.block) {
                block.line.set_text(&format!("{first_text}{suffix}"));
                block.line.set_marks(marks);
            }
            (start.block, caret)
        } else {
            let count = pasted.len();
            let mut caret = (start.block, 0);
            for (offset, mut content) in pasted.into_iter().enumerate() {
                let at = start.block.saturating_add(offset).saturating_add(1);
                let caret_byte = content.text.len();
                if offset + 1 == count {
                    content.text.push_str(&suffix);
                    let mut marks = std::mem::take(&mut content.marks);
                    shift_into(&mut marks, &suffix_marks, caret_byte);
                    content.marks = marks;
                }
                content.id = uuid::Uuid::new_v4().to_string();
                content.authors.clear();
                let mut block = Block::from_content(content);
                block.line.set_local_author(local_author.clone());
                self.blocks.insert(at.min(self.blocks.len()), block);
                caret = (at, caret_byte);
            }
            caret
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

    /// Ctrl+C, Ctrl+X, Ctrl+V and Ctrl+Shift+V (Cmd on a Mac) while the
    /// editor has focus: the editor's own cut, copy and paste, whichever
    /// line holds the caret.
    pub(super) fn try_clipboard_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
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
        if !self.is_focused
            || !(mods.control_key() || mods.super_key())
            || mods.alt_key()
            || (self.focused.is_none() && self.document_selection.is_none())
        {
            return false;
        }
        let command = match (key.to_ascii_lowercase().as_str(), mods.shift_key()) {
            ("c", false) => "copy",
            ("x", false) => "cut",
            ("v", false) => "paste",
            ("v", true) => "paste-plain",
            _ => return false,
        };
        if !self.clipboard_command(command) {
            return false;
        }
        *ctx.handled = true;
        true
    }
}

/// Words copied from within a line keep the spaces at their ends, which
/// HTML does not carry: "very " pasted into "a good day" reads "a very good
/// day". Only for a paste of one paragraph whose words are the copied
/// words; a block's edges are not words'.
fn keep_edge_spaces(blocks: &mut [BlockContent], plain: &str) {
    let [block] = blocks else {
        return;
    };
    let words = plain.trim_matches(' ');
    if block.kind != BlockKind::Paragraph || block.text != words || words.is_empty() {
        return;
    }
    let lead = plain.len() - plain.trim_start_matches(' ').len();
    let mut marks = Vec::new();
    shift_into(&mut marks, &block.marks, lead);
    block.marks = marks;
    plain.clone_into(&mut block.text);
}

/// Add `marks`, moved `by` bytes on, to `into`.
fn shift_into(into: &mut Vec<MarkSpan>, marks: &[MarkSpan], by: usize) {
    for span in marks {
        add_mark_to_range(
            into,
            span.range.start.saturating_add(by)..span.range.end.saturating_add(by),
            span.mark.clone(),
        );
    }
}
