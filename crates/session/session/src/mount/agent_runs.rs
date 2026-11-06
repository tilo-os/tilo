//! The runs the session's apps ask of its agents, fed every pass the facts a
//! run is decided against: who sits here, what each workspace admits, and who
//! keeps the agents its record names.

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
        // The agents the person admitted at the door work in the workspace in
        // front: they take what the person hands them as much as a workspace
        // member does, whether or not the workspace's governance names them.
        let seated = self.seats.readings();
        let (front, workspaces) = self.handle.read(|tree| {
            let front = tree
                .active_workspace()
                .map(|held| held.id().to_string())
                .unwrap_or_default();
            let workspaces: Vec<tilo_agent_runs::WorkspaceFacts> = tree
                .workspaces
                .iter()
                .map(|held| {
                    let mut agents: Vec<String> = held
                        .agent_members_at(now)
                        .into_iter()
                        .map(|(agent, _)| agent)
                        .collect();
                    for seat in &seated {
                        if seat.doing == held.name && !agents.contains(&seat.key) {
                            agents.push(seat.key.clone());
                        }
                    }
                    tilo_agent_runs::WorkspaceFacts::new(
                        &held.id().to_string(),
                        agents,
                        Some(held.policy.clone()),
                    )
                })
                .collect();
            (front, workspaces)
        });
        self.agent_runs
            .here(&self.person, &front, workspaces.clone());
        // The turns the conversations in front of every workspace give this
        // session's agents: read from Chat's record of each, and handed to the
        // broker's run channel.
        let governed: Vec<(String, tilo_action_policy::WorkspacePolicy)> = workspaces
            .iter()
            .filter_map(|facts| Some((facts.workspace.clone(), facts.policy.clone()?)))
            .collect();
        // Who keeps each workspace's agents, read from the workspace's own
        // record of them: an agent's session joins as its keeper's, and is
        // delivered the turns of the conversations it takes part in. The
        // agents kept here are this device's to run: it starts each one's
        // process, which answers the turns the broker delivers it.
        let kept = self.agents.feed(&self.person, &governed);
        if let Some(apps) = &self.launch {
            for agent in &kept {
                apps.start_agent(agent);
            }
        }
        self.conversations.feed(
            &self.person,
            governed
                .into_iter()
                .map(|(workspace, policy)| crate::conversations::WorkspaceFacts {
                    workspace,
                    policy,
                })
                .collect(),
        );
        // The lines people typed in a run's conversation that ended since the last pass are
        // steps of that run: recorded here, where the runs are, and never on the door's thread.
        self.conversation.take_finished();
        // Who a kept notification can be handed to: the agent live in the
        // workspace in front, where there is one.
        let taking = self.agent_runs.taking_agent();
        self.notifications.borrow().set_taking_agent(taking);
    }
}
