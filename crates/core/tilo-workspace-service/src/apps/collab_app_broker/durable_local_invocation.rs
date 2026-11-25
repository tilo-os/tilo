//! Durable local RPCs use the existing journal and invocation dispatcher.
use super::invocation_journal::{InvocationJournal, InvocationKey};
use super::{AppBrokerState, BrokerActor, DurableInvocation, InvocationCaller, InvocationOwner};
use tilo_action_policy::MemberId;
use tilo_actions::remote::{ActionCallError, Caller};

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
                ..
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
                let expected: tilo_action_policy::Actor = current
                    .agent_id
                    .as_ref()
                    .map_or_else(
                        || MemberId::person(super::person_id_for_root(certificate.user_public_key)),
                        |agent| {
                            MemberId::agent(
                                super::delegated_agent_name(agent),
                                super::person_id_for_root(certificate.user_public_key),
                            )
                        },
                    )
                    .into();
                if *actor.actor() == expected {
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
                        actor: actor.actor().clone(),
                    }
                }
            }
            None => InvocationOwner::Local {
                actor: actor.actor().clone(),
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
    ) -> Result<Option<tilo_actions::remote::InvocationState>, tilo_actions::remote::ActionCallError>
    {
        let workspace = caller
            .workspace()
            .ok_or_else(|| ActionCallError::unavailable("caller workspace unavailable"))?;
        let actor = self.caller_actor(caller, workspace);
        let owner = self
            .invocation_owner(Some(origin), workspace, &actor)
            .map_err(ActionCallError::unavailable)?;
        let now = self.policy_now_ns().map_err(ActionCallError::unavailable)? / 1_000_000_000;
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| ActionCallError::unavailable("invocation journal unavailable"))?;
            let Some(journal) = guard
                .as_ref()
                .map_err(|error| ActionCallError::unavailable(error.clone()))?
                .as_ref()
            else {
                return Ok(None);
            };
            let record = self
                .retained_invocation(journal, workspace, &owner, invocation, &actor, now)
                .map_err(ActionCallError::unavailable)?;
            drop(guard);
            record
        };
        Ok(record.map(|record| {
            tilo_actions::remote::InvocationState::new(invocation, record.update)
                .with_untrusted(self.untrusted_job(record.wire_id))
        }))
    }
}
