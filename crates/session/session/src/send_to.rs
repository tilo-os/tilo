//! A message a window sends from its own context: the session makes the agents it names, carries
//! the message to Chat as one action the person's own grants decide, and answers the search the box
//! uses to find whom it goes to.
//! The window says what the box held — the recipients, what the message carries
//! ([`tilo_reference::Attached`]) and its words — and nothing of how that becomes a conversation:
//! Chat's `send` opens the conversation, posts what it carries and shows where it landed. Making
//! the agents a send names is the workspace's governance and Chat's record of its agents, both of
//! which only the session holds, so it happens here first, so the message never goes to an agent
//! the workspace does not list.
//! A box opened with the one place a report goes on it is the same send with that place on it: it
//! is filed there as the channel's report ([`crate::feedback`]), one action of the person's own,
//! and nothing opens over the window it is about.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop_search::Target;
use tilo_desktop_search::model::InvokeTarget;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use ui::floating_surface::{Recipient, RecipientKind};

use crate::Shared;

/// The resource a send is published under: one action every sender runs — a person's box, an
/// agent, a script.
pub(crate) const RESOURCE: &str = "session:send-to";

/// What kind of resource [`RESOURCE`] is.
const KIND: &str = "session.send-to";

/// The sends the last passes carried out, newest last, that its reading keeps.
const KEPT: usize = 16;

/// What the resource offers: a send.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.send-to")]
pub(crate) enum SendToAction {
    /// Send to
    ///
    /// Sends the words and what they carry to whom it names, as a person's send-to box does: the
    /// conversation with them opens or continues in Chat, carrying the items. A recipient is a new
    /// agent, an agent, a person or a conversation, by its id, or by its name alone to be found.
    #[effect(job)]
    Send {
        #[param("Who it goes to: kind new-agent, agent, person or conversation, by id or name")]
        to: Vec<Recipient>,
        #[param("What it carries besides its words")]
        attached: Option<Vec<tilo_reference::Attached>>,
        #[param("A resource it carries, by its id as the catalogue lists it")]
        item: Option<String>,
        #[param("The message")]
        text: String,
        #[param("The workspace it is sent in; the one in front without")]
        workspace: Option<String>,
    },
}

/// One send taken, waiting for the pass that carries it out: who asked, and what.
struct Sending {
    /// Who sent it: the app whose window's box it was, an agent, a script; the person sitting
    /// here without.
    by: Option<tilo_actions::ActionInvoker>,
    to: Vec<Recipient>,
    attached: Vec<tilo_reference::Attached>,
    item: Option<String>,
    text: String,
    workspace: Option<String>,
}

/// A send as carried out: what it carried, whom it reached, and whether it landed.
#[derive(Clone, Debug, serde::Serialize)]
struct Sent {
    to: Vec<Recipient>,
    attached: Vec<tilo_reference::Attached>,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<String>,
}

/// The sends taken and carried out, shared by the service and the pass.
#[derive(Clone, Default)]
pub(crate) struct Sends {
    waiting: Rc<RefCell<Vec<Sending>>>,
    sent: Rc<RefCell<Vec<Sent>>>,
}

/// The send action as a service: it takes each send, and the session's pass carries it out
/// ([`Shared::deliver_sends`]), since a send makes agents and reaches Chat through the doors.
pub(crate) struct SendToService {
    sends: Sends,
}

impl SendToService {
    pub(crate) fn new(sends: Sends) -> Self {
        Self { sends }
    }

    fn take(
        &self,
        by: Option<tilo_actions::ActionInvoker>,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let SendToAction::Send {
            to,
            attached,
            item,
            text,
            workspace,
        } = decode::<SendToAction>(&self.resources(), RESOURCE, "send", arguments)?;
        let attached = attached.unwrap_or_default();
        if text.trim().is_empty() && attached.is_empty() && item.is_none() {
            return Err(DoorError::InvalidArguments(
                "a message needs words or something to carry".into(),
            ));
        }
        self.sends.waiting.borrow_mut().push(Sending {
            by,
            to,
            attached,
            item,
            text,
            workspace,
        });
        Ok(Started::Accepted)
    }
}

impl Service for SendToService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<SendToAction>(ResourceSpec::new(
            RESOURCE,
            KIND,
            crate::i18n::t("send-to-resource"),
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&*self.sends.sent.borrow()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        self.take(None, arguments)
    }

    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        self.take(Some(invoker.clone()), arguments)
    }
}

/// The app that keeps a workspace's conversations.
pub(crate) const CHAT: &str = "so.tilo.chat";

/// The resource Chat publishes a workspace's conversations under.
const CHAT_WORKSPACE: &str = "chat.workspace";

/// The action that sends a message to whom it names.
pub(crate) const SEND: &str = "send";

impl Shared {
    /// Carry out the sends taken since the last pass ([`SendToService`]): each item named by its
    /// id is carried as the record it is, each recipient named only by name is found by the one
    /// search, and the send goes as one action of the sender's own.
    pub(crate) fn deliver_sends(&self) {
        let waiting = std::mem::take(&mut *self.sends.waiting.borrow_mut());
        for sending in waiting {
            let workspace = sending
                .workspace
                .clone()
                .unwrap_or_else(|| self.active_workspace_id());
            let mut attached = sending.attached;
            let found = sending.item.as_deref().map(|item| self.carried(item));
            let refused = match found {
                Some(Err(missing)) => Some(missing),
                Some(Ok(item)) => {
                    attached.push(item);
                    None
                }
                None => None,
            };
            let to: Vec<Recipient> = sending
                .to
                .into_iter()
                .map(|recipient| self.found_recipient(recipient))
                .collect();
            let refused = refused.or_else(|| {
                self.send_to(
                    sending.by.as_ref(),
                    &workspace,
                    &to,
                    &attached,
                    &sending.text,
                )
                .err()
            });
            if let Some(reason) = &refused {
                tracing::warn!(%reason, %workspace, "a send did not land");
            }
            let mut sent = self.sends.sent.borrow_mut();
            sent.push(Sent {
                to,
                attached,
                text: sending.text,
                refused,
            });
            let over = sent.len().saturating_sub(KEPT);
            sent.drain(..over);
        }
    }

    /// The resource `item` names, as a message carries it: the record of the app that publishes
    /// it, found in the catalogue every caller surveys.
    fn carried(&self, item: &str) -> Result<tilo_reference::Attached, String> {
        fn find<'a>(resources: &'a [ResourceSpec], item: &str) -> Option<&'a ResourceSpec> {
            resources.iter().find_map(|resource| {
                (resource.id.as_str() == item)
                    .then_some(resource)
                    .or_else(|| find(&resource.children, item))
            })
        }
        let resources = self.doors.resources();
        let resource =
            find(&resources, item).ok_or_else(|| format!("nothing is published as {item}"))?;
        let app = resource
            .app()
            .ok_or_else(|| format!("{item} is no app's resource"))?;
        Ok(tilo_reference::Attached::resource(
            tilo_reference::ResourceAt {
                app: app.to_owned(),
                record: None,
                reference: tilo_reference::ResourceRef::whole(
                    resource.kind.clone(),
                    item,
                    resource.label.clone(),
                ),
            },
            None,
            resource.label.clone(),
        ))
    }

    /// `recipient` as a send names it: one named only by its name is the first of its kind the
    /// one search finds by that name, and one with an id or a new agent is itself.
    fn found_recipient(&self, recipient: Recipient) -> Recipient {
        if !recipient.id.is_empty() || recipient.kind == RecipientKind::NewAgent {
            return recipient;
        }
        self.recipients(&recipient.name)
            .into_iter()
            .find(|found| found.kind == recipient.kind)
            .unwrap_or(recipient)
    }

    /// Send what a window's box holds: an agent named as a new one is made in
    /// the workspace and kept by this person, and the message reaches Chat as
    /// one action — `chat.workspace#send` — so one change carries every
    /// recipient, what it is about and the words. A report is filed where it
    /// goes, and nothing opens over the window it is about.
    ///
    /// # Errors
    /// The workspace is not open here, an agent could not be made or admitted,
    /// or Chat refused the send.
    fn send_to(
        &self,
        by: Option<&tilo_actions::ActionInvoker>,
        workspace: &str,
        to: &[Recipient],
        attached: &[tilo_reference::Attached],
        text: &str,
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() && attached.is_empty() {
            return Err("a message needs words or something to carry".into());
        }
        // A window's box sends as the person at it; an agent or a script as itself.
        let agent = by.filter(|by| {
            !matches!(
                by.kind,
                tilo_actions::ActorKind::App | tilo_actions::ActorKind::Person
            )
        });
        let (workspace, target) = match addressed(self.feedback.as_ref(), workspace, to)? {
            Addressed::Message { workspace, to } => {
                let resolved = self.recipients_of(&workspace, &to, &agent_name(text, attached))?;
                // Only a person's box opens the conversation where it stood.
                let target = send_target(&workspace, &resolved, attached, text, agent.is_none())?;
                (workspace, target)
            }
            // A report is read where it lands, not floated over the window it was written over: it
            // is filed in the channel and nothing opens.
            Addressed::Report(destination) => {
                let from = by.map_or(CHAT, |by| by.actor.as_str());
                let report = destination.report(from, attached, text)?;
                (
                    destination.workspace.clone(),
                    report_target(&destination, &report)?,
                )
            }
        };
        self.search
            .perform(&workspace, target, &mut |own| {
                let look = &mut tilo_session_core::door::Look::none();
                match agent {
                    Some(by) => self
                        .doors
                        .invoke_as(&by.actor, &own.address, &own.arguments, look),
                    None => self.doors.invoke(&own.address, &own.arguments, look),
                }
                .map(drop)
            })
            .map_err(|error| error.to_string())
    }

    /// Who a message goes to, as the names a box holds: an agent named as a
    /// new one is made here first, so the message never goes to an agent the
    /// workspace does not list. A new agent the person did not name is named
    /// `asked`, by what it is asked to do.
    ///
    /// # Errors
    /// A new agent could not be made or admitted, or nobody is left to go to.
    fn recipients_of(
        &self,
        workspace: &str,
        to: &[Recipient],
        asked: &str,
    ) -> Result<Vec<Recipient>, String> {
        let feed = self.workspace_settings_feed();
        let mut resolved: Vec<Recipient> = Vec::with_capacity(to.len());
        for recipient in to {
            match recipient.kind {
                RecipientKind::NewAgent => {
                    // A new agent takes the workspace's default model and the colour its identity
                    // gives it.
                    let name = match recipient.name.trim() {
                        "" => asked,
                        named => named,
                    };
                    let made = feed.make_agent(Some(workspace), name, "", None)?;
                    resolved.push(Recipient::found(RecipientKind::Agent, made, name));
                }
                RecipientKind::Agent
                | RecipientKind::Person
                | RecipientKind::Conversation
                | RecipientKind::Feedback => {
                    resolved.push(recipient.clone());
                }
            }
        }
        if resolved.is_empty() {
            return Err("a message needs someone to go to".into());
        }
        Ok(resolved)
    }

    /// The one place a report goes, as the chip its box opens with: nothing when this computer is
    /// told nowhere.
    pub(crate) fn feedback_recipient(&self) -> Option<Recipient> {
        self.feedback
            .as_ref()
            .map(crate::feedback::Destination::recipient)
    }

    /// Whether the shell asked to report a problem since the last pass, to be asked of the window
    /// in front then, to open its box to the one place reports go.
    pub(crate) fn take_report_from(&self) -> bool {
        self.report_from.replace(false)
    }

    /// The agents, people and conversations `query` names, as a window's box lists them: found by
    /// the one search every part of the desk shares.
    pub(crate) fn recipients(&self, query: &str) -> Vec<Recipient> {
        self.search.recipients(query)
    }

    /// Whether the shell's send key was pressed since the last pass, to be asked of the window in
    /// front then, with no menu around its box
    /// ([`ui::floating_surface::FloatingSurfaceHostMessage::ShareSelection`]).
    pub(crate) fn take_send_from(&self) -> bool {
        self.send_from.replace(false)
    }
}

/// The most characters a new agent's name takes from what it is asked.
const AGENT_NAME_CHARS: usize = 40;

/// What a new agent nobody named is called: the first words it is asked, else what it is sent,
/// so the many agents a person makes from boxes are told apart in every list and search.
fn agent_name(text: &str, attached: &[tilo_reference::Attached]) -> String {
    let asked = text.lines().map(str::trim).find(|line| !line.is_empty());
    let source = asked
        .or_else(|| {
            attached
                .iter()
                .map(|attached| attached.label().trim())
                .find(|label| !label.is_empty())
        })
        .unwrap_or_default();
    let mut name = String::new();
    for word in source.split_whitespace() {
        let wanted = name
            .chars()
            .count()
            .saturating_add(usize::from(!name.is_empty()))
            .saturating_add(word.chars().count());
        if wanted > AGENT_NAME_CHARS {
            if name.is_empty() {
                name = word
                    .chars()
                    .take(AGENT_NAME_CHARS.saturating_sub(1))
                    .collect();
            }
            name.push('…');
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    let mut letters = name.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => crate::i18n::t("send-to-agent-unnamed"),
    }
}

/// Where a box's send goes, as it named whom it goes to: a report goes to the one place this
/// computer sends reports, and is filed there; any other send is a message in the workspace the box
/// was opened in.
enum Addressed {
    Message {
        workspace: String,
        to: Vec<Recipient>,
    },
    Report(crate::feedback::Destination),
}

/// Address a box's send by whom it named: the one place this computer sends
/// reports, or a message in the workspace it was opened in.
///
/// # Errors
/// A report stood beside another recipient, or this computer is told nowhere
/// to send one.
fn addressed(
    feedback: Option<&crate::feedback::Destination>,
    workspace: &str,
    to: &[Recipient],
) -> Result<Addressed, String> {
    if !to
        .iter()
        .any(|recipient| recipient.kind == RecipientKind::Feedback)
    {
        return Ok(Addressed::Message {
            workspace: workspace.to_owned(),
            to: to.to_vec(),
        });
    }
    if to.len() > 1 {
        return Err("a report goes to the one place it was opened to".to_owned());
    }
    let destination =
        feedback.ok_or_else(|| "this computer is told nowhere to send a report".to_owned())?;
    Ok(Addressed::Report(destination.clone()))
}

/// The one action a report is: the channel's `submit-feedback` in the workspace that keeps it,
/// carrying the report as its review filed it — nothing else, and no window opening over the one
/// reported.
fn report_target(
    destination: &crate::feedback::Destination,
    report: &tilo_feedback::Submission,
) -> Result<Target, String> {
    let report = serde_json::to_value(report).map_err(|error| error.to_string())?;
    Ok(Target::Invoke(Box::new(
        InvokeTarget::action(
            CHAT,
            &destination.workspace,
            destination.item(),
            tilo_feedback::destination::SUBMIT,
        )
        .with_argument("submission", report),
    )))
}

/// The one action a send is: Chat's `send` in `workspace`, carrying whom the
/// message goes to, what it carries and its words. Everything else a send
/// does — making an agent, opening where the message landed — is Chat's part
/// of that same action, never a second invocation, and where the window that
/// sent it stands is this desk's own: the session opens the conversation
/// there, from what the action answers.
fn send_target(
    workspace: &str,
    to: &[Recipient],
    attached: &[tilo_reference::Attached],
    text: &str,
    opens: bool,
) -> Result<Target, String> {
    let to = serde_json::to_value(to).map_err(|error| error.to_string())?;
    let attached = serde_json::to_value(attached).map_err(|error| error.to_string())?;
    Ok(Target::Invoke(Box::new(
        InvokeTarget::action(CHAT, workspace, CHAT_WORKSPACE, SEND)
            .with_argument("to", to)
            .with_argument("attached", attached)
            .with_argument("text", text)
            .with_argument("opens", opens),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::Attached;

    /// What a box holds reaches Chat as one action naming the recipients, what the message carries
    /// and its words: the app opens the conversation and posts there from that one action, nothing
    /// else.
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
            true,
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
            target.arguments.get("opens"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            target.arguments.get("bounds"),
            None,
            "where the box stood is the desk's to place the conversation by, not the chat's"
        );
    }
    /// A new agent nobody named is called by what it is asked, else by what it is sent, so a
    /// person's many agents are told apart.
    #[test]
    fn a_new_agent_is_named_by_what_it_is_asked() {
        let task = Attached::Text {
            text: "Launch checklist".into(),
            from: None,
        };
        assert_eq!(
            agent_name(
                "split this into subtasks\nand label them",
                std::slice::from_ref(&task)
            ),
            "Split this into subtasks"
        );
        assert_eq!(
            agent_name("  ", std::slice::from_ref(&task)),
            "Launch checklist"
        );
        let long = agent_name(
            "summarise every review comment on the release notes and draft replies",
            &[],
        );
        assert_eq!(long, "Summarise every review comment on the…");
        assert!(long.chars().count() <= AGENT_NAME_CHARS + 1);
    }

    /// A report's chip is the one place this computer sends reports: one send
    /// is one report, filed in the channel the destination names in the
    /// workspace that keeps it, carrying what the window picked and the words,
    /// and nothing opens over the window it is about.
    #[test]
    fn a_report_is_one_signed_report_filed_where_the_destination_names() {
        let destination = crate::feedback::Destination {
            workspace: "w_tilo".to_owned(),
            channel: "c_feedback".to_owned(),
        };
        let report = destination.recipient();
        let Addressed::Report(place) =
            addressed(Some(&destination), "w_notes", std::slice::from_ref(&report))
                .expect("a report is addressed")
        else {
            panic!("a report is not a message");
        };
        assert_eq!(place.workspace, "w_tilo", "in the workspace that keeps it");
        assert_eq!(place.channel, "c_feedback");
        let carried = Attached::Text {
            text: "issue 42".into(),
            from: None,
        };
        let filed = place
            .report(
                "so.tilo.notes",
                std::slice::from_ref(&carried),
                "the button does nothing",
            )
            .expect("a report builds");
        let Target::Invoke(target) = report_target(&place, &filed).expect("a report targets")
        else {
            panic!("a report invokes the channel");
        };
        assert_eq!(target.app, CHAT);
        assert_eq!(target.workspace, "w_tilo");
        assert_eq!(
            target.address.resource, "chat.workspace/channels/c_feedback",
            "the channel's own item files reports"
        );
        assert_eq!(target.address.action, "submit-feedback");
        let submission: tilo_feedback::Submission =
            serde_json::from_value(target.arguments["submission"].clone())
                .expect("the reviewed report");
        assert_eq!(
            submission.destination.room, "w_tilo",
            "the report says where it is filed"
        );
        assert_eq!(submission.destination.channel, "c_feedback");
        assert_eq!(submission.feedback.text, "the button does nothing");
        let picked = &submission.feedback.selections[0];
        assert_eq!(
            picked.app, "so.tilo.notes",
            "what the window picked came from the window reported"
        );
        assert_eq!(
            picked.texts[0].parts[0].text, "issue 42",
            "and its words go with the report"
        );

        // A report goes to the one place it goes, and a computer told nowhere sends none.
        let ada = Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada");
        assert!(
            addressed(
                Some(&destination),
                "w_notes",
                &[report.clone(), ada.clone()]
            )
            .is_err(),
            "a report stands beside no other recipient"
        );
        assert!(
            addressed(None, "w_notes", std::slice::from_ref(&report)).is_err(),
            "a computer told nowhere to send a report sends none"
        );
        // Every other send is a message in the workspace its box was opened in, to whom it named.
        let Addressed::Message { workspace, to } =
            addressed(None, "w_notes", std::slice::from_ref(&ada)).expect("an ordinary send")
        else {
            panic!("an ordinary send is a message");
        };
        assert_eq!(workspace, "w_notes");
        assert_eq!(to, [ada], "and goes to whom it named");
    }
}
