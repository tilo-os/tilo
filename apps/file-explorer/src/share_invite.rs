//! Sharing what Files shows: the workspace's invitation.
//!
//! A folder is shared by the workspace it lives in, so what Files offers on it
//! is the invitation into that workspace — the same one the session's invite
//! sheet makes. It asks the desk through the action broker, as the person using
//! Files ([`session_actions::ISSUE_INVITATION`]), naming the part the link
//! brings someone in at and, for a person the directory knows, that only their
//! device can redeem it. Whether the person may invite at all is the workspace's
//! own governance, decided where the invitation is made.

use tilo_actions::InvocationId;
use tilo_actions::remote::session_actions;
use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget,
};
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::time::Duration;

/// The parts a link can bring someone in at, with what each one is called,
/// the way the invite sheet offers them.
pub const ROLES: [(&str, &str); 4] = [
    ("viewer", "files-share-role-viewer"),
    ("commenter", "files-share-role-commenter"),
    ("editor", "files-share-role-editor"),
    ("moderator", "files-share-role-moderator"),
];

/// The part a link brings someone in at when the person does not pick one,
/// as the invite sheet's own default.
pub const DEFAULT_ROLE: &str = "editor";

/// How long a link Files copied stays on the clipboard: long enough to paste
/// it into a message to whoever it is for, and no longer, because the link is
/// what admits them.
pub const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// What the part `role` is called, in the person's words.
#[must_use]
pub fn role_key(role: &str) -> &'static str {
    ROLES
        .iter()
        .find(|(key, _)| *key == role)
        .map_or("files-share-role-editor", |(_, label)| *label)
}

/// An invitation asked of the desk: the part it brings someone in at, and for
/// a person the workspace knows, the link only their device can redeem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub role: String,
    pub recipient: Option<String>,
}

impl Ask {
    /// An invitation at `role`, for anyone holding the link.
    #[must_use]
    pub fn anyone(role: &str) -> Self {
        Self {
            role: role.to_owned(),
            recipient: None,
        }
    }

    /// An invitation only `person`'s device can redeem.
    #[must_use]
    pub fn person(role: &str, person: &str) -> Self {
        Self {
            role: role.to_owned(),
            recipient: Some(person.to_owned()),
        }
    }
}

/// The link an invitation is handed over as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    pub link: String,
    pub role: String,
}

/// The invitation Files has asked for and not yet been answered, and the
/// requests it makes for the session.
#[derive(Debug, Default)]
pub struct ShareInvite {
    requests: Vec<CollabRequest>,
    asking: Option<(InvocationId, Ask)>,
}

impl ShareInvite {
    /// Ask the desk for an invitation. One at a time: a second ask before the
    /// first is answered says so instead of making a second link.
    pub fn ask(&mut self, ask: Ask) -> Result<(), String> {
        if self.asking.is_some() {
            return Err("An invitation is already being made".to_owned());
        }
        let invocation = InvocationId::new();
        let mut arguments = serde_json::Map::new();
        arguments.insert("role".to_owned(), serde_json::Value::from(ask.role.clone()));
        if let Some(recipient) = &ask.recipient {
            arguments.insert(
                "recipient".to_owned(),
                serde_json::Value::from(recipient.clone()),
            );
        }
        self.requests.push(CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: RemoteActionTarget {
                    provider: session_actions::PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new(
                        session_actions::DESK_RESOURCE,
                        session_actions::ISSUE_INVITATION,
                    ),
                },
                argument: InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            }),
        });
        self.asking = Some((invocation, ask));
        Ok(())
    }

    /// Whether an invitation is still being made, and for what.
    #[must_use]
    pub fn asking(&self) -> Option<&Ask> {
        self.asking.as_ref().map(|(_, ask)| ask)
    }

    /// Requests for the session, oldest first.
    pub fn take_requests(&mut self) -> Vec<CollabRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The desk's answer to the invitation asked for here, when `event` is it.
    pub fn handle_event(&mut self, event: &CollabEvent) -> Option<Result<Invitation, String>> {
        let CollabEvent::RemoteAction { message } = event else {
            return None;
        };
        let invocation = match message {
            RemoteActionMessage::Receipt(receipt) => receipt.invocation,
            RemoteActionMessage::Update { invocation, .. } => *invocation,
            _ => return None,
        };
        let ask = self
            .asking
            .as_ref()
            .filter(|(asked, _)| *asked == invocation)?
            .1
            .clone();
        let answer = match message {
            RemoteActionMessage::Receipt(InvocationReceipt { status, .. }) => match status {
                ReceiptStatus::Accepted => {
                    // Follow it: the desk answers once the workspace has made
                    // the invitation.
                    self.requests.push(CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe {
                            invocation,
                            wait_ms: None,
                            seen: None,
                        },
                    });
                    None
                }
                ReceiptStatus::Completed { output } => Some(invitation(&ask, output)),
                ReceiptStatus::Failed { error } => Some(Err(error.message.clone())),
                ReceiptStatus::Rejected { error } => Some(Err(error.to_string())),
                ReceiptStatus::Cancelled { message } => Some(Err(message
                    .clone()
                    .unwrap_or_else(|| "The invitation was cancelled".to_owned()))),
            },
            RemoteActionMessage::Update { state, .. } => {
                let update = &state.as_ref()?.update;
                match update {
                    tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                        Some(invitation(&ask, output))
                    }
                    tilo_actions::InvocationUpdate::Failed { error, .. } => {
                        Some(Err(error.message.clone()))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if answer.is_some() {
            self.asking = None;
        }
        answer
    }
}

/// The link `output` answers with, as the invitation `ask` asked for.
fn invitation(ask: &Ask, output: &tilo_actions::ActionOutput) -> Result<Invitation, String> {
    output
        .value
        .as_ref()
        .and_then(|value| value.get("link"))
        .and_then(serde_json::Value::as_str)
        .map(|link| Invitation {
            link: link.to_owned(),
            role: ask.role.clone(),
        })
        .ok_or_else(|| "The invitation came back without a link".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::{ActionHostError, InvocationReceipt, InvocationState};
    use ui::collaboration::CollabEvent;

    fn receipt(invocation: InvocationId, status: ReceiptStatus) -> CollabEvent {
        CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt { invocation, status }),
        }
    }

    fn update(invocation: InvocationId, update: InvocationUpdate) -> CollabEvent {
        CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(invocation, update)),
            },
        }
    }

    /// The invocation the desk is asked with, and the arguments it carries.
    fn asked(invite: &mut ShareInvite) -> (InvocationId, serde_json::Value) {
        let requests = invite.take_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            },
        ] = requests.as_slice()
        else {
            panic!("one invocation is asked: {requests:?}");
        };
        assert_eq!(
            request.target,
            RemoteActionTarget {
                provider: session_actions::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(
                    session_actions::DESK_RESOURCE,
                    session_actions::ISSUE_INVITATION,
                ),
            }
        );
        let InvokeArgument::Json(arguments) = &request.argument else {
            panic!("an invitation names its part");
        };
        (
            request.invocation,
            serde_json::Value::Object(arguments.clone()),
        )
    }

    #[test]
    fn an_invitation_is_the_desks_own_action_asked_with_its_part_and_person() {
        let mut invite = ShareInvite::default();
        invite
            .ask(Ask::anyone(DEFAULT_ROLE))
            .expect("nothing is being made");
        let (_, arguments) = asked(&mut invite);
        assert_eq!(arguments, serde_json::json!({"role": "editor"}));

        let mut invite = ShareInvite::default();
        invite
            .ask(Ask::person("viewer", "ab".repeat(32).as_str()))
            .expect("nothing is being made");
        let (_, arguments) = asked(&mut invite);
        assert_eq!(
            arguments,
            serde_json::json!({"role": "viewer", "recipient": "ab".repeat(32)}),
            "a link the directory knows the person for is only their device's"
        );
    }

    #[test]
    fn a_second_invitation_waits_for_the_first_and_follows_a_running_one() {
        let mut invite = ShareInvite::default();
        invite.ask(Ask::anyone(DEFAULT_ROLE)).unwrap();
        let (invocation, _) = asked(&mut invite);
        assert_eq!(
            invite.ask(Ask::anyone("viewer")),
            Err("An invitation is already being made".to_owned())
        );
        assert_eq!(
            invite.handle_event(&receipt(invocation, ReceiptStatus::Accepted)),
            None
        );
        let requests = invite.take_requests();
        let [
            CollabRequest::RemoteAction {
                message:
                    RemoteActionMessage::Observe {
                        invocation: followed,
                        ..
                    },
            },
        ] = requests.as_slice()
        else {
            panic!("a running invitation is followed: {requests:?}");
        };
        assert_eq!(*followed, invocation);
        assert_eq!(
            invite.handle_event(&update(
                invocation,
                InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"link": "tilo://join/ab"})),
                        resources: Vec::new(),
                    },
                },
            )),
            Some(Ok(Invitation {
                link: "tilo://join/ab".into(),
                role: "editor".into(),
            }))
        );
        assert!(invite.asking().is_none(), "the answer settles the ask");
        invite
            .ask(Ask::anyone(DEFAULT_ROLE))
            .expect("the next invitation is made again");
    }

    #[test]
    fn a_refused_invitation_says_why_and_an_answered_one_copied_from_the_receipt() {
        let mut invite = ShareInvite::default();
        invite
            .ask(Ask::person(DEFAULT_ROLE, "ab".repeat(32).as_str()))
            .unwrap();
        let (invocation, _) = asked(&mut invite);
        assert_eq!(
            invite.handle_event(&receipt(
                invocation,
                ReceiptStatus::Rejected {
                    error: ActionHostError::NotPermitted,
                },
            )),
            Some(Err(ActionHostError::NotPermitted.to_string())),
            "a member who may not invite is refused by the workspace"
        );

        let mut invite = ShareInvite::default();
        invite.ask(Ask::anyone(DEFAULT_ROLE)).unwrap();
        let (invocation, _) = asked(&mut invite);
        assert_eq!(
            invite.handle_event(&receipt(
                invocation,
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "invite": "ab".repeat(32),
                            "link": "https://tilo.so/join#ab",
                            "role": "editor",
                            "expires_at_ns": 1,
                        })),
                        resources: Vec::new(),
                    },
                },
            )),
            Some(Ok(Invitation {
                link: "https://tilo.so/join#ab".into(),
                role: "editor".into(),
            }))
        );
    }
}
