//! A message a window sends from its own context: the session makes the
//! agents it names, carries the message to Chat as one action the person's
//! own grants decide, and answers the search the box uses to find whom it
//! goes to.
//!
//! The window says what the box held — the recipients, what the message
//! carries ([`tilo_reference::Attached`]) and its words — and nothing of how
//! that becomes a conversation: Chat's `send` opens the conversation, posts
//! what it carries and shows where it landed. Making the agents a send names
//! is the workspace's governance and Chat's record of its agents, both of
//! which only the session holds, so it happens here first, so the message
//! never goes to an agent the workspace does not list.

use tilo_desktop_search::Target;
use tilo_desktop_search::model::InvokeTarget;
use ui::floating_surface::{Recipient, RecipientKind};

use crate::Shared;

/// The app that keeps a workspace's conversations.
const CHAT: &str = "so.tilo.chat";

/// The resource Chat publishes a workspace's conversations under.
const CHAT_WORKSPACE: &str = "chat.workspace";

/// The action that sends a message to whom it names.
pub(crate) const SEND: &str = "send";

impl Shared {
    /// Send what a window's box holds: an agent named as a new one is made in
    /// the workspace and kept by this person, and the message reaches Chat as
    /// one action — `chat.workspace#send` — so one change carries every
    /// recipient, what it is about and the words.
    ///
    /// # Errors
    /// The workspace is not open here, an agent could not be made or admitted,
    /// or Chat refused the send.
    pub(crate) fn send_to(
        &self,
        workspace: &str,
        to: &[Recipient],
        attached: &[tilo_reference::Attached],
        text: &str,
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() && attached.is_empty() {
            return Err("a message needs words or something to carry".into());
        }
        let feed = self.workspace_settings_feed();
        let mut resolved: Vec<Recipient> = Vec::with_capacity(to.len());
        for recipient in to {
            match recipient.kind {
                RecipientKind::NewAgent => {
                    // A new agent takes the workspace's default model and the colour
                    // its identity gives it.
                    let made = feed.make_agent(Some(workspace), &recipient.name, "", None)?;
                    resolved.push(Recipient::found(
                        RecipientKind::Agent,
                        made.clone(),
                        recipient.name.clone(),
                    ));
                }
                RecipientKind::Agent | RecipientKind::Person | RecipientKind::Conversation => {
                    resolved.push(recipient.clone());
                }
            }
        }
        if resolved.is_empty() {
            return Err("a message needs someone to go to".into());
        }
        let to = serde_json::to_value(&resolved).map_err(|error| error.to_string())?;
        let attached = serde_json::to_value(attached).map_err(|error| error.to_string())?;
        let target = Target::Invoke(Box::new(
            InvokeTarget::action(CHAT, workspace, CHAT_WORKSPACE, SEND)
                .with_argument("to", to)
                .with_argument("attached", attached)
                .with_argument("text", text),
        ));
        self.search
            .perform(workspace, target, &mut |own| {
                self.doors
                    .invoke(
                        &own.address,
                        &own.arguments,
                        &mut tilo_surface_host::Look::none(),
                    )
                    .map(drop)
            })
            .map_err(|error| error.to_string())
    }

    /// The agents, people and conversations `query` names, as a window's box
    /// lists them: found by the one search every part of the desk shares.
    pub(crate) fn recipients(&self, query: &str) -> Vec<Recipient> {
        self.search.recipients(query)
    }
}
