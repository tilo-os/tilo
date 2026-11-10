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

use tilo_desktop_search::Target;
use tilo_desktop_search::model::InvokeTarget;
use ui::floating_surface::{Recipient, RecipientKind};

use crate::Shared;

/// The app that keeps a workspace's conversations.
pub(crate) const CHAT: &str = "so.tilo.chat";

/// The resource Chat publishes a workspace's conversations under.
const CHAT_WORKSPACE: &str = "chat.workspace";

/// The action that sends a message to whom it names.
pub(crate) const SEND: &str = "send";

impl Shared {
    /// Send what a window's box holds: an agent named as a new one is made in
    /// the workspace and kept by this person, and the message reaches Chat as
    /// one action — `chat.workspace#send` — so one change carries every
    /// recipient, what it is about and the words. A report is filed where it
    /// goes, and nothing opens over the window it is about.
    ///
    /// # Errors
    /// The workspace is not open here, an agent could not be made or admitted,
    /// or Chat refused the send.
    pub(crate) fn send_to(
        &self,
        app: &str,
        workspace: &str,
        to: &[Recipient],
        attached: &[tilo_reference::Attached],
        text: &str,
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() && attached.is_empty() {
            return Err("a message needs words or something to carry".into());
        }
        let (workspace, target) = match addressed(self.feedback.as_ref(), workspace, to)? {
            Addressed::Message { workspace, to } => {
                let resolved = self.recipients_of(&workspace, &to)?;
                let target = send_target(&workspace, &resolved, attached, text)?;
                (workspace, target)
            }
            // A report is read where it lands, not floated over the window it was written over: it
            // is filed in the channel and nothing opens.
            Addressed::Report(destination) => {
                let report = destination.report(app, attached, text)?;
                (
                    destination.workspace.clone(),
                    report_target(&destination, &report)?,
                )
            }
        };
        self.search
            .perform(&workspace, target, &mut |own| {
                self.doors
                    .invoke(
                        &own.address,
                        &own.arguments,
                        &mut tilo_session_core::door::Look::none(),
                    )
                    .map(drop)
            })
            .map_err(|error| error.to_string())
    }

    /// Who a message goes to, as the names a box holds: an agent named as a
    /// new one is made here first, so the message never goes to an agent the
    /// workspace does not list.
    ///
    /// # Errors
    /// A new agent could not be made or admitted, or nobody is left to go to.
    fn recipients_of(&self, workspace: &str, to: &[Recipient]) -> Result<Vec<Recipient>, String> {
        let feed = self.workspace_settings_feed();
        let mut resolved: Vec<Recipient> = Vec::with_capacity(to.len());
        for recipient in to {
            match recipient.kind {
                RecipientKind::NewAgent => {
                    // A new agent takes the workspace's default model and the colour its identity
                    // gives it.
                    let made = feed.make_agent(Some(workspace), &recipient.name, "", None)?;
                    resolved.push(Recipient::found(
                        RecipientKind::Agent,
                        made.clone(),
                        recipient.name.clone(),
                    ));
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
) -> Result<Target, String> {
    let to = serde_json::to_value(to).map_err(|error| error.to_string())?;
    let attached = serde_json::to_value(attached).map_err(|error| error.to_string())?;
    Ok(Target::Invoke(Box::new(
        InvokeTarget::action(CHAT, workspace, CHAT_WORKSPACE, SEND)
            .with_argument("to", to)
            .with_argument("attached", attached)
            .with_argument("text", text),
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
            None,
            "where the box stood is the desk's to place the conversation by, not the chat's"
        );
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
