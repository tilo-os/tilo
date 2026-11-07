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
        bounds: Option<[f64; 4]>,
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
        let target = send_target(workspace, &resolved, attached, text, bounds)?;
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

    /// Whether the shell's send key was pressed since the last pass, to be
    /// asked of the window in front then, with no menu around its box
    /// ([`ui::floating_surface::FloatingSurfaceHostMessage::ShareSelection`]).
    pub(crate) fn take_send_from(&self) -> bool {
        self.send_from.replace(false)
    }
}

/// The one action a send is: Chat's `send` in `workspace`, carrying whom the
/// message goes to, what it carries and its words. Everything else a send
/// does — making an agent, opening where the message landed — is Chat's part
/// of that same action, never a second invocation.
fn send_target(
    workspace: &str,
    to: &[Recipient],
    attached: &[tilo_reference::Attached],
    text: &str,
    bounds: Option<[f64; 4]>,
) -> Result<Target, String> {
    let to = serde_json::to_value(to).map_err(|error| error.to_string())?;
    let attached = serde_json::to_value(attached).map_err(|error| error.to_string())?;
    Ok(Target::Invoke(Box::new(
        InvokeTarget::action(CHAT, workspace, CHAT_WORKSPACE, SEND)
            .with_argument("to", to)
            .with_argument("attached", attached)
            .with_argument("text", text)
            .with_argument("bounds", bounds),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::Attached;

    /// What a box holds reaches Chat as one action naming the recipients, what
    /// the message carries and its words: the app opens the conversation and
    /// posts there from that one action, nothing else.
    #[test]
    fn a_send_is_one_action_for_the_chat_naming_who_what_and_the_words() {
        let ada =
            Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada").with_detail("online");
        let carried = Attached::Text {
            text: "issue 42".into(),
            from: None,
        };
        let Target::Invoke(target) = send_target(
            "ws",
            std::slice::from_ref(&ada),
            std::slice::from_ref(&carried),
            "look",
            Some([210.0, 120.0, 530.0, 380.0]),
        )
        .expect("a send builds") else {
            panic!("a send invokes the chat");
        };
        assert_eq!(target.app, CHAT);
        assert_eq!(target.workspace, "ws");
        assert_eq!(target.address.resource, CHAT_WORKSPACE);
        assert_eq!(target.address.action, SEND);
        assert_eq!(
            target.arguments.get("text"),
            Some(&serde_json::Value::String("look".into()))
        );
        let to: Vec<Recipient> =
            serde_json::from_value(target.arguments["to"].clone()).expect("the recipients");
        assert_eq!(to, [ada]);
        let attached: Vec<Attached> =
            serde_json::from_value(target.arguments["attached"].clone()).expect("what it carries");
        assert_eq!(attached, [carried]);
        assert_eq!(
            target.arguments.get("bounds"),
            Some(&serde_json::json!([210.0, 120.0, 530.0, 380.0])),
            "the box's place goes with it, so the conversation opens there"
        );
    }
}
