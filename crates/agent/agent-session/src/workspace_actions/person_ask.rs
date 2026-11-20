//! An agent asks the person it works for something in its own words
//! ([`tilo_actions::remote::ASK`]): a decision only they make, or something
//! only they know.
//!
//! The question goes where every question an agent puts to its person goes:
//! to the person's own devices ([`super::WorkspaceDocumentsLink::ask_person`]),
//! where it waits beside the agent's requests to allow an action, in the same
//! place and on the same sheet. It names the agent and the agent session it asks in, as
//! the system knows them, never as the agent words them. The invocation is
//! answered accepted at once and succeeds with the person's answer
//! ([`PersonAnswer`]): one of the question's choices, or what they typed. A
//! person who chooses not to answer ends it declined; one who never answers
//! ends it failed once the question expires.

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, Caller, ConfirmationQuestion, ConfirmationReply, InvocationReceipt,
    InvocationState, InvokeRequest, PersonAnswer, PersonAsk, is_ask,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::outcome::rejected;
use super::{Worker, invoke_arguments};

impl Worker {
    /// Put `request` to the person `caller` acts for when it asks them
    /// ([`is_ask`]), answering it accepted, or refused when it cannot be
    /// asked; `None` for any other request.
    pub(super) fn ask_in_words(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        if !is_ask(&request.target) {
            return None;
        }
        let invocation = request.invocation;
        let receipt = match self.put_to_person(workspace_id, caller, request) {
            Ok(()) => InvocationState::new(invocation, InvocationUpdate::Accepted { invocation })
                .receipt(),
            Err(error) => {
                self.log().forget(&invocation);
                rejected(invocation, error)
            }
        };
        self.log().running.remove(&invocation);
        Some(receipt)
    }

    fn put_to_person(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(), ActionCallError> {
        let refused = |reason: &str| ActionCallError::Refused {
            reason: reason.to_owned(),
        };
        // A person asks nobody: the question is an agent's, to the person it works for.
        let Some(agent) = caller.acting().agent_id().map(str::to_owned) else {
            return Err(refused("only an agent asks its person"));
        };
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionCallError::NotPermitted);
        }
        let ask: PersonAsk =
            serde_json::from_value(Value::Object(invoke_arguments(&request.argument)))
                .map_err(|error| refused(&format!("a question: {error}")))?;
        let ask = ask.checked().map_err(|why| refused(&why))?;
        let person = self
            .bounded(caller)
            .map_or_else(|| caller.clone(), |who| who.person_caller);
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id))
            .unwrap_or_else(|| workspace_id.to_string());
        let mut id = [0_u8; 32];
        getrandom::fill(&mut id).map_err(|error| refused(&error.to_string()))?;
        // The agent by the name its key goes by, as History names its changes, and the agent
        // session by the title its client gave it: who asks is the system's to say.
        let device_label = caller
            .agent_called()
            .map_or_else(|| agent.clone(), str::to_owned);
        let title = super::said_invocation_title(request)
            .filter(|title| *title != device_label)
            .unwrap_or_default();
        let question = ConfirmationQuestion {
            id,
            device_label,
            agent,
            workspace,
            title,
            ask: Some(ask),
            ..ConfirmationQuestion::default()
        };
        let asked = self
            .shared
            .link
            .get()
            .and_then(|link| link.ask_person(workspace_id, &person, question))
            .ok_or_else(|| refused("nobody can be asked: no device of your person is reachable"))?;
        let log = std::sync::Arc::clone(&self.shared);
        let invocation = request.invocation;
        asked.then(move |reply| {
            let update = answered(invocation, reply);
            let mut held = log.log.lock_recover();
            if let Some(entry) = held.entries.get_mut(&invocation)
                && !entry.update.is_terminal()
            {
                entry.update = update;
            }
            // Whoever follows the invocation waits for it to move.
            held.moved();
            drop(held);
        });
        Ok(())
    }
}

/// How the invocation `invocation` ends on the person's `reply`.
fn answered(
    invocation: tilo_actions::InvocationId,
    reply: Option<Result<ConfirmationReply, ActionCallError>>,
) -> InvocationUpdate {
    let answer = match reply {
        Some(Ok(ConfirmationReply::Replied(answer))) => PersonAnswer::Answered { answer },
        Some(Ok(ConfirmationReply::Deny)) => PersonAnswer::Declined,
        Some(Ok(ConfirmationReply::AllowOnce) | Err(_)) | None => {
            return InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "unanswered".to_owned(),
                    message: "your person did not answer in time; ask again later, or go on \
                              without their answer"
                        .to_owned(),
                    details: None,
                    retryable: true,
                },
            };
        }
    };
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: serde_json::to_value(answer).ok(),
            resources: Vec::new(),
        },
    }
}
