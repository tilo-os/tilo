//! Host-owned adaptation of authenticated task channels to the canonical owner.
//! No caller certificate, actor label or new grant is accepted from task arguments.
use crate::collab_broker_capnp::InvocationJob;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskSnapshot};

#[cfg(test)]
#[path = "task_execution_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub struct NativeTaskBinding {
    pub task_id: String,
    pub agent_id: String,
    pub epoch: u64,
    pub revision: u64,
}

/// A current published contract requirement, never a grant from task arguments.
#[derive(Clone, Debug)]
pub struct NativeTaskActionRequirement {
    /// Supplied by the broker's authenticated run channel, never action arguments.
    pub binding: Option<NativeTaskBinding>,
    pub app: String,
    pub resource_kind: String,
    pub resource: String,
    pub action: String,
    pub need: tilo_actions::ActionNeed,
}

impl NativeTaskActionRequirement {
    /// Apply the room's grants for the agent to this action, on the live
    /// admitted document, held to the exact task/run's frozen authority. An unattended run has nobody to ask, so only an action the
    /// agent and its task are allowed passes.
    ///
    /// # Errors
    /// The room is unavailable or the current agent is not allowed this action.
    pub fn check(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        use tilo_action_policy::{Answer, Authority, Context, Subject};
        let doc = peer
            .admitted_room()
            .ok_or("native action policy unavailable")?;
        let policy = tilo_desktop_model::projection::room_policy_of(doc, room);
        let node = tilo_actions::action_node(
            &policy.node(),
            &self.app,
            &self.resource_kind,
            &self.resource,
            self.need.field.as_deref(),
            &self.action,
        );
        let cx = Context::at_time(now);
        let verdict = Authority::person(policy.party_of(Subject::agent(agent))).decide(
            &node,
            self.need.kind,
            &cx,
        );
        let verdict = if self.need.privileged {
            verdict.privileged()
        } else {
            verdict
        };
        let binding = self
            .binding
            .as_ref()
            .ok_or("native task action binding absent")?;
        if binding.agent_id != agent || self.need.privileged {
            return Err("native task action binding invalid or action privileged".into());
        }
        let state = tilo_peer_state::workspace_task::snapshot(doc)?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.id == binding.task_id)
            .ok_or("native task absent")?;
        let control = state
            .controls
            .get(&binding.task_id)
            .ok_or("native task control absent")?;
        let run = control.active_run.as_ref().ok_or("native task run ended")?;
        if run.epoch != binding.epoch
            || run.agent_id != agent
            || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
            || task.assigned_agent.as_deref() != Some(agent)
        {
            return Err("native task run replaced or stopped".into());
        }
        let approval = run.approval.as_ref().ok_or("native task starter absent")?;
        let member = peer.permissions(approval.device, now)?;
        if member.person != approval.person || !member.allows(&node, self.need.kind, now) {
            return Err("native task action is no longer authorized by its starter".into());
        }
        let bound = control
            .execution_authority
            .as_ref()
            .ok_or("native task authority absent")?;
        if bound.epoch != binding.epoch || !bound.allows(&node, self.need.kind, now) {
            return Err("native task action exceeds its frozen execution authority".into());
        }
        if verdict.answer != Answer::Allow {
            let basis = match verdict.parts.first().map(|part| part.basis) {
                Some(tilo_action_policy::Basis::Grant(grant)) => {
                    format!("grant {} on {}", grant.id, grant.node)
                }
                _ => format!("no grant covers {}", verdict.node),
            };
            return Err(unattended_refusal(
                verdict.answer,
                self.need.kind,
                &format!("the agent's grants in this workspace ({basis})"),
            ));
        }
        if let Some(answer) = policy
            .task_answer(&binding.task_id, &node, self.need.kind, &cx)
            .filter(|answer| *answer != Answer::Allow)
        {
            return Err(unattended_refusal(
                answer,
                self.need.kind,
                "this task's own grants",
            ));
        }
        Ok(())
    }
}

/// Why an unattended task run may not take an action `whose` grants answer
/// `answer` (anything but Allow) for: one that asks first has nobody to ask,
/// so it is refused as surely as one they deny, but the agent (and whoever
/// reads its report) should learn which it was rather than retry it.
fn unattended_refusal(
    answer: tilo_action_policy::Answer,
    kind: tilo_action_policy::ActionKind,
    whose: &str,
) -> String {
    if answer == tilo_action_policy::Answer::Ask {
        format!(
            "native task action asks a person first: {whose} answer \"ask\" for {}, and an \
             unattended task run has nobody to ask",
            kind.phrase()
        )
    } else {
        format!(
            "native task action is not allowed: {whose} do not allow {}",
            kind.phrase()
        )
    }
}

pub trait WorkspaceTaskExecution: Send + Sync {
    fn requires_room(&self, room: &str) -> bool;
    /// Read the canonical task controls in this native workspace.
    ///
    /// # Errors
    /// The owner is unavailable or this service does not govern the room.
    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// Read instructions only while the assigned agent remains a current member.
    ///
    /// # Errors
    /// The owner is unavailable or current agent admission ended.
    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// The broker supplies a binding derived from its authenticated channel.
    /// Authority uses the host's existing enrolled attestation, never a wire grant.
    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> tilo_owner_commit::CommitFuture;
    /// Check the current owner capability policy before observing retained action output.
    ///
    /// # Errors
    /// The room or actor is unavailable, or its action capability was revoked.
    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
    /// Admit a host activation in the same serialized task turn as an action.
    /// The callback must retain custody before invoking the ordinary launcher.
    ///
    /// # Errors
    /// Membership, starter authority, frozen scope or instruction frontier changed.
    fn admit_host_launch(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;

    /// Runs ordinary action admission inside the existing serialized task turn.
    ///
    /// # Errors
    /// Current membership, approval or instruction frontier changed, or admission fails.
    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<InvocationJob, String>,
    ) -> Result<InvocationJob, String>;
}
