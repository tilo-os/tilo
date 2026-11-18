//! Agents in the broker: a join admitted by the agent certificate a device of the agent's person
//! signed, or as the agent the workspace keeps for that person, checked again on every
//! revalidation, with whether the agent session it works in still goes.

use super::{
    AdmittedJoinIdentity, AgentCert, AppBrokerState, CrdtRoute, VerifiedJoinIdentity,
    WorkspaceTreeHandle, admit_verified_join_at, authorize_join_certificate, cert_capped_in,
    policy_context, roles,
};

/// Hand an admitted person's session to the agent `cert` names, its chain
/// already verified: the agent acts for that person, as the least of the
/// person's role and the agent cap, in its session alone.
pub(super) fn admit_agent_cert(admitted: &mut AdmittedJoinIdentity, cert: AgentCert) {
    admitted.role = roles::agent_policy::role(admitted.role, roles::agent_policy::ROLE_CAP);
    admitted.agent_id = Some(cert.agent_id.clone());
    admitted.session_delegated = true;
    admitted.agent_cert = Some(Box::new(cert));
}

/// Verify `cert`, carried by a join presented with `device`, at `now_ns`:
/// the device certificate signed it for `workspace`, inside both windows.
pub(super) fn verify_agent_cert(
    cert: &AgentCert,
    device: &tilo_user_identity::DeviceCert,
    workspace: collab_types::WorkspaceId,
    now_ns: u64,
) -> Result<(), String> {
    cert.verify(device, workspace, now_ns)
        .map_err(|error| format!("agent certificate refused: {error}"))
}

/// Admit an agent's join: the certifying device's certificate is admitted to
/// the workspace as any device's is (in its device list, not revoked), it
/// signed the agent certificate for this workspace inside both windows, and
/// the join proof is the agent key's. The session is then the person's,
/// handed to the agent ([`admit_agent_cert`]). Whether the workspace lists the
/// agent and its session still goes is the caller's to ask
/// ([`AppBrokerState::agent_session_goes`]).
pub(super) fn verify_agent_join(
    route: &CrdtRoute,
    presentation: &runtime_collab::AgentJoinIdentity,
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    if presentation.workspace_id != route.cluster_id {
        return Err("agent join is scoped to a different workspace".into());
    }
    let device: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (_, revoked, enrolled) =
        authorize_join_certificate(route, &device, trusted_devices, authority)?;
    if revoked {
        return Err("the certifying device has been revoked".into());
    }
    if !enrolled {
        return Err("the certifying device is not enrolled".into());
    }
    let registered = authority.map_or(
        Ok(None),
        crate::apps::certificate_authority::CertificateAuthority::policy_time,
    )?;
    let now_ns = policy_context::sample(registered.clone(), registered.as_ref())?.1;
    let cert = AgentCert::decode(&presentation.certificate)
        .map_err(|error| format!("agent certificate refused: {error}"))?;
    verify_agent_cert(
        &cert,
        &device,
        collab_types::WorkspaceId::parse(&route.cluster_id)
            .ok_or("the agent's workspace is not a workspace id")?,
        now_ns,
    )?;
    let join = runtime_identity::collaboration_join_digest(&route.cluster_id)
        .map_err(|_| "the agent's join cannot be encoded".to_string())?;
    cert.verify_agent_signature(&join, &presentation.proof)
        .map_err(|_| "the agent's join proof is not its key's".to_string())?;
    let verified = VerifiedJoinIdentity::of(
        &device,
        cert_capped_in(authority, &route.cluster_id, &device),
    );
    let membership =
        membership.ok_or_else(|| "agent join has no workspace membership authority".to_string())?;
    let mut admitted = membership.decide_in_workspace(&route.cluster_id, |workspace| {
        admit_verified_join_at(workspace, route, verified, now_ns / 1_000_000_000)
    })??;
    admitted.certificate = Some(device);
    admit_agent_cert(&mut admitted, cert);
    Ok(admitted)
}

/// A certified agent joins only as itself.
pub(super) fn joins_as(admitted: &AdmittedJoinIdentity, kind: &str) -> Result<(), String> {
    if admitted.agent_cert.is_some() && kind != "agent" {
        return Err("a certified agent joins only as itself".into());
    }
    Ok(())
}

impl AppBrokerState {
    /// Admit a certified agent's join under `handle`: the id its certificate
    /// names, in a session that still goes.
    pub(super) fn admit_agent_join(
        &self,
        workspace: &str,
        handle: &str,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let cert = admitted
            .agent_cert
            .as_deref()
            .ok_or("agent certificate missing")?;
        if handle.trim() != cert.agent_id {
            return Err("an agent joins under the id its certificate names".into());
        }
        self.agent_session_goes(workspace, cert)
            .map(drop)
            .map_err(|error| format!("agent join rejected: {error}"))
    }

    /// The agent session `cert` names, while it goes in `workspace`: the
    /// workspace lists its agent, kept by the person whose device signed it,
    /// and the session's connection lasts.
    pub(super) fn agent_session_goes(
        &self,
        workspace: &str,
        cert: &AgentCert,
    ) -> Result<crate::apps::services::GoingSession, String> {
        self.agent_listed(workspace, cert)?;
        self.services
            .agent_sessions
            .as_deref()
            .ok_or("this host serves no agent sessions")?
            .going(workspace, &cert.agent_id, &cert.session)
            .ok_or_else(|| "the agent's session has ended".to_owned())
    }

    /// The agent session `caller` works in, while it goes: a certified agent's connection
    /// (`origin`) by its certificate, a turn's by the session its caller names. `None` for a
    /// caller in no agent session.
    pub(super) fn agent_session_of(
        &self,
        caller: Option<&tilo_actions::remote::Caller>,
        origin: Option<&super::InvocationCaller>,
        workspace: &str,
    ) -> Option<Result<crate::apps::services::GoingSession, String>> {
        if let Some(super::InvocationCaller::Local { admitted, .. }) = origin
            && let Some(cert) = admitted.agent_cert.as_deref()
        {
            return Some(self.agent_session_goes(workspace, cert));
        }
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
            .keeper(workspace, agent)
            .ok_or_else(|| format!("the workspace lists no agent {agent}"))?;
        self.services
            .agent_sessions
            .as_deref()
            .ok_or("this host serves no agent sessions")?
            .going(workspace, agent, session)
            .ok_or_else(|| "the turn the agent answers is over".to_owned())
    }

    /// Whether agents in `workspace` act only in an agent session
    /// ([`crate::apps::services::AgentSessions::confines`]).
    pub fn confines_agents(&self, workspace: &str) -> bool {
        self.services
            .agent_sessions
            .as_deref()
            .is_some_and(|sessions| sessions.confines(workspace))
    }

    /// Whether the workspace's record of its agents lists the agent `cert`
    /// names, kept by the person whose device signed it.
    fn agent_listed(&self, workspace: &str, cert: &AgentCert) -> Result<(), String> {
        let roster = self
            .services
            .agents
            .as_deref()
            .ok_or("this host reads no workspace's agents")?;
        let keeper = roster
            .keeper(workspace, &cert.agent_id)
            .ok_or_else(|| format!("the workspace lists no agent {}", cert.agent_id))?;
        let person = tilo_broker_core::person_id_for_root(cert.user_public_key);
        if keeper != person {
            return Err(format!(
                "the agent {} is {keeper}'s, not this device's person's",
                cert.agent_id
            ));
        }
        Ok(())
    }
}

/// Admit agent `agent_id`, kept in `workspace`, to join on the session of the
/// person who keeps it. The workspace's record of its agents is the whole
/// admission: no membership row and no grant is asked for, and the agent acts
/// for its keeper, capped ([`roles::agent_policy::ROLE_CAP`]), inside the
/// turns its conversations start. A record that no longer names it, or names
/// another keeper, refuses the join and every later revalidation of it.
pub(super) fn admit_kept_agent(
    roster: &dyn crate::apps::services::AgentRoster,
    workspace: &str,
    admitted: &mut AdmittedJoinIdentity,
    agent_id: &str,
) -> Result<(), String> {
    tilo_peer_state::workspace_membership::valid_agent_id(agent_id)?;
    if admitted.verified.capped {
        return Err(format!(
            "the agent {agent_id} runs only on a device of the session that keeps it"
        ));
    }
    let keeper = roster
        .keeper(workspace, agent_id)
        .ok_or_else(|| format!("the workspace keeps no active agent {agent_id}"))?;
    if keeper != admitted.actor_key {
        return Err(format!(
            "the agent {agent_id} is {keeper}'s to launch, not this session's"
        ));
    }
    admitted.role = roles::agent_policy::role(admitted.role, roles::agent_policy::ROLE_CAP);
    admitted.agent_id = Some(agent_id.to_string());
    admitted.session_delegated = true;
    Ok(())
}
