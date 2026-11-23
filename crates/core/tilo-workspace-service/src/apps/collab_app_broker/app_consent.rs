//! Asking the person before an application uses another application's
//! action, and keeping their answer.
//!
//! An app reaches its own actions and records with its person's authority
//! ([`AppBrokerState::app_api`]). Another app's action it takes only as the
//! workspace's grants naming the app say ([`Grantee::App`]), decided as an
//! agent's are. Where none answers, the person is asked on the sheet the host
//! draws ([`AppActionConsentPrompter`]), the sheet an agent's question uses;
//! "Always" gives the grant in the workspace, where it shows under Apps in
//! Privacy & Security and is revoked like any other. Until they answer, the
//! request is refused. Nobody present means nobody to ask: the request is
//! refused without a sheet, with the reason. So is a call outside any
//! workspace, or for an action nobody offers the person.

use crate::apps::BrokerError;
use std::sync::Arc;

use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
use tilo_action_policy::{Answer, Basis, Grant, Grantee, Lifetime};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, RemoteActionMessage};

use super::AppBrokerState;
use super::action_provider::{ConnectionActionProvider, RowScope};
use crate::apps::app_actions::{
    APP_ACTION_ASK, AppActionAnswer, AppActionConsentPrompter, AppActionConsentRequest,
};

/// What happens to an application's invocation before it is served.
pub(crate) enum ConsentStep {
    /// It holds what it needs already: serve it as it is.
    Serve,
    /// Nobody can be asked: refuse it, saying why.
    Refuse(ActionCallError),
    /// The person is being asked.
    Ask(Box<PendingConsent>),
}

/// A consent question on screen, and the grant "Always" gives.
pub(crate) struct PendingConsent {
    request: AppActionConsentRequest,
    grant: Grant,
    answer: tokio::sync::oneshot::Receiver<AppActionAnswer>,
}

impl AppBrokerState {
    /// Ask whoever answers for the person using an application from now on.
    pub fn set_app_action_consent(&self, prompter: Arc<dyn AppActionConsentPrompter>) {
        if let Ok(mut current) = self.app_action_consent.write() {
            *current = Some(prompter);
        }
    }

    /// What to do with `request`, which application `app_id` makes under its
    /// own identity on a connection `person` admitted.
    ///
    /// The person is asked only for an action the catalogue offers them, in a
    /// workspace this machine governs, that no grant of the workspace lets
    /// the app take or keeps it from. Without a person or a workspace, nobody
    /// can let it, and it is refused.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> ConsentStep {
        let (consent, grant) = match self.consent_question(person, app_id, request) {
            Ok(Some(question)) => question,
            Ok(None) => return ConsentStep::Serve,
            Err(refused) => return ConsentStep::Refuse(refused),
        };
        let Ok(mut pending) = self.app_action_consent_pending.lock() else {
            return ConsentStep::Refuse(ActionCallError::NotPermitted);
        };
        if pending.contains(&grant.id) {
            return ConsentStep::Refuse(ActionCallError::NotPermitted);
        }
        let prompter = self
            .app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone());
        let Some(answer) = prompter.and_then(|prompter| prompter.ask(consent.clone())) else {
            return ConsentStep::Refuse(ActionCallError::NotPermitted);
        };
        pending.insert(grant.id.clone());
        ConsentStep::Ask(Box::new(PendingConsent {
            request: consent,
            grant,
            answer,
        }))
    }

    /// The question to put to the person, and the grant answering it for
    /// good: `None` when the call needs none, `Err` with what to tell the app
    /// when nobody can answer it or a grant keeps the app from it.
    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Result<Option<(AppActionConsentRequest, Grant)>, ActionCallError> {
        let refused = |reason: &str| ActionCallError::Refused {
            reason: reason.to_owned(),
        };
        let person_id = person.acting().person_id().ok_or_else(|| {
            refused("no person uses this app to let it take another app's action")
        })?;
        let workspace = person.workspace().ok_or_else(|| {
            refused("this app is in no workspace that could let it take another app's action")
        })?;
        // What nobody offers the person, nobody can let the app take: running
        // it would do what the person was never shown.
        let (target_app, action) = self
            .offer(person, request)
            .ok_or(ActionCallError::UnknownAction)?;
        // Governing the workspace is never one click away for an app.
        let governance = self
            .services
            .workspace_governance
            .as_ref()
            .is_some_and(|governance| governance.app_id() == target_app);
        if action.contract.kind == tilo_actions::ActionKind::Outside
            || governance
            || target_app == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
        {
            return Ok(None);
        }
        let address = &action.address;
        let need = action.contract.need();
        let grant = answer_grant(
            workspace,
            Grantee::app(app_id),
            (
                &AnswerScope::action(&target_app, &action.resource_kind, &address.action),
                need.kind,
            ),
            Answer::Allow,
            Grantee::person(person_id),
        );
        match self.app_answer(workspace, app_id, &target_app, &action)? {
            Some(Answer::Allow) => return Ok(None),
            Some(Answer::Deny) => {
                return Err(refused(
                    "this workspace does not let this app take that app's action",
                ));
            }
            Some(Answer::Ask) | None => {}
        }
        let contract = &action.contract;
        // What the call would run with, field by field, so the person sees what it changes.
        let draft = super::rendered_draft(&action, &request.argument);
        Ok(Some((
            AppActionConsentRequest {
                handle: format!("{APP_ACTION_ASK}{}", request.invocation.0),
                workspace: workspace.to_string(),
                person: person_id.to_owned(),
                app_id: app_id.to_string(),
                agent_session: None,
                target_app,
                resource_label: action.resource_label.clone(),
                action_label: action.action_label.clone(),
                kind: contract.kind,
                risk: contract.risk,
                confirming: false,
                moderation: None,
                scope: None,
                draft,
                ask: None,
            },
            grant,
        )))
    }

    /// What the grants of `workspace` naming app `app_id` answer for its use
    /// of `action`, which `target_app` offers: `None` where none does.
    fn app_answer(
        &self,
        workspace: &str,
        app_id: &str,
        target_app: &str,
        action: &tilo_actions::ActionDescriptor,
    ) -> Result<Option<Answer>, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let need = action.contract.need();
        let now = tilo_clock::unix_now();
        self.membership
            .as_ref()
            .ok_or_else(|| unavailable("workspace authority unavailable".into()))?
            .decide_in_workspace(workspace, |held| {
                let held = held?;
                let node = tilo_actions::action_node(
                    &held.node(),
                    target_app,
                    &action.resource_kind,
                    &action.address.resource,
                    need.field.as_deref(),
                    &action.address.action,
                );
                let decision = held.policy.party_of(Grantee::app(app_id)).decide(
                    &node,
                    need.kind,
                    &tilo_action_policy::Context::at_secs(now),
                );
                Some(match decision.basis {
                    Basis::NoGrant => None,
                    _ => Some(decision.answer),
                })
            })
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("workspace no longer hosted".into()))
    }

    /// The action `request` asks for, and the app that offers it, as the
    /// person using the asking app is offered it: an instance's action in the
    /// catalogue, or what the app a target names by its id offers.
    fn offer(
        &self,
        person: &Caller,
        request: &InvokeRequest,
    ) -> Option<(String, tilo_actions::ActionDescriptor)> {
        if let Some((workspace, app_id, action)) = self.application_offer(request) {
            let action = action.filter(|_| person.workspace() == Some(workspace.as_str()))?;
            return Some((
                app_id,
                action.descriptor(tilo_actions::ExecutionLocus::default()),
            ));
        }
        let row = self
            .eager_catalogue(person, RowScope::Provider(&request.target.provider))
            .0
            .into_iter()
            .find(|row| {
                row.provider == request.target.provider
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|descriptor| descriptor.address == request.target.address)
            })?;
        Some((row.app_id, row.action?))
    }

    /// The question `consent` asked is off the screen.
    fn finish_app_action_consent(&self, consent: &PendingConsent) {
        if let Ok(mut pending) = self.app_action_consent_pending.lock() {
            pending.remove(&consent.grant.id);
        }
    }

    /// Keep the person's answer, allowed for `lifetime`, as theirs: a grant in
    /// the workspace for as long as they say, none for this call alone.
    /// `Err` with what to tell them when it could not be kept.
    async fn keep_app_action_consent(
        &self,
        consent: &PendingConsent,
        lifetime: Lifetime,
    ) -> Result<(), String> {
        match lifetime {
            // The call it was given for, and no other. A session's answer is
            // not one a workspace keeps: only this call has it.
            Lifetime::Once | Lifetime::Session => return Ok(()),
            Lifetime::Until(_) | Lifetime::UntilRevoked => {}
        }
        let grants = self
            .services
            .workspace_grants
            .as_ref()
            .filter(|grants| grants.may_record(&consent.request.workspace, &consent.request.person))
            .ok_or_else(|| "this device cannot give grants in this workspace".to_owned())?;
        grants
            .record(
                &consent.request,
                consent.grant.clone().with_lifetime(lifetime),
            )
            .await
            .map(drop)
    }
}

/// Answer an application's invocation once the person has answered whether
/// it may: denied, refused; allowed, served once the answer is kept, and
/// refused with the reason when it could not be.
pub(super) fn serve_after_consent(
    state: Arc<AppBrokerState>,
    consent: Box<PendingConsent>,
    provider: ConnectionActionProvider,
    caller: Caller,
    message: RemoteActionMessage,
) -> super::wire::Answer<RemoteActionMessage> {
    use tilo_actions::remote::refuse;
    Box::pin(async move {
        let mut consent = consent;
        // A consent sheet shows no draft: only the choice counts. Resolved by
        // the sheet: the person's answer, or its sender dropped with the
        // sheet, which refuses the call as unanswered, not as declined.
        let answered = (&mut consent.answer).await;
        state.finish_app_action_consent(&consent);
        let reply = match answered {
            Err(_) => refuse(
                &message,
                ActionCallError::Unavailable {
                    reason: "the sheet closed without the person's answer".into(),
                },
            ),
            Ok(AppActionAnswer { lifetime: None, .. }) => {
                refuse(&message, ActionCallError::NotPermitted)
            }
            Ok(AppActionAnswer {
                lifetime: Some(lifetime),
                ..
            }) => {
                if let Err(reason) = state.keep_app_action_consent(&consent, lifetime).await {
                    refuse(&message, ActionCallError::Refused { reason })
                } else {
                    // Answered at admission, as before waits existed: an answer
                    // given for this call is spent by that admission, never
                    // held open while the action runs.
                    let message = match message {
                        RemoteActionMessage::Invoke(mut request) => {
                            request.wait_ms = None;
                            RemoteActionMessage::Invoke(request)
                        }
                        other => other,
                    };
                    let person = Some(consent.request.person.clone());
                    super::approval::serve_answering(
                        Arc::clone(&state),
                        provider,
                        caller,
                        person,
                        message,
                    )
                    .await
                }
            }
        };
        reply.ok_or_else(|| BrokerError::failed("the action provider dropped the request".into()))
    })
}
