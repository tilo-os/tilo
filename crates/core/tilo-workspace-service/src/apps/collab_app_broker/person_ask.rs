//! An agent asks the person it works for something only they can answer, in
//! its own words ([`tilo_actions::remote::ASK`]): a decision only they make,
//! or something only they know.
//!
//! The question waits for them as the call's own asked record: the call is
//! refused with what they are shown, its words and its choices
//! ([`ActionCallError::asks`]), and the dispatch keeps it. Their answer
//! admits the same call again carrying the choice they picked
//! ([`InvokeRequest::approval`]), and the invocation succeeds with it. A
//! person who declines ends it in the dispatch, declined.
//!
//! Only an agent asks, and only the person it acts for: the caller is this
//! device's own session, handed to the agent by its person. Nothing about a
//! grant changes; the question is no action of any app or record.

use std::sync::Arc;

use tilo_action_policy::MemberId;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    PersonAnswer, PersonAsk, Reservation,
};

use super::AppBrokerState;
use super::invocation_journal::InvocationOwner;
use super::published_invocation::InvocationCaller;

impl AppBrokerState {
    /// Put `request` to the person `caller` acts for, when it is an agent
    /// asking them in its own words ([`tilo_actions::remote::is_ask`]):
    /// refused with the question until they answer it, and succeeding with the
    /// choice they picked ([`PersonAnswer`]) when their answer admits it again.
    /// Any other caller, or a request that is no question, is refused without
    /// asking anyone.
    pub(super) fn invoke_person_ask(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let refused = |reason: &str| ActionCallError::Refused {
            reason: reason.to_owned(),
        };
        // A person asks nobody: the question is an agent's, to the person it
        // works for on this device, whose session handed it to the agent.
        let Some(agent) = caller.acting().agent_id().map(str::to_owned) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        if caller.device_public_key().is_some()
            || !super::provided::is_local_host_caller(caller, origin)
            || caller.acting_for_person().is_none()
            || caller.workspace().is_none()
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let ask: PersonAsk = match &request.argument {
            InvokeArgument::Json(arguments) => {
                match serde_json::from_value(serde_json::Value::Object(arguments.clone())) {
                    Ok(ask) => ask,
                    Err(error) => {
                        return Pending::ready(Err(refused(&format!("a question: {error}"))));
                    }
                }
            }
            _ => return Pending::ready(Err(refused("a question asks in JSON"))),
        };
        let ask = match ask.checked() {
            Ok(ask) => ask,
            Err(why) => return Pending::ready(Err(refused(&why))),
        };
        let Some(approval) = request.approval() else {
            return Pending::ready(Err(ActionCallError::asks(AskedFacts {
                words: ask.question,
                choices: ask.choices,
                ..AskedFacts::by(crate::workspaces::asking::asked_by(caller))
            })));
        };
        let Some(choice) = approval.choice.and_then(|picked| ask.choices.get(picked)) else {
            return Pending::ready(Err(refused(
                "an answer to a question names one of its choices",
            )));
        };
        let answer = PersonAnswer::Answered {
            choice: choice.clone(),
        };
        // A retry under the same id observes the first answer.
        let reservation = match self.reserve_invocation(caller, request) {
            Ok(reservation) => reservation,
            Err(error) => return Pending::ready(Err(error)),
        };
        if matches!(reservation, Reservation::Admitted) {
            return Pending::ready(
                self.remote_job(caller, &request.invocation)
                    .map(|job| self.receipt_for(request.invocation, job))
                    .ok_or(ActionCallError::UnknownInvocation),
            );
        }
        let Ok(job) = self.invocation_id.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        ) else {
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the invocation handle space is exhausted".into(),
            }));
        };
        if let Ok(mut log) = self.remote_invocations.lock() {
            log.complete(&request.invocation, job);
        }
        let invocation = request.invocation;
        self.record_completion(
            job,
            InvocationOwner::Local {
                actor: tilo_actions::Actor::from(MemberId::agent(agent)),
            },
            InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: serde_json::to_value(answer).ok(),
                    resources: Vec::new(),
                },
            },
        );
        Pending::ready(Ok(self.receipt_for(invocation, job)))
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::InvocationId;
    use tilo_actions::remote::asked::{Approval, RecordRef};
    use tilo_actions::remote::{Confirmation, InvokeArgument, ReceiptStatus};

    use super::*;
    use crate::apps::services::ProcessServices;

    const WORKSPACE: &str = "w1";

    fn state() -> Arc<AppBrokerState> {
        AppBrokerState::test_state(None, ProcessServices::default()).0
    }

    fn caller(agent: &str) -> Caller {
        Caller::local_person("ada")
            .in_workspace(WORKSPACE)
            .delegated_to(agent)
            .in_new_execution()
    }

    fn request(argument: &serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: tilo_actions::remote::ask_target(),
            argument: InvokeArgument::Json(argument.as_object().cloned().unwrap_or_default()),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn question() -> InvokeRequest {
        request(&serde_json::json!({
            "question": "Shall I file the budget under Spring?",
            "choices": ["Yes, file it", "Leave it for now"],
        }))
    }

    /// `asked`, admitted again on the person's answer picking `choice`.
    fn answered(asked: &InvokeRequest, choice: usize) -> InvokeRequest {
        let mut again = asked.clone();
        again.confirmation = Some(Confirmation::Asked(Approval::new(
            RecordRef {
                workspace: WORKSPACE.to_owned(),
                record: format!("tilo.invocations/{}", asked.invocation.0),
                device: None,
            },
            Some(choice),
        )));
        again
    }

    fn invoked(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("an ask is answered at once")
    }

    tilo_test_env::scenario! {
        fn an_agent_asks_the_person_and_their_choice_is_its_invocation;

        phase an_agents_question_waits_for_the_person_in_its_own_words;
        {
            let state = state();
            let agent = caller("mcp-1234");
            let asked = question();
            let Err(ActionCallError::ConfirmationRequired(facts)) = invoked(&state, &agent, &asked)
            else {
                panic!("a question nobody answered waits for its person");
            };
            assert_eq!(facts.words, "Shall I file the budget under Spring?");
            assert_eq!(facts.choices, ["Yes, file it", "Leave it for now"]);
            assert!(matches!(
                &facts.by,
                tilo_actions::remote::asked::AskedBy::Agent(named) if named.id == "mcp-1234"
            ));
        }

        phase their_answer_admits_it_again_and_it_succeeds_with_their_choice;
        {
            let state = state();
            let agent = caller("mcp-1234");
            let asked = answered(&question(), 1);
            let receipt = invoked(&state, &agent, &asked).expect("admitted on the answer");
            assert!(matches!(receipt.status, ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted));
            let update = state
                .action_observe(&agent, &asked.invocation)
                .expect("the answered question")
                .update;
            let InvocationUpdate::Succeeded { output, .. } = update else {
                panic!("the answer is no success: {update:?}");
            };
            assert_eq!(
                serde_json::from_value::<PersonAnswer>(output.value.expect("the answer")).unwrap(),
                PersonAnswer::Answered {
                    choice: "Leave it for now".to_owned()
                }
            );
            // A retry under the same id observes it.
            let again = invoked(&state, &agent, &asked).expect("the same invocation");
            assert_eq!(again.invocation, asked.invocation);
            // A choice the question did not offer answers nothing.
            assert!(matches!(
                invoked(&state, &agent, &answered(&question(), 2)),
                Err(ActionCallError::Refused { .. })
            ));
        }

        phase only_an_agent_acting_for_the_person_at_this_device_asks;
        {
            let state = state();
            let refused = [
                // A person asks nobody.
                Caller::local_person("ada").in_workspace(WORKSPACE).in_new_execution(),
                // An app answers for the person, but asks nothing of them.
                Caller::local_app("so.tilo.example")
                    .acting_for("ada")
                    .in_workspace(WORKSPACE),
                // An agent another device's session handed to, not this device's.
                Caller::authenticated([7; 32], [8; 32])
                    .in_workspace(WORKSPACE)
                    .delegated_to("mcp-1234")
                    .in_new_execution(),
            ];
            for caller in refused {
                assert!(
                    !matches!(
                        invoked(&state, &caller, &question()),
                        Ok(_) | Err(ActionCallError::ConfirmationRequired(_))
                    ),
                    "{:?} must not ask this device's person",
                    caller.acting()
                );
            }
        }

        phase a_question_that_is_no_question_is_refused_without_asking;
        {
            let state = state();
            let agent = caller("mcp-1234");
            for argument in [
                serde_json::json!({"question": "  "}),
                serde_json::json!({"question": "Shall I?", "choices": ["yes", "yes"]}),
                serde_json::json!({"question": "Which dates suit you?"}),
                serde_json::json!({"question": "Shall I?", "choices": ["yes"], "typed": true}),
            ] {
                assert!(matches!(
                    invoked(&state, &agent, &request(&argument)),
                    Err(ActionCallError::Refused { .. })
                ));
            }
        }
    }
}
