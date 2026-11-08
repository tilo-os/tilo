//! The agents the session's workspaces keep, fed every pass the turns their conversations give them
//! and what each workspace admits.

use crate::Shared;

impl Shared {
    /// Feed the agents every pass what their work is decided against: every workspace with the
    /// grants its people gave, and the turns the conversations of each give its agents. Who keeps
    /// each workspace's agents is read from the workspace's own record of them, as the broker
    /// admits an agent's session on: an agent works in the turns of the conversations it takes part
    /// in, and its actions are admitted as its keeper's grants say. The agents kept here are this
    /// device's to run: it starts each one's process, which answers the turns the broker delivers
    /// it.
    pub(crate) fn feed_agents(&self) {
        let workspaces: Vec<(String, tilo_action_policy::WorkspacePolicy)> =
            self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .map(|held| (held.id().to_string(), held.policy.clone()))
                    .collect()
            });
        self.agents
            .charge(&workspaces, &self.person, &self.conversations.take_spent());
        let kept = self.agents.feed(&self.person, &workspaces);
        if let Some(apps) = &self.launch {
            for agent in &kept {
                apps.start_agent(agent);
            }
        }
        self.conversations.feed(
            &self.person,
            workspaces
                .into_iter()
                .map(|(workspace, policy)| crate::conversations::WorkspaceFacts {
                    workspace,
                    policy,
                })
                .collect(),
        );
    }
}
