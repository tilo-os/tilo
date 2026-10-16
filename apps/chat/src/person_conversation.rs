//! Native person-only conversations. Bodies and drafts never enter the room record.

use crate::app::{ChatApp, Focus};
use crate::composer::ConversationKey;
use crate::effects::ChatEffect;
use ui::framework::{Effect, EffectAnswer, Effected, LiveRow};

impl Drop for ChatApp {
    fn drop(&mut self) {
        self.close_person_conversation();
    }
}

impl ChatApp {
    pub(crate) fn close_person_conversation(&mut self) {
        if let Some(peer) = self.direct_person.take()
            && let Some(token) = self.person_subscription.take()
        {
            ui::person_chat::close_current(&peer, token);
        }
        self.person_subscription = None;
        self.person_messages.clear();
        self.person_body_views.clear();
        self.person_error = None;
    }

    pub(crate) fn person_conversation_key(&self, public_key: &str) -> ConversationKey {
        ConversationKey::Person {
            workspace: self.active_ws.clone(),
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
        let key = self.person_conversation_key(peer);
        self.composer_main.set_conversation_draft(&key);
        self.draft_main = self.composer_main.private_plain().unwrap_or_default();
        self.composer_main
            .set_placeholder(&crate::i18n::t("chat-person-chat-placeholder"));
    }

    pub(crate) fn open_person_conversation(&mut self, public_key: &str) -> Result<(), String> {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        let conversation = ui::person_chat::open(public_key)?;
        if conversation.peer != public_key {
            return Err(crate::i18n::t("chat-person-chat-unavailable"));
        }
        // Open acquires the new owner first, including when reopening the same
        // peer. Release this view's previous selection without touching floats.
        self.close_person_conversation();
        self.direct_agent = None;
        self.direct_person = Some(public_key.to_owned());
        self.person_subscription = ui::person_chat::subscription_token(public_key);
        self.person_workspace = Some(self.active_ws.clone());
        self.active_channel = self.person_conversation_key(public_key).draft_key();
        self.person_messages = conversation.messages;
        self.person_error = None;
        self.inbox_open = false;
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

    pub(crate) fn refresh_person_conversation(&mut self) {
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
        if self.person_workspace.as_deref() != Some(self.active_ws.as_str()) {
            if self.direct_person.is_some() {
                self.leave_floated_agent_route();
            }
            self.person_messages.clear();
            self.person_body_views.clear();
            return;
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
        if let Err(error) = self.read_person_conversation(&public_key) {
            self.person_messages.clear();
            self.person_body_views.clear();
            self.person_error = Some(error);
        }
    }

    pub(crate) fn read_person_conversation(&mut self, public_key: &str) -> EffectAnswer {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        let conversation = ui::person_chat::read(public_key)?;
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

    pub(crate) fn send_person_message(&mut self, public_key: &str, text: &str) -> EffectAnswer {
        self.person_contacts = ui::person_chat::contacts();
        self.require_person_contact(public_key)?;
        if text.trim().is_empty() {
            return Err(crate::i18n::t("chat-person-chat-empty"));
        }
        let conversation = ui::person_chat::send(public_key, text)?;
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
        let key = self.person_conversation_key(&public_key);
        let revision = self.composer_main.conversation_revision(&key);
        match self.send_person_message(&public_key, &text) {
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
                false
            }
        }
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
