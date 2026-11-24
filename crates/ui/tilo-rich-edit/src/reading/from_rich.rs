//! A document's rich text as the blocks a reading page is set from: its paragraphs, headings,
//! quotes, list items and section breaks, their inline styling kept to what a page sets — bold,
//! italic and code.
//!
//! Marks are counted in characters of the paragraph's text ([`tilo_ui_graph::rich`]), so a run
//! here is a stretch of that text no mark begins or ends inside.

use std::collections::BTreeSet;

use tilo_ui_graph::rich::{Attr, Block as WireBlock, RichText};

use super::model::{Block, Span};

/// The blocks `rich` holds, in order, as a page sets them.
#[must_use]
pub fn blocks_of(rich: &RichText) -> Vec<Block> {
    let mut blocks = Vec::with_capacity(rich.blocks.len());
    let mut counters: Vec<usize> = Vec::new();
    for wire in &rich.blocks {
        let block = block_of(wire, &mut counters);
        blocks.push(block);
    }
    blocks
}

fn int_attr(block: &WireBlock, key: &str) -> Option<i64> {
    match block.attrs.get(key) {
        Some(Attr::Int(value)) => Some(*value),
        _ => None,
    }
}

fn bool_attr(block: &WireBlock, key: &str) -> Option<bool> {
    match block.attrs.get(key) {
        Some(Attr::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn block_of(wire: &WireBlock, counters: &mut Vec<usize>) -> Block {
    let kind = wire.kind.as_str();
    if kind != "list_item" {
        counters.clear();
    }
    match kind {
        "heading" => {
            let level = u8::try_from(int_attr(wire, "level").unwrap_or(1).clamp(1, 3)).unwrap_or(1);
            Block::Heading {
                level,
                spans: spans_of(wire),
            }
        }
        "quote" | "blockquote" => Block::Quote {
            spans: spans_of(wire),
        },
        "divider" => Block::Separator,
        "list_item" => {
            let ordered = bool_attr(wire, "ordered") == Some(true);
            let level =
                usize::try_from(int_attr(wire, "level").unwrap_or(0).clamp(0, 5)).unwrap_or(0);
            counters.resize(level + 1, 0);
            let index = if ordered {
                if let Some(count) = counters.get_mut(level) {
                    *count = count.saturating_add(1);
                    *count
                } else {
                    1
                }
            } else {
                0
            };
            // An item at a deeper level starts that level's count; a shallower one drops the
            // nested counts under it.
            counters.truncate(level + 1);
            Block::ListItem {
                spans: spans_of(wire),
                ordered,
                index,
            }
        }
        // A table is kept as its Markdown source in a paragraph: a page sets that source as the
        // paragraph it is, so a reader meets the table where it stands rather than not at all.
        _ => Block::Paragraph {
            spans: spans_of(wire),
        },
    }
}

/// The spans of one paragraph: its text cut where a mark begins or ends, each run carrying the
/// bold, italic and code that cover all of it.
fn spans_of(wire: &WireBlock) -> Vec<Span> {
    let text = wire.text.as_str();
    let chars = text.chars().count();
    let mut cuts: BTreeSet<usize> = BTreeSet::from([0, chars]);
    for mark in &wire.marks {
        if matches!(mark.value, Attr::Bool(false)) {
            continue;
        }
        if mark.start < mark.end && mark.end <= chars {
            cuts.insert(mark.start);
            cuts.insert(mark.end);
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut previous = 0usize;
    let mut pending_space = false;
    for cut in cuts {
        if cut <= previous {
            continue;
        }
        let run: String = text.chars().skip(previous).take(cut - previous).collect();
        let look = RunLook {
            bold: covered(wire, previous, cut, "strong"),
            italic: covered(wire, previous, cut, "em"),
            code: wire.marks.iter().any(|mark| {
                mark.start <= previous
                    && mark.end >= cut
                    && mark.name == "fontFamily"
                    && matches!(&mark.value, Attr::Text(family) if family == "mono")
            }),
        };
        pending_space = push_run(&mut spans, &run, look, pending_space);
        previous = cut;
    }
    spans
}

/// Whether the mark `name` is set over the whole of `start..end`.
fn covered(wire: &WireBlock, start: usize, end: usize, name: &str) -> bool {
    wire.marks.iter().any(|mark| {
        mark.name == name
            && mark.start <= start
            && mark.end >= end
            && !matches!(mark.value, Attr::Bool(false))
    })
}

/// How a run of a paragraph's text is set: the marks that cover all of it.
#[derive(Clone, Copy)]
struct RunLook {
    bold: bool,
    italic: bool,
    code: bool,
}

/// Push `run` as one or more spans: its lines break where the paragraph's text does (a `<br/>`
/// or a verse line), and the whitespace around it says whether a word break came before it.
/// Answers whether the run ended on a word break, which the span after it takes.
fn push_run(spans: &mut Vec<Span>, run: &str, look: RunLook, pending_before: bool) -> bool {
    let mut pending = pending_before;
    let parts: Vec<&str> = run.split('\n').collect();
    for (index, part) in parts.iter().enumerate() {
        let trimmed = part.trim();
        let line_break = index > 0;
        if trimmed.is_empty() {
            if !part.is_empty() {
                pending = true;
            }
            continue;
        }
        let opens_with_space = part.chars().next().is_some_and(char::is_whitespace);
        let space_before = !spans.is_empty() && (pending || line_break || opens_with_space);
        spans.push(Span {
            text: trimmed.to_owned(),
            math: None,
            bold: look.bold,
            italic: look.italic,
            code: look.code,
            space_before,
            line_break,
            line_indent: 0,
        });
        pending = part.ends_with(char::is_whitespace);
    }
    pending
}

#[cfg(test)]
mod tests {
    use tilo_ui_graph::rich::{Mark, RichText};

    use super::*;
    use crate::reading::model::Block;

    fn wire(kind: &str, text: &str) -> WireBlock {
        WireBlock {
            kind: kind.to_owned(),
            text: text.to_owned(),
            ..WireBlock::default()
        }
    }

    fn mark(start: usize, end: usize, name: &str) -> Mark {
        Mark {
            start,
            end,
            name: name.to_owned(),
            value: Attr::Bool(true),
        }
    }

    #[test]
    fn a_paragraph_keeps_what_a_page_sets_of_its_marks() {
        let mut block = wire("paragraph", "a bold and italic word");
        block.marks = vec![mark(2, 6, "strong"), mark(11, 17, "em")];
        let rich = RichText {
            blocks: vec![block],
            embeds: Vec::new(),
        };
        let blocks = blocks_of(&rich);
        let Block::Paragraph { spans } = &blocks[0] else {
            panic!("a paragraph reads as one");
        };
        let words: Vec<(&str, bool, bool)> = spans
            .iter()
            .map(|span| (span.text.as_str(), span.bold, span.italic))
            .collect();
        assert_eq!(
            words,
            vec![
                ("a", false, false),
                ("bold", true, false),
                ("and", false, false),
                ("italic", false, true),
                ("word", false, false),
            ]
        );
        assert_eq!(blocks[0].plain_text(), "a bold and italic word");
    }

    #[test]
    fn headings_are_their_level_and_dividers_break_a_section() {
        let mut heading = wire("heading", "One");
        heading.attrs.insert("level".to_owned(), Attr::Int(4));
        let rich = RichText {
            blocks: vec![heading, wire("divider", "")],
            embeds: Vec::new(),
        };
        let blocks = blocks_of(&rich);
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 3,
                spans: vec![Span::plain("One")]
            }
        );
        assert_eq!(blocks[1], Block::Separator);
    }

    #[test]
    fn ordered_items_count_within_their_level() {
        let mut one = wire("list_item", "one");
        one.attrs.insert("ordered".to_owned(), Attr::Bool(true));
        let two = one.clone();
        let mut nested = wire("list_item", "nested");
        nested.attrs.insert("ordered".to_owned(), Attr::Bool(true));
        nested.attrs.insert("level".to_owned(), Attr::Int(1));
        let rich = RichText {
            blocks: vec![one, two, nested],
            embeds: Vec::new(),
        };
        let blocks = blocks_of(&rich);
        let index = |block: &Block| match block {
            Block::ListItem { ordered, index, .. } => (*ordered, *index),
            _ => panic!("a list item reads as one"),
        };
        assert_eq!(index(&blocks[0]), (true, 1));
        assert_eq!(index(&blocks[1]), (true, 2));
        assert_eq!(index(&blocks[2]), (true, 1), "a nested item counts its own");
    }

    #[test]
    fn a_line_break_in_the_text_starts_a_new_line() {
        let rich = RichText {
            blocks: vec![wire("paragraph", "one\ntwo")],
            embeds: Vec::new(),
        };
        let Block::Paragraph { spans } = &blocks_of(&rich)[0] else {
            panic!("a paragraph");
        };
        assert_eq!(spans.len(), 2);
        assert!(spans[1].line_break);
        assert!(spans[1].space_before);
    }
}
