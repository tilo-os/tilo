//! Task agents in the broker: a join admitted by the task delegation the
//! asking device signed, and the same chain checked again on every
//! revalidation, with whether the task still goes.

use super::{
    AdmittedJoinIdentity, CapnpBrokerState, CrdtRoute, TaskDelegation, VerifiedJoinIdentity,
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
/// device certificate signed it for `room`, inside both windows.
pub(super) fn verify_task_delegation(
    task: &TaskDelegation,
    cert: &tilo_user_identity::DeviceCert,
    room: &str,
    now_ns: u64,
) -> Result<(), String> {
    task.verify(cert, room, now_ns)
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

/// Admit a task agent's join: the asking device's certificate is admitted
/// to the room as any device's is (in its device list, not revoked), it
/// signed the task delegation for this room inside both windows, and the
/// join proof is the task key's. The session is then the person's, handed
/// to the task agent ([`admit_task_delegation`]). Whether its task still
/// goes is the caller's to ask, with the room's tracker.
pub(super) fn verify_task_agent_join(
    route: &CrdtRoute,
    presentation: &runtime_collab::TaskAgentJoinIdentity,
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::peer_actions::CertificateAuthority>,
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
        crate::peer_actions::CertificateAuthority::policy_time,
    )?;
    let now_ns = policy_context::sample(registered.clone(), registered.as_ref())?.1;
    let task = TaskDelegation::decode(&presentation.delegation)
        .map_err(|error| format!("task delegation refused: {error}"))?;
    verify_task_delegation(&task, &cert, &route.cluster_id, now_ns)?;
    task.verify_agent_signature(
        &runtime_identity::collaboration_join_signing_bytes(&route.cluster_id),
        &presentation.proof,
    )
    .map_err(|_| "the task agent's join proof is not its key's".to_string())?;
    let verified =
        VerifiedJoinIdentity::of(&cert, cert_capped_in(authority, &route.cluster_id, &cert));
    let membership = membership
        .ok_or_else(|| "task agent join has no workspace membership authority".to_string())?;
    let mut admitted = membership.decide_in_room(&route.cluster_id, |workspace| {
        admit_verified_join_at(workspace, route, verified, now_ns / 1_000_000_000)
    })??;
    admitted.certificate = Some(cert);
    admit_task_delegation(&mut admitted, task);
    Ok(admitted)
}

/// A task agent joins only as itself, an agent.
///
/// # Errors
/// A task agent's session joins as anything else.
pub(super) fn joins_as(admitted: &AdmittedJoinIdentity, kind: &str) -> Result<(), String> {
    if admitted.task.is_some() && kind != "agent" {
        return Err("a task agent joins only as itself".into());
    }
    Ok(())
}

/// A task agent is no member: its verified task delegation is its
/// certification, and it is in the roster it sees as itself.
pub(super) fn self_row(admitted: &AdmittedJoinIdentity) -> Option<ui::collaboration::Collaborator> {
    admitted
        .task
        .as_ref()
        .map(|task| ui::collaboration::Collaborator {
            id: format!("agent:{}", task.agent_id),
            name: task.agent_id.clone(),
            colour: None,
            is_agent: true,
            is_self: true,
            joined_at: None,
        })
}

impl CapnpBrokerState {
    /// Admit a task agent's join under `handle`: the id its delegation names,
    /// for a task that still goes.
    ///
    /// # Errors
    /// Another id, or the task ended or cannot be read.
    pub(super) fn admit_task_join(
        &self,
        room: &str,
        handle: &str,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let task = admitted.task.as_deref().ok_or("task delegation missing")?;
        if handle.trim() != task.agent_id {
            return Err("a task agent joins under the id its delegation names".into());
        }
        self.task_goes(room, task)
            .map_err(|error| format!("task agent join rejected: {error}"))
    }

    /// Whether the task `task` was delegated for still goes in `room`'s
    /// tracker ([`task_still_goes`]).
    ///
    /// # Errors
    /// The tracker cannot be read, or the task ended.
    pub(super) fn task_goes(&self, room: &str, task: &TaskDelegation) -> Result<(), String> {
        let runs = self
            .tracker_runs(room)
            .ok_or("the room's tracker is unavailable")?
            .runs(room)?;
        task_still_goes(&runs, task)
    }
}
