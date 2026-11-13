//! Asking the person before an application uses another application's
//! action, and keeping their answer.
//!
//! An app reaches its own actions and records with its person's authority
//! ([`AppBrokerState::app_door`]). Another app's action it takes only as the
//! person let it: an answer kept in their settings (`settings_store::app_consent`),
//! given once on the sheet the host draws ([`AppActionConsentPrompter`]).
//! Until they answer, the request is refused. Nobody present means nobody to
//! ask: the request is refused without a sheet.

use crate::BrokerError;
use std::sync::Arc;

use tilo_actions::remote::{Caller, CallerPrincipal, InvokeRequest, RemoteActionMessage};

use super::AppBrokerState;
use super::action_host::{ConnectionActionHost, RowScope};
use crate::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};

/// What happens to an application's invocation before it is served.
pub(crate) enum ConsentStep {
    /// It holds what it needs already: serve it as it is.
    Serve,
    /// Nobody can be asked: refuse it.
    Refuse,
    /// The person is being asked.
    Ask(Box<PendingConsent>),
}

/// An action one app asks to take of another: the app that offers it, and
/// the action as offered.
struct Offer {
    app_id: String,
    action: tilo_actions::ActionDescriptor,
}

/// Which app wants which app's action.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConsentKey {
    app_id: String,
    target_app: String,
    resource: String,
    action: String,
}

impl ConsentKey {
    fn of(request: &AppActionConsentRequest) -> Self {
        Self {
            app_id: request.app_id.clone(),
            target_app: request.target_app.clone(),
            resource: request.resource.clone(),
            action: request.action.clone(),
        }
    }

    fn answered(&self) -> settings_store::app_consent::Consent {
        settings_store::app_consent::Consent::new(
            self.app_id.clone(),
            self.target_app.clone(),
            self.resource.clone(),
            self.action.clone(),
        )
    }
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
    /// in their settings, or for this session.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> ConsentStep {
        let Some(consent) = self.consent_question(person, app_id, request) else {
            return ConsentStep::Serve;
        };
        let key = ConsentKey::of(&consent);
        let Ok(mut pending) = self.app_action_consent_pending.lock() else {
            return ConsentStep::Refuse;
        };
        if pending.contains(&key) {
            return ConsentStep::Refuse;
        }
        let prompter = self
            .app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone());
        let Some(answer) = prompter.and_then(|prompter| prompter.ask(consent.clone())) else {
            return ConsentStep::Refuse;
        };
        pending.insert(key);
        ConsentStep::Ask(Box::new(PendingConsent {
            request: consent,
            answer,
        }))
    }

    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Option<AppActionConsentRequest> {
        let CallerPrincipal::Person(person_id) = person.principal() else {
            return None;
        };
        let workspace = person.workspace()?;
        let consents = self.services.app_consents.as_ref()?;
        let offer = self.offer(person, request)?;
        // Governing the workspace is never one click away for an app.
        let governance = self
            .services
            .workspace_governance
            .as_ref()
            .is_some_and(|governance| governance.app_id() == offer.app_id);
        if offer.action.contract.kind == tilo_actions::ActionKind::Outside
            || governance
            || offer.app_id == tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID
        {
            return None;
        }
        let key = ConsentKey {
            app_id: app_id.to_owned(),
            target_app: offer.app_id.clone(),
            resource: offer.action.address.resource.clone(),
            action: offer.action.address.action.clone(),
        };
        if consents.allows(&key.answered())
            || self
                .app_consent_session
                .lock()
                .is_ok_and(|held| held.contains(&key))
        {
            return None;
        }
        let Offer {
            app_id: target_app,
            action,
        } = offer;
        let contract = &action.contract;
        // What the call would run with, field by field, so the person sees what it changes.
        let draft = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => {
                Some(tilo_actions::Draft::render(&action, arguments))
            }
            _ => None,
        }
        .filter(|draft| !draft.fields.is_empty());
        Some(AppActionConsentRequest {
            handle: format!("app-action:{}", request.invocation.0),
            workspace: workspace.to_string(),
            person: person_id.clone(),
            app_id: app_id.to_string(),
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
            preview: None,
            scope: None,
            draft,
        })
    }

    /// The action `request` asks for, as the person using the asking app is
    /// offered it: an instance's action in the catalogue, or what the app a
    /// target names by its id offers.
    fn offer(&self, person: &Caller, request: &InvokeRequest) -> Option<Offer> {
        if let Some((workspace, app_id, action)) = self.application_offer(request) {
            let action = action.filter(|_| person.workspace() == Some(workspace.as_str()))?;
            return Some(Offer {
                app_id,
                action: action.descriptor(tilo_actions::ExecutionLocus::default()),
            });
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
        Some(Offer {
            app_id: row.app_id,
            action: row.action?,
        })
    }

    /// The question `consent` asked is off the screen.
    fn finish_app_action_consent(&self, consent: &PendingConsent) {
        if let Ok(mut pending) = self.app_action_consent_pending.lock() {
            pending.remove(&ConsentKey::of(&consent.request));
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
        let consents = self
            .services
            .app_consents
            .as_ref()
            .ok_or_else(|| "this device keeps no answers".to_owned())?;
        let key = ConsentKey::of(&consent.request);
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
            AppActionConsent::Always => consents.allow(&key.answered()),
        }
    }
}

/// Answer an application's invocation once the person has answered whether
/// it may: denied, refused; allowed, served once the answer is kept, and
/// refused with the reason when it could not be.
pub(super) fn serve_after_consent(
    state: Arc<AppBrokerState>,
    consent: Box<PendingConsent>,
    host: ConnectionActionHost,
    caller: Caller,
    message: RemoteActionMessage,
) -> super::wire::Answer<RemoteActionMessage> {
    use tilo_actions::remote::{ActionHostError, refuse};
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
                ActionHostError::Unavailable {
                    reason: "the sheet closed without the person's answer".into(),
                },
            ),
            Ok(answer) if !answer.consent.grants() => {
                refuse(&message, ActionHostError::NotPermitted)
            }
            Ok(answer) => {
                if let Err(reason) = state.keep_app_action_consent(&consent, answer.consent) {
                    refuse(&message, ActionHostError::Refused { reason })
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
                    super::witness::serve_answering(
                        Arc::clone(&state),
                        host,
                        caller,
                        person,
                        message,
                    )
                    .await
                }
            }
        };
        reply.ok_or_else(|| BrokerError::failed("the action host dropped the request".into()))
    })
}
