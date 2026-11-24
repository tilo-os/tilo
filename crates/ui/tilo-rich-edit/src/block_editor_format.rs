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
    /// The paragraph's rows apart, as a multiple of the normal leading; `None` is the
    /// document's own single spacing.
    LineSpacing(Option<f32>),
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
            Self::LineSpacing(multiplier) => style.line_spacing = *multiplier,
            Self::Style(name) => style.style.clone_from(name),
        }
    }
}

/// The paragraph change `make` builds from the points a command names, or none for a number it
/// does not hold; `0` clears the setting.
fn points(argument: &str, make: fn(Option<f32>) -> ParagraphChange) -> Option<ParagraphChange> {
    argument
        .trim()
        .trim_end_matches("pt")
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|points| (0.0..=288.0).contains(points))
        .map(|points| make((points > 0.0).then_some(points)))
}

/// The paragraph change a spacing command names: the rows stand a multiple of the normal
/// leading apart, `1` (or `0`) setting the document's own single spacing.
fn line_spacing(argument: &str) -> Option<ParagraphChange> {
    let multiplier = argument
        .trim()
        .trim_end_matches('x')
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|multiplier| multiplier.is_finite())?;
    if multiplier <= 1.001 {
        Some(ParagraphChange::LineSpacing(None))
    } else {
        Some(ParagraphChange::LineSpacing(Some(
            multiplier.clamp(1.0, 3.0),
        )))
    }
}

/// `text` without `removed`, and where each later position lands: each range's end and the
/// shift every position at or past it takes.
fn strip_ranges(
    text: &str,
    removed: &mut Vec<std::ops::Range<usize>>,
) -> (String, Vec<(usize, isize)>) {
    removed.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in removed.drain(..) {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut taken = 0usize;
    let mut shifts: Vec<(usize, isize)> = Vec::new();
    for range in &merged {
        out.push_str(text.get(cursor..range.start).unwrap_or_default());
        cursor = range.end;
        taken = taken.saturating_add(range.end.saturating_sub(range.start));
        #[expect(
            clippy::cast_possible_wrap,
            reason = "a document's length is far below the width of an isize"
        )]
        let shift = -(taken as isize);
        shifts.push((range.end, shift));
    }
    out.push_str(text.get(cursor..).unwrap_or_default());
    (out, shifts)
}

/// Where `at` stands after the removals `shifts` names.
fn shifted(at: usize, shifts: &[(usize, isize)]) -> usize {
    let shift = shifts
        .iter()
        .rev()
        .find(|(end, _)| *end <= at)
        .map_or(0, |(_, shift)| *shift);
    usize::try_from(at.saturating_add_signed(shift)).unwrap_or(0)
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

    /// Take every tracked change in the document, keeping the words that were inserted and the
    /// words that were not deleted, or reject them all, taking back the insertions and keeping
    /// what was deleted. Each block the change touches is rewritten as one edit.
    pub fn resolve_changes(&mut self, accept: bool) -> bool {
        self.resolve_changes_of(None, accept)
    }

    /// [`Self::resolve_changes`] for one tracked change, named by the id the document keeps it
    /// under: the person takes or rejects the change they are reading, not the whole document.
    pub fn resolve_change(&mut self, id: &str, accept: bool) -> bool {
        self.resolve_changes_of(Some(id), accept)
    }

    fn resolve_changes_of(&mut self, only: Option<&str>, accept: bool) -> bool {
        self.note_edit_anywhere();
        if self.read_only {
            return false;
        }
        let mine = |mark: &Mark| only.is_none_or(|id| mark.change() == Some(id));
        let dropped =
            |mark: &Mark| mine(mark) && matches!(mark, Mark::Insertion(_) | Mark::Deletion(_));
        let removed = |mark: &Mark| {
            mine(mark)
                && if accept {
                    matches!(mark, Mark::Deletion(_))
                } else {
                    matches!(mark, Mark::Insertion(_))
                }
        };
        let mut touched: Vec<usize> = Vec::new();
        for (index, block) in self.blocks.iter_mut().enumerate() {
            let marks = block.line.marks().to_vec();
            let tracked = marks.iter().any(|span| {
                mine(&span.mark) && matches!(span.mark, Mark::Insertion(_) | Mark::Deletion(_))
            });
            if !tracked {
                continue;
            }
            let text = block.line.value().to_owned();
            let mut ranges: Vec<std::ops::Range<usize>> = marks
                .iter()
                .filter(|span| removed(&span.mark))
                .map(|span| span.range.start.min(text.len())..span.range.end.min(text.len()))
                .filter(|range| range.start < range.end)
                .collect();
            let (next_text, shifts) = strip_ranges(&text, &mut ranges);
            let next_marks: Vec<MarkSpan> = marks
                .into_iter()
                .filter(|span| !dropped(&span.mark))
                .filter_map(|span| {
                    let start = shifted(span.range.start, &shifts);
                    let end = shifted(span.range.end, &shifts);
                    (start < end).then(|| MarkSpan::new(start..end, span.mark))
                })
                .collect();
            block.line.set_text(&next_text);
            block.line.set_marks(next_marks);
            touched.push(index);
        }
        if touched.is_empty() {
            return false;
        }
        let (Some(&first), Some(&last)) = (touched.first(), touched.last()) else {
            return false;
        };
        self.edit_scope
            .widen(EditScope::Blocks(first..last.saturating_add(1)));
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
                ParagraphChange::LineSpacing(multiplier) => {
                    block.line.set_line_spacing(multiplier.unwrap_or(1.0));
                    relayout = true;
                }
                ParagraphChange::SpaceBefore(_)
                | ParagraphChange::SpaceAfter(_)
                | ParagraphChange::Indent(_)
                | ParagraphChange::FirstLineIndent(_)
                | ParagraphChange::Style(_) => relayout = true,
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

    /// Make the selected paragraphs a quote, or a paragraph again when every one of them
    /// already is a quote: the paragraph panel's Quote style. Code and tables stay what they
    /// are. `false` when nothing changed.
    pub fn toggle_quote(&mut self) -> bool {
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
        let off = targets.iter().all(|index| {
            self.blocks
                .get(*index)
                .is_some_and(|block| matches!(block.kind, BlockKind::Quote))
        });
        let kind = if off {
            BlockKind::Paragraph
        } else {
            BlockKind::Quote
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
    /// `numbers`, `heading:<level>`, `spacing:<1|1.15|1.5|2>` or `link:<address>`, then a count the view changes every
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
            "before" => points(argument, ParagraphChange::SpaceBefore)
                .is_some_and(|change| self.set_paragraph(change)),
            "after" => points(argument, ParagraphChange::SpaceAfter)
                .is_some_and(|change| self.set_paragraph(change)),
            "indent" => points(argument, ParagraphChange::Indent)
                .is_some_and(|change| self.set_paragraph(change)),
            "hanging" => points(argument, ParagraphChange::FirstLineIndent)
                .is_some_and(|change| self.set_paragraph(change)),
            "spacing" => line_spacing(argument).is_some_and(|change| self.set_paragraph(change)),
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
                        "quote" => self.toggle_quote(),
                        "list item" | "list" | "bulleted list" => self.toggle_list(false),
                        _ => false,
                    };
                    named || kind
                }
            }
            "changes" => match argument.trim() {
                "accept" => self.resolve_changes(true),
                "reject" => self.resolve_changes(false),
                _ => false,
            },
            "change" => match argument.split_once(':') {
                Some(("accept", id)) if !id.is_empty() => self.resolve_change(id, true),
                Some(("reject", id)) if !id.is_empty() => self.resolve_change(id, false),
                _ => false,
            },
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
        assert!(
            editor
                .active_marks()
                .contains(&Mark::Color([0x09, 0x69, 0xda]))
        );
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

    /// A paragraph set double-spaced stands twice as tall, and single spacing takes the
    /// document's own leading back.
    #[test]
    fn line_spacing_spaces_a_paragraphs_rows() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("first row\nsecond row")]);
        editor.focus_block(0, CursorPlacement::End);
        let single = editor.blocks[0].line.row_height(1.0);
        assert_eq!(editor.blocks[0].style.line_spacing, None, "single to start");

        assert!(editor.format_asked(Some("spacing:2:1".into())));
        assert_eq!(editor.blocks[0].style.line_spacing, Some(2.0));
        let doubled = editor.blocks[0].line.row_height(1.0);
        assert!(
            (doubled - single * 2.0).abs() < 0.5,
            "double spacing stands twice as tall: {single} then {doubled}"
        );
        assert!(
            !editor.format_asked(Some("spacing:2:1".into())),
            "the ask already answered changes nothing"
        );

        assert!(editor.format_asked(Some("spacing:1:2".into())));
        assert_eq!(editor.blocks[0].style.line_spacing, None);
        assert!((editor.blocks[0].line.row_height(1.0) - single).abs() < 0.5);
    }
    /// The Quote and list styles the paragraph panel offers make the paragraph that kind, and
    /// asking for the same style again gives the paragraph back.
    #[test]
    fn quote_and_list_styles_make_the_paragraph_that_kind() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("said this")]);
        editor.focus_block(0, CursorPlacement::End);

        assert!(editor.format_asked(Some("style:Quote:1".into())));
        assert_eq!(editor.blocks[0].kind, BlockKind::Quote);
        assert_eq!(editor.blocks[0].style.style.as_deref(), Some("Quote"));
        assert!(editor.format_asked(Some("style:Quote:2".into())));
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);

        assert!(editor.format_asked(Some("style:List item:3".into())));
        assert!(matches!(editor.blocks[0].kind, BlockKind::Bullet { .. }));
        assert!(editor.format_asked(Some("style:List item:4".into())));
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
    }
    /// A tracked insertion and a tracked deletion are one document's change: accepting keeps
    /// the inserted words and removes the deleted ones, and rejecting does the opposite. The
    /// marks go with the change either way.
    #[test]
    fn tracked_changes_are_taken_or_rejected() {
        let changed = || {
            let mut block = Block::paragraph("keepadded");
            block.line.set_marks(vec![
                MarkSpan::new(0..4, Mark::Deletion("c1".into())),
                MarkSpan::new(4..9, Mark::Insertion("c2".into())),
            ]);
            block
        };

        let mut accepted = BlockEditor::new();
        accepted.set_blocks(vec![changed()]);
        assert!(accepted.format_asked(Some("changes:accept:1".into())));
        assert_eq!(accepted.blocks[0].text(), "added");
        assert!(accepted.blocks[0].line.marks().is_empty());

        let mut rejected = BlockEditor::new();
        rejected.set_blocks(vec![changed()]);
        assert!(rejected.format_asked(Some("changes:reject:1".into())));
        assert_eq!(rejected.blocks[0].text(), "keep");
        assert!(rejected.blocks[0].line.marks().is_empty());
    }
    /// One tracked change is taken without the other: accepting the deletion removes only the
    /// deleted words, and the insertion stands as a change until it too is decided.
    #[test]
    fn one_tracked_change_is_taken_alone() {
        let mut editor = BlockEditor::new();
        let mut block = Block::paragraph("oldnew");
        block.line.set_marks(vec![
            MarkSpan::new(0..3, Mark::Deletion("d1".into())),
            MarkSpan::new(3..6, Mark::Insertion("i1".into())),
        ]);
        editor.set_blocks(vec![block]);

        assert!(editor.format_asked(Some("change:accept:d1:1".into())));
        assert_eq!(editor.blocks[0].text(), "new");
        assert_eq!(
            editor.blocks[0].line.marks(),
            [MarkSpan::new(0..3, Mark::Insertion("i1".into()))]
        );

        assert!(editor.format_asked(Some("change:reject:i1:2".into())));
        assert_eq!(editor.blocks[0].text(), "");
        assert!(editor.blocks[0].line.marks().is_empty());
    }
}
