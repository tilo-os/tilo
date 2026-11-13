//! Asking the person before an application uses another application's
//! action it holds no grant for, and recording what they answer.
//!
//! [`AppBrokerState::app_door`] decides such a request for the app, which
//! holds only what the workspace grants it. Without a grant the request
//! would simply be refused; here the person who admitted the connection is
//! asked first, when they are present and may grant it. The answer is an
//! ordinary grant to the application in the workspace's records, given by
//! that person through the host's
//! [`AppActionGrants`](crate::services::AppActionGrants). "Allow once" lives
//! only for the call it was given for.

use crate::BrokerError;
use std::sync::Arc;

use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Lifetime, TreePath};
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeRequest, RemoteActionMessage};

use super::action_host::{ConnectionActionHost, RowScope};
use super::{AppBrokerState, BrokerActor};
use crate::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};

/// What happens to an application's invocation before it is served.
pub(crate) enum ConsentStep {
    /// It holds what it needs already, or the person could not grant it
    /// anyway: serve it as it is.
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

/// What one grant question asks: in which workspace which app wants which
/// app's action.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConsentKey {
    workspace: String,
    app_id: String,
    target_app: String,
    resource: String,
    action: String,
}

impl ConsentKey {
    fn of(request: &AppActionConsentRequest) -> Self {
        Self {
            workspace: request.workspace.clone(),
            app_id: request.app_id.clone(),
            target_app: request.target_app.clone(),
            resource: request.resource.clone(),
            action: request.action.clone(),
        }
    }
}

/// A grant question on screen.
pub(crate) struct PendingConsent {
    request: AppActionConsentRequest,
    /// Where the action sits in the tree (its resource kind) and what it
    /// is, which the grant must cover.
    target: (String, tilo_actions::ActionNeed),
    answer: tokio::sync::oneshot::Receiver<AppActionAnswer>,
}

impl AppBrokerState {
    /// Ask whoever answers for the person using an application from now on.
    pub fn set_app_action_consent(&self, prompter: Arc<dyn AppActionConsentPrompter>) {
        if let Ok(mut current) = self.app_action_consent.write() {
            *current = Some(prompter);
        }
    }

    /// What to do with `request`, which application `app_id` makes under
    /// its own identity on a connection `person` admitted.
    ///
    /// The person is asked only for an action the catalogue offers them,
    /// that the app holds no standing grant for (a grant given once for
    /// another call does not count), in a workspace this machine governs and
    /// they administer. A grant they could not make is never offered.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> ConsentStep {
        let Some((consent, target)) = self.consent_question(person, app_id, request) else {
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
            target,
            answer,
        }))
    }

    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Option<(AppActionConsentRequest, (String, tilo_actions::ActionNeed))> {
        let CallerPrincipal::Person(person_id) = person.principal() else {
            return None;
        };
        let workspace = person.workspace()?;
        let grants = self.services.app_action_grants.as_ref()?;
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
        if !grants.may_grant(workspace, person_id) {
            return None;
        }
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        let as_app = BrokerActor::agent(format!("app:{app_id}"));
        // Standing grants only: a one-use grant is not consent to keep
        // acting. The question needs this one workspace without
        // those grants, so only it is copied, never the whole tree.
        let standing_allows = self
            .membership
            .as_ref()?
            .decide_in_workspace(workspace, |workspace| {
                let mut standing = workspace?.clone();
                standing
                    .policy
                    .grants
                    .retain(|grant| grant.lifetime != Lifetime::Once);
                // A standing grant that asks first already answered the
                // question: each call is confirmed, nothing more granted.
                Some(Self::offered(
                    &standing,
                    &as_app,
                    now,
                    &offer.app_id,
                    (
                        &offer.action.resource_kind,
                        &offer.action.address.resource,
                        &offer.action.address.action,
                    ),
                    &offer.action.contract,
                ))
            })
            .ok()??;
        if standing_allows {
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
        let consent = AppActionConsentRequest {
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
        };
        Some((consent, (action.resource_kind.clone(), contract.need())))
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

    /// Record the grant `answer` gives, as the person who gave it. `None`
    /// when nothing is granted: a denial, or a person who may no longer
    /// grant it.
    fn record_app_action_grant(
        &self,
        consent: &PendingConsent,
        answer: AppActionConsent,
    ) -> Option<crate::services::GrantRecording> {
        let lifetime = answer.grant_lifetime()?;
        let request = &consent.request;
        let grants = self.services.app_action_grants.as_ref()?;
        if !grants.may_grant(&request.workspace, &request.person) {
            return None;
        }
        let (resource_kind, need) = &consent.target;
        let grant = Grant::new(
            format!("app-action:{}", uuid::Uuid::new_v4()),
            Grantee::app(request.app_id.clone()),
            tilo_actions::action_node(
                &TreePath::workspace(request.workspace.clone()),
                &request.target_app,
                resource_kind,
                &request.resource,
                need.field.as_deref(),
                &request.action,
            ),
            tilo_action_policy::ActionKind::ALL
                .into_iter()
                .filter(|kind| *kind <= need.kind)
                .fold(KindSet::NONE, KindSet::with),
            Answer::Allow,
            Grantee::person(request.person.clone()),
        )
        .with_lifetime(lifetime);
        Some(grants.record(request, grant, answer))
    }
}

/// Answer an application's invocation once the person has answered whether
/// it may: denied, refused; allowed, served once the grant is in force, and
/// refused with the reason when it could not be put in force. A grant given once
/// is spent by the admission it lets through.
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
        // A grant question shows no draft: only the choice counts. Resolved
        // by the sheet: the person's answer, or its sender dropped with the
        // sheet, which refuses the call as unanswered, not as declined.
        let answered = (&mut consent.answer).await;
        state.finish_app_action_consent(&consent);
        // The grant is in force before the call it was given for goes on:
        // a call held while it is recorded, refused when it is not.
        let recorded = match answered {
            Err(_) => Err(ActionHostError::Unavailable {
                reason: "the sheet closed without the person's answer".into(),
            }),
            Ok(answer) => match state.record_app_action_grant(&consent, answer.consent) {
                None => Err(ActionHostError::NotPermitted),
                Some(recording) => recording
                    .await
                    .map_err(|reason| ActionHostError::Refused { reason }),
            },
        };
        let reply = if let Err(error) = recorded {
            refuse(&message, error)
        } else {
            // Answered at admission, as before waits existed: a grant
            // given for this call is spent by that admission, never held
            // open while the action runs.
            let message = match message {
                RemoteActionMessage::Invoke(mut request) => {
                    request.wait_ms = None;
                    RemoteActionMessage::Invoke(request)
                }
                other => other,
            };
            let person = consent.request.person.clone();
            super::witness::serve_answering(Arc::clone(&state), host, caller, person, message).await
        };
        reply.ok_or_else(|| BrokerError::failed("the action host dropped the request".into()))
    })
}
