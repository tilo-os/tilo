//! The runs the session's apps ask of its agents, fed every pass the facts a
//! run is decided against: who sits here, and what each workspace admits.

use crate::Shared;

impl Shared {
    /// Feed the runs who sits here, the workspace in front, and every
    /// workspace with the agents it admits and the grants its people gave: an
    /// ask is the person's and is made in the workspace in front, a run is
    /// its workspace's (so moving to another workspace does not stall it),
    /// only that workspace's agents are delivered, and their actions are
    /// admitted as those grants say. The runs keep it, so an unchanged pass
    /// costs a comparison.
    pub(crate) fn feed_agent_runs(&self) {
        let now = tilo_clock::unix_now();
        let (front, workspaces) = self.handle.read(|tree| {
            let front = tree
                .active_workspace()
                .map(|held| held.room().to_string())
                .unwrap_or_default();
            let workspaces = tree
                .workspaces
                .iter()
                .map(|held| {
                    tilo_agent_runs::WorkspaceFacts::new(
                        &held.room().to_string(),
                        held.agent_members_at(now)
                            .into_iter()
                            .map(|(agent, _)| agent)
                            .collect(),
                        Some(held.policy.clone()),
                    )
                })
                .collect();
            (front, workspaces)
        });
        self.agent_runs.here(&self.person, &front, workspaces);
    }
}
