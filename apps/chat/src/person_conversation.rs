//! Person-only conversations. Bodies and drafts never enter the room record.

use crate::app::{ChatApp, Focus};
use crate::composer::ConversationKey;
use crate::effects::ChatEffect;
use ui::framework::{Effect, EffectAnswer, Effected, LiveRow};

impl Drop for ChatApp {
    fn drop(&mut self) {
        self.close_person_conversation();
    }
}

/// A conversation being opened, by its peer, until the room answers.
pub(crate) type Opening = (
    String,
    ui::person_chat::Pending<ui::person_chat::Conversation>,
);

/// A message on its way, kept until the room accepts or refuses it.
pub(crate) struct Sending {
    peer: String,
    key: ConversationKey,
    revision: Option<u64>,
    reply: ui::person_chat::Pending<ui::person_chat::Conversation>,
}

impl ChatApp {
    pub(crate) fn close_person_conversation(&mut self) {
        if let Some(peer) = self.direct_person.take()
            && let Some(token) = self.person_subscription.take()
        {
            ui::person_chat::close_current(&peer, token);
        }
        self.person_subscription = None;
        self.person_sending = None;
        self.person_messages.clear();
        self.person_body_views.clear();
        self.person_error = None;
    }

    pub(crate) fn person_conversation_key(public_key: &str) -> ConversationKey {
        ConversationKey::Person {
            workspace: crate::app::ROOM_SCOPE.to_owned(),
            public_key: public_key.to_owned(),
        }
    }

    pub(crate) fn person_display_name(&self, public_key: &str) -> String {
        self.person_contacts
            .iter()
            .find(|contact| contact.public_key == public_key)
            .map_or_else(
                || crate::identity::unnamed_label(public_key),
                |contact| contact.display_name.clone(),
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
    /// the room has it open: at once from the native host, a moment later
    /// in a page, which never waits for the room on the UI thread.
    pub(crate) fn open_person_conversation(&mut self, public_key: &str) -> Result<(), String> {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        let opening = ui::person_chat::open(public_key);
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
        // Open acquires the new owner first, including when reopening the same
        // peer. Release this view's previous selection without touching floats.
        self.close_person_conversation();
        self.direct_person = Some(public_key.to_owned());
        self.person_subscription = ui::person_chat::subscription_token(public_key);
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

    /// Take up what the room answered an open or a send since the last sync.
    fn settle_person_replies(&mut self) {
        self.person_abandoned
            .retain(|(peer, opening)| match opening.take() {
                Some(Ok(_)) => {
                    ui::person_chat::close(peer);
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

    pub(crate) fn refresh_person_conversation(&mut self) {
        self.settle_person_replies();
        // Native contacts hydrate after the first mounted frame. Keep the
        // semantic rows current even before any conversation is selected.
        let contacts = ui::person_chat::contacts();
        let changed = contacts.len() != self.person_contacts.len()
            || contacts
                .iter()
                .zip(&self.person_contacts)
                .any(|(new, old)| {
                    new.public_key != old.public_key || new.display_name != old.display_name
                });
        self.person_contacts = contacts;
        if changed {
            self.request_relayout();
        }
        let Some(public_key) = self.direct_person.clone() else {
            return;
        };
        if self.person_subscription.is_none()
            || self.person_subscription != ui::person_chat::subscription_token(&public_key)
        {
            self.person_messages.clear();
            self.person_body_views.clear();
            self.person_error = Some(crate::i18n::t("chat-person-chat-unavailable"));
            return;
        }
        match ui::person_chat::snapshot(&public_key) {
            Ok(conversation) if conversation.peer == public_key => {
                if conversation.messages.len() != self.person_messages.len() {
                    self.request_relayout();
                }
                self.person_messages = conversation.messages;
                self.person_error = None;
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

    /// An explicit read of the conversation with `public_key`, answered
    /// once the room has (an invocation's read is checked against its
    /// original custody there).
    pub(crate) fn read_person_conversation(
        &mut self,
        public_key: &str,
    ) -> Result<ui::person_chat::Pending<ui::person_chat::Conversation>, String> {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        Ok(ui::person_chat::read(public_key))
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

    /// Send `text` to `public_key`; the reply says when the room accepted it.
    pub(crate) fn send_person_message(
        &mut self,
        public_key: &str,
        text: &str,
    ) -> Result<ui::person_chat::Pending<ui::person_chat::Conversation>, String> {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        if text.trim().is_empty() {
            return Err(crate::i18n::t("chat-person-chat-empty"));
        }
        Ok(ui::person_chat::send(public_key, text))
    }

    /// What the room answered a message to `public_key`, as the effect's value.
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
    /// clears once the room accepted the message, not before.
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
        let reply = match self.send_person_message(&public_key, &text) {
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

    pub(crate) fn person_conversation_rows() -> Vec<LiveRow> {
        // Publication can run before widget sync after a snapshot wake. Read
        // the same bounded native snapshot rather than a stale painted list.
        let _ = ui::person_chat::contacts();
        ui::person_chat::contacts_snapshot()
            .iter()
            .map(|contact| {
                LiveRow::new(
                    format!("chat.person:{}", contact.public_key),
                    "chat.person",
                    &contact.display_name,
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
