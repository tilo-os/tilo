//! Durable local RPCs use the existing journal and invocation dispatcher.
use super::invocation_journal::{InvocationJournal, InvocationKey};
use super::{
    AdmittedJoinIdentity, AppBrokerState, BrokerActor, CallerInvocation, CrdtRoute,
    DurableInvocation, InvocationCaller, InvocationEnvelope, InvocationOwner, JoinedChannel,
    PublishedAction, PublishedSurface, VerifiedActionCaller, mpsc,
};
use tilo_actions::remote::Caller;

#[derive(Clone)]
pub(super) struct LocalCaller {
    peer: u64,
    route: CrdtRoute,
    admitted: AdmittedJoinIdentity,
    /// Validated from `admitted` once, at capture.
    owner: InvocationOwner,
    time: crate::peer_actions::PolicyTime,
}
impl LocalCaller {
    /// The room it acts in and who the host admitted on it.
    pub(super) fn admitted_in(&self) -> (String, AdmittedJoinIdentity) {
        (self.route.cluster_id.clone(), self.admitted.clone())
    }
    /// The route its connection was admitted on.
    pub(super) const fn route(&self) -> &CrdtRoute {
        &self.route
    }
    pub(super) fn capture(channel: &JoinedChannel) -> Result<Self, String> {
        Self::admitted_on(
            &channel.state,
            channel.peer_id,
            channel.route(),
            &channel.verified_identity,
        )
    }
    /// The caller `admitted` on connection `peer` of `route`, now: what
    /// [`Self::capture`] takes from a channel, for a connection the broker
    /// serves in this process (the desktop's agent door).
    pub(super) fn admitted_on(
        state: &AppBrokerState,
        peer: u64,
        route: CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<Self, String> {
        let time = state
            .registered_policy_time()?
            .ok_or("durable local caller needs original policy time")?;
        let caller = Self {
            peer,
            route,
            owner: Self::owner_of(admitted)?,
            admitted: admitted.clone(),
            time,
        };
        caller.current(state)?;
        Ok(caller)
    }
    fn owner_of(admitted: &AdmittedJoinIdentity) -> Result<InvocationOwner, String> {
        admitted.invocation_owner()
    }
    pub(super) fn owner(&self) -> InvocationOwner {
        self.owner.clone()
    }
    /// Check the exact host action against the live authenticated connection,
    /// including its device ceiling. Call inside the owner's serialized admission.
    pub(super) fn authorize_host_action(
        &self,
        state: &AppBrokerState,
        room: &str,
        requirement: &crate::services::NativeTaskActionRequirement,
    ) -> Result<Vec<tilo_action_policy::GrantId>, String> {
        self.current(state)?;
        // The host action serves the run's task: it is its starter's.
        let actor = requirement.binding.actor()?;
        let now = state.checked_policy_time(Some(&self.time))?.1 / 1_000_000_000;
        let spent = state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(room, |workspace| {
                let workspace = workspace.ok_or("workspace no longer hosted")?;
                let (answer, spent) = actor.action_verdict_in(
                    workspace,
                    &requirement.app,
                    (
                        &requirement.resource_kind,
                        &requirement.resource,
                        &requirement.action,
                    ),
                    &requirement.need,
                    now,
                );
                // An answer asking first admits what the person confirmed;
                // it is then refused only when no longer allowed at all.
                let admitted = if requirement.confirmed {
                    answer != tilo_action_policy::Answer::Deny
                } else {
                    answer.admits(actor.direct())
                };
                if !admitted {
                    return Err("caller no longer authorized for invocation".to_string());
                }
                Ok(spent)
            })??;
        state.checked_policy_time(Some(&self.time))?;
        Ok(spent)
    }

    pub(super) fn authorize_retained_task(
        &self,
        state: &AppBrokerState,
        (record, actor): (&DurableInvocation, &super::BrokerActor),
    ) -> Result<crate::services::NativeTaskActionRequirement, String> {
        self.current(state)?;
        let contract: tilo_actions::ActionContract = serde_json::from_value(
            record
                .intent
                .get("contract")
                .cloned()
                .ok_or("retained task contract absent")?,
        )
        .map_err(|_| "retained task contract invalid")?;
        let app = record
            .intent
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("retained task app absent")?;
        let action = record
            .intent
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or("retained task action absent")?;
        let resource = record
            .intent
            .get("resource")
            .and_then(serde_json::Value::as_str)
            .ok_or("retained task resource absent")?;
        let resource_kind = record
            .intent
            .get("resource_kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("retained task resource kind absent")?;
        let now = state.checked_policy_time(Some(&self.time))?.1 / 1_000_000_000;
        let need = contract.need();
        // A task's child was admitted for an agent, which an answer asking
        // first admits only on its person's confirmation: observing what was
        // admitted needs the action not to be refused since, for the run's
        // starter.
        state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(&self.route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("workspace no longer hosted")?;
                if actor.action_answer_in(
                    workspace,
                    app,
                    (resource_kind, resource, action),
                    &need,
                    now,
                ) == tilo_action_policy::Answer::Deny
                {
                    return Err("caller no longer authorized for invocation".to_string());
                }
                Ok(())
            })??;
        Ok(crate::services::NativeTaskActionRequirement {
            binding: match &record.key.owner {
                InvocationOwner::NativeTask { task, agent, epoch } => {
                    crate::services::NativeTaskBinding {
                        task_id: task.clone(),
                        agent_id: agent.clone(),
                        epoch: *epoch,
                        revision: 0,
                        starter: None,
                    }
                }
                _ => return Err("the retained invocation is not a run's".into()),
            },
            app: app.into(),
            resource_kind: resource_kind.into(),
            resource: resource.into(),
            action: action.into(),
            need,
            // Observing what was admitted: an admission that asked first
            // was answered by the person.
            consequential: false,
            attended: false,
            confirmed: true,
        })
    }
    pub(super) fn current(&self, state: &AppBrokerState) -> Result<AdmittedJoinIdentity, String> {
        if !state.is_accepting() {
            return Err("invocation broker retired".into());
        }
        state.checked_policy_time(Some(&self.time))?;
        if let Some(registration) = &self.admitted.worker_registration {
            let workers = state
                .host_workers
                .lock()
                .map_err(|_| "worker registry unavailable")?;
            if workers
                .get(self.peer, &registration.route)
                .and_then(|worker| worker.worker_registration.as_ref())
                != Some(registration)
            {
                return Err("original local caller connection retired or replaced".into());
            }
        }
        let current = state
            .revalidate_admission(&self.route, &self.admitted)
            .map_err(|error| error.to_string())?;
        if Self::owner_of(&current)? != self.owner {
            return Err("original local caller changed".into());
        }
        Ok(current)
    }
    fn origin(&self) -> InvocationCaller {
        InvocationCaller::Local {
            route: self.route.clone(),
            admitted: Box::new(self.admitted.clone()),
            time: Some(self.time.clone()),
        }
    }
}

/// Borrowed, genuine caller evidence; never constructed from an actor string.
#[derive(Clone, Copy)]
pub(super) enum DurableContext<'a> {
    Peer(&'a VerifiedActionCaller, &'a CallerInvocation),
    Task {
        caller: &'a LocalCaller,
        binding: &'a crate::services::NativeTaskBinding,
        invocation: tilo_actions::InvocationId,
        intent: &'a serde_json::Value,
    },
}
impl<'a> DurableContext<'a> {
    pub(super) fn owner(self) -> Result<InvocationOwner, String> {
        match self {
            Self::Peer(c, _) => c.owner(),
            Self::Task { binding, .. } => Ok(InvocationOwner::NativeTask {
                agent: binding.agent_id.clone(),
                task: binding.task_id.clone(),
                epoch: binding.epoch,
            }),
        }
    }
    pub(super) fn time(self) -> Option<&'a crate::peer_actions::PolicyTime> {
        match self {
            Self::Peer(c, _) => c.policy_time.as_ref(),
            Self::Task { caller, .. } => Some(&caller.time),
        }
    }
    pub(super) fn invocation(self) -> tilo_actions::InvocationId {
        match self {
            Self::Peer(_, r) => r.invocation,
            Self::Task { invocation, .. } => invocation,
        }
    }
    pub(super) fn intent(self) -> serde_json::Value {
        match self {
            Self::Peer(_, r) => r.intent(),
            Self::Task { intent, .. } => intent.clone(),
        }
    }
    pub(super) fn origin(self) -> InvocationCaller {
        match self {
            Self::Peer(c, _) => InvocationCaller::Certified {
                caller: Box::new(c.clone()),
                // This path has only the original signed device restriction.
                foreign_restriction: false,
            },
            Self::Task { caller, .. } => caller.origin(),
        }
    }
    pub(super) fn network(self) -> Option<tilo_relay_session::client::CertifiedAction> {
        match self {
            Self::Peer(c, _) => c.network.clone(),
            Self::Task { .. } => None,
        }
    }
    pub(super) fn before_admission(
        self,
        state: &AppBrokerState,
        peer: u64,
        offered: &(PublishedSurface, PublishedAction),
        sender: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) -> Result<(), String> {
        let current = match self {
            Self::Peer(c, _) => state.current_action_caller(c)?,
            Self::Task { caller, .. } => caller.current(state)?,
        };
        state.require_current_stable_sender(peer, sender)?;
        let now = state.checked_policy_time(self.time())?.1 / 1_000_000_000;
        let target = (
            offered.0.app_id.as_str(),
            offered.1.resource_kind.as_str(),
            offered.1.resource.as_str(),
            offered.1.action.as_str(),
        );
        let need = offered.1.contract.need();
        if let Self::Task { binding, .. } = self {
            // The child serves the run's task: it is admitted for its starter.
            let actor = binding.actor()?;
            // A task's child was admitted in its run's serialized turn, which
            // pauses an action asking first until its person answers; the
            // dispatch then takes a token bound to exactly what they
            // confirmed. Here it is only refused when no longer allowed at all.
            state
                .membership
                .as_ref()
                .ok_or("workspace authority unavailable")?
                .decide_in_room(&offered.0.route.cluster_id, |workspace| {
                    let workspace = workspace.ok_or("workspace no longer hosted")?;
                    if actor.action_answer_in(
                        workspace,
                        target.0,
                        (target.1, target.2, target.3),
                        &need,
                        now,
                    ) == tilo_action_policy::Answer::Deny
                    {
                        return Err("caller no longer authorized for invocation".to_string());
                    }
                    Ok(())
                })??;
        } else {
            state.authorize_invocation_permission(
                &offered.0.route.cluster_id,
                target,
                &need,
                &current.broker_actor(),
                now,
            )?;
        }
        state.checked_policy_time(self.time())?;
        Ok(())
    }
}

/// The action check of one admission, decided on the caller's borrowed
/// sample of exactly that room's committed workspace.
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
    /// Revoke the `Once` grants an admitted action spent in `room`
    /// ([`tilo_action_policy::Verdict::spent`]), through the host that
    /// records the room's grants.
    pub(super) fn spend_once_grants(&self, room: &str, spent: &[tilo_action_policy::GrantId]) {
        let Some(grants) = self.services.app_action_grants.as_ref() else {
            return;
        };
        for grant in spent {
            grants.revoke(room, grant.as_str());
        }
    }
}

impl AppBrokerState {
    pub(super) fn authorize_invocation_permission(
        &self,
        room: &str,
        (app, resource_kind, resource, action): (&str, &str, &str, &str),
        need: &tilo_actions::ActionNeed,
        actor: &BrokerActor,
        now: u64,
    ) -> Result<(), String> {
        self.membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(room, |workspace| {
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
    /// Who owns an invocation admitted for `actor` in `room` under `origin`:
    /// exactly the owner its admission records, so an observation or a retry
    /// looks it up under the same key the admission wrote.
    pub(super) fn invocation_owner(
        &self,
        origin: Option<&InvocationCaller>,
        room: &str,
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
                if route.cluster_id != room {
                    return Err("original local caller does not match invocation room".into());
                }
                let expected = current.agent_id.as_ref().map_or_else(
                    || super::principal_id_for_root(certificate.user_public_key),
                    |agent| format!("agent:{}", agent.strip_prefix("agent:").unwrap_or(agent)),
                );
                if actor.id == expected && actor.is_agent == current.agent_id.is_some() {
                    InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: current.agent_id.clone(),
                    }
                } else {
                    // The connection's custody attests only its own admitted
                    // Person or agent. An application acting under its own
                    // identity, or an effect continuing another caller's
                    // invocation, is decided as that actor and stays
                    // actor-owned: never attested as the connection's.
                    InvocationOwner::Local {
                        actor: actor.id.clone(),
                    }
                }
            }
            None => InvocationOwner::Local {
                actor: actor.id.clone(),
            },
        })
    }

    /// The record `journal` retains for `invocation` as `owner`'s, once its
    /// action is still allowed for `actor`: what an observation or a repeated
    /// request is answered with, never a permit to run it again.
    pub(super) fn retained_invocation(
        &self,
        journal: &InvocationJournal,
        room: &str,
        owner: &InvocationOwner,
        invocation: tilo_actions::InvocationId,
        actor: &BrokerActor,
        now: u64,
    ) -> Result<Option<DurableInvocation>, String> {
        let key = InvocationKey {
            room: room.into(),
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
            room,
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
    /// # Errors
    /// The journal is unavailable, or the caller's current permission no
    /// longer covers the invocation.
    pub(super) fn retained_observation(
        &self,
        caller: &Caller,
        origin: &InvocationCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<tilo_actions::remote::InvocationState>, tilo_actions::remote::ActionHostError>
    {
        let room = caller
            .room()
            .ok_or_else(|| unavailable("caller room unavailable"))?;
        let actor = self.caller_actor(caller, room);
        // Ordinary durable observations may reconcile effects too. Only a room
        // that confines its agents to their runs keeps a task agent from
        // observing on a grant alone: that needs an authenticated admission.
        if actor.is_agent && self.confines_agents_to_tasks(room) {
            return Err(unavailable(
                "native task agent requires authenticated task action admission",
            ));
        }
        let owner = self
            .invocation_owner(Some(origin), room, &actor)
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
                .retained_invocation(journal, room, &owner, invocation, &actor, now)
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
