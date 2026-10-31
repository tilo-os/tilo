//! The runs the session's apps ask of its agents, fed every pass the facts a
//! run is decided against: who sits here, and what each workspace's room
//! admits.

use crate::Shared;

impl Shared {
    /// Feed the runs who sits here, the room in front, and every workspace's
    /// room with the agents it admits and the grants its people gave: an ask
    /// is the person's and is made in the room in front, a run is its room's
    /// (so moving to another workspace does not stall it), only that room's
    /// agents are delivered, and their actions are admitted as those grants
    /// say. The runs keep it, so an unchanged pass costs a comparison.
    pub(crate) fn feed_agent_runs(&self) {
        let now = tilo_clock::unix_now();
        let (front, rooms) = self.handle.read(|tree| {
            let front = tree
                .active_workspace()
                .map(|workspace| workspace.room().to_string())
                .unwrap_or_default();
            let rooms = tree
                .workspaces
                .iter()
                .map(|workspace| {
                    tilo_agent_runs::RoomFacts::new(
                        &workspace.room().to_string(),
                        workspace
                            .agent_members_at(now)
                            .into_iter()
                            .map(|(agent, _)| agent)
                            .collect(),
                        Some(workspace.policy.clone()),
                    )
                })
                .collect();
            (front, rooms)
        });
        self.agent_runs.here(&self.person, &front, rooms);
    }
}
