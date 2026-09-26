//! Asking the person before an application uses another application's
//! action it holds no grant for, and recording what they answer.
//!
//! [`CapnpBrokerState::app_door`] decides such a request for the app, which
//! holds only what the workspace grants it. Without a grant the request
//! would simply be refused; here the person who admitted the connection is
//! asked first, when they are present and may grant it. The answer is an
//! ordinary capability in the workspace's records, granted by that person
//! and noted in the workspace's history. "Allow once" lives only for the
//! call it was given for.

use std::sync::Arc;

use tilo_actions::remote::{Caller, CallerPrincipal, InvokeRequest};

use super::{Actor, CapnpBrokerState, Verb, permission_verb};
use crate::shell::app_action_consent::{
    AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use crate::shell::state::{CapOverride, Expiry, Scope, Subject, WorkspaceAuthority as _};

/// What happens to an application's invocation before it is served.
pub(crate) enum ConsentStep {
    /// It holds what it needs already, or the person could not grant it
    /// anyway: serve it as it is.
    Serve,
    /// Nobody can be asked: refuse it.
    Refuse,
    /// The person is being asked.
    Ask(PendingConsent),
}

/// A grant question on screen.
pub(crate) struct PendingConsent {
    pub(crate) request: AppActionConsentRequest,
    /// The verb the action asks of its caller, which a grant must carry.
    verb: Verb,
    pub(crate) answer: tokio::sync::oneshot::Receiver<AppActionConsent>,
}

/// A grant recorded from the person's answer.
pub(crate) struct RecordedGrant {
    room: String,
    capability: String,
    pub(crate) consumed_on_use: bool,
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
        ConsentStep::Ask(PendingConsent {
            request: consent,
            verb,
            answer,
        })
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
        let row = self
            .eager_catalogue(person, true)
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
        if descriptor.contract.permission == tilo_actions::PermissionRequirement::Admin
            || row.app_id == crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID
        {
            return None;
        }
        let membership = self.membership.as_ref()?;
        let tree = membership.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)?;
        if !tree.governs_room(workspace.room) || !workspace.may_administer(person_id) {
            return None;
        }
        let mut standing = workspace.clone();
        standing
            .capabilities
            .retain(|capability| capability.expiry != Expiry::AfterUse);
        let as_app = Actor::app(app_id).in_room(room.to_string());
        if Self::allowed(
            &standing,
            &as_app,
            &row.app_id,
            &descriptor.address.action,
            descriptor.contract.permission,
        ) {
            return None;
        }
        drop(tree);
        let contract = &descriptor.contract;
        let consent = AppActionConsentRequest {
            handle: format!("app-action:{}", request.invocation.0),
            room: room.to_string(),
            person: person_id.clone(),
            app_id: app_id.to_string(),
            target_app: row.app_id,
            action: descriptor.address.action.clone(),
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
    pub(crate) fn finish_app_action_consent(&self, consent: &PendingConsent) {
        if let Ok(mut pending) = self.app_action_consent_pending.lock() {
            pending.remove(&(consent.request.room.clone(), consent.request.app_id.clone()));
        }
    }

    /// Record the grant `answer` gives, as the person who gave it, and note
    /// it in the workspace's history. `None` when nothing is granted: a
    /// denial, or a person who may no longer grant it.
    pub(crate) fn record_app_action_grant(
        &self,
        consent: &PendingConsent,
        answer: AppActionConsent,
    ) -> Option<RecordedGrant> {
        let expiry = answer.expiry()?;
        let request = &consent.request;
        let membership = self.membership.as_ref()?;
        let mut tree = membership.write();
        let workspace_id = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == request.room)?
            .id;
        let mut capability = CapOverride::grant(
            Subject::app(request.app_id.clone()),
            Scope::Action {
                app: request.target_app.clone(),
                action: request.action.clone(),
            },
            consent.verb,
        )
        .expiring(expiry);
        if expiry == Expiry::AfterUse {
            // Its own id, so using it removes exactly this grant.
            capability = capability.with_id(format!("app-action-once:{}", uuid::Uuid::new_v4()));
        }
        let id = tree.grant_in_workspace(workspace_id, &request.person, capability)?;
        let lifetime = match answer {
            AppActionConsent::Once => "once",
            AppActionConsent::Session => "for this session",
            AppActionConsent::Always | AppActionConsent::Deny => "in this workspace",
        };
        let context = tilo_actions::ActionContext::for_resource(
            format!(
                "Allowed {} to use {}: {} {lifetime}",
                tilo_desktop_model::apps::app_label(&request.app_id),
                tilo_desktop_model::apps::app_label(&request.target_app),
                request.action_label,
            ),
            format!("workspace.permissions:{}", request.room),
            "workspace.permissions",
            "App actions",
        )
        .with_app(
            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
            "Workspace permissions",
        );
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            tilo_actions::InvocationId::new(),
            &request.person,
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        Some(RecordedGrant {
            room: request.room.clone(),
            capability: id,
            consumed_on_use: expiry == Expiry::AfterUse,
        })
    }

    /// Remove a grant given for one call, now that the call was decided.
    pub(crate) fn consume_app_action_grant(&self, grant: &RecordedGrant) {
        let Some(membership) = self.membership.as_ref() else {
            return;
        };
        let mut tree = membership.write();
        let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room.to_string() == grant.room)
        else {
            return;
        };
        let workspace_id = workspace.id;
        if workspace.revoke_capability(&grant.capability) {
            tree.doc_revoke_capability(workspace_id, &grant.capability);
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
            tree.flush_to_disk();
        }
    }
}
