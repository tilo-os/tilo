//! Chat's direct conversations: the conversation two of the workspace's
//! people have, opened and sent to by the app's operations
//! (`open-direct-conversation`, `send-direct-message`); its messages are read
//! from the resource, as every conversation's are.
//!
//! A direct conversation is not created: its id names its members
//! ([`Chat::direct_id`]), so every device that opens the same conversation
//! opens the same one, and neither has to write anything first. Its messages
//! are the chat's messages in the chat's parts, beside every channel's; its
//! read mark is this person's, in the resource.

use crate::projection::ChatProjection as _;
use tilo_actions::policy::MemberId;
use ui::framework::{Effected, LiveRow};
use ui::tilo_resource::{Id, Invoke};

use crate::app::{ChatApp, Focus};
use crate::chat::Chat;
use crate::effects::{ChatEffect, item_row_id};
use crate::messages::chat_messages;
use crate::parts::ChatMessages;

impl ChatApp {
    /// A direct conversation names its members even before they have sent its first message.
    pub(crate) fn active_direct_members(&self) -> Option<Vec<MemberId>> {
        match self.channel(&self.active_channel) {
            Some(channel) => channel.direct.then(|| channel.members.clone()),
            None => Chat::direct_members(&Id::from_raw(self.active_channel.clone())),
        }
    }

    /// The person `key` (their root key) names here, and the one conversation
    /// with them. A window the workspace has not told its members yet says so,
    /// and so does a key the workspace's member list does not hold: two
    /// different things to tell the person asking.
    pub(crate) fn direct_conversation(&self, key: &str) -> Result<(String, Id), String> {
        let person = self.workspace_member(MemberId::person(key.to_owned()))?;
        let id = Chat::direct_id([self.me.id.clone(), person]);
        Ok((key.to_owned(), id))
    }

    /// `member`, when the workspace lists them and they are not this window's own person. The
    /// workspace's member list, as presence delivers it, is who is in it: a person is one before
    /// they ever open Chat.
    fn workspace_member(&self, member: MemberId) -> Result<MemberId, String> {
        if !self.workspace_presence_answers {
            return Err(crate::i18n::t("chat-direct-unavailable"));
        }
        if self.is_me(&member) || !self.workspace_names.contains_key(&member) {
            return Err(crate::i18n::t("chat-person-unknown"));
        }
        Ok(member)
    }

    /// Provision the caller's assistant on explicit request and reveal the actual agent
    /// only after its resource accepts the change.
    pub(crate) fn set_up_personal_assistant(
        &self,
        cx: &mut ui::framework::EffectCx<Chat>,
    ) -> ui::framework::EffectAnswer {
        use ui::tilo_workspace_agent::WorkspaceAgents;
        let keeper = cx
            .person()
            .filter(|keeper| !keeper.is_empty() && self.me.id.person_id() == Some(keeper.as_str()))
            .ok_or_else(|| crate::i18n::t("chat-assistant-needs-person"))?;
        let agents = self
            .agents_record()
            .ok_or_else(|| crate::i18n::t("chat-direct-unavailable"))?;
        let mut open = self.opener();
        let reply = cx.reply();
        let updated = agents.clone();
        agents.invoke_then(
            &WorkspaceAgents::ENSURE_PERSONAL_ASSISTANT.with(()),
            move |applied| {
                let opened = applied.map_err(|why| why.to_string()).and_then(|()| {
                    let state = updated.get();
                    let assistant = state
                        .personal_assistant(&keeper)
                        .ok_or_else(|| crate::i18n::t("chat-direct-unavailable"))?;
                    let at = ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>(
                        "agents",
                        &Id::from_raw(assistant.id.0.clone()),
                    );
                    open(&at);
                    Ok(tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"agent": assistant.id.0})),
                        resources: Vec::new(),
                    })
                });
                reply.finish(opened);
            },
        );
        Ok(Effected::Pending)
    }

    /// Show the conversation with `agent`, as search's agent hit does: a
    /// direct conversation's id names the two it is between, so an agent the
    /// window has not talked to yet still opens where the conversation is.
    pub(crate) fn open_agent(&mut self, agent: &str, chat: &Chat) {
        let id = Chat::direct_id([self.me.id.clone(), MemberId::agent(agent.to_owned())]);
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
        if let Some(bar) = &self.bar {
            bar.put_drawer_away();
        }
        // Opened to write in: the keyboard goes to its composer, away from a thread or an edit.
        self.set_composer_focus();
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
                let message = Id::from_raw(ui::tilo_resource::fresh_uuid().simple().to_string());
                let post = post(id, &message, text, None, None, Vec::new());
                chain.invoke_then(&post, move |posted| {
                    then(posted.map(drop).map_err(|error| error.to_string()));
                });
            }
            Err(unavailable) => then(Err(unavailable)),
        }
    }

    fn messages_chain(&self) -> Result<&ui::framework::Chain<ChatMessages>, String> {
        self.chain
            .as_ref()
            .ok_or_else(|| crate::i18n::t("chat-direct-unavailable"))
    }

    /// Send what a box held: the words and everything they carry go to the
    /// conversation with whom it names — a person, an agent, several of
    /// them, or a conversation that is already there. When it `opens`, the
    /// conversation opens as a window of the shell, floated where the box
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
        // The post carries the message's id, named here, so the sender is told it at once, so
        // what asked can stand the turn it opened (canvas.md §3) by the message, not by its
        // words, even while the part the post goes into is still arriving and writes it later.
        let message = Id::from_raw(ui::tilo_resource::fresh_uuid().simple().to_string());
        self.messages_chain()?
            .invoke(&post(
                &id,
                &message,
                text,
                parent.map(Id::from_raw),
                body,
                attached,
            ))
            .map_err(|error| error.to_string())?;
        let message = message.to_string();
        // The conversation stands where the box stood: a window of the shell,
        // floated over it, that the workspace's document keeps for every
        // member.
        let anchor = item_row_id("channels", id.as_str());
        if opens {
            ui::shell_tabs::open_floated(anchor.clone());
        }
        Ok(Effected::value(serde_json::json!({
            "conversation": id.to_string(),
            "anchor": anchor,
            "message": message,
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
            if !conversation.id.is_empty() {
                return Ok(Id::from_raw(conversation.id.clone()));
            }
            let name = conversation.name.trim().to_lowercase();
            return chat
                .channels
                .iter()
                .find(|(_, channel)| channel.name.trim().to_lowercase() == name)
                .map(|(id, _)| id.clone())
                .ok_or_else(|| crate::i18n::t("chat-send-needs-recipient"));
        }
        let mut members = vec![self.me.id.clone()];
        for recipient in to {
            match recipient.kind {
                RecipientKind::Person | RecipientKind::Agent => {
                    members.push(self.member_named(recipient)?);
                }
                // What a box holds of a new agent is made before it sends
                // ([`crate::effects::ChatEffect::Send`]): an agent still
                // nameless here is one the workspace never listed. A report's
                // destination is the session's to address, as a conversation
                // in the workspace that keeps it.
                RecipientKind::Assistant
                | RecipientKind::NewAgent
                | RecipientKind::Conversation
                | RecipientKind::Feedback => {
                    return Err(crate::i18n::t("chat-send-needs-recipient"));
                }
            }
        }
        if members.len() < 2 {
            return Err(crate::i18n::t("chat-send-needs-recipient"));
        }
        Ok(Chat::direct_id(members))
    }

    /// The member `recipient` names: by its id, or, named by its name alone (as an agent's send
    /// names one), the workspace's person or agent called that.
    fn member_named(
        &self,
        recipient: &ui::floating_surface::Recipient,
    ) -> Result<MemberId, String> {
        let agent = recipient.kind == ui::floating_surface::RecipientKind::Agent;
        if !recipient.id.is_empty() {
            if agent {
                return Ok(MemberId::agent(recipient.id.clone()));
            }
            let person = ui::tilo_resource::PersonId::parse(&recipient.id)
                .map_err(|_| crate::i18n::t("chat-person-unknown"))?;
            return self.workspace_member(MemberId::Person(person));
        }
        let name = recipient.name.trim().to_lowercase();
        if name.is_empty() {
            return Err(crate::i18n::t("chat-person-unknown"));
        }
        let mut called: Vec<&MemberId> = self
            .workspace_names
            .iter()
            .filter(|(member, named)| {
                named.trim().to_lowercase() == name && member.is_person() != agent
            })
            .map(|(member, _)| member)
            .collect();
        called.sort();
        called
            .into_iter()
            .find_map(|member| {
                if agent {
                    Some(member.clone())
                } else {
                    self.workspace_member(member.clone()).ok()
                }
            })
            .ok_or_else(|| crate::i18n::t("chat-person-unknown"))
    }

    /// The person the direct conversation `channel` is with, when it is one of this window's
    /// person with exactly one other person: what the app's direct operations are asked on. A
    /// conversation with an agent, or one of more than two people, is not one of them.
    fn direct_peer(&self, channel: &crate::model::Channel) -> Option<String> {
        self.conversation_peer(&channel.members)
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

    /// Whether this person can write to their own assistant in this conversation.
    pub(crate) fn can_ask_personal_assistant_in(&self, channel: &str) -> bool {
        if !channel.starts_with(crate::chat::DIRECT_CONVERSATION_PREFIX) || !self.can_participate()
        {
            return false;
        }
        let Some(keeper) = self.me.id.person_id() else {
            return false;
        };
        let Some(agents) = self.agents_record().filter(|record| record.may_read()) else {
            return false;
        };
        agents
            .get()
            .personal_assistant(keeper)
            .is_some_and(|assistant| {
                let peer = MemberId::agent(assistant.id.0.clone());
                Chat::direct_id([self.me.id.clone(), peer]).as_str() == channel
            })
    }

    /// The agent in this person's direct conversation with their assistant. A confined
    /// view without roster access leaves the keeper check to the signed service.
    pub(crate) fn personal_dm(&self) -> Option<String> {
        let keeper = self.me.id.person_id()?;
        let agent = self.conversation_agent()?;
        let dm = Chat::direct_id([self.me.id.clone(), MemberId::agent(&agent)]);
        if self.alone.is_some() || dm.as_str() != self.active_channel {
            return None;
        }
        if let Some(record) = self.agents_record().filter(|record| record.may_read()) {
            let state = record.get();
            if state.personal_assistant(keeper)?.id.0 != agent {
                return None;
            }
        }
        Some(agent)
    }

    pub(crate) fn agents_record(
        &self,
    ) -> Option<ui::prelude::WorkspaceResource<ui::tilo_workspace_agent::WorkspaceAgents>> {
        self.agents
            .as_ref()?
            .open(&ui::resource::workspace::document_id(
                ui::tilo_workspace_agent::KIND,
            ))
    }

    /// The rows this app offers for its conversations with the workspace's
    /// people, so a caller finds them by the operation it wants rather than
    /// by a written address. Every person has one, not only those the
    /// conversation with whom holds a message yet: a caller asks for the
    /// row before it can write the first message. Its messages are read as
    /// any conversation's, from the chat's resource and its parts.
    pub(crate) fn direct_conversation_rows(&self) -> Vec<LiveRow> {
        self.channels
            .iter()
            .filter_map(|channel| {
                let public_key = self.direct_peer(channel)?;
                let bindings = ui::tilo_resource::RowBindings::new()
                    .bind(
                        ChatEffect::OPEN_DIRECT_CONVERSATION,
                        [("public_key", public_key.clone().into())],
                    )
                    .bind(
                        ChatEffect::SEND_DIRECT_MESSAGE,
                        [("public_key", public_key.clone().into())],
                    );
                Some(LiveRow::typed_with::<chat_model::rows::DirectConversation>(
                    format!("chat.conversation:{public_key}"),
                    &channel.name,
                    &bindings,
                ))
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

/// The resource's `post` of `text`, with what it carries, the paragraphs it
/// was written in, and the mentions the window resolved, to the conversation
/// `id`.
fn post(
    id: &Id,
    message: &Id,
    text: &str,
    parent: Option<Id>,
    body: Option<Vec<tilo_document::ClipBlock>>,
    attachments: Vec<tilo_reference::Attached>,
) -> Invoke<ChatMessages> {
    ChatMessages::POST.with(chat_messages::Post {
        message: crate::messages::NewMessage {
            id: Some(message.clone()),
            channel: id.clone(),
            text: text.to_owned(),
            parent,
            quoted: None,
            body,
            attachments,
            mentions: None,
            written: None,
            origin: None,
        },
    })
}

#[cfg(test)]
mod conversation_tests;
