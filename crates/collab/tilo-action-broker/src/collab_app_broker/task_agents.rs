//! Task agents in the broker: a join admitted by the task delegation the
//! asking device signed, and the same chain checked again on every
//! revalidation, with whether the task still goes.

use super::{
    AdmittedJoinIdentity, AppBrokerState, CrdtRoute, TaskDelegation, VerifiedJoinIdentity,
    WorkspaceTreeHandle, admit_verified_join_at, authorize_join_certificate, cert_capped_in,
    policy_context, roles,
};

/// Hand an admitted person's session to the task agent `task` names, its
/// chain already verified: the agent acts for that person, as the least of
/// the person's role and the agent cap, on its task alone. No membership row
/// or grant is asked for: the device's signature is the agent's
/// certification.
pub(super) fn admit_task_delegation(admitted: &mut AdmittedJoinIdentity, task: TaskDelegation) {
    admitted.role = roles::agent_policy::role(admitted.role, roles::agent_policy::ROLE_CAP);
    admitted.agent_id = Some(task.agent_id.clone());
    admitted.display_handle = Some(task.agent_id.clone());
    admitted.session_delegated = true;
    admitted.task = Some(Box::new(task));
}

/// Verify `task`, carried by a join presented with `cert`, at `now_ns`: the
/// device certificate signed it for `workspace`, inside both windows.
pub(super) fn verify_task_delegation(
    task: &TaskDelegation,
    cert: &tilo_user_identity::DeviceCert,
    workspace: collab_types::WorkspaceId,
    now_ns: u64,
) -> Result<(), String> {
    task.verify(cert, workspace, now_ns)
        .map_err(|error| format!("task delegation refused: {error}"))
}

/// Whether the task `task` was delegated for still goes in `runs`: a going
/// run of its issue for its agent, carrying exactly this delegation.
/// Finishing or cancelling the task ends the delegation here.
pub(super) fn task_still_goes(
    runs: &[crate::services::TrackerRun],
    task: &TaskDelegation,
) -> Result<(), String> {
    let encoded = task.encode();
    let Some(run) = runs.iter().find(|run| run.issue == task.work_group) else {
        return Err("the task this agent was made for has ended (its issue holds no run)".into());
    };
    if run.agent != task.agent_id || run.delegation() != Ok(Some(encoded.as_slice())) {
        return Err(
            "the task this agent was made for has ended (its issue's run is another agent's)"
                .into(),
        );
    }
    if !run.active {
        return Err("the task this agent was made for has ended (its run ended)".into());
    }
    Ok(())
}

/// Whether a task agent's session is still heard: its task goes
/// ([`task_still_goes`]), or its own agent just finished it (done or failed),
/// so it hears how its last steps ended and tells its host. A run its person
/// stopped, or a newer run of the issue, ends the session at once. A run
/// finished this way admits no action: each is admitted against a going
/// run on its own.
///
/// # Errors
/// As [`task_still_goes`], for a run its agent did not finish.
pub(super) fn task_still_heard(
    runs: &[crate::services::TrackerRun],
    task: &TaskDelegation,
) -> Result<(), String> {
    let finished_by_it = runs.iter().any(|run| {
        run.issue == task.work_group
            && run.finished_by_agent
            && run.agent == task.agent_id
            && run.delegation() == Ok(Some(task.encode().as_slice()))
    });
    if finished_by_it {
        Ok(())
    } else {
        task_still_goes(runs, task)
    }
}

/// Admit a task agent's join: the asking device's certificate is admitted
/// to the workspace as any device's is (in its device list, not revoked), it
/// signed the task delegation for this workspace inside both windows, and the
/// join proof is the task key's. The session is then the person's, handed
/// to the task agent ([`admit_task_delegation`]). Whether its task still
/// goes is the caller's to ask, with the workspace's tracker.
pub(super) fn verify_task_agent_join(
    route: &CrdtRoute,
    presentation: &runtime_collab::TaskAgentJoinIdentity,
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::certificate_authority::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    if presentation.workspace_id != route.cluster_id {
        return Err("task agent join is scoped to a different workspace".into());
    }
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (_, revoked, enrolled) =
        authorize_join_certificate(route, &cert, trusted_devices, authority, None)?;
    if revoked {
        return Err("the delegating device has been revoked".into());
    }
    if !enrolled {
        return Err("the delegating device is not enrolled".into());
    }
    let registered = authority.map_or(
        Ok(None),
        crate::certificate_authority::CertificateAuthority::policy_time,
    )?;
    let now_ns = policy_context::sample(registered.clone(), registered.as_ref())?.1;
    let task = TaskDelegation::decode(&presentation.delegation)
        .map_err(|error| format!("task delegation refused: {error}"))?;
    verify_task_delegation(
        &task,
        &cert,
        collab_types::WorkspaceId::parse(&route.cluster_id)
            .ok_or("the task's workspace is not a workspace id")?,
        now_ns,
    )?;
    task.verify_agent_signature(
        &runtime_identity::collaboration_join_signing_bytes(&route.cluster_id),
        &presentation.proof,
    )
    .map_err(|_| "the task agent's join proof is not its key's".to_string())?;
    let verified =
        VerifiedJoinIdentity::of(&cert, cert_capped_in(authority, &route.cluster_id, &cert));
    let membership = membership
        .ok_or_else(|| "task agent join has no workspace membership authority".to_string())?;
    let mut admitted = membership.decide_in_workspace(&route.cluster_id, |workspace| {
        admit_verified_join_at(workspace, route, verified, now_ns / 1_000_000_000)
    })??;
    admitted.certificate = Some(cert);
    admit_task_delegation(&mut admitted, task);
    Ok(admitted)
}

/// A task agent joins only as itself, an agent.
pub(super) fn joins_as(admitted: &AdmittedJoinIdentity, kind: &str) -> Result<(), String> {
    if admitted.task.is_some() && kind != "agent" {
        return Err("a task agent joins only as itself".into());
    }
    Ok(())
}

/// A task agent is no member: its verified task delegation is its
/// certification, and it is in the roster it sees as itself.
pub(super) fn self_row(admitted: &AdmittedJoinIdentity) -> Option<ui::collaboration::Member> {
    admitted.task.as_ref().map(|task| {
        collab_types::Member::of_software(
            collab_types::MemberId::agent(task.agent_id.clone()),
            task.agent_id.clone(),
            None,
        )
        .claiming_self()
    })
}

impl AppBrokerState {
    /// Admit a task agent's join under `handle`: the id its delegation names,
    /// for a task that still goes.
    pub(super) fn admit_task_join(
        &self,
        workspace: &str,
        handle: &str,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let task = admitted.task.as_deref().ok_or("task delegation missing")?;
        if handle.trim() != task.agent_id {
            return Err("a task agent joins under the id its delegation names".into());
        }
        self.task_goes(workspace, task)
            .map_err(|error| format!("task agent join rejected: {error}"))
    }

    /// Whether the session of the task agent `task` was delegated for is
    /// still heard in `workspace` ([`task_still_heard`]).
    pub(super) fn task_heard(&self, workspace: &str, task: &TaskDelegation) -> Result<(), String> {
        let runs = self
            .tracker_runs(workspace)
            .ok_or("the workspace's tracker is unavailable")?
            .runs(workspace)?;
        task_still_heard(&runs, task)
    }

    /// Whether the task `task` was delegated for still goes in `workspace`'s
    /// tracker ([`task_still_goes`]).
    pub(super) fn task_goes(&self, workspace: &str, task: &TaskDelegation) -> Result<(), String> {
        let runs = self
            .tracker_runs(workspace)
            .ok_or("the workspace's tracker is unavailable")?
            .runs(workspace)?;
        task_still_goes(&runs, task)
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
