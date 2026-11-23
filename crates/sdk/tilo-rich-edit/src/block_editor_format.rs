//! Formatting a toolbar applies: marks over the selection, across every paragraph it spans, and lists
//! and headings over the selected paragraphs.

use super::{BlockEditor, BlockKind, DocumentPosition, EditScope};
use ui::widget::marks::{Mark, MarkSpan};

impl BlockEditor {
    pub fn toggle_mark(&mut self, mark: Mark) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            if start.block != end.block {
                return self.toggle_mark_across(start, end, &mark);
            }
        }
        let Some(index) = self.focused.filter(|index| *index < self.blocks.len()) else {
            return false;
        };
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        block.line.toggle_mark(mark);
        self.edit_scope
            .widen(EditScope::Blocks(index..index.saturating_add(1)));
        self.dirty = true;
        true
    }

    /// [`Self::toggle_mark`] over a selection from `start` to `end` that
    /// spans paragraphs: the mark comes off when it already covers every
    /// selected character, and goes on over all of them otherwise.
    fn toggle_mark_across(
        &mut self,
        start: DocumentPosition,
        end: DocumentPosition,
        mark: &Mark,
    ) -> bool {
        let ranges: Vec<(usize, std::ops::Range<usize>)> = (start.block..=end.block)
            .filter_map(|index| {
                let block = self.blocks.get(index)?;
                if !block.kind.is_editable()
                    || matches!(
                        block.kind,
                        BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                    )
                {
                    return None;
                }
                let len = block.line.value().len();
                let from = if index == start.block {
                    start.byte.min(len)
                } else {
                    0
                };
                let to = if index == end.block {
                    end.byte.min(len)
                } else {
                    len
                };
                (from < to).then_some((index, from..to))
            })
            .collect();
        let (Some((first, _)), Some((last, _))) = (ranges.first(), ranges.last()) else {
            return false;
        };
        let touched = *first..last.saturating_add(1);
        let covered = ranges.iter().all(|(index, range)| {
            self.blocks.get(*index).is_some_and(|block| {
                ui::widget::marks::range_fully_covered(block.line.marks(), range, mark)
            })
        });
        for (index, range) in ranges {
            let Some(block) = self.blocks.get_mut(index) else {
                continue;
            };
            let mut spans = block.line.marks().to_vec();
            if covered {
                ui::widget::marks::remove_mark_from_range(&mut spans, range, mark);
            } else {
                ui::widget::marks::add_mark_to_range(&mut spans, range, mark.clone());
            }
            block.line.set_marks(spans);
        }
        self.edit_scope.widen(EditScope::Blocks(touched));
        self.dirty = true;
        true
    }

    /// Toggle a list as a toolbar button does: the selected paragraphs, or
    /// the focused one, become a bulleted list (a numbered one when
    /// `ordered`), each keeping its depth; when every one of them already is
    /// that list, they become paragraphs again. Code and tables stay what
    /// they are. `false` when nothing changed.
    pub fn toggle_list(&mut self, ordered: bool) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        let Some(range) = self.selected_block_range() else {
            return false;
        };
        let listed = |kind: &BlockKind| {
            if ordered {
                matches!(kind, BlockKind::Numbered { .. })
            } else {
                matches!(kind, BlockKind::Bullet { .. })
            }
        };
        let targets: Vec<usize> = range
            .clone()
            .filter(|index| {
                self.blocks.get(*index).is_some_and(|block| {
                    block.kind.is_editable()
                        && !matches!(
                            block.kind,
                            BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                        )
                })
            })
            .collect();
        if targets.is_empty() {
            return false;
        }
        let off = targets.iter().all(|index| {
            self.blocks
                .get(*index)
                .is_some_and(|block| listed(&block.kind))
        });
        let kind = if off {
            BlockKind::Paragraph
        } else if ordered {
            BlockKind::Numbered { indent: 0 }
        } else {
            BlockKind::Bullet { indent: 0 }
        };
        let mut changed = false;
        for index in targets {
            changed |= self.try_convert_to(index, kind.clone());
        }
        if changed {
            self.edit_scope.widen(EditScope::Blocks(range));
            self.dirty = true;
        }
        changed
    }

    pub const HEADING_LEVELS: std::ops::RangeInclusive<u8> = 1..=3;

    pub fn set_heading(&mut self, level: Option<u8>) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        let Some(range) = self.selected_block_range() else {
            return false;
        };
        let kind = match level {
            Some(level) => BlockKind::Heading(
                level.clamp(*Self::HEADING_LEVELS.start(), *Self::HEADING_LEVELS.end()),
            ),
            None => BlockKind::Paragraph,
        };
        let targets: Vec<usize> = range
            .clone()
            .filter(|index| {
                self.blocks.get(*index).is_some_and(|block| {
                    block.kind.is_editable()
                        && !matches!(
                            block.kind,
                            BlockKind::CodeBlock { .. } | BlockKind::Table { .. }
                        )
                })
            })
            .collect();
        if targets.is_empty() {
            return false;
        }
        let mut changed = false;
        for index in targets {
            changed |= self.try_convert_to(index, kind.clone());
        }
        if changed {
            self.edit_scope.widen(EditScope::Blocks(range));
            self.dirty = true;
        }
        changed
    }

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

    /// A view asking for a formatting command, as a toolbar beside the document does: `bold`,
    /// `italic`, `underline`, `strike`, `bullets`, `numbers`, `heading:<level>` or
    /// `link:<address>`, then a count the view changes every time it asks (`bold:1`,
    /// `bold:2`), so the same command asked twice is two commands. The ask is answered once:
    /// the same ask again changes nothing. Answers whether it changed the document.
    pub fn format_asked(&mut self, asked: Option<String>) -> bool {
        if asked == self.asked_format {
            return false;
        }
        self.asked_format.clone_from(&asked);
        let Some(asked) = asked else {
            return false;
        };
        // The count is after the last colon; what the ask is, is before it.
        let command = asked.rsplit_once(':').map_or(asked.as_str(), |(it, _)| it);
        let (name, argument) = command
            .split_once(':')
            .map_or((command, ""), |(name, argument)| (name, argument));
        match name {
            "bold" => self.toggle_mark(Mark::Bold),
            "italic" => self.toggle_mark(Mark::Italic),
            "underline" => self.toggle_mark(Mark::Underline),
            "strike" => self.toggle_mark(Mark::Strikethrough),
            "bullets" => self.toggle_list(false),
            "numbers" => self.toggle_list(true),
            "heading" => match argument.trim().parse::<u8>() {
                Ok(0) => self.set_heading(None),
                Ok(level) => self.set_heading(Some(level)),
                Err(_) => false,
            },
            "link" => self.insert_link(argument),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, CursorPlacement};

    /// A toolbar's ask formats once: the same ask again is the command it already ran, and what
    /// the ask names is what it formats.
    #[test]
    fn a_formatting_ask_applies_once() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("hello world")]);
        editor.focus_block(0, CursorPlacement::End);
        assert!(editor.format_asked(Some("bold:1".into())), "the ask bolds");
        assert_eq!(editor.active_marks(), [Mark::Bold]);
        assert!(
            !editor.format_asked(Some("bold:1".into())),
            "the same ask is the command it already ran"
        );
        assert_eq!(editor.active_marks(), [Mark::Bold], "it is still bold");
        assert!(
            editor.format_asked(Some("bold:2".into())),
            "a new ask unbolds"
        );
        assert!(editor.active_marks().is_empty());
        assert!(
            !editor.format_asked(Some("nonsense:1".into())),
            "a command the editor has not is no command"
        );
        assert!(!editor.format_asked(None));
    }
}
