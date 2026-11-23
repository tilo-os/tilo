//! Agents in the broker: a join on a member's own session, for an agent the workspace's record
//! lists, and the agent sessions agents work in. The agents are the workspace's: any member's
//! admitted device is a runner for one, and the agent acts for that member, capped, so the
//! workspace's grants decide what it does. Checked again on every revalidation, with whether the
//! agent session it works in still goes.

use super::{AdmittedJoinIdentity, AppBrokerState, roles};

impl AppBrokerState {
    /// The agent session `caller` works in, while it goes, by the session its caller names.
    /// `None` for a caller in no agent session.
    pub(super) fn agent_session_of(
        &self,
        caller: Option<&tilo_actions::remote::Caller>,
        workspace: &str,
    ) -> Option<Result<crate::apps::services::GoingSession, String>> {
        let acting = caller?.acting();
        let (session, agent) = acting.agent_session.as_ref().zip(acting.agent_id())?;
        Some(self.turn_session_goes(workspace, agent, session.as_str()))
    }

    /// The session `session` of `agent`, while it answers a turn in `workspace`: the workspace
    /// lists the agent, and the turn goes.
    pub(super) fn turn_session_goes(
        &self,
        workspace: &str,
        agent: &str,
        session: &str,
    ) -> Result<crate::apps::services::GoingSession, String> {
        self.services
            .agents
            .as_deref()
            .ok_or("this host reads no workspace's agents")?
            .lists(workspace, agent)
            .then_some(())
            .ok_or_else(|| format!("the workspace lists no agent {agent}"))?;
        self.services
            .agent_sessions
            .as_deref()
            .ok_or("this host serves no agent sessions")?
            .going(workspace, agent, session)
            .ok_or_else(|| "the turn the agent answers is over".to_owned())
    }

    /// That an agent working for its person on this device asks for `request` under the key
    /// its person certified for it, held to the bound its record keeps, checked as a host checks
    /// one ([`tilo_server_protocol::invocation::verify_agent_invocation`]) against the
    /// revocations the workspace's document records. Everyone else is admitted as they are.
    pub(super) fn signed_by_agent(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::ActionCallError;
        let (Some(person), Some(key)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return Ok(());
        };
        if caller.roster_created_at().is_none() {
            return Ok(());
        }
        let refused = |reason: String| ActionCallError::Refused { reason };
        let workspace = caller.workspace().ok_or(ActionCallError::NotPermitted)?;
        // A sub-agent works under the key of the agent that heads its line.
        let agent = caller
            .approved_agent()
            .ok_or(ActionCallError::NotPermitted)?;
        let revocations = self
            .membership
            .as_ref()
            .and_then(|membership| membership.workspace_revocations(workspace))
            .ok_or_else(|| refused("this device holds no revocations of the workspace".into()))?;
        let cert = tilo_server_protocol::invocation::verify_agent_invocation(
            workspace,
            request,
            (person, agent, key),
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| refused(format!("{}: {error}", error.code())))?;
        // A key its person's device made acts only as far as the record it read bounds it: one
        // whose bound nobody read is refused, never left unbounded.
        if cert.capabilities.agent.is_some() && caller.granted(&[]).is_empty() {
            return Err(refused(
                "the agent's bound was not read, so it may act on nothing".into(),
            ));
        }
        Ok(())
    }
}

/// Admit agent `agent_id`, listed in `workspace`, to join on the session of a
/// member running it. The workspace's record of its agents and the member's own
/// admission are the whole of it: the agent acts for that member, capped
/// ([`roles::agent_policy::ROLE_CAP`]), so the workspace's grants decide what
/// it does. A record that no
/// longer lists it refuses the join and every later revalidation of it.
pub(super) fn admit_listed_agent(
    roster: &dyn crate::apps::services::AgentRoster,
    workspace: &str,
    admitted: &mut AdmittedJoinIdentity,
    agent_id: &str,
) -> Result<(), String> {
    tilo_peer_state::workspace_membership::valid_agent_id(agent_id)?;
    if admitted.verified.capped {
        return Err(format!(
            "the agent {agent_id} runs only on a member's own device, not on one the workspace caps"
        ));
    }
    if !roster.lists(workspace, agent_id) {
        return Err(format!("the workspace keeps no active agent {agent_id}"));
    }
    admitted.role = roles::agent_policy::role(admitted.role);
    admitted.agent_id = Some(agent_id.to_string());
    admitted.session_delegated = true;
    Ok(())
}
