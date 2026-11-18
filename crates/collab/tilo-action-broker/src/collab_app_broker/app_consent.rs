//! Asking the person before an application uses another application's
//! action, and keeping their answer.
//!
//! An app reaches its own actions and records with its person's authority
//! ([`AppBrokerState::app_api`]). Another app's action it takes only as the
//! person let it: an answer kept in their settings (`settings_store::app_consent`),
//! given once on the sheet the host draws ([`AppActionConsentPrompter`]).
//! Until they answer, the request is refused. Nobody present means nobody to
//! ask: the request is refused without a sheet, with the reason. So is a
//! call outside any workspace, on a device that keeps no answers, or for an
//! action nobody offers the person.

use crate::BrokerError;
use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, RemoteActionMessage};

use super::AppBrokerState;
use super::action_provider::{ConnectionActionProvider, RowScope};
use crate::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
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

/// Which app wants which app's action.
pub(crate) type ConsentKey = settings_store::app_consent::Consent;

fn key_of(request: &AppActionConsentRequest) -> ConsentKey {
    ConsentKey::new(
        &request.app_id,
        &request.target_app,
        &request.resource,
        &request.action,
    )
}

/// A consent question on screen.
pub(crate) struct PendingConsent {
    request: AppActionConsentRequest,
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
    /// workspace this machine governs, that this app was not let take before:
    /// in their settings, or for this session. Without a person, a workspace
    /// or a place to keep the answer, nobody can let it, and it is refused.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> ConsentStep {
        let consent = match self.consent_question(person, app_id, request) {
            Ok(Some(consent)) => consent,
            Ok(None) => return ConsentStep::Serve,
            Err(refused) => return ConsentStep::Refuse(refused),
        };
        let key = key_of(&consent);
        let Ok(mut pending) = self.app_action_consent_pending.lock() else {
            return ConsentStep::Refuse(ActionCallError::NotPermitted);
        };
        if pending.contains(&key) {
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
        pending.insert(key);
        ConsentStep::Ask(Box::new(PendingConsent {
            request: consent,
            answer,
        }))
    }

    /// The question to put to the person: `None` when the call needs none,
    /// `Err` with what to tell the app when nobody can answer it.
    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Result<Option<AppActionConsentRequest>, ActionCallError> {
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
            || target_app == tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID
        {
            return Ok(None);
        }
        let settings = self.services.settings.as_deref().ok_or_else(|| {
            refused("this device keeps no answers for one app taking another app's action")
        })?;
        let address = &action.address;
        let key = ConsentKey::new(app_id, &target_app, &address.resource, &address.action);
        if settings_store::app_consent::allows(settings, &key)
            || self
                .app_consent_session
                .lock()
                .is_ok_and(|held| held.contains(&key))
        {
            return Ok(None);
        }
        let contract = &action.contract;
        // What the call would run with, field by field, so the person sees what it changes.
        let draft = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => {
                Some(tilo_actions::Draft::render(&action, arguments))
            }
            _ => None,
        }
        .filter(|draft| !draft.fields.is_empty());
        Ok(Some(AppActionConsentRequest {
            handle: format!("app-action:{}", request.invocation.0),
            workspace: workspace.to_string(),
            person: person_id.to_owned(),
            app_id: app_id.to_string(),
            agent_session: None,
            target_app,
            resource: action.address.resource.clone(),
            action: action.address.action.clone(),
            resource_label: action.resource_label.clone(),
            action_label: action.action_label.clone(),
            risk: contract.risk,
            needs_confirmation: contract.advertised_confirmation()
                != tilo_actions::ConfirmationPolicy::None
                || contract.risk.requires_confirmation(),
            confirming: false,
            moderation: None,
            scope: None,
            draft,
        }))
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
            pending.remove(&key_of(&consent.request));
        }
    }

    /// Keep the answer `answer` gives as the person who gave it: in their
    /// settings for as long as they say, or for this session alone. `Err`
    /// with what to tell them when it could not be kept.
    fn keep_app_action_consent(
        &self,
        consent: &PendingConsent,
        answer: AppActionConsent,
    ) -> Result<(), String> {
        let settings = self
            .services
            .settings
            .as_deref()
            .ok_or_else(|| "this device keeps no answers".to_owned())?;
        let key = key_of(&consent.request);
        match answer {
            // The call it was given for, and no other.
            AppActionConsent::Once | AppActionConsent::Deny => Ok(()),
            AppActionConsent::Session => self
                .app_consent_session
                .lock()
                .map(|mut held| {
                    held.insert(key);
                })
                .map_err(|_| "this session's answers are unreadable".to_owned()),
            AppActionConsent::Always => settings_store::app_consent::allow(settings, &key)
                .map_err(|error| format!("the answer could not be saved: {error}")),
        }
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
            Ok(answer) if !answer.consent.grants() => {
                refuse(&message, ActionCallError::NotPermitted)
            }
            Ok(answer) => {
                if let Err(reason) = state.keep_app_action_consent(&consent, answer.consent) {
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
                    let person = consent.request.person.clone();
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
