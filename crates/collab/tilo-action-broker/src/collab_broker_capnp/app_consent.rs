//! Asking the person before an application uses another application's
//! action it holds no grant for, and recording what they answer.
//!
//! [`CapnpBrokerState::app_door`] decides such a request for the app, which
//! holds only what the workspace grants it. Without a grant the request
//! would simply be refused; here the person who admitted the connection is
//! asked first, when they are present and may grant it. The answer is an
//! ordinary grant to the application in the workspace's records, given by
//! that person through the host's
//! [`AppActionGrants`](crate::services::AppActionGrants). "Allow once" lives
//! only for the call it was given for.

use std::sync::Arc;

use capnp::capability::Promise;
use runtime_collab::raw::channel_handle;
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
use tilo_actions::remote::{Caller, CallerPrincipal, InvokeRequest, RemoteActionMessage};

use super::action_host::{ConnectionActionHost, RowScope};
use super::{BrokerActor, CapnpBrokerState};
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
    /// Where the action sits in the tree (its resource kind) and what it
    /// is, which the grant must cover.
    target: (String, tilo_actions::ActionNeed),
    answer: tokio::sync::oneshot::Receiver<AppActionConsent>,
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
        let Some((consent, target)) = self.consent_question(person, app_id, request) else {
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
        if descriptor.contract.kind == tilo_actions::ActionKind::Outside
            || governance
            || row.app_id == tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID
        {
            return None;
        }
        if !grants.may_grant(room, person_id) {
            return None;
        }
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        let as_app = BrokerActor::agent(format!("app:{app_id}"));
        // Standing grants only: a one-use grant is not consent to keep
        // acting. The question needs this one room's workspace without
        // those grants, so only it is copied, never the whole tree.
        let standing_allows = self
            .membership
            .as_ref()?
            .decide_in_room(room, |workspace| {
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
                    &row.app_id,
                    (
                        &descriptor.resource_kind,
                        &descriptor.address.resource,
                        &descriptor.address.action,
                    ),
                    &descriptor.contract,
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
            confirming: false,
            moderation: None,
            preview: None,
            scope: None,
            draft: None,
        };
        Some((consent, (descriptor.resource_kind.clone(), contract.need())))
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
    ) -> Option<String> {
        let lifetime = answer.grant_lifetime()?;
        let request = &consent.request;
        let grants = self.services.app_action_grants.as_ref()?;
        if !grants.may_grant(&request.room, &request.person) {
            return None;
        }
        let (resource_kind, need) = &consent.target;
        let grant = Grant::new(
            format!("app-action:{}", uuid::Uuid::new_v4()),
            Subject::App(request.app_id.clone()),
            tilo_actions::action_node(
                &TreePath::room(request.room.clone()),
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
            Subject::Person(request.person.clone()),
        )
        .with_lifetime(lifetime);
        grants.record(request, grant, answer)
    }
}

/// Answer an application's invocation once the person has answered whether
/// it may: denied, refused; allowed, recorded and served. A grant given once
/// is spent by the admission it lets through.
pub(super) fn serve_after_consent(
    state: Arc<CapnpBrokerState>,
    consent: Box<PendingConsent>,
    host: ConnectionActionHost,
    caller: Caller,
    message: RemoteActionMessage,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    use tilo_actions::remote::{ActionHostError, RemoteActionFrame, refuse};
    Promise::from_future(async move {
        let mut consent = consent;
        let answer = (&mut consent.answer)
            .await
            .unwrap_or(AppActionConsent::Deny);
        state.finish_app_action_consent(&consent);
        let reply = if state.record_app_action_grant(&consent, answer).is_none() {
            refuse(&message, ActionHostError::NotPermitted)
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
        let reply = reply
            .ok_or_else(|| capnp::Error::failed("the action host dropped the request".into()))?;
        let bytes = RemoteActionFrame::new(reply)
            .encode()
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_frame(&bytes);
        Ok(())
    })
}
