//! A turn as it reads on the canvas (canvas.md §7, "An answer reads like an answer"): its reply
//! set as text, what the agent is doing now, and its steps folded into one quiet line.

use super::super::tests::{keyed, passes, rich_texts, texts, view_cx};
use super::super::{Channel, Message, Person, Work};
use super::*;
use tilo_view::Value;

/// The marks over a tree's rich text, by name and the words they cover.
fn rich_marks(node: &Node) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for value in node.props.values() {
        if let Value::Rich(rich) = value {
            for block in &rich.blocks {
                for mark in &block.marks {
                    let words: String = block
                        .text
                        .chars()
                        .skip(mark.start)
                        .take(mark.end.saturating_sub(mark.start))
                        .collect();
                    out.push((mark.name.clone(), words));
                }
            }
        }
    }
    for child in &node.children {
        out.extend(rich_marks(child));
    }
    out
}

/// A turn of one question and `reply`, as Chat reads it.
fn turn_answered(reply: Message) -> Turn {
    let channel = Channel {
        id: "c1".to_owned(),
        people: vec![
            Person {
                id: "person:pat".to_owned(),
                name: "Pat".to_owned(),
                agent: false,
            },
            Person {
                id: "agent:assistant".to_owned(),
                name: "Assistant".to_owned(),
                agent: true,
            },
        ],
        messages: vec![
            Message {
                id: "q1".to_owned(),
                author: "person:pat".to_owned(),
                text: "which of my tasks are due this week?".to_owned(),
                ..Message::default()
            },
            reply,
        ],
        ..Channel::default()
    };
    Turn::of(&channel, "q1").expect("the turn")
}

/// An answer reads like an answer (canvas.md §7): its Markdown is set as text, so the person
/// reads bold words, never the asterisks around them, and a list's items as a list.
#[test]
fn a_turns_reply_reads_its_markdown_as_text() {
    let turn = turn_answered(Message {
        id: "r1".to_owned(),
        author: "agent:assistant".to_owned(),
        text: "Two are due:\n\n- **Send the invoice** by Friday\n- Call *Ada*".to_owned(),
        work: Some(Work {
            state: "done".to_owned(),
            ..Work::default()
        }),
        ..Message::default()
    });
    let channel = Channel::default();
    let tree = super::turn(&turn, &view_cx(&channel));
    passes(&tree);
    let read = rich_texts(&tree);
    assert_eq!(
        read,
        ["Two are due:", "Send the invoice by Friday", "Call Ada"],
        "the reply is its words, without the Markdown's marks: {read:?}"
    );
    let marks = rich_marks(&tree);
    assert!(
        marks.contains(&("strong".to_owned(), "Send the invoice".to_owned()))
            && marks.contains(&("em".to_owned(), "Ada".to_owned())),
        "what the Markdown made bold or italic is: {marks:?}"
    );
    assert!(
        !texts(&tree).iter().any(|text| text.contains("**")),
        "no asterisk is shown as written"
    );
}

/// While the agent works, the turn says what it is doing now, from the step it took last,
/// and never a count of steps or "Writing…".
#[test]
fn a_working_turn_says_what_it_is_doing_now() {
    let turn = turn_answered(Message {
        id: "r1".to_owned(),
        author: "agent:assistant".to_owned(),
        work: Some(Work {
            state: "doing".to_owned(),
            steps: vec!["Open Tasks".to_owned(), "Read your tasks".to_owned()],
            ..Work::default()
        }),
        ..Message::default()
    });
    let channel = Channel::default();
    let tree = super::turn(&turn, &view_cx(&channel));
    passes(&tree);
    let said = texts(&tree);
    assert!(
        said.iter().any(|text| text == "Read your tasks\u{2026}"),
        "the live line is the step under way: {said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|text| text.contains("steps") || text.contains("Writing")),
        "no count of steps and no \"Writing\" while it works: {said:?}"
    );
    // Asked and not taken yet: it is thinking.
    let waiting = turn_answered(Message {
        id: "r1".to_owned(),
        author: "agent:assistant".to_owned(),
        work: Some(Work {
            state: "taken".to_owned(),
            ..Work::default()
        }),
        ..Message::default()
    });
    assert!(
        texts(&super::turn(&waiting, &view_cx(&channel)))
            .iter()
            .any(|text| text == "Thinking\u{2026}")
    );
}

/// Once done, what the agent did is one quiet line that unfolds to every step.
#[test]
fn a_done_turns_steps_are_one_line_that_unfolds() {
    assert_eq!(steps_line(&["Open Tasks".to_owned()]), "Open Tasks");
    assert_eq!(
        steps_line(&[
            "Open Tasks".to_owned(),
            "Find tasks".to_owned(),
            "Read a task".to_owned()
        ]),
        "Open Tasks and 2 more steps"
    );
    let turn = turn_answered(Message {
        id: "r1".to_owned(),
        author: "agent:assistant".to_owned(),
        text: "Done.".to_owned(),
        work: Some(Work {
            state: "done".to_owned(),
            steps: vec!["Open Tasks".to_owned(), "Find tasks".to_owned()],
            ..Work::default()
        }),
        ..Message::default()
    });
    let channel = Channel::default();
    let tree = super::turn(&turn, &view_cx(&channel));
    let folded = keyed(&tree, "ui.DisclosureRow", "@steps");
    assert_eq!(folded.len(), 1, "one line for what the agent did");
    assert_eq!(
        texts(folded[0]),
        ["Open Tasks and 1 more step", "Open Tasks", "Find tasks"],
        "the line, and every step under it when it unfolds"
    );
}

/// A long answer never grows past what the canvas shows (canvas.md §7, "Everything stays in
/// view"): the card's words stand in a scroll, so its place hugs them up to the visible area and
/// they wrap and scroll there.
#[test]
fn a_turns_words_scroll_inside_its_card() {
    let turn = turn_answered(Message {
        id: "r1".to_owned(),
        author: "agent:assistant".to_owned(),
        text: "Nine tasks are due this week. ".repeat(40),
        work: Some(Work {
            state: "done".to_owned(),
            ..Work::default()
        }),
        ..Message::default()
    });
    let channel = Channel::default();
    let tree = super::turn(&turn, &view_cx(&channel));
    passes(&tree);
    assert_eq!(tree.kind, "ui.Scroll", "the card scrolls what it holds");
    let card = tree.children.first().expect("the card in the scroll");
    assert_eq!(
        card.props.get("padding"),
        Some(&Value::text("section")),
        "the card keeps its padding inside the scroll"
    );
}
