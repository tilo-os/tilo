//! Chat's direct conversations: the conversation two of the workspace's
//! people have, opened, read and sent to by the app's operations
//! (`open-direct-conversation`, `read-direct-conversation`,
//! `send-direct-message`).
//!
//! A direct conversation is not created: its id names its participants
//! ([`Chat::direct_id`]), so every device that opens the same conversation
//! opens the same one, and neither has to write anything first. Its messages
//! are the chat's messages in the chat's parts, beside every channel's; its
//! read mark is this person's, in the record.

use ui::framework::{EffectAnswer, Effected, LiveRow};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::Id;

use crate::app::{ChatApp, Focus};
use crate::effects::ChatEffect;
use crate::messages::chat_part;
use crate::parts::ChatPart;
use crate::record::{Chat, Participant};

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
            .find(|person| person.id.0 == key && !person.agent)
        else {
            return Err(crate::i18n::t("chat-person-unknown"));
        };
        if person.id.0 == self.me.id {
            return Err(crate::i18n::t("chat-person-unknown"));
        }
        let id = Chat::direct_id([
            Participant::person(self.me.id.clone()),
            Participant::person(person.id.0.clone()),
        ]);
        Ok((person.id.0.clone(), id))
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
        self.mark_conversation_read(chat);
        self.restore_attached_agent_surface(&channel);
        self.request_relayout();
    }

    /// The conversation with `peer` as the app answers a read of it: its
    /// messages in the order the window shows them, and how many of them this
    /// person has not seen.
    pub(crate) fn read_direct_answer(&mut self, peer: &str, id: &Id, chat: &Chat) -> EffectAnswer {
        let channel = id.to_string();
        let held = self.held_messages();
        let rows = chat.message_rows(&held, &channel);
        let messages: Vec<serde_json::Value> = rows
            .iter()
            .map(|message| {
                serde_json::json!({
                    "id": message.id,
                    "author": message.author,
                    "text": message.text,
                    "at": message.ts,
                })
            })
            .collect();
        let unread = chat.unread(&held, &channel, &self.me.id);
        if self.active_channel == channel {
            self.messages = rows;
        }
        Ok(Effected::value(serde_json::json!({
            "peer": peer,
            "messages": messages,
            "unread": unread,
        })))
    }

    /// Post `text` to the conversation `id` now, as the record's `post`
    /// does, and say whether it was taken.
    pub(crate) fn post_to_conversation(&self, id: &Id, text: &str) -> Result<(), String> {
        let Some(chain) = &self.chain else {
            return Err(crate::i18n::t("chat-direct-unavailable"));
        };
        chain
            .invoke(&ChatPart::POST.with(chat_part::Post {
                message: crate::messages::NewMessage {
                    channel: id.clone(),
                    text: text.to_owned(),
                    parent: None,
                    body: None,
                    attachment: None,
                    mentions: None,
                    written: None,
                },
            }))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// The person the direct conversation `channel` is with, when it is one
    /// of this window's person with exactly one other person: what the app's
    /// direct operations are asked on. A conversation with an agent, or one
    /// of more than two people, is not one of them.
    fn direct_peer(&self, channel: &crate::model::Channel) -> Option<String> {
        if !channel.direct || channel.participants.len() != 2 {
            return None;
        }
        let participants: Vec<Participant> = channel
            .participants
            .iter()
            .filter_map(|key| Participant::parse(key))
            .collect();
        if participants.len() != 2 || participants.iter().any(Participant::is_agent) {
            return None;
        }
        participants
            .into_iter()
            .find(|participant| participant.id() != self.me.id)
            .map(|participant| participant.id().to_owned())
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
    /// row before it can write the first message.
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
                    .effect(&ChatEffect::ReadDirectConversation {
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

#[cfg(test)]
mod conversation_tests;
