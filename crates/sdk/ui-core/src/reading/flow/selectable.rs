//! A block's selectable text, read from the document alone.

use crate::reading::{Block, Span};

/// Builds canonical text the way the flow does: words joined by single
/// spaces, with the char offsets of hard line breaks.
#[derive(Default)]
struct Canonical {
    text: String,
    chars: usize,
    hard_breaks: Vec<usize>,
}

impl Canonical {
    fn push(&mut self, word: &str, space_before: bool, line_break: bool) {
        if !self.text.is_empty() && space_before {
            if line_break {
                self.hard_breaks.push(self.chars);
            }
            self.text.push(' ');
            self.chars += 1;
        }
        self.text.push_str(word);
        self.chars += word.chars().count();
    }

    fn push_spans(&mut self, spans: &[Span]) {
        for span in spans {
            if span.math.is_some() {
                self.push(&span.text, span.space_before, span.line_break);
                continue;
            }
            for (wi, word) in span.text.split_whitespace().enumerate() {
                let space_before = if wi == 0 { span.space_before } else { true };
                self.push(word, space_before, wi == 0 && span.line_break);
            }
        }
    }
}

/// A block's canonical selectable text and its hard-break offsets, as its
/// [`super::LaidBlock::text`] and [`super::LaidBlock::hard_breaks`] hold
/// them, read from the document alone: selections are in these char offsets
/// whether or not the block has been laid out yet.
#[must_use]
pub fn selectable_text(block: &Block) -> (String, Vec<usize>) {
    match block {
        Block::Math { source, .. } => (source.clone(), Vec::new()),
        Block::Separator => (String::new(), Vec::new()),
        Block::Code { text, .. } => {
            let mut out = Canonical::default();
            for (number, line) in text.split('\n').enumerate() {
                if number > 0 {
                    out.hard_breaks.push(out.chars);
                    out.text.push(' ');
                    out.chars += 1;
                }
                out.text.push_str(line);
                out.chars += line.chars().count();
            }
            (out.text, out.hard_breaks)
        }
        Block::Table { rows } => {
            // Cells are joined by a space; a cell's own breaks are not kept.
            let mut text = String::new();
            for cell in rows.iter().flat_map(|row| row.cells.iter()) {
                let mut words = Canonical::default();
                words.push_spans(&cell.spans);
                if words.text.is_empty() {
                    continue;
                }
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&words.text);
            }
            (text, Vec::new())
        }
        _ => {
            let mut out = Canonical::default();
            out.push_spans(block.spans());
            (out.text, out.hard_breaks)
        }
    }
}
