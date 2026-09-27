//! A message body as rich text, and the plain text and mentions read from
//! it.
//!
//! A body is paragraphs ([`BlockContent`]) written in the SDK editor: text
//! with marks, a mention being a marked run naming a participant. Its plain
//! text (paragraphs joined by a line break) is what search, notifications,
//! agents and edit history read, and the byte offsets of its mentions are
//! offsets into that text. A message written before bodies were rich has
//! only its plain text and resolved mentions; [`legacy`] shows it as one
//! paragraph with the same mentions marked.

use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};

use crate::mention::{Mention, Participant};

/// The plain text of `blocks`: one line per paragraph.
pub fn plain_text(blocks: &[BlockContent]) -> String {
    blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A body written before bodies were rich: its text as one paragraph (line
/// breaks inside it), each mention marked where the message said it was.
pub fn legacy(id: &str, text: &str, mentions: &[Mention]) -> Vec<BlockContent> {
    let marks = mentions
        .iter()
        .filter(|mention| {
            mention.len > 0
                && mention.end() <= text.len()
                && text.is_char_boundary(mention.start)
                && text.is_char_boundary(mention.end())
        })
        .map(|mention| {
            MarkSpan::new(
                mention.start..mention.end(),
                Mark::Mention(mention.user.clone()),
            )
        })
        .collect();
    vec![BlockContent {
        marks,
        ..BlockContent::paragraph(format!("{id}/body"), text)
    }]
}

/// Who `blocks` mention: every mention mark, at its byte offset in
/// [`plain_text`], and every `@name` typed by hand that no mark covers,
/// resolved against `participants` as a hand-typed mention always was.
pub fn mentions(blocks: &[BlockContent], participants: &[Participant]) -> Vec<Mention> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    for block in blocks {
        for span in &block.marks {
            if let Mark::Mention(user) = &span.mark {
                found.push(Mention {
                    user: user.clone(),
                    start: offset.saturating_add(span.range.start),
                    len: span.range.len(),
                    is_agent: participants
                        .iter()
                        .any(|participant| participant.id == *user && participant.is_agent),
                });
            }
        }
        offset = offset.saturating_add(block.text.len()).saturating_add(1);
    }
    let text = plain_text(blocks);
    for typed in crate::mention::resolve(&text, participants) {
        let overlaps = found
            .iter()
            .any(|marked| typed.start < marked.end() && marked.start < typed.end());
        if !overlaps {
            found.push(typed);
        }
    }
    found.sort_by_key(|mention| mention.start);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ada() -> Participant {
        Participant {
            id: "u-ada".into(),
            name: "Ada".into(),
            is_agent: false,
        }
    }

    #[test]
    fn a_body_reads_back_as_plain_text_and_its_mentions() {
        let mut first = BlockContent::paragraph("a", "hi @Ada");
        first.marks = vec![MarkSpan::new(3..7, Mark::Mention("u-ada".into()))];
        let second = BlockContent::paragraph("b", "and @Ada again");
        let blocks = [first, second];
        assert_eq!(plain_text(&blocks), "hi @Ada\nand @Ada again");
        let found = mentions(&blocks, &[ada()]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!((found[0].start, found[0].len), (3, 4));
        assert_eq!(found[0].slice(&plain_text(&blocks)), "@Ada");
        assert_eq!(
            found[1].slice(&plain_text(&blocks)),
            "@Ada",
            "typed by hand"
        );
    }

    #[test]
    fn an_old_message_shows_its_text_with_its_mentions_marked() {
        let text = "ping @Ada\nsecond line";
        let mentions = [Mention {
            user: "u-ada".into(),
            start: 5,
            len: 4,
            is_agent: false,
        }];
        let blocks = legacy("m1", text, &mentions);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, text);
        assert_eq!(
            blocks[0].marks,
            [MarkSpan::new(5..9, Mark::Mention("u-ada".into()))]
        );
        let stale = [Mention {
            start: 90,
            ..mentions[0].clone()
        }];
        assert!(
            legacy("m1", text, &stale)[0].marks.is_empty(),
            "a stale offset is skipped"
        );
    }
}
