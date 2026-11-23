//! An agent asks the person it works for something in its own words
//! ([`tilo_actions::remote::ASK`]): a decision only they make, or something
//! only they know.
//!
//! The question is the call itself, kept as its asked invocation record like
//! every call that waits for the person's yes, with the agent's words and its
//! choices ([`super::asking`]). It names the agent as the system knows it,
//! never as the agent words itself. The person's approve picks one of the
//! choices and admits the call again, which then succeeds with that choice
//! ([`PersonAnswer`]); their decline ends it declined.

use serde_json::Value;
use tilo_actions::remote::asked::{AskedBy, AskedFacts, Named};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvocationState, InvokeRequest, PersonAnswer,
    PersonAsk, is_ask,
};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_server_protocol::WorkspaceId;

use super::outcome::rejected;
use super::{Worker, invoke_arguments};

impl Worker {
    /// Answer `request` when it asks the person `caller` acts for
    /// ([`is_ask`]): with the choice they approved, else kept as its asked
    /// record, or refused when it cannot be asked; `None` for any other
    /// request.
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
        let receipt = match self.question(workspace_id, caller, request) {
            Ok((ask, agent)) => match request.approval() {
                Some(approval) => {
                    let update = chosen(invocation, &ask, approval.choice);
                    if let Some(entry) = self.log().entries.get_mut(&invocation) {
                        entry.update = update.clone();
                    }
                    InvocationState::new(invocation, update).receipt()
                }
                None => {
                    let facts = AskedFacts {
                        words: ask.question.clone(),
                        choices: ask.choices.clone(),
                        conversation: caller.turn().map(|turn| turn.conversation.clone()),
                        ..AskedFacts::by(AskedBy::Agent(agent))
                    };
                    self.keep_asked(workspace_id, caller, request, facts)
                }
            },
            Err(error) => {
                self.log().forget(&invocation);
                rejected(invocation, error)
            }
        };
        self.log().running.remove(&invocation);
        Some(receipt)
    }

    /// The question `request` puts, and the agent that asks it.
    fn question(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(PersonAsk, Named), ActionCallError> {
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
        // The agent by the name its key goes by, as History names its changes.
        let name = caller
            .agent_called()
            .map_or_else(|| agent.clone(), str::to_owned);
        Ok((ask, Named { id: agent, name }))
    }
}

/// How `invocation` ends on the person's approve of `ask`, picking `choice`: that choice, or
/// refused when it names none of the question's.
fn chosen(
    invocation: tilo_actions::InvocationId,
    ask: &PersonAsk,
    choice: Option<usize>,
) -> InvocationUpdate {
    let Some(choice) = choice.and_then(|at| ask.choices.get(at)) else {
        return InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "invalid_request".to_owned(),
                message: "the answer picks none of the question's choices".to_owned(),
                details: None,
                retryable: false,
            },
        };
    };
    let answer = PersonAnswer::Answered {
        choice: choice.trim().to_owned(),
    };
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: serde_json::to_value(answer).ok(),
            resources: Vec::new(),
        },
    }
}
