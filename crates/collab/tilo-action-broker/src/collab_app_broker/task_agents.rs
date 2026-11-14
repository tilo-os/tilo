//! Door agents in the broker: a join admitted by the agent certificate a
//! device of the agent's person signed, checked again on every revalidation,
//! with whether its session's run still goes.

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
    admitted.display_handle = Some(cert.agent_id.clone());
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

/// Whether the session `cert` names still goes in `runs`: a going run of
/// that session for its agent, carrying exactly this certificate.
pub(super) fn session_goes(
    runs: &[crate::services::TrackerRun],
    cert: &AgentCert,
) -> Result<(), String> {
    let encoded = cert.encode();
    let Some(run) = runs.iter().find(|run| run.issue == cert.session) else {
        return Err("the agent's session has ended (it holds no run)".into());
    };
    if run.agent != cert.agent_id || run.delegation() != Ok(Some(encoded.as_slice())) {
        return Err("the agent's session has ended (its run is another agent's)".into());
    }
    if !run.active {
        return Err("the agent's session has ended (its run ended)".into());
    }
    Ok(())
}

/// Whether an agent's session is still heard: it goes ([`session_goes`]),
/// or its own agent just finished it (done or failed), so it hears how its
/// last steps ended. A run its person stopped, or a newer run, ends the
/// session at once. A run finished this way admits no action.
///
/// # Errors
/// As [`session_goes`], for a run its agent did not finish.
pub(super) fn session_heard(
    runs: &[crate::services::TrackerRun],
    cert: &AgentCert,
) -> Result<(), String> {
    let finished_by_it = runs.iter().any(|run| {
        run.issue == cert.session
            && run.finished_by_agent
            && run.agent == cert.agent_id
            && run.delegation() == Ok(Some(cert.encode().as_slice()))
    });
    if finished_by_it {
        Ok(())
    } else {
        session_goes(runs, cert)
    }
}

/// Admit an agent's join: the certifying device's certificate is admitted to
/// the workspace as any device's is (in its device list, not revoked), it
/// signed the agent certificate for this workspace inside both windows, and
/// the join proof is the agent key's. The session is then the person's,
/// handed to the agent ([`admit_agent_cert`]). Whether the workspace lists the
/// agent and its session still goes is the caller's to ask.
pub(super) fn verify_agent_join(
    route: &CrdtRoute,
    presentation: &runtime_collab::AgentJoinIdentity,
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::certificate_authority::CertificateAuthority>,
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
        crate::certificate_authority::CertificateAuthority::policy_time,
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
    cert.verify_agent_signature(
        &runtime_identity::collaboration_join_signing_bytes(&route.cluster_id),
        &presentation.proof,
    )
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
    /// names, an agent the workspace lists as kept by the certificate's person,
    /// in a session that still goes.
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
        self.agent_listed(workspace, cert)
            .and_then(|()| self.session_still_goes(workspace, cert))
            .map_err(|error| format!("agent join rejected: {error}"))
    }

    /// Whether the workspace's record of its agents lists the agent `cert`
    /// names, kept by the person whose device signed it.
    pub(super) fn agent_listed(&self, workspace: &str, cert: &AgentCert) -> Result<(), String> {
        let roster = self
            .services
            .agents
            .as_deref()
            .ok_or("this host reads no workspace's agents")?;
        let keeper = roster
            .keeper(workspace, &cert.agent_id)
            .ok_or_else(|| format!("the workspace lists no agent {}", cert.agent_id))?;
        let person = tilo_broker_core::person_id_for_root(cert.user_public_key);
        if !tilo_action_policy::same_person(&keeper, &person) {
            return Err(format!(
                "the agent {} is {keeper}'s, not this device's person's",
                cert.agent_id
            ));
        }
        Ok(())
    }

    /// Whether the session of the agent `cert` names is still heard in
    /// `workspace` ([`session_heard`]).
    pub(super) fn session_still_heard(
        &self,
        workspace: &str,
        cert: &AgentCert,
    ) -> Result<(), String> {
        let runs = self
            .tracker_runs(workspace)
            .ok_or("the workspace's tracker is unavailable")?
            .runs(workspace)?;
        session_heard(&runs, cert)
    }

    /// Whether the session `cert` names still goes in `workspace`'s tracker
    /// ([`session_goes`]).
    pub(super) fn session_still_goes(
        &self,
        workspace: &str,
        cert: &AgentCert,
    ) -> Result<(), String> {
        let runs = self
            .tracker_runs(workspace)
            .ok_or("the workspace's tracker is unavailable")?
            .runs(workspace)?;
        session_goes(&runs, cert)
    }
}

/// Admit agent `agent_id`, kept in `workspace`, to join on the session of the
/// person who keeps it. The workspace's record of its agents is the whole
/// admission: no membership row and no grant is asked for, and the agent acts
/// for its keeper, capped ([`roles::agent_policy::ROLE_CAP`]), inside the
/// turns its conversations start. A record that no longer names it, or names
/// another keeper, refuses the join and every later revalidation of it.
pub(super) fn admit_kept_agent(
    roster: &dyn crate::services::AgentRoster,
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
    if !tilo_action_policy::same_person(&keeper, &admitted.principal_id) {
        return Err(format!(
            "the agent {agent_id} is {keeper}'s to launch, not this session's"
        ));
    }
    admitted.role = roles::agent_policy::role(admitted.role, roles::agent_policy::ROLE_CAP);
    admitted.agent_id = Some(agent_id.to_string());
    admitted.display_handle = Some(agent_id.to_string());
    admitted.session_delegated = true;
    Ok(())
}
