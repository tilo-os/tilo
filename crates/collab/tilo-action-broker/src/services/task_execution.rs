//! Host-owned adaptation of authenticated task channels to the canonical owner.
//! No caller certificate, actor label or new grant is accepted from task arguments.
use crate::collab_broker_capnp::InvocationJob;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskSnapshot};

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
    pub app: String,
    pub resource_kind: String,
    pub resource: String,
    pub action: String,
    pub need: tilo_actions::ActionNeed,
}

impl NativeTaskActionRequirement {
    /// Apply the room's grants for the agent to this action, on the live
    /// admitted document. An unattended run has nobody to ask, so only an
    /// action the agent is allowed passes.
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
        let verdict = Authority::person(policy.party_of(Subject::agent(agent))).decide(
            &node,
            self.need.kind,
            &Context::at_time(now),
        );
        let verdict = if self.need.privileged {
            verdict.privileged()
        } else {
            verdict
        };
        if verdict.answer != Answer::Allow {
            return Err("native task action is no longer authorized by its owner".into());
        }
        Ok(())
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
    ) -> tilo_peer_state::owner_commit::CommitFuture;
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
