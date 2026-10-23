//! Whether an agent may still act in the room: what a run of it checks
//! before each action.

use super::SystemTime;
use super::facades::PeerWorkspace;

impl PeerWorkspace<'_> {
    /// Whether `agent_id` is still admitted to the room. A check of existing
    /// capabilities, never a grant.
    /// # Errors
    /// The agent was removed or its admission lapsed, or the room's document
    /// is unavailable.
    pub fn check_admitted_agent(self, agent_id: &str, now: SystemTime) -> Result<(), String> {
        let doc = self
            .peer
            .core
            .room_doc
            .as_deref()
            .ok_or("admitted room document unavailable")?;
        let policy = tilo_desktop_model::projection::room_policy_of(doc, &self.peer.core.room);
        if !super::agent_is_member(&policy, agent_id, now) {
            return Err("the agent is no longer admitted".into());
        }
        Ok(())
    }
}
