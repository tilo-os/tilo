//! Asking the person before an application uses another application's
//! action it holds no grant for, and recording what they answer.
//!
//! [`CapnpBrokerState::app_door`] decides such a request for the app, which
//! holds only what the workspace grants it. Without a grant the request
//! would simply be refused; here the person who admitted the connection is
//! asked first, when they are present and may grant it. The answer is an
//! ordinary capability in the workspace's records, granted by that person
//! through the host's [`AppActionGrants`](crate::services::AppActionGrants).
//! "Allow once" lives only for the call it was given for.

use std::sync::Arc;

use capnp::capability::Promise;
use collab_types::caps::{CapOverride, Expiry, Scope, Subject};
use runtime_collab::raw::channel_handle;
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeRequest, RemoteActionMessage};

use super::action_host::{ConnectionActionHost, RowScope};
use super::{Actor, CapnpBrokerState, Verb, permission_verb};
use crate::app_actions::{AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest};

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

/// A grant question on screen.
pub(crate) struct PendingConsent {
    request: AppActionConsentRequest,
    /// The verb the action asks of its caller, which a grant must carry.
    verb: Verb,
    answer: tokio::sync::oneshot::Receiver<AppActionConsent>,
}

/// A grant recorded from the person's answer.
struct RecordedGrant {
    room: String,
    capability: String,
    consumed_on_use: bool,
}

impl CapnpBrokerState {
    /// Ask whoever answers for the person using an application from now on.
    pub(crate) fn set_app_action_consent(&self, prompter: Arc<dyn AppActionConsentPrompter>) {
        if let Ok(mut current) = self.app_action_consent.write() {
            *current = Some(prompter);
        }
    }

    /// What to do with `request`, which application `app_id` makes under
    /// its own identity on a connection `person` admitted.
    ///
    /// The person is asked only for an action the catalogue offers them,
    /// that the app holds no standing grant for (a grant given once for
    /// another call does not count), in a room this machine governs and
    /// they administer. A grant they could not make is never offered.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> ConsentStep {
        let Some((consent, verb)) = self.consent_question(person, app_id, request) else {
            return ConsentStep::Serve;
        };
        let key = (consent.room.clone(), consent.app_id.clone());
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
            verb,
            answer,
        }))
    }

    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Option<(AppActionConsentRequest, Verb)> {
        let CallerPrincipal::Person(person_id) = person.principal() else {
            return None;
        };
        let room = person.room()?;
        let grants = self.services.app_action_grants.as_ref()?;
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
        let descriptor = row.action?;
        // Governing the workspace is never one click away for an app.
        let governance = self
            .services
            .workspace_governance
            .as_ref()
            .is_some_and(|governance| governance.app_id() == row.app_id);
        if descriptor.contract.permission == tilo_actions::PermissionRequirement::Admin
            || governance
            || row.app_id == tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID
        {
            return None;
        }
        if !grants.may_grant(room, person_id) {
            return None;
        }
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        let as_app = Actor::app(app_id).in_room(room.to_string());
        // Standing grants only: a one-use grant is not consent to keep
        // acting. The question needs this one room's workspace without
        // those grants, so only it is copied, never the whole tree.
        let standing_allows = self
            .membership
            .as_ref()?
            .decide_in_room(room, |workspace| {
                let mut standing = workspace?.clone();
                standing
                    .capabilities
                    .retain(|capability| capability.expiry != Expiry::AfterUse);
                Some(Self::allowed(
                    &standing,
                    &as_app,
                    now,
                    &row.app_id,
                    &descriptor.address.resource,
                    &descriptor.address.action,
                    descriptor.contract.permission,
                ))
            })
            .ok()??;
        if standing_allows {
            return None;
        }
        let contract = &descriptor.contract;
        let consent = AppActionConsentRequest {
            handle: format!("app-action:{}", request.invocation.0),
            room: room.to_string(),
            person: person_id.clone(),
            app_id: app_id.to_string(),
            target_app: row.app_id,
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            risk: contract.risk,
            needs_confirmation: contract.advertised_confirmation()
                != tilo_actions::ConfirmationPolicy::None
                || contract.risk.requires_confirmation(),
        };
        Some((
            consent,
            permission_verb(contract.permission).unwrap_or(Verb::Read),
        ))
    }

    /// The question `consent` asked is off the screen.
    fn finish_app_action_consent(&self, consent: &PendingConsent) {
        if let Ok(mut pending) = self.app_action_consent_pending.lock() {
            pending.remove(&(consent.request.room.clone(), consent.request.app_id.clone()));
        }
    }

    /// Record the grant `answer` gives, as the person who gave it. `None`
    /// when nothing is granted: a denial, or a person who may no longer
    /// grant it.
    fn record_app_action_grant(
        &self,
        consent: &PendingConsent,
        answer: AppActionConsent,
    ) -> Option<RecordedGrant> {
        let expiry = answer.expiry()?;
        let request = &consent.request;
        let grants = self.services.app_action_grants.as_ref()?;
        if !grants.may_grant(&request.room, &request.person) {
            return None;
        }
        let mut capability = CapOverride::grant(
            Subject::app(request.app_id.clone()),
            Scope::Action {
                app: request.target_app.clone(),
                resource: request.resource.clone(),
                action: request.action.clone(),
            },
            consent.verb,
        )
        .expiring(expiry);
        if expiry == Expiry::AfterUse {
            // Its own id, so using it removes exactly this grant.
            capability = capability.with_id(format!("app-action-once:{}", uuid::Uuid::new_v4()));
        }
        let id = grants.record(request, capability, answer)?;
        Some(RecordedGrant {
            room: request.room.clone(),
            capability: id,
            consumed_on_use: expiry == Expiry::AfterUse,
        })
    }

    /// Remove a grant given for one call, now that the call was decided.
    fn consume_app_action_grant(&self, grant: &RecordedGrant) {
        if let Some(grants) = self.services.app_action_grants.as_ref() {
            grants.revoke(&grant.room, &grant.capability);
        }
    }
}

/// Answer an application's invocation once the person has answered whether
/// it may: denied, refused; allowed, recorded and served, and a grant given
/// once removed as soon as the call is decided.
pub(super) fn serve_after_consent(
    state: Arc<CapnpBrokerState>,
    consent: Box<PendingConsent>,
    host: ConnectionActionHost,
    caller: Caller,
    message: RemoteActionMessage,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    use tilo_actions::remote::{ActionHostError, RemoteActionFrame, refuse, serve};
    Promise::from_future(async move {
        let mut consent = consent;
        let answer = (&mut consent.answer)
            .await
            .unwrap_or(AppActionConsent::Deny);
        state.finish_app_action_consent(&consent);
        let reply = match state.record_app_action_grant(&consent, answer) {
            None => refuse(&message, ActionHostError::NotPermitted),
            Some(grant) => {
                // Answered at admission, as before waits existed: a grant
                // given for this call is spent once the call is decided,
                // never held open while the action runs.
                let message = match message {
                    RemoteActionMessage::Invoke(mut request) => {
                        request.wait_ms = None;
                        RemoteActionMessage::Invoke(request)
                    }
                    other => other,
                };
                let answered = match serve(&host, &caller, message) {
                    Some(answer) => answer.await,
                    None => None,
                };
                if grant.consumed_on_use {
                    state.consume_app_action_grant(&grant);
                }
                answered
            }
        };
        let reply = reply
            .ok_or_else(|| capnp::Error::failed("the action host dropped the request".into()))?;
        let bytes = RemoteActionFrame::new(reply)
            .encode()
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_frame(&bytes);
        Ok(())
    })
}
