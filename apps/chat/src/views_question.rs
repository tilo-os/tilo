//! The question a message asks, on the conversation's page: the card
//! [`crate::question_card`] words, built from the described components. An
//! answer is Chat's own `answer-question`, pressed by the page's reader; a
//! choice's answer is sent as their message, which is what its asker reads
//! next.

use tilo_actions::policy::MemberId;
use tilo_view::node;
use tilo_view::ui::{self, Gap};
use tilo_view::{Arg, Node, ViewCx};

use super::{Channel, Message, send};
use crate::question::Question;
use crate::question_card::{Line, QuestionShown, Reader, shown};

/// What `question` shows `me`, reading a conversation whose messages after
/// it are `later`.
pub(super) fn shown_to(
    question: &Question,
    me: &MemberId,
    later: &[crate::model::Message],
) -> Option<QuestionShown> {
    shown(
        question,
        &Reader {
            mine: question.asks(me),
            name: None,
            open: !later.iter().any(|said| question.asks(&said.author)),
        },
    )
}

/// The card under `message`'s words: its lines, then its answers as buttons.
pub(super) fn card(c: &Channel, message: &Message, shown: &QuestionShown, cx: &ViewCx) -> Node {
    let mut column: Vec<Node> = shown
        .lines
        .iter()
        .enumerate()
        .map(|(index, (said, line))| {
            let text = ui::TextBuilder::new(
                &cx.key(format!("question-{}-{index}", message.id)),
                said.as_str(),
            );
            match line {
                Line::Title => text.role(ui::TextRole::Label).into(),
                Line::Field | Line::Status => {
                    text.role(ui::TextRole::Caption).secondary(true).into()
                }
            }
        })
        .collect();
    if !shown.answers.is_empty() {
        let answers = shown
            .answers
            .iter()
            .map(|offered| {
                let button: Node = ui::ButtonBuilder::new(
                    &cx.key(format!("answer-{}-{}", message.id, offered.id)),
                    offered.label.as_str(),
                )
                .role(if offered.primary {
                    ui::ButtonRole::Primary
                } else {
                    ui::ButtonRole::Secondary
                })
                .into();
                if shown.posts {
                    send(cx, button, "press", c, Arg::text(offered.label.clone()))
                } else {
                    let fired = cx.action_with(
                        &button,
                        "press",
                        "answer-question",
                        vec![Arg::text(message.id.clone()), Arg::text(offered.id.clone())],
                    );
                    node::on(button, "press", fired)
                }
            })
            .collect();
        column.push(ui::row(
            &cx.key(format!("answers-{}", message.id)),
            Gap::Tight,
            answers,
        ));
    }
    ui::stack(&cx.key(format!("asked-{}", message.id)), Gap::Tight, column)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{keyed, passes, view_cx};
    use super::*;
    use crate::question_card::Offered;

    fn asking(shown: QuestionShown) -> Channel {
        let mut message = Message::by(MemberId::agent("tide"));
        message.id = "m_asked".to_owned();
        message.text = "Send this answer?".to_owned();
        message.question = Some(shown);
        Channel {
            id: "c_crit".to_owned(),
            name: "crit".to_owned(),
            messages: vec![message],
            ..Channel::default()
        }
    }

    fn offered(id: &str, label: &str, primary: bool) -> Offered {
        Offered {
            id: id.to_owned(),
            label: label.to_owned(),
            primary,
        }
    }

    #[test]
    fn the_page_draws_a_question_with_the_shared_button_and_chats_own_verb() {
        let channel = asking(QuestionShown {
            lines: vec![
                ("Send message".to_owned(), Line::Title),
                ("To: jonas@northzone.example".to_owned(), Line::Field),
            ],
            answers: vec![
                offered("allow", "Allow once", true),
                offered("deny", "Don’t allow", false),
            ],
            card: true,
            posts: false,
        });
        let cx = view_cx(&channel);
        let tree = super::super::page(&channel, &cx);
        passes(&tree);
        let buttons = keyed(&tree, "ui.Button", "@answer-m_asked-");
        assert_eq!(buttons.len(), 2, "each answer is a button");
        assert_eq!(
            cx.effect(&buttons[0].key, "press"),
            Some(tilo_view::Effect::Action {
                action: "answer-question".to_owned(),
                args: vec![Arg::text("m_asked"), Arg::text("allow")],
            }),
            "pressing it is the conversation's own verb, naming the message and the answer"
        );
        assert_eq!(keyed(&tree, "ui.Text", "@question-m_asked-").len(), 2);
    }

    #[test]
    fn a_choice_on_the_page_sends_the_answer_as_the_readers_message() {
        let channel = asking(QuestionShown {
            answers: vec![offered("today", "Today", false)],
            posts: true,
            ..QuestionShown::default()
        });
        let cx = view_cx(&channel);
        let tree = super::super::page(&channel, &cx);
        passes(&tree);
        let buttons = keyed(&tree, "ui.Button", "@answer-m_asked-today");
        assert_eq!(buttons.len(), 1);
        assert!(
            matches!(
                cx.effect(&buttons[0].key, "press"),
                Some(tilo_view::Effect::Action { action, args })
                    if action == "send" && args.last() == Some(&Arg::text("Today"))
            ),
            "it is sent as Chat sends what the composer holds"
        );
    }
}
