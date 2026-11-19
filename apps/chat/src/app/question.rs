//! Answering the question a message asks: the press of one of its answers by
//! the person it asks, at this device.

use ui::tilo_resource::Id;

use super::ChatApp;
use crate::messages::{NewMessage, chat_messages};
use crate::parts::ChatMessages;
use crate::question::CHOICE;

impl ChatApp {
    /// The device an answer pressed here names: this one, when its host
    /// named it.
    fn answering_device(&self) -> Option<String> {
        self.device.clone().filter(|device| !device.is_empty())
    }

    /// Give `answer` to the question `message` asks this person. A choice's
    /// answer is then posted as their own message, which is what its asker
    /// reads next.
    ///
    /// Nothing is sent for a question that asks someone else or no longer
    /// waits: its buttons are not drawn, and the resource refuses it anyway.
    pub(crate) fn answer_question(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        message: &str,
        answer: &str,
    ) {
        let Some(m) = self.message(message) else {
            return;
        };
        let Some(question) = m.question.as_ref() else {
            return;
        };
        if !question.waiting() || !self.is_me(&question.person) {
            return;
        }
        let said = (question.kind.0 == CHOICE)
            .then(|| question.label(answer).map(str::to_owned))
            .flatten();
        let channel = m.channel.clone();
        let given = self.try_apply_part(
            doc,
            ChatMessages::ANSWER_QUESTION.with(chat_messages::AnswerQuestion {
                message: Id::from_raw(message),
                answer: answer.to_owned(),
                device: self.answering_device(),
                detail: None,
            }),
        );
        if given.is_ok()
            && let Some(text) = said
        {
            self.apply_part(
                doc,
                ChatMessages::POST.with(chat_messages::Post {
                    message: NewMessage {
                        id: None,
                        channel: Id::from_raw(channel),
                        text,
                        parent: None,
                        quoted: None,
                        body: None,
                        attachments: Vec::new(),
                        mentions: None,
                        written: None,
                        origin: None,
                    },
                }),
            );
        }
        self.request_relayout();
    }
}
