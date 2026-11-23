//! An agent asks the person it works for something only they can answer, in
//! its own words ([`tilo_actions::remote::ASK`]): a decision only they make,
//! or something only they know.
//!
//! The question is put on this device's sheet ([`AppActionConsentPrompter`]),
//! beside every other question their agents' steps raise, and the invocation
//! answers with the one of the question's choices they picked. A person who
//! chooses not to answer ends it declined; one who never
//! answers leaves it waiting, as their answer on the sheet is the invocation's
//! own outcome.
//!
//! Only an agent asks, and only the person it acts for: the caller is this
//! device's own session, handed to the agent by its person. Nothing about a
//! grant changes; the question is no action of any app or record.

use std::sync::Arc;

use tilo_action_policy::MemberId;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    PersonAnswer, PersonAsk, Reservation,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};

use super::AppBrokerState;
use super::invocation_journal::InvocationOwner;
use super::published_invocation::InvocationCaller;
use crate::apps::app_actions::{
    AGENT_ASK, AppActionAnswer, AppActionConsentPrompter, AppActionConsentRequest,
};

impl AppBrokerState {
    /// Put `request` to the person `caller` acts for, when it is an agent
    /// asking them in its own words ([`tilo_actions::remote::is_ask`]):
    /// answered accepted at once, and succeeding with the person's answer
    /// ([`PersonAnswer`]) once they give it. Any other caller, or a request
    /// that is no question, is refused without asking anyone.
    #[expect(
        clippy::too_many_lines,
        reason = "one question's admission, sheet and answer in a single auditable sequence"
    )]
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
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let Some(person) = caller.acting_for_person().map(str::to_owned) else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let Some(workspace) = caller.workspace().map(str::to_owned) else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
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
        let Some(prompter) = self
            .app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())
        else {
            return Pending::ready(Err(refused(
                "nobody is at this desktop to answer the agent's question",
            )));
        };
        // A retry under the same id observes the first question, never asks it
        // again.
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
        let abandon = |error: ActionCallError| {
            if let Ok(mut log) = self.remote_invocations.lock() {
                log.abandon(&request.invocation);
            }
            error
        };
        let Ok(job) = self.invocation_id.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        ) else {
            return Pending::ready(Err(abandon(ActionCallError::Unavailable {
                reason: "the invocation handle space is exhausted".into(),
            })));
        };
        let question = AppActionConsentRequest {
            handle: format!("{AGENT_ASK}{}", request.invocation.0),
            workspace,
            person,
            app_id: agent.clone(),
            agent_session: caller
                .acting()
                .agent_session
                .as_ref()
                .map(|session| session.as_str().to_owned()),
            target_app: String::new(),
            resource_label: String::new(),
            action_label: String::new(),
            kind: tilo_actions::ActionKind::Undoable,
            risk: tilo_actions::RiskLevel::None,
            confirming: false,
            moderation: None,
            scope: caller.agent_name().map(str::to_owned),
            draft: None,
            ask: Some(ask.clone()),
        };
        let handle = question.handle.clone();
        let Some(answer) = prompter.ask(question) else {
            return Pending::ready(Err(abandon(refused(
                "nobody is at this desktop to answer the agent's question",
            ))));
        };
        let runtime = match super::launchable::delivery_runtime() {
            Ok(runtime) => runtime,
            Err(reason) => {
                return Pending::ready(Err(abandon(ActionCallError::Unavailable { reason })));
            }
        };
        if let Ok(mut log) = self.remote_invocations.lock() {
            log.complete(&request.invocation, job);
        }
        let owner = InvocationOwner::Local {
            actor: MemberId::agent(agent),
        };
        let invocation = request.invocation;
        self.record_completion(
            job,
            owner.clone(),
            InvocationUpdate::Accepted { invocation },
        );
        let state = Arc::clone(self);
        let asked = Asking {
            prompter,
            handle,
            answered: false,
        };
        runtime.spawn(async move {
            let mut asked = asked;
            let update = answered(invocation, &ask, answer.await);
            asked.answered = true;
            state.record_completion(job, owner, update);
        });
        Pending::ready(Ok(self.receipt_for(invocation, job)))
    }
}

/// How the invocation `invocation` ends on the person's `answer` to `ask`: with
/// the choice they picked, declined where they chose not to answer, or failed
/// where the sheet went without one of its choices.
fn answered(
    invocation: InvocationId,
    ask: &PersonAsk,
    answer: Result<AppActionAnswer, tokio::sync::oneshot::error::RecvError>,
) -> InvocationUpdate {
    let value = match answer {
        Ok(AppActionAnswer {
            reply: Some(reply), ..
        }) if ask.answered_by(&reply) => PersonAnswer::Answered {
            choice: reply.trim().to_owned(),
        },
        Ok(AppActionAnswer { reply: None, .. }) => PersonAnswer::Declined,
        Ok(AppActionAnswer { reply: Some(_), .. }) | Err(_) => {
            return InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "unanswered".to_owned(),
                    message: "your person did not answer; ask again later, or go on without \
                              their answer"
                        .to_owned(),
                    details: None,
                    retryable: true,
                },
            };
        }
    };
    InvocationUpdate::Succeeded {
        invocation,
        output: tilo_actions::ActionOutput {
            value: serde_json::to_value(value).ok(),
            resources: Vec::new(),
        },
    }
}

/// A question on the sheet, withdrawn unless it was answered.
struct Asking {
    prompter: Arc<dyn AppActionConsentPrompter>,
    handle: String,
    answered: bool,
}

impl Drop for Asking {
    fn drop(&mut self) {
        if !self.answered {
            self.prompter.withdraw(&self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use tilo_actions::InvocationId;
    use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
    use tokio::sync::oneshot;

    use super::*;
    use crate::apps::services::ProcessServices;

    const WORKSPACE: &str = "w1";

    /// The person at this desktop, as the sheet asks them: each question it
    /// was asked, with the answer it is still waiting for.
    #[derive(Default)]
    struct Person {
        asked: Mutex<Vec<AppActionConsentRequest>>,
        answers: Mutex<Vec<oneshot::Sender<AppActionAnswer>>>,
        withdrawn: Mutex<Vec<String>>,
    }

    impl Person {
        fn only(&self) -> AppActionConsentRequest {
            let asked = self.asked.lock().unwrap();
            assert_eq!(asked.len(), 1, "the person is asked once");
            asked[0].clone()
        }

        fn answered(&self, answer: AppActionAnswer) {
            self.answers
                .lock()
                .unwrap()
                .pop()
                .expect("a question is up")
                .send(answer)
                .expect("the asker still waits");
        }
    }

    impl AppActionConsentPrompter for Person {
        fn ask(
            &self,
            request: AppActionConsentRequest,
        ) -> Option<oneshot::Receiver<AppActionAnswer>> {
            let (answer, answered) = oneshot::channel();
            self.asked.lock().unwrap().push(request);
            self.answers.lock().unwrap().push(answer);
            Some(answered)
        }

        fn withdraw(&self, handle: &str) {
            self.withdrawn.lock().unwrap().push(handle.to_owned());
        }
    }

    fn state(person: &Arc<Person>) -> Arc<AppBrokerState> {
        let (state, _bridge) = AppBrokerState::test_state(None, ProcessServices::default());
        state.set_app_action_consent(person.clone());
        state
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
            .expect("an ask is admitted at once")
    }

    /// The invocation's end, once the person's answer lands on the delivery
    /// runtime.
    fn settled(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        id: &InvocationId,
    ) -> InvocationUpdate {
        for _ in 0..500 {
            if let Some(update) = state.action_observe(caller, id).map(|state| state.update)
                && update.is_terminal()
            {
                return update;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the invocation never ended");
    }

    fn asked_update(update: &InvocationUpdate) -> PersonAnswer {
        let InvocationUpdate::Succeeded { output, .. } = update else {
            panic!("the answer is no success: {update:?}");
        };
        serde_json::from_value(output.value.clone().expect("the answer"))
            .expect("a person's answer")
    }

    tilo_test_env::scenario! {
        fn an_agent_asks_the_person_here_and_their_answer_is_its_invocation;

        phase an_agents_question_is_put_on_this_devices_sheet_and_succeeds_with_the_answer;
        {
            let person = Arc::new(Person::default());
            let state = state(&person);
            let agent = caller("mcp-1234");
            let asked = question();
            let receipt = invoked(&state, &agent, &asked).expect("admitted");
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "the question waits on the person");

            let question = person.only();
            assert_eq!(question.handle, format!("{AGENT_ASK}{}", asked.invocation.0));
            assert_eq!(question.person, "ada");
            assert_eq!(question.workspace, WORKSPACE);
            assert_eq!(question.app_id, "mcp-1234");
            let ask = question.ask.expect("a question in the agent's words");
            assert_eq!(ask.question, "Shall I file the budget under Spring?");
            assert_eq!(ask.choices, ["Yes, file it", "Leave it for now"]);

            // The person picks a choice: the invocation succeeds with it.
            person.answered(AppActionAnswer::answered("Yes, file it".to_owned()));
            let update = settled(&state, &agent, &asked.invocation);
            assert_eq!(
                asked_update(&update),
                PersonAnswer::Answered {
                    choice: "Yes, file it".to_owned()
                }
            );

            // A retry under the same id observes it; the person is not asked again.
            let again = invoked(&state, &agent, &asked).expect("the same invocation");
            assert_eq!(again.invocation, asked.invocation);
            assert_eq!(person.asked.lock().unwrap().len(), 1);
        }

        phase a_person_who_declines_ends_the_question_declined;
        {
            let person = Arc::new(Person::default());
            let state = state(&person);
            let agent = caller("mcp-1234");
            let asked = question();
            invoked(&state, &agent, &asked).expect("admitted");
            person.only();
            person.answered(AppActionAnswer::DENY);
            let update = settled(&state, &agent, &asked.invocation);
            assert_eq!(asked_update(&update), PersonAnswer::Declined);
        }

        phase a_sheet_that_goes_without_an_answer_fails_the_question;
        {
            let person = Arc::new(Person::default());
            let state = state(&person);
            let agent = caller("mcp-1234");
            let asked = question();
            invoked(&state, &agent, &asked).expect("admitted");
            person.only();
            drop(person.answers.lock().unwrap().pop());
            let update = settled(&state, &agent, &asked.invocation);
            let InvocationUpdate::Failed { error, .. } = update else {
                panic!("a sheet dropped without an answer fails it: {update:?}");
            };
            assert_eq!(error.code, "unanswered");
        }

        phase only_an_agent_acting_for_the_person_at_this_device_asks;
        {
            let person = Arc::new(Person::default());
            let state = state(&person);
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
                let asked = question();
                assert!(
                    invoked(&state, &caller, &asked).is_err(),
                    "{:?} must not ask this device's person",
                    caller.acting()
                );
            }
            assert!(person.asked.lock().unwrap().is_empty(), "nobody was asked");
        }

        phase a_question_that_is_no_question_is_refused_without_asking;
        {
            let person = Arc::new(Person::default());
            let state = state(&person);
            let agent = caller("mcp-1234");
            for argument in [
                serde_json::json!({"question": "  "}),
                serde_json::json!({"question": "Shall I?", "choices": ["yes", "yes"]}),
                serde_json::json!({"question": "Which dates suit you?"}),
                serde_json::json!({"question": "Shall I?", "choices": ["yes"], "typed": true}),
            ] {
                let asked = request(&argument);
                assert!(matches!(
                    invoked(&state, &agent, &asked),
                    Err(ActionCallError::Refused { .. })
                ));
            }
            assert!(person.asked.lock().unwrap().is_empty(), "nobody was asked");
        }
    }
}
