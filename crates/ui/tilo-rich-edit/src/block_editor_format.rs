//! Formatting a toolbar applies: marks over the selection, across every paragraph it spans, and lists
//! and headings over the selected paragraphs.

use super::{BlockEditor, BlockKind, DocumentPosition, EditScope};
use ui::widget::marks::{Mark, MarkSpan};

/// One change a view asks of a paragraph's own look, as a toolbar or a paragraph panel offers
/// it: where its lines stand, the space it keeps above and below, its indent, and the named
/// style it is set in.
#[derive(Clone, Debug, PartialEq)]
pub enum ParagraphChange {
    Align(ui::richtext::TextAlign),
    SpaceBefore(Option<f32>),
    SpaceAfter(Option<f32>),
    Indent(Option<f32>),
    FirstLineIndent(Option<f32>),
    Style(Option<String>),
}

impl ParagraphChange {
    fn apply(&self, style: &mut crate::ParagraphStyle) {
        match self {
            Self::Align(align) => style.align = Some(*align),
            Self::SpaceBefore(points) => style.space_before = *points,
            Self::SpaceAfter(points) => style.space_after = *points,
            Self::Indent(points) => style.indent = *points,
            Self::FirstLineIndent(points) => style.first_line_indent = *points,
            Self::Style(name) => style.style.clone_from(name),
        }
    }
}

/// The points a command names, or none for a number it does not hold; `0` clears.
fn points(argument: &str) -> Option<Option<f32>> {
    argument
        .trim()
        .trim_end_matches("pt")
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|points| (0.0..=288.0).contains(points))
        .map(|points| (points > 0.0).then_some(points))
}

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

    /// [`Self::set_look_mark`] over a selection that spans paragraphs: the kind is taken off
    /// every selected character and the new look put on it, so a colour, size, face or baseline
    /// applies to the whole selection whatever it held before.
    fn set_look_across(
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
        let raised = matches!(mark, Mark::Superscript | Mark::Subscript);
        let already = raised
            && ranges.iter().all(|(index, range)| {
                self.blocks.get(*index).is_some_and(|block| {
                    ui::widget::marks::range_covered_by(block.line.marks(), range, mark)
                })
            });
        for (index, range) in ranges {
            let Some(block) = self.blocks.get_mut(index) else {
                continue;
            };
            let mut spans = block.line.marks().to_vec();
            ui::widget::marks::remove_mark_from_range(&mut spans, range.clone(), mark);
            if !already {
                ui::widget::marks::add_mark_to_range(&mut spans, range, mark.clone());
            }
            block.line.set_marks(spans);
        }
        self.edit_scope.widen(EditScope::Blocks(touched));
        self.dirty = true;
        true
    }

    /// What of a paragraph's own look a command sets.
    pub fn set_paragraph(&mut self, change: ParagraphChange) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        let Some(range) = self.selected_block_range() else {
            return false;
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
        let mut relayout = false;
        for index in targets {
            let Some(block) = self.blocks.get_mut(index) else {
                continue;
            };
            let before = block.style.clone();
            change.apply(&mut block.style);
            if block.style == before {
                continue;
            }
            changed = true;
            match &change {
                ParagraphChange::Align(align) => {
                    block.line.set_align(*align);
                }
                ParagraphChange::SpaceBefore(_)
                | ParagraphChange::SpaceAfter(_)
                | ParagraphChange::Indent(_)
                | ParagraphChange::FirstLineIndent(_) => relayout = true,
                ParagraphChange::Style(_) => relayout = true,
            }
        }
        if changed {
            self.edit_scope.widen(EditScope::Blocks(range));
            self.dirty = true;
        }
        if relayout {
            self.restack();
        }
        changed
    }

    /// Set the run's own look — colour, size, face or baseline shift — over the selection, or
    /// at the caret for the text typed next, replacing what that kind held there. A raised or
    /// lowered stretch asked for again becomes plain text. `false` when nothing changed.
    pub fn set_look_mark(&mut self, mark: Mark) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        if let Some(selection) = self.document_selection {
            let (start, end) = selection.ordered();
            if start.block != end.block {
                return self.set_look_across(start, end, &mark);
            }
        }
        let Some(index) = self.focused.filter(|index| *index < self.blocks.len()) else {
            return false;
        };
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        block.line.set_look_mark(mark);
        self.edit_scope
            .widen(EditScope::Blocks(index..index.saturating_add(1)));
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
    /// `italic`, `underline`, `strike`, `super`, `sub`, `size:<points>`,
    /// `font:<sans|serif|mono>`, `color:<#rrggbb>`, `highlight:<#rrggbb>`, `bullets`,
    /// `numbers`, `heading:<level>` or `link:<address>`, then a count the view changes every
    /// time it asks (`bold:1`, `bold:2`), so the same command asked twice is two commands. The
    /// ask is answered once: the same ask again changes nothing. Answers whether it changed the
    /// document.
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
            "super" => self.set_look_mark(Mark::Superscript),
            "sub" => self.set_look_mark(Mark::Subscript),
            "size" => match argument.trim().parse::<u16>() {
                Ok(points) if points > 0 => self.set_look_mark(Mark::Size(points)),
                _ => false,
            },
            "font" => crate::rich_doc::family_of(argument)
                .is_some_and(|family| self.set_look_mark(Mark::Font(family))),
            "color" => crate::rich_doc::rgb_of(argument)
                .is_some_and(|rgb| self.set_look_mark(Mark::Color(rgb))),
            "highlight" => crate::rich_doc::rgb_of(argument)
                .is_some_and(|rgb| self.set_look_mark(Mark::Highlight(rgb))),
            "align" => crate::rich_doc::align_of(argument)
                .is_some_and(|align| self.set_paragraph(ParagraphChange::Align(align))),
            "before" => points(argument)
                .is_some_and(|points| self.set_paragraph(ParagraphChange::SpaceBefore(points))),
            "after" => points(argument)
                .is_some_and(|points| self.set_paragraph(ParagraphChange::SpaceAfter(points))),
            "indent" => points(argument)
                .is_some_and(|points| self.set_paragraph(ParagraphChange::Indent(points))),
            "hanging" => points(argument).is_some_and(|points| {
                self.set_paragraph(ParagraphChange::FirstLineIndent(points))
            }),
            // A named style is its name, and for the styles every document has it is also what
            // the paragraph is: a heading style makes a heading, as the paragraph panel does.
            "style" => {
                let name = argument.trim();
                if name.is_empty() {
                    false
                } else {
                    let named = self.set_paragraph(ParagraphChange::Style(Some(name.to_owned())));
                    let kind = match name.to_lowercase().as_str() {
                        "body" | "normal" => self.set_heading(None),
                        "heading 1" | "heading1" => self.set_heading(Some(1)),
                        "heading 2" | "heading2" => self.set_heading(Some(2)),
                        "heading 3" | "heading3" => self.set_heading(Some(3)),
                        _ => false,
                    };
                    named || kind
                }
            }
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
    use ui::richtext::TextAlign;

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
    /// A raised stretch asked for twice is plain text again, and raising one that is lowered
    /// replaces it: a run is one or the other, never both.
    #[test]
    fn raised_and_lowered_text_toggle_as_one_kind() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("x2 and H2O")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.blocks[0].line.inner_mut().set_selection(1, 2);

        assert!(editor.set_look_mark(Mark::Superscript));
        assert_eq!(
            editor.blocks[0].line.marks(),
            [MarkSpan::new(1..2, Mark::Superscript)]
        );
        assert!(editor.set_look_mark(Mark::Subscript));
        assert_eq!(
            editor.blocks[0].line.marks(),
            [MarkSpan::new(1..2, Mark::Subscript)],
            "raising it again lowers it instead of carrying both"
        );
        assert!(editor.set_look_mark(Mark::Subscript));
        assert!(
            editor.blocks[0].line.marks().is_empty(),
            "asking for the same baseline again is plain text"
        );
    }

    /// A colour, a size or a face replaces what that kind held over the selection rather than
    /// taking it off: asking for another colour re-colours the words.
    #[test]
    fn a_runs_look_replaces_what_that_kind_held() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("hello world")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.blocks[0].line.inner_mut().set_selection(0, 5);

        assert!(editor.set_look_mark(Mark::Color([0xcf, 0x22, 0x2e])));
        assert!(editor.set_look_mark(Mark::Color([0x09, 0x69, 0xda])));
        assert!(editor.set_look_mark(Mark::Size(14)));
        assert!(editor.set_look_mark(Mark::Highlight([0xff, 0xf8, 0xc5])));
        assert!(editor.set_look_mark(Mark::Font(ui::richtext::FontFamily::Serif)));
        assert_eq!(
            editor.blocks[0].line.marks(),
            [
                MarkSpan::new(0..5, Mark::Color([0x09, 0x69, 0xda])),
                MarkSpan::new(0..5, Mark::Size(14)),
                MarkSpan::new(0..5, Mark::Highlight([0xff, 0xf8, 0xc5])),
                MarkSpan::new(0..5, Mark::Font(ui::richtext::FontFamily::Serif)),
            ]
        );
    }

    /// The commands a toolbar asks reach the same marks: a size, a face, an ink, a band and a
    /// baseline, each naming what it sets.
    #[test]
    fn a_toolbars_look_commands_format_the_selection() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("hello world")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.blocks[0].line.inner_mut().set_selection(0, 5);

        assert!(editor.format_asked(Some("color:#0969da:1".into())));
        assert!(editor.format_asked(Some("size:14:2".into())));
        assert!(editor.format_asked(Some("font:serif:3".into())));
        assert!(editor.format_asked(Some("highlight:#fff8c5:4".into())));
        assert!(editor.format_asked(Some("super:5".into())));
        assert!(editor.active_marks().contains(&Mark::Color([0x09, 0x69, 0xda])));
        assert!(editor.active_marks().contains(&Mark::Size(14)));
        assert!(
            editor
                .active_marks()
                .contains(&Mark::Font(ui::richtext::FontFamily::Serif))
        );
        assert!(
            editor
                .active_marks()
                .contains(&Mark::Highlight([0xff, 0xf8, 0xc5]))
        );
        assert!(editor.active_marks().contains(&Mark::Superscript));
    }
    /// A paragraph's own look — its alignment, the space above and below it, its indent and
    /// the named style it is set in — reaches the block the caret is in, and a built-in style
    /// name also makes the paragraph that kind.
    #[test]
    fn a_paragraphs_look_is_set_from_the_toolbar() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("one"), Block::paragraph("two")]);
        editor.focus_block(0, CursorPlacement::End);

        assert!(editor.set_paragraph(ParagraphChange::Align(TextAlign::Center)));
        assert_eq!(editor.blocks[0].style.align, Some(TextAlign::Center));
        assert!(
            !editor.set_paragraph(ParagraphChange::Align(TextAlign::Center)),
            "asking for the alignment it holds changes nothing"
        );

        assert!(editor.format_asked(Some("indent:12:1".into())));
        assert_eq!(editor.blocks[0].style.indent, Some(12.0));
        assert!(editor.format_asked(Some("before:6:2".into())));
        assert_eq!(editor.blocks[0].style.space_before, Some(6.0));
        assert!(editor.format_asked(Some("after:18:3".into())));
        assert_eq!(editor.blocks[0].style.space_after, Some(18.0));
        assert!(editor.format_asked(Some("indent:0:4".into())));
        assert_eq!(
            editor.blocks[0].style.indent, None,
            "no indent is the document's own"
        );

        assert!(editor.format_asked(Some("style:Heading 2:5".into())));
        assert_eq!(editor.blocks[0].style.style.as_deref(), Some("Heading 2"));
        assert_eq!(editor.blocks[0].kind, BlockKind::Heading(2));
        assert!(editor.format_asked(Some("style:Body:6".into())));
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
    }
}