//! Markdown typed as formatting: `**bold**` becomes bold text the moment the
//! closing delimiter is typed.
//!
//! [`markdown_input`] decides which keystroke completes a span; this module
//! renders the decision on the editor's own model. The delimiters leave the
//! block's text and the content gets the matching [`Mark`], so the host
//! writes two deletions and a mark: the content keeps its identity in the
//! document. A Backspace right after the conversion puts the markdown back
//! as typed, the way it reverts an autocorrection.

use std::ops::Range;

use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{BlockEditor, BlockKind};
use crate::widget::EventCtx;
use crate::widget::markdown_input::{self, InlineKind};
use crate::widget::marks::{self, Mark, MarkSpan};

/// A block's text, marks and caret at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LineState {
    text: String,
    marks: Vec<MarkSpan>,
    caret: usize,
}

/// The span last formatted from its markdown, kept so an immediate
/// Backspace can put the markdown back.
#[derive(Debug, Clone)]
pub(super) struct InlineConversion {
    block_id: String,
    /// The block as typed, closing delimiter included.
    typed: LineState,
    /// The block once formatted.
    formatted: LineState,
}

/// The focused block before an event that may type a closing delimiter.
pub(super) struct TypedMarkdownOrigin {
    index: usize,
    block_id: String,
    text: String,
    /// What the event types.
    typed: String,
    in_code: bool,
}

fn mark_for(kind: InlineKind) -> Mark {
    match kind {
        InlineKind::Bold => Mark::Bold,
        InlineKind::Italic => Mark::Italic,
        InlineKind::Code => Mark::Code,
        InlineKind::Strikethrough => Mark::Strikethrough,
    }
}

fn ends_with_delimiter(text: &str) -> bool {
    text.ends_with(['*', '_', '`', '~'])
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

impl BlockEditor {
    /// The focused block before `event`, when the event may type a closing
    /// delimiter into prose: what [`Self::format_typed_markdown`] compares
    /// the result with.
    pub(super) fn typed_markdown_origin(&self, event: &WindowEvent) -> Option<TypedMarkdownOrigin> {
        let typed = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        text: Some(text),
                        ..
                    },
                ..
            } => text.as_str(),
            WindowEvent::Ime(Ime::Commit(text)) => text.as_str(),
            _ => return None,
        };
        if !ends_with_delimiter(typed) {
            return None;
        }
        let index = self.focused?;
        let block = self.blocks.get(index)?;
        if !block.kind.is_editable()
            || matches!(block.kind, BlockKind::CodeBlock { .. })
            || !block.line.inline_formatting()
        {
            return None;
        }
        Some(TypedMarkdownOrigin {
            index,
            block_id: block.id.clone(),
            text: block.text().to_owned(),
            typed: typed.to_owned(),
            in_code: block.line.caret_in_code(),
        })
    }

    /// Format the span the event that started at `origin` completed: the
    /// delimiters leave the text and the content is marked.
    pub(super) fn format_typed_markdown(&mut self, origin: &TypedMarkdownOrigin) {
        if origin.in_code {
            return;
        }
        let Some(block) = self
            .blocks
            .get_mut(origin.index)
            .filter(|block| block.id == origin.block_id)
        else {
            return;
        };
        let line = &mut block.line;
        let now = line.value();
        let caret = line.cursor_pos();
        // The event typed the delimiter right before the caret (the word
        // before it may also have been autocorrected).
        let Some(head) = now
            .get(..caret)
            .filter(|head| now != origin.text && head.ends_with(origin.typed.as_str()))
        else {
            return;
        };
        if line.selection_range().is_some() {
            return;
        }
        let Some(found) = markdown_input::detect_inline(head) else {
            return;
        };
        // A delimiter inside inline code is code.
        if line.marks().iter().any(|span| {
            matches!(span.mark, Mark::Code)
                && (overlaps(&span.range, &found.open) || overlaps(&span.range, &found.close))
        }) {
            return;
        }
        let (Some(head), Some(content), Some(tail)) = (
            now.get(..found.open.start),
            now.get(found.content.clone()),
            now.get(found.close.end..),
        ) else {
            return;
        };
        let typed = LineState {
            text: now.to_owned(),
            marks: line.marks().to_vec(),
            caret,
        };
        let mut spans = typed.marks.clone();
        marks::shift_after_edit(&mut spans, found.close.start, 0, found.close.len());
        marks::shift_after_edit(&mut spans, found.open.start, 0, found.open.len());
        let start = found.open.start;
        marks::add_mark_to_range(
            &mut spans,
            start..start.saturating_add(content.len()),
            mark_for(found.kind),
        );
        let formatted = LineState {
            text: format!("{head}{content}{tail}"),
            marks: spans,
            caret: caret
                .saturating_sub(found.open.len())
                .saturating_sub(found.close.len()),
        };
        show(line, &formatted);
        // The typed closing delimiter ended the span: what is typed next is
        // plain, though the document would carry the mark on over it.
        line.close_marks_at(formatted.caret, vec![mark_for(found.kind)]);
        self.inline_conversion = Some(InlineConversion {
            block_id: origin.block_id.clone(),
            typed,
            formatted,
        });
        self.edit_pending = true;
        self.dirty = true;
    }

    /// Backspace right after a conversion puts the markdown back as typed.
    /// Any other input ends the chance to.
    pub(super) fn revert_inline_conversion(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        let backspace = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        ..
                    },
                ..
            } => {
                if matches!(
                    logical_key,
                    Key::Named(
                        NamedKey::Shift
                            | NamedKey::Control
                            | NamedKey::Alt
                            | NamedKey::AltGraph
                            | NamedKey::Super
                            | NamedKey::Meta
                    )
                ) {
                    return false;
                }
                *logical_key == Key::Named(NamedKey::Backspace)
            }
            WindowEvent::Ime(Ime::Commit(_)) | WindowEvent::MouseInput { .. } => false,
            _ => return false,
        };
        let Some(conversion) = self.inline_conversion.take() else {
            return false;
        };
        let modifiers = ctx.modifiers.state();
        if !backspace || modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() {
            return false;
        }
        let Some(block) = self
            .focused
            .and_then(|index| self.blocks.get_mut(index))
            .filter(|block| block.id == conversion.block_id)
        else {
            return false;
        };
        let line = &mut block.line;
        let shown = LineState {
            text: line.value().to_owned(),
            marks: line.marks().to_vec(),
            caret: line.cursor_pos(),
        };
        if shown != conversion.formatted || line.selection_range().is_some() {
            return false;
        }
        show(line, &conversion.typed);
        self.edit_pending = true;
        self.dirty = true;
        *ctx.handled = true;
        true
    }
}

fn show(line: &mut crate::widget::RichTextLine, state: &LineState) {
    line.set_text(&state.text);
    line.set_marks(state.marks.clone());
    line.set_cursor_pos(state.caret);
}

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT};
    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    use super::super::{Block, BlockEditor, CursorPlacement};
    use crate::testing::Dispatch;
    use crate::widget::Widget;
    use crate::widget::marks::{Mark, MarkSpan};

    fn press(editor: &mut BlockEditor, doc: &mut AutoCommit, key: Key) {
        let text = match &key {
            Key::Character(text) => Some(text.clone()),
            Key::Named(NamedKey::Space) => Some(" ".into()),
            _ => None,
        };
        let event = crate::synthetic_input::key_event(key, text, ElementState::Pressed);
        Dispatch::new()
            .doc(doc)
            .send(editor, &crate::synthetic_input::keyboard_input(event));
        editor.sync(doc, &ROOT);
    }

    /// Type `text` key by key into a fresh editor holding `block`, the way a
    /// physical keyboard delivers it (Space is its own named key).
    fn typed_into(block: Block, text: &str) -> (BlockEditor, AutoCommit) {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![block]);
        editor.focus_block(0, CursorPlacement::End);
        type_more(&mut editor, &mut doc, text);
        (editor, doc)
    }

    fn type_more(editor: &mut BlockEditor, doc: &mut AutoCommit, text: &str) {
        for character in text.chars() {
            let key = if character == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(character.to_string().into())
            };
            press(editor, doc, key);
        }
    }

    fn shown(editor: &BlockEditor) -> (String, Vec<MarkSpan>) {
        let block = &editor.blocks()[0];
        (block.text().to_owned(), block.line.marks().to_vec())
    }

    #[test]
    fn typed_markdown_becomes_formatting_when_it_closes() {
        for (typed, text, marks) in [
            ("*hi*", "hi", vec![MarkSpan::new(0..2, Mark::Italic)]),
            ("_hi_", "hi", vec![MarkSpan::new(0..2, Mark::Italic)]),
            ("**b**", "b", vec![MarkSpan::new(0..1, Mark::Bold)]),
            ("__b__", "b", vec![MarkSpan::new(0..1, Mark::Bold)]),
            ("`c`", "c", vec![MarkSpan::new(0..1, Mark::Code)]),
            ("~~s~~", "s", vec![MarkSpan::new(0..1, Mark::Strikethrough)]),
            ("a **b** c", "a b c", vec![MarkSpan::new(2..3, Mark::Bold)]),
            (
                "*one* and *two*",
                "one and two",
                vec![
                    MarkSpan::new(0..3, Mark::Italic),
                    MarkSpan::new(8..11, Mark::Italic),
                ],
            ),
            ("snake_case_name", "snake_case_name", vec![]),
            ("2 * 3 * 4", "2 * 3 * 4", vec![]),
            ("**not closed*", "**not closed*", vec![]),
        ] {
            let (editor, _) = typed_into(Block::paragraph(""), typed);
            assert_eq!(shown(&editor), (text.to_owned(), marks), "{typed:?}");
            assert_eq!(
                editor.blocks()[0].line.cursor_pos(),
                text.len(),
                "{typed:?}: the caret stays after what was typed"
            );
        }
    }

    #[test]
    fn markdown_formats_in_every_prose_block_and_never_in_code() {
        let (heading, _) = typed_into(Block::heading(2, "A "), "*taste*");
        assert_eq!(
            shown(&heading),
            (
                "A taste".to_owned(),
                vec![MarkSpan::new(2..7, Mark::Italic)]
            )
        );

        let (code, _) = typed_into(Block::code_block(None, ""), "**x** `y`");
        assert_eq!(shown(&code), ("**x** `y`".to_owned(), vec![]));

        // Inside inline code a delimiter is code too.
        let mut inline = Block::paragraph("a*b");
        inline.line.set_marks(vec![MarkSpan::new(0..3, Mark::Code)]);
        let (inline, _) = typed_into(inline, "*");
        assert_eq!(
            shown(&inline),
            ("a*b*".to_owned(), vec![MarkSpan::new(0..4, Mark::Code)])
        );
    }

    #[test]
    fn only_a_typed_closing_delimiter_ends_a_mark_for_what_follows() {
        // Typing on right after bold text goes on in bold, as the document
        // carries it.
        let mut bold = Block::paragraph("ab");
        bold.line.set_marks(vec![MarkSpan::new(0..2, Mark::Bold)]);
        let (bold, _) = typed_into(bold, "c");
        assert_eq!(
            shown(&bold),
            ("abc".to_owned(), vec![MarkSpan::new(0..3, Mark::Bold)])
        );
        // After a typed closing delimiter it is plain.
        let (mut editor, mut doc) = typed_into(Block::paragraph(""), "**b**");
        type_more(&mut editor, &mut doc, " c");
        assert_eq!(
            shown(&editor),
            ("b c".to_owned(), vec![MarkSpan::new(0..1, Mark::Bold)])
        );
    }

    #[test]
    fn backspace_right_after_formatting_puts_the_markdown_back() {
        let (mut editor, mut doc) = typed_into(Block::paragraph("a "), "**b**");
        press(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        assert_eq!(shown(&editor), ("a **b**".to_owned(), vec![]));
        assert_eq!(editor.blocks()[0].line.cursor_pos(), "a **b**".len());
        // The markdown stays as typed: the next Backspace deletes a letter.
        press(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        assert_eq!(shown(&editor), ("a **b*".to_owned(), vec![]));

        // Once anything else is typed, Backspace deletes as usual.
        let (mut editor, mut doc) = typed_into(Block::paragraph(""), "*i*");
        type_more(&mut editor, &mut doc, "x");
        press(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        assert_eq!(
            shown(&editor),
            ("i".to_owned(), vec![MarkSpan::new(0..1, Mark::Italic)])
        );
    }

    #[test]
    fn contractions_are_corrected_at_the_word_boundary_except_in_code() {
        let (editor, _) = typed_into(Block::paragraph(""), "it doesnt work");
        assert_eq!(shown(&editor).0, "it doesn't work");

        let (editor, _) = typed_into(Block::paragraph(""), "I cant ");
        assert_eq!(shown(&editor).0, "I cant ", "a word of its own stays");

        let (editor, _) = typed_into(Block::code_block(None, ""), "it doesnt work");
        assert_eq!(
            shown(&editor).0,
            "it doesnt work",
            "code is never corrected"
        );

        let (editor, _) = typed_into(Block::paragraph(""), "`doesnt` ok");
        assert_eq!(
            shown(&editor),
            (
                "doesnt ok".to_owned(),
                vec![MarkSpan::new(0..6, Mark::Code)]
            ),
            "inline code is never corrected"
        );

        let (mut editor, mut doc) = typed_into(Block::paragraph(""), "it doesnt ");
        assert_eq!(shown(&editor).0, "it doesn't ");
        press(&mut editor, &mut doc, Key::Named(NamedKey::Backspace));
        assert_eq!(
            shown(&editor).0,
            "it doesnt",
            "Backspace right after a correction reverts it"
        );
    }
}
