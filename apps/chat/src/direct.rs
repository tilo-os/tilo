//! Chat's direct conversations as its views show them: the state, drafts and
//! effects of the person-only conversation, answered by the conversation's
//! record ([`ui::direct::Records`]).

use std::rc::Rc;

use crate::app::{ChatApp, Focus};
use crate::composer::ConversationKey;
use crate::effects::ChatEffect;
use ui::direct::Records;
use ui::framework::{Effect, EffectAnswer, Effected, LiveRow};

impl Drop for ChatApp {
    fn drop(&mut self) {
        self.close_person_conversation();
    }
}

impl ChatApp {
    /// The records this window keeps its conversations in.
    fn direct_records(&self) -> Result<Rc<Records>, String> {
        self.direct
            .clone()
            .ok_or_else(|| crate::i18n::t("chat-person-chat-unavailable"))
    }

    /// The people this window may talk to, each with whether a conversation
    /// of theirs holds messages.
    pub(crate) fn direct_contacts(&self) -> Vec<ui::person_chat::Contact> {
        self.direct
            .as_ref()
            .map_or_else(Vec::new, |records| records.contacts())
    }

    /// Whether this view holds the conversation with `peer` still open: the
    /// records hold theirs until the window closes.
    pub(crate) fn retains_person(&self, peer: &str) -> bool {
        self.direct.is_some() && self.direct_person.as_deref() == Some(peer)
    }

    pub(crate) fn close_person_conversation(&mut self) {
        self.direct_person = None;
        self.person_messages.clear();
        self.person_body_views.clear();
        self.person_error = None;
    }

    pub(crate) fn person_conversation_key(public_key: &str) -> ConversationKey {
        ConversationKey::Person {
            workspace: crate::app::WORKSPACE_SCOPE.to_owned(),
            public_key: public_key.to_owned(),
        }
    }

    pub(crate) fn person_display_name(&self, public_key: &str) -> String {
        self.person_contacts
            .iter()
            .find(|contact| contact.public_key == public_key)
            .map_or_else(
                || crate::identity::unnamed_label(public_key),
                |contact| self.contact_name(contact),
            )
    }

    /// What a contact is called: the name they go by here now, as
    /// everyone here sees it (they may have renamed themself since the
    /// workspace's list was read), else the name the workspace keeps for them, else
    /// the unnamed person's tag. Never blank.
    pub(crate) fn contact_name(&self, contact: &ui::person_chat::Contact) -> String {
        [
            self.workspace_names.get(contact.public_key.as_str()),
            Some(&contact.display_name),
        ]
        .into_iter()
        .flatten()
        .map(|name| name.trim())
        .find(|name| !name.is_empty())
        .map_or_else(
            || crate::identity::unnamed_label(&contact.public_key),
            str::to_owned,
        )
    }

    fn require_person_contact(&self, public_key: &str) -> Result<(), String> {
        if self
            .person_contacts
            .iter()
            .any(|contact| contact.public_key == public_key)
        {
            Ok(())
        } else {
            Err(crate::i18n::t("chat-person-chat-unavailable"))
        }
    }

    pub(crate) fn sync_person_draft(&mut self) {
        let Some(peer) = self.direct_person.as_deref() else {
            return;
        };
        let key = Self::person_conversation_key(peer);
        self.composer_main.set_conversation_draft(&key);
        self.draft_main = self.composer_main.private_plain().unwrap_or_default();
        self.composer_main
            .set_placeholder(&crate::i18n::t("chat-person-chat-placeholder"));
    }

    /// Open the conversation with `public_key`: the view moves to it once it
    /// is open.
    pub(crate) fn open_person_conversation(&mut self, public_key: &str) -> Result<(), String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        let records = self.direct_records()?;
        let conversation = records.open(public_key)?;
        self.opened_person_conversation(public_key, conversation)
    }

    fn opened_person_conversation(
        &mut self,
        public_key: &str,
        conversation: ui::person_chat::Conversation,
    ) -> Result<(), String> {
        if conversation.peer != public_key {
            return Err(crate::i18n::t("chat-person-chat-unavailable"));
        }
        self.close_person_conversation();
        self.direct_person = Some(public_key.to_owned());
        self.active_channel = Self::person_conversation_key(public_key).draft_key();
        self.person_messages = conversation.messages;
        self.person_error = None;
        self.thread_open = None;
        self.editing = None;
        self.focus = Focus::Main;
        self.scroll = 0.0;
        self.sync_person_draft();
        let channel = self.active_channel.clone();
        self.restore_attached_agent_surface(&channel);
        self.request_relayout();
        Ok(())
    }

    /// How many messages of `peer`'s conversation this person has not read.
    pub(crate) fn direct_unread(&self, peer: &str) -> u32 {
        self.direct.as_ref().map_or(0, |direct| direct.unread(peer))
    }

    /// Tell the direct conversations who the workspace's people are now,
    /// and import a few conversations the old private store still holds:
    /// each is a migration the window does once, not a reason to stall.
    fn refresh_direct_people(&self) {
        let Some(direct) = self.direct.clone() else {
            return;
        };
        direct.set_people(self.direct_people.clone());
        // A page holds no old store: it imports nothing, and the device that
        // hosts the workspace brings the conversations over.
        direct.import_old_store();
    }

    pub(crate) fn refresh_person_conversation(&mut self) {
        self.refresh_direct_people();
        // Contacts hydrate after the first mounted frame. Keep the semantic
        // rows current even before any conversation is selected.
        let contacts = self.direct_contacts();
        let changed = contacts != self.person_contacts;
        self.person_contacts = contacts;
        if changed {
            self.request_relayout();
        }
        let Some(public_key) = self.direct_person.clone() else {
            return;
        };
        let shown = match self.direct.clone() {
            Some(records) => records.read(&public_key),
            None => return,
        };
        match shown {
            Ok(conversation) if conversation.peer == public_key => {
                if conversation.messages.len() != self.person_messages.len() {
                    self.request_relayout();
                }
                self.person_messages = conversation.messages;
                self.person_error = None;
                // What is on screen is read, up to where it reaches.
                if let Some(direct) = &self.direct {
                    direct.mark_read(&public_key);
                }
            }
            Ok(_) => {
                self.person_messages.clear();
                self.person_body_views.clear();
                self.person_error = Some(crate::i18n::t("chat-person-chat-unavailable"));
            }
            Err(error) => {
                self.person_messages.clear();
                self.person_body_views.clear();
                self.person_error = Some(error);
            }
        }
    }

    /// An explicit read of the conversation with `public_key`.
    pub(crate) fn read_person_conversation(
        &mut self,
        public_key: &str,
    ) -> Result<ui::person_chat::Conversation, String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        self.direct_records()?.open(public_key)
    }

    /// What a read answered, as the effect's value.
    pub(crate) fn read_person_answer(
        &mut self,
        public_key: &str,
        answer: Result<ui::person_chat::Conversation, String>,
    ) -> EffectAnswer {
        let conversation = answer?;
        if conversation.peer != public_key {
            return Err(crate::i18n::t("chat-person-chat-unavailable"));
        }
        let value = serde_json::json!({
            "peer": conversation.peer,
            "messages": conversation.messages.iter().map(|message| serde_json::json!({
                "id": message.id, "author": message.author, "text": message.text,
            })).collect::<Vec<_>>(),
            // What the sidebar counts for this conversation: a reader asks the
            // conversation how much of it its person has not read.
            "unread": self.direct_unread(public_key),
        });
        if self.direct_person.as_deref() == Some(public_key) {
            self.person_messages = conversation.messages;
            self.person_error = None;
        }
        Ok(Effected::value(value))
    }

    /// Send `text` to `public_key`; the answer says when it was accepted.
    pub(crate) fn send_person_message(
        &mut self,
        public_key: &str,
        text: &str,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<ui::person_chat::Conversation, String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        if text.trim().is_empty() {
            return Err(crate::i18n::t("chat-person-chat-empty"));
        }
        self.direct_records()?
            .send(public_key, text, invocation.unwrap_or_default())
    }

    /// What an answer to a message to `public_key` says, as the effect's value.
    pub(crate) fn sent_person_answer(
        &mut self,
        public_key: &str,
        answer: Result<ui::person_chat::Conversation, String>,
    ) -> EffectAnswer {
        let conversation = answer?;
        if conversation.peer != public_key {
            return Err(crate::i18n::t("chat-person-chat-unavailable"));
        }
        if self.direct_person.as_deref() == Some(public_key) {
            self.person_messages = conversation.messages;
            self.person_error = None;
            self.scroll = 0.0;
        }
        self.request_relayout();
        Ok(Effected::value(
            serde_json::json!({"peer": public_key, "accepted": true}),
        ))
    }

    /// Send what the composer holds to the open conversation. Its draft
    /// clears once the conversation accepted the message, not before.
    pub(crate) fn send_person_focused(&mut self) -> bool {
        let Some(public_key) = self.direct_person.clone() else {
            return false;
        };
        if !self.main_composer_can_edit() {
            return false;
        }
        let text = self.composer_main.private_plain().unwrap_or_default();
        if text.trim().is_empty() {
            return false;
        }
        let key = Self::person_conversation_key(&public_key);
        let revision = self.composer_main.conversation_revision(&key);
        let answer =
            self.send_person_message(&public_key, &text, Some(tilo_actions::InvocationId::new()));
        match self.sent_person_answer(&public_key, answer) {
            Ok(_) => {
                if let Some(revision) = revision
                    && self
                        .composer_main
                        .clear_conversation_revision(&key, revision)
                {
                    self.draft_main.clear();
                }
                true
            }
            Err(error) => {
                self.person_error = Some(error);
                self.request_relayout();
                false
            }
        }
    }

    pub(crate) fn person_conversation_rows(&self) -> Vec<LiveRow> {
        self.direct_contacts()
            .iter()
            .map(|contact| {
                let name = if contact.display_name.trim().is_empty() {
                    crate::identity::unnamed_label(&contact.public_key)
                } else {
                    contact.display_name.clone()
                };
                LiveRow::new(
                    format!("chat.person:{}", contact.public_key),
                    "chat.person",
                    &name,
                )
                .effect(&ChatEffect::OpenDirectConversation {
                    public_key: contact.public_key.clone(),
                })
                .effect(&ChatEffect::ReadDirectConversation {
                    public_key: contact.public_key.clone(),
                })
                .operation(
                    ChatEffect::SendDirectMessage {
                        public_key: contact.public_key.clone(),
                        text: String::new(),
                    }
                    .bound_asking(&["text"]),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod conversation_tests;
