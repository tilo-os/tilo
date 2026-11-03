//! Chat's direct conversations as its views show them: the state, drafts and
//! effects of the person-only conversation, answered by the pair records this
//! window holds ([`Records`]). A page keeps no workspace records of its own
//! and reaches its conversations through its host on the person door until
//! the record is there; both answer this module's [`Answer`].

use std::rc::Rc;

use crate::app::{ChatApp, Focus};
use crate::composer::ConversationKey;
use crate::direct::Records;
use crate::effects::ChatEffect;
use ui::framework::{Effect, EffectAnswer, Effected, LiveRow};

impl Drop for ChatApp {
    fn drop(&mut self) {
        self.close_person_conversation();
    }
}

/// What answered a conversation call: the records here at once, or the host
/// of a page later.
pub(crate) enum Answer<T> {
    Now(Result<T, String>),
    Later(ui::person_chat::Pending<T>),
}

impl<T: Clone> Answer<T> {
    /// The answer, once it is here; taken, it is gone.
    pub(crate) fn take(&self) -> Option<Result<T, String>> {
        match self {
            Self::Now(answer) => Some(answer.clone()),
            Self::Later(pending) => pending.take(),
        }
    }

    /// The answer still on its way, for a window that waits for it.
    pub(crate) fn later(self) -> Option<ui::person_chat::Pending<T>> {
        match self {
            Self::Now(_) => None,
            Self::Later(pending) => Some(pending),
        }
    }
}

/// A conversation being opened, by its peer, until it answers.
pub(crate) type Opening = (String, Answer<ui::person_chat::Conversation>);

/// A message on its way, kept until the conversation accepts or refuses it.
pub(crate) struct Sending {
    peer: String,
    key: ConversationKey,
    revision: Option<u64>,
    reply: Answer<ui::person_chat::Conversation>,
}

impl ChatApp {
    /// The records this window keeps its conversations in, where it keeps
    /// them here at all: a page has none, and asks its host.
    fn direct_records(&self) -> Option<&Rc<Records>> {
        self.direct.as_ref()
    }

    /// The people this window may talk to, each with whether a conversation
    /// of theirs holds messages.
    pub(crate) fn direct_contacts(&self) -> Vec<ui::person_chat::Contact> {
        if let Some(records) = self.direct_records() {
            return records.contacts();
        }
        // Publication can run before widget sync after a snapshot wake. Read
        // the same bounded host snapshot rather than a stale painted list.
        let _ = ui::person_chat::contacts();
        ui::person_chat::contacts_snapshot()
    }

    /// Whether this view holds the conversation with `peer` still open, so a
    /// floating view of it keeps its lane: the records hold theirs until the
    /// window closes; a page's lane is retained one owner more.
    pub(crate) fn retains_person(&self, peer: &str) -> bool {
        self.direct_records().is_some() || ui::person_chat::retain(peer)
    }

    pub(crate) fn close_person_conversation(&mut self) {
        if let Some(peer) = self.direct_person.take()
            && self.direct_records().is_none()
        {
            ui::person_chat::close(&peer);
        }
        self.person_sending = None;
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

    /// Open the conversation with `public_key`. The view moves to it once
    /// it is open: at once from the records this window holds, a moment later
    /// in a page, which never waits for its host on the UI thread.
    pub(crate) fn open_person_conversation(&mut self, public_key: &str) -> Result<(), String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        let opening = match self.direct_records() {
            Some(records) => Answer::Now(records.open(public_key)),
            None => Answer::Later(ui::person_chat::open(public_key)),
        };
        if let Some(answer) = opening.take() {
            return self.opened_person_conversation(public_key, answer);
        }
        if let Some(replaced) = self
            .person_opening
            .replace((public_key.to_owned(), opening))
        {
            self.person_abandoned.push(replaced);
        }
        Ok(())
    }

    fn opened_person_conversation(
        &mut self,
        public_key: &str,
        answer: Result<ui::person_chat::Conversation, String>,
    ) -> Result<(), String> {
        let conversation = answer?;
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

    /// Take up what an open or a send answered since the last sync.
    fn settle_person_replies(&mut self) {
        let page = self.direct_records().is_none();
        self.person_abandoned
            .retain(|(peer, opening)| match opening.take() {
                Some(Ok(_)) => {
                    if page {
                        ui::person_chat::close(peer);
                    }
                    false
                }
                Some(Err(_)) => false,
                None => true,
            });
        if let Some((peer, opening)) = self.person_opening.take() {
            match opening.take() {
                Some(answer) => {
                    if let Err(error) = self.opened_person_conversation(&peer, answer) {
                        self.person_error = Some(error);
                        self.request_relayout();
                    }
                }
                None => self.person_opening = Some((peer, opening)),
            }
        }
        if let Some(sending) = self.person_sending.take() {
            match sending.reply.take() {
                Some(answer) => self.sent_person_message(&sending, answer),
                None => self.person_sending = Some(sending),
            }
        }
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
        direct.import_old_store();
    }

    pub(crate) fn refresh_person_conversation(&mut self) {
        self.refresh_direct_people();
        self.settle_person_replies();
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
        let shown = match self.direct_records() {
            Some(records) => records.read(&public_key),
            None => ui::person_chat::snapshot(&public_key),
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

    /// An explicit read of the conversation with `public_key`, answered at
    /// once from the records here or later by a page's host.
    pub(crate) fn read_person_conversation(
        &mut self,
        public_key: &str,
    ) -> Result<Answer<ui::person_chat::Conversation>, String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        Ok(match self.direct_records() {
            Some(records) => Answer::Now(records.read(public_key)),
            None => Answer::Later(ui::person_chat::read(public_key)),
        })
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
            })).collect::<Vec<_>>()
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
    ) -> Result<Answer<ui::person_chat::Conversation>, String> {
        self.person_contacts = self.direct_contacts();
        self.require_person_contact(public_key)?;
        if text.trim().is_empty() {
            return Err(crate::i18n::t("chat-person-chat-empty"));
        }
        Ok(match self.direct_records() {
            Some(records) => Answer::Now(records.send(
                public_key,
                text,
                invocation.map_or_else(uuid::Uuid::new_v4, |id| id.0),
            )),
            None => Answer::Later(ui::person_chat::with_invocation(invocation, || {
                ui::person_chat::send(public_key, text)
            })),
        })
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
        if !self.main_composer_can_edit() || self.person_sending.is_some() {
            return false;
        }
        let text = self.composer_main.private_plain().unwrap_or_default();
        if text.trim().is_empty() {
            return false;
        }
        let key = Self::person_conversation_key(&public_key);
        let revision = self.composer_main.conversation_revision(&key);
        let reply = match self.send_person_message(&public_key, &text, None) {
            Ok(reply) => reply,
            Err(error) => {
                self.person_error = Some(error);
                return false;
            }
        };
        let sending = Sending {
            peer: public_key,
            key,
            revision,
            reply,
        };
        match sending.reply.take() {
            Some(answer) => self.sent_person_message(&sending, answer),
            None => self.person_sending = Some(sending),
        }
        true
    }

    fn sent_person_message(
        &mut self,
        sending: &Sending,
        answer: Result<ui::person_chat::Conversation, String>,
    ) {
        match self.sent_person_answer(&sending.peer, answer) {
            Ok(_) => {
                if let Some(revision) = sending.revision
                    && self
                        .composer_main
                        .clear_conversation_revision(&sending.key, revision)
                {
                    self.draft_main.clear();
                }
            }
            Err(error) => self.person_error = Some(error),
        }
        self.request_relayout();
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
