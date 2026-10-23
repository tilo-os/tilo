//! A person answers what an issue's run asks on the host's own sheet.
//!
//! A run pauses before an action its grants ask first about and asks on its
//! issue ([`super::tracker_runs`]). The Tasks app shows the question, and its
//! answer button sends the tracker's `answer-confirmation` through the
//! session's door; the host, not the app, then puts the question to the
//! person on its confirmation sheet: Allow once, Allow for the issue, or
//! Deny. The invocation goes on with the person's answer in place of the
//! app's. "Allow for the issue" is first recorded as the person, through the
//! host's [`AppActionGrants`](crate::services::AppActionGrants), as an answer
//! grant naming the issue for that one action
//! ([`tilo_action_policy::agent_answers::answer_grant`]): it shows in the
//! workspace's history and settings and is revoked like any grant.

use std::sync::Arc;

use capnp::capability::Promise;
use runtime_collab::raw::channel_handle;
use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
use tilo_action_policy::{Answer, Grant, Subject};
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeArgument, InvokeRequest};

use super::CapnpBrokerState;
use super::action_host::ConnectionActionHost;
use crate::app_actions::{AppActionConsent, AppActionConsentRequest};
use crate::services::{TRACKER_APP, TRACKER_ISSUE_PREFIX, TrackerAnswer};

/// The tracker's verb a person answers a run's question with.
const ANSWER: &str = "answer-confirmation";

/// What happens to an invocation before it is served.
pub(crate) enum AnswerStep {
    /// It answers no run's question: serve it as it is.
    Serve,
    /// It answers a question nobody here can put to its person: refuse it.
    Refuse,
    /// The person is being asked.
    Ask(Box<PendingAnswer>),
}

/// A run's question on the person's screen.
pub(crate) struct PendingAnswer {
    request: AppActionConsentRequest,
    /// What "Allow for the issue" grants, where the person may grant it.
    grant: Option<Grant>,
    answer: tokio::sync::oneshot::Receiver<AppActionConsent>,
}

impl CapnpBrokerState {
    /// What to do with `invoke`, from a connection of `app_id` used by
    /// `caller`: the tracker app's answer to a question one of its issues'
    /// runs asks is put to the run's person on the sheet, and refused when
    /// it cannot be.
    pub(crate) fn run_question(
        &self,
        app_id: &str,
        caller: &Caller,
        invoke: &InvokeRequest,
    ) -> AnswerStep {
        if app_id != TRACKER_APP || invoke.target.address.action != ANSWER {
            return AnswerStep::Serve;
        }
        self.ask_run_question(caller, invoke)
            .map_or(AnswerStep::Refuse, |pending| {
                AnswerStep::Ask(Box::new(pending))
            })
    }

    fn ask_run_question(&self, caller: &Caller, invoke: &InvokeRequest) -> Option<PendingAnswer> {
        let CallerPrincipal::Person(person) = caller.principal() else {
            return None;
        };
        let room = caller.room()?;
        let InvokeArgument::Json(arguments) = &invoke.argument else {
            return None;
        };
        let issue = format!(
            "{TRACKER_ISSUE_PREFIX}{}",
            arguments.get("issue")?.as_str()?
        );
        let run = self
            .tracker_runs(room)?
            .runs(room)
            .ok()?
            .into_iter()
            .find(|run| run.issue == issue && run.active && run.starter == *person)?;
        let question = run
            .question
            .filter(|question| question.answer == TrackerAnswer::Waiting)?;
        let draft = serde_json::from_value::<tilo_actions::Draft>(question.draft).ok();
        let asked = self.published_surfaces().into_iter().find_map(|surface| {
            (surface.app_id == question.app && surface.route.cluster_id == room)
                .then(|| {
                    surface.actions.into_iter().find(|action| {
                        action.action == question.action
                            && tilo_actions::selection::logical_resource_id(&action.resource)
                                == question.resource
                    })
                })
                .flatten()
        });
        let may_grant = self
            .services
            .app_action_grants
            .as_ref()
            .is_some_and(|grants| grants.may_grant(room, person));
        let grant = asked
            .as_ref()
            .filter(|action| may_grant && !action.contract.need().privileged)
            .map(|action| {
                answer_grant(
                    room,
                    Subject::task(&issue),
                    (
                        &AnswerScope::action(&question.app, &action.resource_kind, &action.action),
                        action.contract.need().kind,
                    ),
                    Answer::Allow,
                    Subject::person(person),
                )
            });
        let labels = |of: fn(&tilo_actions::Draft) -> &String, published: Option<&String>| {
            draft
                .as_ref()
                .map(of)
                .or(published)
                .cloned()
                .unwrap_or_else(|| question.action.clone())
        };
        let request = AppActionConsentRequest {
            handle: format!("run-question:{}", question.id),
            room: room.to_owned(),
            person: person.clone(),
            app_id: run.agent,
            target_app: question.app.clone(),
            resource: question.resource.clone(),
            action: question.action.clone(),
            resource_label: labels(
                |draft| &draft.resource_label,
                asked.as_ref().map(|action| &action.resource_label),
            ),
            action_label: labels(
                |draft| &draft.action_label,
                asked.as_ref().map(|action| &action.action_label),
            ),
            // An action no longer published is read as the gravest it may be.
            risk: asked
                .as_ref()
                .map_or(tilo_actions::RiskLevel::Destructive, |action| {
                    action.contract.risk
                }),
            needs_confirmation: true,
            confirming: true,
            moderation: None,
            scope: grant.is_some().then_some(run.title),
            draft,
        };
        let prompter = self
            .app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())?;
        let answer = prompter.ask(request.clone())?;
        Some(PendingAnswer {
            request,
            grant,
            answer,
        })
    }
}

/// Serve `message`, the tracker's answer, once the person has given theirs
/// on the sheet: with their answer in place of the app's, after recording
/// the grant "Allow for the issue" gives.
pub(super) fn serve_answered(
    state: Arc<CapnpBrokerState>,
    pending: Box<PendingAnswer>,
    (host, caller): (ConnectionActionHost, Caller),
    message: tilo_actions::remote::RemoteActionMessage,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    Promise::from_future(async move {
        let PendingAnswer {
            request,
            grant,
            answer,
        } = *pending;
        let answer = answer.await.unwrap_or(AppActionConsent::Deny);
        if answer == AppActionConsent::Always
            && let (Some(grant), Some(grants)) = (grant, state.services.app_action_grants.as_ref())
            && grants.record(&request, grant, answer).is_none()
        {
            tracing::warn!("the grant for the issue could not be recorded");
        }
        let message = match message {
            RemoteActionMessage::Invoke(mut request) => {
                if let InvokeArgument::Json(arguments) = &mut request.argument {
                    // Only what the person chose: no edits or reason the
                    // app supplied on their behalf.
                    arguments.remove("edits");
                    arguments.remove("reason");
                    arguments.insert("confirm".into(), (answer != AppActionConsent::Deny).into());
                }
                RemoteActionMessage::Invoke(request)
            }
            other => other,
        };
        let person = request.person;
        let reply = super::witness::serve_answering(state, host, caller, person, message)
            .await
            .ok_or_else(|| capnp::Error::failed("the action host dropped the request".into()))?;
        let bytes = RemoteActionFrame::new(reply)
            .encode()
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_frame(&bytes);
        Ok(())
    })
}
