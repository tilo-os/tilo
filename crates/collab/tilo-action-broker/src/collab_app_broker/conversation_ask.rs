//! A conversation turn's action that asks first is put to the person on the
//! system's own sheet, and the turn waits for their answer.
//!
//! A turn has no Tracker issue to hang a question on
//! (`super::tracker_runs`), so its question goes to the person on the sheet
//! the system draws ([`AppActionConsentPrompter::ask`], the sheet an
//! application's grant question and an agent's confirmation already use).
//! The question lives here, keyed by the run, the action and the exact
//! arguments, so the agent's re-asks find it:
//!
//! - still waiting: [`tilo_actions::draft::AWAITING_CONFIRMATION`] again;
//!   the child does not start, and the agent asks again with a hold;
//! - confirmed: the child runs with the arguments the person confirmed, as
//!   their edits make them, admitted as confirmed, and the answer is spent;
//! - declined: [`tilo_actions::draft::DECLINED`], in the person's words.
//!
//! The person answers on the broker's sheet while the agent's hold waits.
//! The sheet announces the answer ([`tilo_identity_host::policy_change`]),
//! which is what wakes the held child to be decided again: a held re-ask
//! never polls a clock. The answer itself is read when the step is decided,
//! so an ask that arrives after the person answered still finds it.

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_actions::{Draft, DraftEdit};

use super::native_task_confirmation::NativeGate;
use super::{AppBrokerState, PendingTaskRun, PublishedAction};
use crate::app_actions::AppActionConsentRequest;

/// One question a conversation turn asks its person.
struct ConversationAsk {
    /// The run it belongs to, by its task id: the question goes when the run
    /// does.
    task: String,
    /// The sheet's answer, read when the step is decided again: the person
    /// answered it, or the sheet closed without one.
    answer: tokio::sync::oneshot::Receiver<crate::app_actions::AppActionAnswer>,
}

/// What the person answered a conversation turn's question.
enum Answered {
    Confirmed(Vec<DraftEdit>),
    Declined,
}

/// The questions conversation turns wait on.
#[derive(Default)]
pub(super) struct ConversationAsks {
    questions: Mutex<HashMap<String, ConversationAsk>>,
}

impl ConversationAsks {
    /// What the person answered `key`, if they have: read here rather than
    /// taken by a thread of its own, so an ask made after their answer never
    /// misses it. A sheet that closed unanswered takes its question with it.
    fn take_answered(&self, key: &str) -> Option<Answered> {
        let mut questions = self.questions.lock().ok()?;
        let answer = questions.get_mut(key)?.answer.try_recv();
        match answer {
            Ok(answer) if answer.allowed() => {
                questions.remove(key);
                Some(Answered::Confirmed(answer.edits))
            }
            Ok(_) => {
                questions.remove(key);
                Some(Answered::Declined)
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                questions.remove(key);
                None
            }
        }
    }

    /// Whether no question waits now.
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.questions
            .lock()
            .is_ok_and(|questions| questions.is_empty())
    }

    fn remove(&self, key: &str) {
        if let Ok(mut questions) = self.questions.lock() {
            questions.remove(key);
        }
    }

    fn retain_running(&self, runs: &[crate::services::TrackerRun]) {
        let Ok(mut questions) = self.questions.lock() else {
            return;
        };
        questions.retain(|_, question| {
            runs.iter()
                .any(|run| run.issue == question.task && run.active)
        });
    }
}

/// What a conversation's ask decides for one step, whichever door the step
/// came in by: an app action's child and a turn's own host step wait on the
/// same question, on the same sheet.
pub(super) enum AskGate {
    /// Nothing asks first: the step runs as asked.
    Proceed,
    /// The person confirmed it: with these arguments, as they changed them.
    Confirmed {
        argument: String,
        confirmation: String,
    },
    /// The question stands on the person's sheet: nothing runs until they
    /// answer there.
    Held { reason: String },
    /// The person declined it, in their words.
    Declined { reason: String },
}

impl AppBrokerState {
    /// [`super::native_confirmation_for`] for a run that answers a person's
    /// message in a conversation: the question goes to the system's sheet.
    pub(super) fn conversation_confirmation(
        &self,
        run: &PendingTaskRun,
        lease: &str,
        (app, offered): (&str, &PublishedAction),
        (resource, action): (&str, &str),
        argument: Option<&str>,
    ) -> Result<NativeGate, String> {
        let descriptor = offered.descriptor(tilo_actions::ExecutionLocus::ClientLocal);
        let mut current_binding = run.native.clone();
        current_binding.revision = runtime_collab::native_task_revision(lease)
            .ok_or("invalid native lease")?
            .1;
        let app = app.to_owned();
        let gate = self.conversation_gate(
            &run.task_id,
            &descriptor,
            (resource, action),
            argument,
            |contract_argument| {
                let current = self.tracker_run_of(run)?;
                let requirement = crate::services::NativeTaskActionRequirement {
                    binding: current_binding.clone(),
                    app: app.clone(),
                    resource_kind: descriptor.resource_kind.clone(),
                    resource: tilo_actions::selection::logical_resource_id(resource).into_owned(),
                    action: descriptor.address.action.clone(),
                    need: descriptor.contract.need(),
                    consequential: descriptor
                        .contract
                        .asks_for_argument(contract_argument.or(argument)),
                    attended: descriptor.contract.is_attended(),
                    confirmed: false,
                };
                self.tracker_runs(&run.cluster_id)
                    .ok_or("the tracker is unavailable")?
                    .authorize_action(&run.cluster_id, &current, &requirement)
            },
        )?;
        Ok(match gate {
            AskGate::Proceed => NativeGate::Proceed,
            AskGate::Confirmed {
                argument,
                confirmation,
            } => NativeGate::Confirmed {
                argument,
                confirmation,
            },
            // The app action's child hears the wait it always did, and the
            // person's decline in their own words.
            AskGate::Held { reason } | AskGate::Declined { reason } => return Err(reason),
        })
    }

    /// The person's answer to one step a conversation turn takes — an app's
    /// action or the session's own step — through the system's sheet, or the
    /// refusal the workspace's grants give. The step's own authorization is
    /// `authorize`, given the exact argument slot the grants are decided
    /// with.
    pub(super) fn conversation_gate(
        &self,
        task: &str,
        descriptor: &tilo_actions::ActionDescriptor,
        (resource, action): (&str, &str),
        argument: Option<&str>,
        authorize: impl FnOnce(Option<&str>) -> Result<(), String>,
    ) -> Result<AskGate, String> {
        let prefixed =
            argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX));
        let json = prefixed.or(argument);
        let arguments = match json {
            Some(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
                .map_err(|_| "task action arguments are not a JSON object")?,
            None => serde_json::Map::new(),
        };
        let draft = Draft::render(descriptor, &arguments);
        let logical = tilo_actions::selection::logical_resource_id(resource).into_owned();
        let key = question_key(task, &logical, action, &arguments);
        // The person's answer, where they gave one: what it decides.
        match self.conversation_asks.take_answered(&key) {
            Some(Answered::Confirmed(edits)) => {
                let edited = draft.apply_edits(&arguments, &edits)?;
                let edited = serde_json::Value::Object(edited).to_string();
                return Ok(AskGate::Confirmed {
                    argument: if prefixed.is_some() {
                        format!("{}{edited}", tilo_actions::ARGUMENTS_JSON_PREFIX)
                    } else {
                        edited
                    },
                    confirmation: key,
                });
            }
            Some(Answered::Declined) => {
                return Ok(AskGate::Declined {
                    reason: conversation_declined(&draft.action_label),
                });
            }
            None => {}
        }
        let contract_argument =
            json.map(|text| format!("{}{text}", tilo_actions::ARGUMENTS_JSON_PREFIX));
        match authorize(contract_argument.as_deref().or(argument)) {
            Ok(()) => {
                // Nothing asks: a question left from an earlier shape of the
                // arguments goes, so the sheet never shows a stale one.
                self.conversation_asks.remove(&key);
                Ok(AskGate::Proceed)
            }
            Err(error)
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&error) =>
            {
                self.raise_conversation_ask(&key, task, &draft, (&logical, action))?;
                Ok(AskGate::Held {
                    reason: conversation_awaiting(&draft.action_label),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Put the question about `key` to the person on the system's sheet,
    /// once: an agent's re-asks find the question already up.
    fn raise_conversation_ask(
        &self,
        key: &str,
        task: &str,
        draft: &Draft,
        (resource, action): (&str, &str),
    ) -> Result<(), String> {
        let mut questions = self
            .conversation_asks
            .questions
            .lock()
            .map_err(|_| "the conversation's questions are unavailable")?;
        if questions.contains_key(key) {
            return Ok(());
        }
        let prompter = self
            .app_action_consent
            .read()
            .map_err(|_| "the confirmation sheet is unavailable")?
            .clone();
        let Some(answer) = prompter
            .and_then(|prompter| prompter.ask(conversation_request(draft, resource, action)))
        else {
            return Err(conversation_no_sheet(&draft.action_label));
        };
        questions.insert(
            key.to_owned(),
            ConversationAsk {
                task: task.to_owned(),
                answer,
            },
        );
        drop(questions);
        Ok(())
    }

    /// Take away the questions of runs no longer going: a question whose
    /// turn ended stands on nobody's screen. Dropping it withdraws it.
    pub(super) fn prune_conversation_asks(&self, runs: &[crate::services::TrackerRun]) {
        self.conversation_asks.retain_running(runs);
    }
}

/// The question as the system's sheet shows it: the draft the person
/// reviews, and the one confirmation that answers it.
fn conversation_request(draft: &Draft, resource: &str, action: &str) -> AppActionConsentRequest {
    AppActionConsentRequest {
        handle: format!("turn-question:{}", uuid::Uuid::new_v4().simple()),
        workspace: String::new(),
        person: String::new(),
        app_id: String::new(),
        target_app: String::new(),
        resource: resource.to_owned(),
        action: action.to_owned(),
        resource_label: draft.resource_label.clone(),
        action_label: draft.action_label.clone(),
        risk: tilo_actions::RiskLevel::Destructive,
        needs_confirmation: true,
        confirming: true,
        moderation: None,
        preview: None,
        scope: None,
        draft: Some(draft.clone()),
    }
}

/// What names one question: the run, the action and its exact arguments, so
/// an agent's re-ask finds the answer the person gave.
fn question_key(
    task: &str,
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> String {
    format!(
        "{task}\0{resource}\0{action}\0{}",
        hex::encode(tilo_actions::draft::arguments_digest(arguments))
    )
}

/// What an agent is told while the person has not answered its question.
fn conversation_awaiting(label: &str) -> String {
    format!(
        "{}: {label} waits for the person to confirm it",
        tilo_actions::draft::AWAITING_CONFIRMATION
    )
}

/// What an agent is told when the person declined its question.
fn conversation_declined(label: &str) -> String {
    format!(
        "{}: the person declined {label}",
        tilo_actions::draft::DECLINED
    )
}

/// Why nobody can be asked: the host installed no sheet.
fn conversation_no_sheet(label: &str) -> String {
    format!(
        "{label} asks the person to confirm it first, and this device has no sheet to ask them on"
    )
}
