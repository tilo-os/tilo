//! A person answers what an issue's run asks on the host's own sheet.
//!
//! A run pauses before an action its grants ask first about and asks on its
//! issue ([`super::tracker_runs`]). The Tasks app shows the question, and its
//! answer button sends the tracker's `answer-confirmation` through the
//! session's door; the host, not the app, then puts the question to the
//! person on its confirmation sheet: Allow once, Allow for the issue, or
//! Deny. The invocation goes on with the person's answer in place of the
//! app's, carrying the person's edits to the step's draft and none the app
//! supplied: the step then runs with exactly the arguments their edits make
//! ([`super::tracker_runs::answered`]). "Allow for the issue" is first
//! recorded as the person, through the host's
//! [`AppActionGrants`](crate::services::AppActionGrants), as an answer
//! grant naming the issue for that one action
//! ([`tilo_action_policy::agent_answers::answer_grant`]): it shows in the
//! workspace's history and settings and is revoked like any grant.
//!
//! A run's question also reaches its person as an ordinary notification
//! ([`AppActionConsentPrompter::notify`]), never as a sheet raised on its
//! own: its Answer puts the same question on the same sheet, and the host
//! then gives the tracker the person's answer through their own door.

use crate::BrokerError;
use std::sync::Arc;

use tilo_action_policy::{Answer, Grant, Grantee};
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeArgument, InvokeRequest};

use super::AppBrokerState;
use super::action_host::ConnectionActionHost;
use crate::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use crate::services::{
    TRACKER_APP, TRACKER_ISSUE_PREFIX, TrackerAnswer, TrackerRun, issue_answer_id,
    issue_answer_scope,
};

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
    answer: tokio::sync::oneshot::Receiver<AppActionAnswer>,
}

impl AppBrokerState {
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
        let workspace = caller.workspace()?;
        let InvokeArgument::Json(arguments) = &invoke.argument else {
            return None;
        };
        let issue = format!(
            "{TRACKER_ISSUE_PREFIX}{}",
            arguments.get("issue")?.as_str()?
        );
        let (request, grant) = self.run_question_for(workspace, person, &issue)?;
        let answer = self.consent_prompter()?.ask(request.clone())?;
        Some(PendingAnswer {
            request,
            grant,
            answer,
        })
    }

    pub(super) fn consent_prompter(&self) -> Option<Arc<dyn AppActionConsentPrompter>> {
        self.app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())
    }

    /// What `issue`'s going run, started by `person` in `workspace`, asks them,
    /// as the sheet puts it, and the grant "Allow for the issue" records
    /// where they may give it.
    pub(super) fn run_question_for(
        &self,
        workspace: &str,
        person: &str,
        issue: &str,
    ) -> Option<(AppActionConsentRequest, Option<Grant>)> {
        let run = self
            .tracker_runs(workspace)?
            .runs(workspace)
            .ok()?
            .into_iter()
            .find(|run| run.issue == issue && run.active && run.starter == person)?;
        // A run that keeps its own answers (an agent door connection's) takes its person's
        // "allow always" itself, for as long as it goes; any other is granted in the workspace's
        // record, where its person may grant.
        let keeps_answers = self
            .tracker_runs(workspace)
            .is_some_and(|tracker| tracker.asks_on_host(workspace, &run));
        let question = run
            .question
            .filter(|question| question.answer == TrackerAnswer::Waiting)?;
        let (draft, preview) = super::super::services::confirmation_preview(&question);
        let draft = (!draft.fields.is_empty()).then_some(draft);
        let asked = self.published_surfaces().into_iter().find_map(|surface| {
            (surface.app_id == question.app && surface.route.cluster_id == workspace)
                .then(|| {
                    surface.actions.into_iter().find(|action| {
                        action.action == question.action
                            && tilo_actions::selection::logical_resource_id(&action.resource)
                                == question.resource
                    })
                })
                .flatten()
        });
        let may_grant = keeps_answers
            || self
                .services
                .workspace_grants
                .as_ref()
                .is_some_and(|grants| grants.may_record(workspace, person));
        let grant = asked
            .as_ref()
            .filter(|action| may_grant && !action.contract.need().privileged)
            .map(|action| {
                let (scope, kind) = (
                    issue_answer_scope(
                        &question.app,
                        (&action.resource_kind, &action.action),
                        action.contract.asks_for_argument(None),
                    ),
                    action.contract.need().kind,
                );
                Grant::new(
                    issue_answer_id(issue, (&scope, kind)),
                    Grantee::agent(run.agent.clone()),
                    scope.node(workspace),
                    tilo_action_policy::KindSet::of(kind),
                    Answer::Allow,
                    Grantee::person(person),
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
            workspace: workspace.to_owned(),
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
            && let (Some(grant), Some(grants)) = (grant, self.services.workspace_grants.as_ref())
            && let Err(error) = grants.record(request, grant, answer).await
        {
            tracing::warn!(%error, "the grant for the issue could not be recorded");
        }
    }

    /// Tell each person here about what their going runs in `workspace` ask, as
    /// `runs` holds them, once per question, and take back the notices of
    /// questions answered or gone.
    pub(super) fn notify_run_questions(self: &Arc<Self>, workspace: &str, runs: &[TrackerRun]) {
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
        notified.retain(|handle, notice_workspace| {
            let keep = notice_workspace != workspace || waiting.contains(handle);
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
            let Some((request, grant)) = self.run_question_for(workspace, &run.starter, &run.issue)
            else {
                continue;
            };
            let state = Arc::clone(self);
            let issue = run.issue.clone();
            let notice = request.clone();
            let answer = Box::new(move |answer| state.give_answer(notice, grant, &issue, answer));
            if prompter.notify(request, answer) {
                notified.insert(handle, workspace.to_owned());
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
        answer: tokio::sync::oneshot::Receiver<AppActionAnswer>,
    ) {
        let Ok(runtime) = super::launchable::delivery_runtime() else {
            return;
        };
        let issue = issue.to_owned();
        runtime.spawn(async move {
            // Resolved by the sheet: the person's answer, or its sender
            // dropped with the sheet (the session went away). That is no
            // answer of theirs: nothing is told to the tracker, the question
            // stays asked, and the next round notifies it again.
            let Ok(answer) = answer.await else {
                tracing::warn!(
                    handle = %request.handle,
                    "the sheet closed without the person's answer; the run's question stays asked"
                );
                self.run_question_notices
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&request.handle);
                return;
            };
            self.record_answer(&request, grant, answer.consent).await;
            let invoke = match self.tracker_answer(&request, &issue, &answer) {
                Ok(Some(invoke)) => invoke,
                Ok(None) => {
                    tracing::warn!("the tracker that asked is no longer published");
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "the person's edits do not fit the question");
                    return;
                }
            };
            let caller =
                Caller::local_person(request.person.clone()).in_workspace(&request.workspace);
            match self.action_invoke_local_person(&caller, &invoke).await {
                Some(Ok(_)) => {}
                Some(Err(error)) => tracing::warn!(%error, "the person's answer was refused"),
                None => tracing::warn!("the person's answer was dropped"),
            }
        });
    }

    /// The tracker's `answer-confirmation` of `issue`, giving the person's
    /// `answer` to `request`: on its live window, else launching it. `None`
    /// when no tracker publishes it.
    fn tracker_answer(
        &self,
        request: &AppActionConsentRequest,
        issue: &str,
        answer: &AppActionAnswer,
    ) -> Result<Option<InvokeRequest>, String> {
        use tilo_actions::remote::{Confirmation, RemoteActionTarget};
        let workspace = request.workspace.as_str();
        let mut arguments = serde_json::Map::new();
        answer_arguments(request, answer, &mut arguments)?;
        let Some(id) = issue.strip_prefix(TRACKER_ISSUE_PREFIX) else {
            return Ok(None);
        };
        let found = self.published_surfaces().into_iter().find_map(|surface| {
            let action = (surface.app_id == TRACKER_APP && surface.route.cluster_id == workspace)
                .then(|| {
                    surface
                        .actions
                        .iter()
                        .find(|action| action.action == ANSWER)
                        .cloned()
                })??;
            Some((surface, action))
        });
        let Some((surface, action)) = found else {
            return Ok(None);
        };
        arguments.insert("issue".into(), id.into());
        Ok(Some(InvokeRequest {
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
        }))
    }
}

/// Set the tracker's `answer-confirmation` `arguments` to the person's
/// `answer` to `request`, and to nothing the app supplied on their behalf:
/// whether they confirmed, and their edits to the draft the sheet showed,
/// each checked against it ([`tilo_actions::Draft::check_edits`]). The
/// broker applies them to the step's arguments when the step runs, and the
/// confirmation admits exactly the arguments they make.
fn answer_arguments(
    request: &AppActionConsentRequest,
    answer: &AppActionAnswer,
    arguments: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    arguments.remove("edits");
    arguments.remove("reason");
    arguments.insert("confirm".into(), answer.allowed().into());
    let edits = answer.allowed_edits();
    if edits.is_empty() {
        return Ok(());
    }
    request
        .draft
        .as_ref()
        .ok_or("nothing in this question can be changed")?
        .check_edits(edits)?;
    arguments.insert(
        "edits".into(),
        serde_json::to_value(edits).map_err(|error| error.to_string())?,
    );
    Ok(())
}

/// The sheet's and the notice's handle of the run's question `id`.
fn run_question_handle(id: &str) -> String {
    format!("run-question:{id}")
}

/// Serve `message`, the tracker's answer, once the person has given theirs
/// on the sheet: with their answer in place of the app's, after recording
/// the grant "Allow for the issue" gives.
pub(super) fn serve_answered(
    state: Arc<AppBrokerState>,
    pending: Box<PendingAnswer>,
    (host, caller): (ConnectionActionHost, Caller),
    message: tilo_actions::remote::RemoteActionMessage,
) -> super::wire::Answer<tilo_actions::remote::RemoteActionMessage> {
    use tilo_actions::remote::RemoteActionMessage;
    Box::pin(async move {
        let PendingAnswer {
            request,
            grant,
            answer,
        } = *pending;
        // Resolved by the sheet: the person's answer, or its sender dropped
        // with the sheet. A sheet that closed unanswered is no denial of
        // theirs: the tracker's call is refused with why, and the question
        // stays asked.
        let Ok(answer) = answer.await else {
            let refused = tilo_actions::remote::refuse(
                &message,
                tilo_actions::remote::ActionHostError::Unavailable {
                    reason: "the sheet closed without the person's answer".into(),
                },
            );
            return refused.ok_or_else(dropped);
        };
        state.record_answer(&request, grant, answer.consent).await;
        let message = match message {
            RemoteActionMessage::Invoke(mut invoke) => {
                if let InvokeArgument::Json(arguments) = &mut invoke.argument
                    && let Err(reason) = answer_arguments(&request, &answer, arguments)
                {
                    // Nothing is answered for the person: the question
                    // stays asked.
                    let refused = tilo_actions::remote::refuse(
                        &RemoteActionMessage::Invoke(invoke),
                        tilo_actions::remote::ActionHostError::InvalidRequest { reason },
                    );
                    return refused.ok_or_else(dropped);
                }
                RemoteActionMessage::Invoke(invoke)
            }
            other => other,
        };
        let person = request.person;
        super::witness::serve_answering(state, host, caller, person, message)
            .await
            .ok_or_else(dropped)
    })
}

fn dropped() -> BrokerError {
    BrokerError::failed("the action host dropped the request".into())
}
