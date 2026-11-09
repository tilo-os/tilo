//! Sharing what Files shows: the desk's own invite sheet.
//!
//! A folder is shared by the workspace it lives in, so what Files offers on it
//! is the sheet the bar's invite button opens — the one place a link into the
//! workspace is made, the part it brings someone in at picked and the people
//! the directory knows offered. Files asks the desk for it through the action
//! broker, as the person using Files ([`session_actions::SHOW_INVITE`]); whether
//! they may invite at all is the workspace's own governance, decided where the
//! sheet and the invitation are.

use tilo_actions::InvocationId;
use tilo_actions::remote::session_actions;
use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
};
use ui::collaboration::{CollabEvent, CollabRequest};

/// The sheet Files has asked the desk to open and not yet been answered, and
/// the requests it makes for the session.
#[derive(Debug, Default)]
pub struct ShareInvite {
    requests: Vec<CollabRequest>,
    asking: Option<InvocationId>,
}

impl ShareInvite {
    /// Ask the desk to open its invite sheet on the workspace in front. One at
    /// a time: a second ask before the first is answered says so instead of
    /// opening it twice.
    pub fn ask(&mut self) -> Result<(), String> {
        if self.asking.is_some() {
            return Err("The invite sheet is already opening".to_owned());
        }
        let invocation = InvocationId::new();
        self.requests.push(CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: session_actions::target(session_actions::SHOW_INVITE),
                argument: InvokeArgument::Json(serde_json::Map::new()),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            }),
        });
        self.asking = Some(invocation);
        Ok(())
    }

    /// Whether the sheet is still being opened.
    #[must_use]
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Requests for the session, oldest first.
    pub fn take_requests(&mut self) -> Vec<CollabRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The desk's answer to the sheet asked for here, when `event` is it.
    pub fn handle_event(&mut self, event: &CollabEvent) -> Option<Result<(), String>> {
        let CollabEvent::RemoteAction { message } = event else {
            return None;
        };
        let invocation = match message {
            RemoteActionMessage::Receipt(receipt) => receipt.invocation,
            RemoteActionMessage::Update { invocation, .. } => *invocation,
            _ => return None,
        };
        self.asking.filter(|asked| *asked == invocation)?;
        let answer = match message {
            RemoteActionMessage::Receipt(InvocationReceipt { status, .. }) => match status {
                ReceiptStatus::Accepted => {
                    // Follow it: the desk answers once the sheet is up.
                    self.requests.push(CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe {
                            invocation,
                            wait_ms: None,
                            seen: None,
                        },
                    });
                    None
                }
                ReceiptStatus::Completed { .. } => Some(Ok(())),
                ReceiptStatus::Failed { error } => Some(Err(error.message.clone())),
                ReceiptStatus::Rejected { error } => Some(Err(error.to_string())),
                ReceiptStatus::Cancelled { message } => Some(Err(message
                    .clone()
                    .unwrap_or_else(|| "Opening the invite sheet was cancelled".to_owned()))),
            },
            RemoteActionMessage::Update { state, .. } => match &state.as_ref()?.update {
                tilo_actions::InvocationUpdate::Succeeded { .. } => Some(Ok(())),
                tilo_actions::InvocationUpdate::Failed { error, .. } => {
                    Some(Err(error.message.clone()))
                }
                _ => None,
            },
            _ => None,
        };
        if answer.is_some() {
            self.asking = None;
        }
        answer
    }
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

    /// The invocation the desk is asked with, which asks for its invite sheet.
    fn asked(invite: &mut ShareInvite) -> InvocationId {
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
            session_actions::target(session_actions::SHOW_INVITE)
        );
        let InvokeArgument::Json(arguments) = &request.argument else {
            panic!("the sheet is asked for on the workspace in front");
        };
        assert!(
            arguments.is_empty(),
            "the sheet takes the workspace in front"
        );
        request.invocation
    }

    #[test]
    fn sharing_asks_the_desk_for_its_invite_sheet() {
        let mut invite = ShareInvite::default();
        invite.ask().expect("nothing is opening");
        let invocation = asked(&mut invite);
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
            panic!("a sheet still opening is followed: {requests:?}");
        };
        assert_eq!(*followed, invocation);
        assert_eq!(
            invite.handle_event(&update(
                invocation,
                InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
            )),
            Some(Ok(()))
        );
        assert!(!invite.asking(), "the answer settles the ask");
        invite.ask().expect("the sheet opens again");
    }

    #[test]
    fn a_second_ask_waits_and_a_refused_one_says_why() {
        let mut invite = ShareInvite::default();
        invite.ask().unwrap();
        let invocation = asked(&mut invite);
        assert_eq!(
            invite.ask(),
            Err("The invite sheet is already opening".to_owned())
        );
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
        assert!(!invite.asking());
    }
}
