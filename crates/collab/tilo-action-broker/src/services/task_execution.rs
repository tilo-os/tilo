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
    pub resource: String,
    pub action: String,
    pub permission: tilo_actions::PermissionRequirement,
}

impl NativeTaskActionRequirement {
    /// Apply the existing action capability rule to the live admitted document.
    ///
    /// # Errors
    /// The room is unavailable or the current agent lacks this action capability.
    pub fn check(
        &self,
        peer: &tilo_peer_state::StatePeer,
        room: &str,
        agent: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        use collab_types::caps::{Actor, CapDecision, ResourceRef, decide_capability};
        let doc = peer
            .admitted_room()
            .ok_or("native action policy unavailable")?;
        let (_, _, caps) = tilo_peer_state::room::extract_governed_fields(doc);
        let seconds = now
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|_| "native action clock unavailable")?
            .as_secs();
        let actor = Actor::agent(agent).in_room(room);
        let resource = ResourceRef::Action {
            app: self.app.clone(),
            resource: self.resource.clone(),
            action: self.action.clone(),
        };
        if crate::collab_broker_capnp::permission_verb(self.permission).is_some_and(|verb| {
            decide_capability(&actor, &resource, verb, &caps, Some(seconds)) != CapDecision::Allow
        }) {
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
