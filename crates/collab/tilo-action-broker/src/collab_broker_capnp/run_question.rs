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
//!
//! A run's question also reaches its person as an ordinary notification
//! ([`AppActionConsentPrompter::notify`]), never as a sheet raised on its
//! own: its Answer puts the same question on the same sheet, and the host
//! then gives the tracker the person's answer through their own door.

use std::sync::Arc;

use capnp::capability::Promise;
use runtime_collab::raw::channel_handle;
use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
use tilo_action_policy::{Answer, Grant, Subject};
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeArgument, InvokeRequest};

use super::CapnpBrokerState;
use super::action_host::ConnectionActionHost;
use crate::app_actions::{AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest};
use crate::services::{TRACKER_APP, TRACKER_ISSUE_PREFIX, TrackerAnswer, TrackerRun};

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
        let (request, grant) = self.run_question_for(room, person, &issue)?;
        let answer = self.consent_prompter()?.ask(request.clone())?;
        Some(PendingAnswer {
            request,
            grant,
            answer,
        })
    }

    fn consent_prompter(&self) -> Option<Arc<dyn AppActionConsentPrompter>> {
        self.app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())
    }

    /// What `issue`'s going run, started by `person` in `room`, asks them,
    /// as the sheet puts it, and the grant "Allow for the issue" records
    /// where they may give it.
    fn run_question_for(
        &self,
        room: &str,
        person: &str,
        issue: &str,
    ) -> Option<(AppActionConsentRequest, Option<Grant>)> {
        let run = self
            .tracker_runs(room)?
            .runs(room)
            .ok()?
            .into_iter()
            .find(|run| run.issue == issue && run.active && run.starter == person)?;
        let question = run
            .question
            .filter(|question| question.answer == TrackerAnswer::Waiting)?;
        let (draft, preview) = super::super::services::confirmation_preview(&question);
        let draft = (!draft.fields.is_empty()).then_some(draft);
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
                    Subject::task(issue),
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
            handle: run_question_handle(&question.id),
            room: room.to_owned(),
            person: person.to_owned(),
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
            // A question the tracker answered with `answered` settles a
            // confirmed preview as it is: what it shows must not be
            // editable here, or the person's edits would never reach it.
            preview,
            scope: grant.is_some().then_some(run.title),
            draft,
        };
        Some((request, grant))
    }

    /// Record the grant "Allow for the issue" gives, when `answer` gave it.
    async fn record_answer(
        &self,
        request: &AppActionConsentRequest,
        grant: Option<Grant>,
        answer: AppActionConsent,
    ) {
        if answer == AppActionConsent::Always
            && let (Some(grant), Some(grants)) = (grant, self.services.app_action_grants.as_ref())
            && let Err(error) = grants.record(request, grant, answer).await
        {
            tracing::warn!(%error, "the grant for the issue could not be recorded");
        }
    }

    /// Tell each person here about what their going runs in `room` ask, as
    /// `runs` holds them, once per question, and take back the notices of
    /// questions answered or gone.
    pub(super) fn notify_run_questions(self: &Arc<Self>, room: &str, runs: &[TrackerRun]) {
        let Some(prompter) = self.consent_prompter() else {
            return;
        };
        let waiting: std::collections::HashSet<String> = runs
            .iter()
            .filter(|run| run.active)
            .filter_map(|run| run.question.as_ref())
            .filter(|question| question.answer == TrackerAnswer::Waiting)
            .map(|question| run_question_handle(&question.id))
            .collect();
        let mut notified = self
            .run_question_notices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        notified.retain(|handle, notice_room| {
            let keep = notice_room != room || waiting.contains(handle);
            if !keep {
                prompter.withdraw(handle);
            }
            keep
        });
        for run in runs.iter().filter(|run| run.active) {
            let Some(question) = run
                .question
                .as_ref()
                .filter(|question| question.answer == TrackerAnswer::Waiting)
            else {
                continue;
            };
            let handle = run_question_handle(&question.id);
            if notified.contains_key(&handle) {
                continue;
            }
            let Some((request, grant)) = self.run_question_for(room, &run.starter, &run.issue)
            else {
                continue;
            };
            let state = Arc::clone(self);
            let issue = run.issue.clone();
            let notice = request.clone();
            let answer = Box::new(move |answer| state.give_answer(notice, grant, &issue, answer));
            if prompter.notify(request, answer) {
                notified.insert(handle, room.to_owned());
            }
        }
    }

    /// Once the person answered `request` (the question `issue`'s run asks)
    /// on the sheet, record the grant it gives and give the tracker their
    /// answer through their own door.
    fn give_answer(
        self: Arc<Self>,
        request: AppActionConsentRequest,
        grant: Option<Grant>,
        issue: &str,
        answer: tokio::sync::oneshot::Receiver<AppActionConsent>,
    ) {
        let Ok(runtime) = super::launchable::delivery_runtime() else {
            return;
        };
        let issue = issue.to_owned();
        runtime.spawn(async move {
            let answer = answer.await.unwrap_or(AppActionConsent::Deny);
            self.record_answer(&request, grant, answer).await;
            let Some(invoke) = self.tracker_answer(&request.room, &issue, answer) else {
                tracing::warn!("the tracker that asked is no longer published");
                return;
            };
            let caller = Caller::local_person(request.person.clone()).in_room(&request.room);
            match self.action_invoke_local_person(&caller, &invoke).await {
                Some(Ok(_)) => {}
                Some(Err(error)) => tracing::warn!(%error, "the person's answer was refused"),
                None => tracing::warn!("the person's answer was dropped"),
            }
        });
    }

    /// The tracker's `answer-confirmation` of `issue` in `room`, giving
    /// `answer`: on its live window, else launching it.
    fn tracker_answer(
        &self,
        room: &str,
        issue: &str,
        answer: AppActionConsent,
    ) -> Option<InvokeRequest> {
        use tilo_actions::remote::{Confirmation, RemoteActionTarget};
        let id = issue.strip_prefix(TRACKER_ISSUE_PREFIX)?;
        let (surface, action) = self.published_surfaces().into_iter().find_map(|surface| {
            let action = (surface.app_id == TRACKER_APP && surface.route.cluster_id == room)
                .then(|| {
                    surface
                        .actions
                        .iter()
                        .find(|action| action.action == ANSWER)
                        .cloned()
                })??;
            Some((surface, action))
        })?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("issue".into(), id.into());
        arguments.insert("confirm".into(), (answer != AppActionConsent::Deny).into());
        Some(InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: if surface.live {
                    surface.peer_id.to_string()
                } else {
                    super::launchable_provider(&surface)
                },
                address: tilo_actions::ActionAddress::new(&action.resource, ANSWER),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: matches!(
                action.contract.advertised_confirmation(),
                tilo_actions::ConfirmationPolicy::Confirm
            )
            .then_some(Confirmation::Confirmed),
            revision: None,
            wait_ms: None,
            appearance: None,
        })
    }
}

/// The sheet's and the notice's handle of the run's question `id`.
fn run_question_handle(id: &str) -> String {
    format!("run-question:{id}")
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
        state.record_answer(&request, grant, answer).await;
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
