//! Asking the person before an application uses another application's
//! action, and keeping their answer.
//!
//! An app reaches its own actions and records with its person's authority
//! ([`AppBrokerState::app_api`]). Another app's action it takes only as the
//! workspace's grants naming the app say ([`Grantee::App`]), decided as an
//! agent's are. Where none answers, the call waits for the person as its asked
//! record ([`ActionCallError::asks`]): their approve admits it again, and
//! approving it for good gives the grant naming the app in the workspace, where
//! it shows under Apps in Privacy & Security and is revoked like any other. A
//! call outside any workspace, or for an action nobody offers the person, is
//! refused with the reason.

use tilo_action_policy::{Answer, Basis, Grantee};
use tilo_actions::remote::asked::{AskedBy, AskedFacts};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};

use super::AppBrokerState;
use super::action_provider::RowScope;

impl AppBrokerState {
    /// Whether application `app_id` may make `request` under its own identity on a connection
    /// `person` admitted: where it needs nothing, or the person approved it.
    ///
    /// # Errors
    /// The call waits for the person ([`ActionCallError::ConfirmationRequired`]): it is for an
    /// action the catalogue offers them, in a workspace this machine hosts, that no grant of
    /// the workspace lets the app take or keeps it from. Without a person or a workspace, nobody
    /// can let it, and it is refused.
    pub(crate) fn app_action_consent(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Result<(), ActionCallError> {
        if request.approval().is_some() {
            return Ok(());
        }
        match self.consent_question(person, app_id, request)? {
            None => Ok(()),
            Some(facts) => Err(ActionCallError::asks(facts)),
        }
    }

    /// What the person is shown: `None` when the call needs no answer, `Err` with what to tell
    /// the app when nobody can answer it or a grant keeps the app from it.
    fn consent_question(
        &self,
        person: &Caller,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Result<Option<AskedFacts>, ActionCallError> {
        let refused = |reason: &str| ActionCallError::Refused {
            reason: reason.to_owned(),
        };
        person.acting().person_id().ok_or_else(|| {
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
        match self.app_answer(workspace, app_id, &target_app, &action)? {
            Some(Answer::Allow) => return Ok(None),
            Some(Answer::Deny) => {
                return Err(refused(
                    "this workspace does not let this app take that app's action",
                ));
            }
            Some(Answer::Ask) | None => {}
        }
        let by = AskedBy::App {
            app: app_id.to_owned(),
            title: app_id.to_owned(),
            author: None,
        };
        Ok(Some(super::approval::described(
            AskedFacts::by(by),
            action,
            &request.argument,
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
                    &held.policy.node(),
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
}
