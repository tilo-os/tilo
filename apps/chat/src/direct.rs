//! Chat's direct conversations: the conversation two of the workspace's
//! people have, opened and sent to by the app's operations
//! (`open-direct-conversation`, `send-direct-message`); its messages are read
//! from the record, as every conversation's are.
//!
//! A direct conversation is not created: its id names its participants
//! ([`Chat::direct_id`]), so every device that opens the same conversation
//! opens the same one, and neither has to write anything first. Its messages
//! are the chat's messages in the chat's parts, beside every channel's; its
//! read mark is this person's, in the record.

use tilo_actions::policy::MemberId;
use ui::framework::{Effected, LiveRow};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::{Id, Invoke};

use crate::app::{ChatApp, Focus};
use crate::effects::{ChatEffect, item_row_id};
use crate::messages::chat_part;
use crate::parts::ChatPart;
use crate::record::Chat;

impl ChatApp {
    /// The person the operation's `key` names here, and the one conversation
    /// with them. A workspace whose chat this window does not hold yet says
    /// so, and so does a chat that knows no such person: two different
    /// things to tell the person asking.
    pub(crate) fn direct_conversation(
        &self,
        key: &str,
        chat: &Chat,
    ) -> Result<(String, Id), String> {
        if chat.people.is_empty() {
            return Err(crate::i18n::t("chat-direct-unavailable"));
        }
        let Some(person) = chat
            .people
            .iter()
            .find(|person| person.id.0 == key && !person.is_agent())
        else {
            return Err(crate::i18n::t("chat-person-unknown"));
        };
        if person.id.0 == self.me.id {
            return Err(crate::i18n::t("chat-person-unknown"));
        }
        let id = Chat::direct_id([
            MemberId::person(self.me.id.clone()),
            MemberId::person(person.id.0.clone()),
        ]);
        Ok((person.id.0.clone(), id))
    }

    /// Show the conversation with `agent`, as search's agent hit does: a
    /// direct conversation's id names the two it is between, so an agent the
    /// window has not talked to yet still opens where the conversation is.
    pub(crate) fn open_agent(&mut self, agent: &str, chat: &Chat) {
        let id = Chat::direct_id([
            MemberId::person(self.me.id.clone()),
            MemberId::agent(agent.to_owned()),
        ]);
        self.open_conversation(&id, chat);
    }

    /// Show the conversation `id`, as choosing it in the sidebar does, and
    /// mark it read up to what this window holds.
    pub(crate) fn open_conversation(&mut self, id: &Id, chat: &Chat) {
        let channel = id.to_string();
        self.call.expanded = false;
        self.active_channel.clone_from(&channel);
        self.thread_open = None;
        self.editing = None;
        self.focus = Focus::Main;
        self.scroll = 0.0;
        self.composer_main.set_key(&channel);
        self.composer_main
            .set_placeholder(&self.conversation_placeholder(&channel));
        self.messages = chat.message_rows(&self.held_messages(), &channel);
        // The conversation in view is read by the pass that follows, as a
        // channel is ([`ChatApp::mark_active_read`]): a read receipt sent from
        // here would continue this view's invocation, whose reach is reading,
        // and a write beyond it is the person's to confirm — which nobody
        // asked them.
        self.request_relayout();
    }

    /// Post `text` to the conversation `id`, as [`Self::send_message`] does,
    /// and tell `then` whether it was taken: at once, or, on a device whose
    /// messages are still on their way from the workspace (back from away),
    /// once they are here and it was written after them.
    pub(crate) fn post_to_conversation_then(
        &self,
        id: &Id,
        text: &str,
        then: impl FnOnce(Result<(), String>) + 'static,
    ) {
        match self.messages_chain() {
            Ok(chain) => {
                chain.invoke_then(&post(id, text, None, None, Vec::new()), move |posted| {
                    then(posted.map(drop).map_err(|error| error.to_string()));
                });
            }
            Err(unavailable) => then(Err(unavailable)),
        }
    }

    fn messages_chain(&self) -> Result<&ui::framework::Chain<ChatPart>, String> {
        self.chain
            .as_ref()
            .ok_or_else(|| crate::i18n::t("chat-direct-unavailable"))
    }

    /// Send what a box held: the words and everything they carry go to the
    /// conversation with whom it names — a person, an agent, several of
    /// them, or a conversation that is already there. When it `opens`, the
    /// conversation opens as a window of the desk, floated where the box
    /// stood ([`ui::shell_tabs::open_floated`]). The window that sent it goes
    /// on showing what it showed, its box ready for the next message.
    ///
    /// # Errors
    /// The box names nobody this chat knows, a conversation beside other
    /// recipients, or a message with neither words nor anything to carry.
    pub(crate) fn send_message(
        &self,
        message: SendMessage<'_>,
        chat: &Chat,
    ) -> Result<Effected, String> {
        let SendMessage {
            to,
            attached,
            text,
            parent,
            body,
            opens,
        } = message;
        if text.trim().is_empty() && attached.is_empty() {
            return Err(crate::i18n::t("chat-send-empty"));
        }
        let id = self.conversation_named(to, chat)?;
        self.messages_chain()?
            .invoke(&post(&id, text, parent.map(Id::from_raw), body, attached))
            .map_err(|error| error.to_string())?;
        // The conversation stands where the box stood: a window of the desk,
        // floated over it, that the workspace's record keeps for every
        // member.
        let anchor = item_row_id("channels", id.as_str());
        if opens {
            ui::shell_tabs::open_floated(anchor.clone());
        }
        Ok(Effected::value(serde_json::json!({
            "conversation": id.to_string(),
            "anchor": anchor,
            "accepted": true,
        })))
    }

    /// The conversation a message to `to` lands in: the one a recipient names,
    /// or the one between this person and everyone else the box holds — the
    /// same id on every device, so a message to someone already talked to
    /// reopens their conversation rather than making a second.
    fn conversation_named(
        &self,
        to: &[ui::floating_surface::Recipient],
        chat: &Chat,
    ) -> Result<Id, String> {
        use ui::floating_surface::RecipientKind;
        if let Some(conversation) = to
            .iter()
            .find(|recipient| recipient.kind == RecipientKind::Conversation)
        {
            if to.len() > 1 {
                return Err(crate::i18n::t("chat-send-one-conversation"));
            }
            return Ok(Id::from_raw(conversation.id.clone()));
        }
        let mut participants = vec![MemberId::person(self.me.id.clone())];
        for recipient in to {
            match recipient.kind {
                RecipientKind::Person => {
                    if !chat.people.iter().any(|person| person.id.0 == recipient.id) {
                        return Err(crate::i18n::t("chat-person-unknown"));
                    }
                    participants.push(MemberId::person(recipient.id.clone()));
                }
                RecipientKind::Agent => {
                    participants.push(MemberId::agent(recipient.id.clone()));
                }
                // What a box holds of a new agent is made before it sends
                // ([`crate::effects::ChatEffect::Send`]): an agent still
                // nameless here is one the workspace never listed. A report's
                // destination is the session's to address, as a conversation
                // in the workspace that keeps it.
                RecipientKind::NewAgent | RecipientKind::Conversation | RecipientKind::Feedback => {
                    return Err(crate::i18n::t("chat-send-needs-recipient"));
                }
            }
        }
        if participants.len() < 2 {
            return Err(crate::i18n::t("chat-send-needs-recipient"));
        }
        Ok(Chat::direct_id(participants))
    }

    /// The person the direct conversation `channel` is with, when it is one of this window's
    /// person with exactly one other person: what the app's direct operations are asked on. A
    /// conversation with an agent, or one of more than two people, is not one of them.
    fn direct_peer(&self, channel: &crate::model::Channel) -> Option<String> {
        self.conversation_peer(&channel.participants)
            .filter(|peer| channel.direct && peer.is_person())
            .map(|peer| peer.id().to_owned())
    }

    /// The direct conversations the sidebar lists: those holding messages,
    /// and the one open.
    pub(crate) fn direct_contacts(&self) -> Vec<crate::model::Channel> {
        self.channels
            .iter()
            .filter(|channel| {
                channel.direct
                    && (self.direct_held.contains(&channel.id) || self.active_channel == channel.id)
            })
            .cloned()
            .collect()
    }

    /// The rows this app offers for its conversations with the workspace's
    /// people, so a caller finds them by the operation it wants rather than
    /// by a written address. Every person has one, not only those the
    /// conversation with whom holds a message yet: a caller asks for the
    /// row before it can write the first message. Its messages are read as
    /// any conversation's, from the chat's record and its parts.
    pub(crate) fn direct_conversation_rows(&self) -> Vec<LiveRow> {
        self.channels
            .iter()
            .filter_map(|channel| {
                let public_key = self.direct_peer(channel)?;
                Some(
                    LiveRow::new(
                        format!("chat.conversation:{public_key}"),
                        "chat.conversation",
                        &channel.name,
                    )
                    .effect(&ChatEffect::OpenDirectConversation {
                        public_key: public_key.clone(),
                    })
                    .operation(
                        ChatEffect::SendDirectMessage {
                            public_key,
                            text: String::new(),
                        }
                        .bound_asking(&["text"]),
                    ),
                )
            })
            .collect()
    }
}

/// What a box holds for one send: who it goes to, what it says, what it
/// carries, the message it answers in a thread, its own paragraphs, and
/// whether the conversation opens where the box stood.
pub(crate) struct SendMessage<'a> {
    pub to: &'a [ui::floating_surface::Recipient],
    pub attached: Vec<tilo_reference::Attached>,
    pub text: &'a str,
    /// The message it answers in a thread, by its id.
    pub parent: Option<String>,
    pub body: Option<Vec<tilo_document::ClipBlock>>,
    pub opens: bool,
}

/// The record's `post` of `text`, with what it carries and the paragraphs it
/// was written in, to the conversation `id`.
fn post(
    id: &Id,
    text: &str,
    parent: Option<Id>,
    body: Option<Vec<tilo_document::ClipBlock>>,
    attachments: Vec<tilo_reference::Attached>,
) -> Invoke<ChatPart> {
    ChatPart::POST.with(chat_part::Post {
        message: crate::messages::NewMessage {
            channel: id.clone(),
            text: text.to_owned(),
            parent,
            quoted: None,
            body,
            attachments,
            mentions: None,
            written: None,
        },
    })
}

#[cfg(test)]
mod conversation_tests;
