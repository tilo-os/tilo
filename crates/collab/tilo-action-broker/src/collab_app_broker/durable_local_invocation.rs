//! Durable local RPCs use the existing journal and invocation dispatcher.
use super::invocation_journal::{InvocationJournal, InvocationKey};
use super::{
    AppBrokerState, BrokerActor, CallerInvocation, DurableInvocation, InvocationCaller,
    InvocationEnvelope, InvocationOwner, PublishedAction, PublishedSurface, VerifiedActionCaller,
    mpsc,
};
use tilo_action_policy::MemberId;
use tilo_actions::remote::Caller;

/// Borrowed, genuine caller evidence; never constructed from an actor string.
#[derive(Clone, Copy)]
pub(super) enum DurableContext<'a> {
    /// Only the durable admission tests build this caller kind: the action
    /// api admits a device's request on its own path.
    #[allow(
        dead_code,
        reason = "driven by the durable admission tests, which reach it through a test-only entrypoint"
    )]
    Peer(&'a VerifiedActionCaller, &'a CallerInvocation),
}
impl<'a> DurableContext<'a> {
    pub(super) fn owner(self) -> Result<InvocationOwner, String> {
        let Self::Peer(c, _) = self;
        c.owner()
    }
    pub(super) fn time(self) -> Option<&'a crate::policy_clock::PolicyTime> {
        let Self::Peer(c, _) = self;
        c.policy_time.as_ref()
    }
    pub(super) fn invocation(self) -> tilo_actions::InvocationId {
        let Self::Peer(_, r) = self;
        r.invocation
    }
    pub(super) fn intent(self) -> serde_json::Value {
        let Self::Peer(_, r) = self;
        r.intent()
    }
    pub(super) fn origin(self) -> InvocationCaller {
        let Self::Peer(c, _) = self;
        InvocationCaller::Certified {
            caller: Box::new(c.clone()),
            // This path has only the original signed device restriction.
            foreign_restriction: false,
        }
    }
    pub(super) fn before_admission(
        self,
        state: &AppBrokerState,
        peer: u64,
        offered: &(PublishedSurface, PublishedAction),
        sender: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) -> Result<(), String> {
        let Self::Peer(c, _) = self;
        let current = state.current_action_caller(c)?;
        state.require_current_stable_sender(peer, sender)?;
        let now = state.checked_policy_time(self.time())?.1 / 1_000_000_000;
        let target = (
            offered.0.app_id.as_str(),
            offered.1.resource_kind.as_str(),
            offered.1.resource.as_str(),
            offered.1.action.as_str(),
        );
        let need = offered.1.contract.need();
        state.authorize_invocation_permission(
            &offered.0.route.cluster_id,
            target,
            &need,
            &current.broker_actor(),
            now,
        )?;
        state.checked_policy_time(self.time())?;
        Ok(())
    }
}

/// The action check of one admission, decided on the caller's borrowed
/// sample of exactly that workspace's committed workspace.
pub(super) fn authorize_action_in(
    workspace: &tilo_desktop_model::Workspace,
    (app, resource_kind, resource, action): (&str, &str, &str, &str),
    need: &tilo_actions::ActionNeed,
    actor: &BrokerActor,
    now: u64,
) -> Result<(), String> {
    // Genuine durable callers revalidate live membership and delegation
    // separately; this is the action's own grant.
    if !actor
        .action_answer_in(workspace, app, (resource_kind, resource, action), need, now)
        .admits(actor.direct())
    {
        return Err("caller no longer authorized for invocation".into());
    }
    Ok(())
}

impl AppBrokerState {
    /// Revoke the `Once` grants an admitted action spent in `workspace`
    /// ([`tilo_action_policy::Verdict::spent`]), through the host that
    /// records the workspace's grants.
    pub(super) fn spend_once_grants(&self, workspace: &str, spent: &[tilo_action_policy::GrantId]) {
        let Some(grants) = self.services.workspace_grants.as_ref() else {
            return;
        };
        for grant in spent {
            grants.revoke(workspace, grant.as_str());
        }
    }
}

impl AppBrokerState {
    pub(super) fn authorize_invocation_permission(
        &self,
        workspace: &str,
        (app, resource_kind, resource, action): (&str, &str, &str, &str),
        need: &tilo_actions::ActionNeed,
        actor: &BrokerActor,
        now: u64,
    ) -> Result<(), String> {
        self.membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_workspace(workspace, |workspace| {
                let workspace = workspace.ok_or("workspace no longer hosted")?;
                authorize_action_in(
                    workspace,
                    (app, resource_kind, resource, action),
                    need,
                    actor,
                    now,
                )
            })?
    }
    pub(super) fn require_current_stable_sender(
        &self,
        peer: u64,
        sender: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) -> Result<(), String> {
        if !self
            .stable_invocation_subscribers
            .lock()
            .map_err(|_| "application capability unavailable")?
            .contains(&peer)
        {
            return Err("application_stable_identity_unavailable".into());
        }
        if sender.is_closed()
            || !self
                .invocation_subscribers
                .lock()
                .map_err(|_| "application invocation stream unavailable")?
                .get(&peer)
                .is_some_and(|current| !current.is_closed() && current.same_channel(sender))
        {
            return Err("application invocation stream closed or replaced".into());
        }
        Ok(())
    }
}

fn unavailable(reason: impl Into<String>) -> tilo_actions::remote::ActionHostError {
    tilo_actions::remote::ActionHostError::Unavailable {
        reason: reason.into(),
    }
}

impl AppBrokerState {
    /// Who owns an invocation admitted for `actor` in `workspace` under `origin`:
    /// exactly the owner its admission records, so an observation or a retry
    /// looks it up under the same key the admission wrote.
    pub(super) fn invocation_owner(
        &self,
        origin: Option<&InvocationCaller>,
        workspace: &str,
        actor: &BrokerActor,
    ) -> Result<InvocationOwner, String> {
        Ok(match origin {
            Some(InvocationCaller::Certified { caller, .. }) => caller.owner()?,
            Some(InvocationCaller::NativePerson { certificate, .. }) => {
                InvocationOwner::AuthenticatedLocal {
                    root: certificate.user_public_key,
                    device: certificate.device_public_key,
                    agent: None,
                }
            }
            Some(InvocationCaller::Local {
                route,
                admitted,
                time,
            }) => {
                self.checked_policy_time(time.as_ref())?;
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| error.to_string())?;
                let certificate = current
                    .certificate
                    .as_ref()
                    .ok_or("original caller certificate missing")?;
                if route.cluster_id != workspace {
                    return Err("original local caller does not match invocation workspace".into());
                }
                let expected = current.agent_id.as_ref().map_or_else(
                    || MemberId::person(super::person_id_for_root(certificate.user_public_key)),
                    |agent| MemberId::agent(super::delegated_agent_name(agent)),
                );
                if *actor.member() == expected {
                    InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: current.agent_id.clone(),
                    }
                } else {
                    // The connection's ownership attests only its own admitted
                    // Person or agent. An application acting under its own
                    // identity, or an effect continuing another caller's
                    // invocation, is decided as that actor and stays
                    // actor-owned: never attested as the connection's.
                    InvocationOwner::Local {
                        actor: actor.member().clone(),
                    }
                }
            }
            None => InvocationOwner::Local {
                actor: actor.member().clone(),
            },
        })
    }

    /// The record `journal` retains for `invocation` as `owner`'s, once its
    /// action is still allowed for `actor`: what an observation or a repeated
    /// request is answered with, never a permit to run it again.
    pub(super) fn retained_invocation(
        &self,
        journal: &InvocationJournal,
        workspace: &str,
        owner: &InvocationOwner,
        invocation: tilo_actions::InvocationId,
        actor: &BrokerActor,
        now: u64,
    ) -> Result<Option<DurableInvocation>, String> {
        let key = InvocationKey {
            workspace: workspace.into(),
            host: journal.host(),
            owner: owner.clone(),
            invocation,
        };
        let Some(record) = journal.lookup(&key)? else {
            return Ok(None);
        };
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent.get("contract").cloned().unwrap_or_default())
                .map_err(|_| "retained invocation contract invalid")?;
        let field = |name| {
            record
                .intent
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "retained invocation intent invalid".to_string())
        };
        self.authorize_invocation_permission(
            workspace,
            (
                field("app_id")?,
                field("resource_kind")?,
                field("resource")?,
                field("action")?,
            ),
            &contract.need(),
            actor,
            now,
        )?;
        Ok(Some(record))
    }

    /// Invocation `invocation` as `caller` last had it, from the journal: the
    /// answer the connection, its application and this process holding it
    /// cannot take away. `None` when the caller had nothing admitted under
    /// that id.
    pub(super) fn retained_observation(
        &self,
        caller: &Caller,
        origin: &InvocationCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<tilo_actions::remote::InvocationState>, tilo_actions::remote::ActionHostError>
    {
        let workspace = caller
            .workspace()
            .ok_or_else(|| unavailable("caller workspace unavailable"))?;
        let actor = self.caller_actor(caller, workspace);
        // Ordinary durable observations may reconcile effects too. A workspace
        // that confines its agents keeps an agent from observing on a grant
        // alone: only one in an agent session that still goes observes.
        if actor.is_agent() && !actor.is_app() && self.confines_agents(workspace) {
            match self.agent_session_of(Some(caller), Some(origin), workspace) {
                Some(Ok(_)) => {}
                Some(Err(ended)) => return Err(unavailable(ended)),
                None => {
                    return Err(unavailable("an agent acts only in an agent session"));
                }
            }
        }
        let owner = self
            .invocation_owner(Some(origin), workspace, &actor)
            .map_err(unavailable)?;
        let now = self.policy_now_ns().map_err(unavailable)? / 1_000_000_000;
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| unavailable("invocation journal unavailable"))?;
            let Some(journal) = guard
                .as_ref()
                .map_err(|error| unavailable(error.clone()))?
                .as_ref()
            else {
                return Ok(None);
            };
            let record = self
                .retained_invocation(journal, workspace, &owner, invocation, &actor, now)
                .map_err(unavailable)?;
            drop(guard);
            record
        };
        Ok(record.map(|record| {
            tilo_actions::remote::InvocationState::new(invocation, record.update)
                .with_untrusted(self.untrusted_job(record.wire_id))
        }))
    }
}
