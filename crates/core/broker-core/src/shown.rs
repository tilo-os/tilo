//! What a host says of an action a window shows ([`tilo_actions::shown`]): every host decides
//! it this way, with the actor's ordinary answer in the workspace that keeps the node.

use tilo_action_policy::Answer;
use tilo_actions::shown::{ActionVerdict, NOT_HERE, REFUSED, ShownAction};
use tilo_desktop_model::Workspace;

use crate::BrokerActor;

impl BrokerActor {
    /// What `workspace`, which keeps the node `action` asks about (`None` where this host does
    /// not keep it), says of this actor taking the action at `now`: the actor's ordinary answer
    /// there. One that relies on a grant given once asks first: the window sends it through this
    /// host's dispatch, which spends the grant as it admits the call, so the window never runs
    /// it on its own.
    #[must_use]
    pub fn shown_verdict(
        &self,
        workspace: Option<&Workspace>,
        action: &ShownAction,
        now: u64,
    ) -> ActionVerdict {
        let Some(workspace) = workspace else {
            return ActionVerdict::Refused(NOT_HERE.into());
        };
        // Whether the app reaches outside is the workspace's say over the app.
        if let Some(verdict) = tilo_actions::shown::reach_verdict(
            action,
            &workspace.policy.workspace,
            &workspace.policy.grants,
            &tilo_action_policy::Context::at_secs(now),
        ) {
            return verdict;
        }
        let node = workspace.policy.node().join(&action.node);
        match self.verdict_in(workspace, &node, action.kind_needed, action.privileged, now) {
            (Answer::Allow, spent) if spent.is_empty() => ActionVerdict::Allowed,
            (Answer::Allow | Answer::Ask, _) => ActionVerdict::AsksFirst,
            (Answer::Deny, _) => ActionVerdict::Refused(REFUSED.into()),
        }
    }

    /// What the device answers of `action` besides itself, shown in a window of `workspace`: the
    /// action as declared, and at each node below that a grant of this actor's names, where the
    /// answer may differ. A record the window had not asked about takes the nearest of them.
    #[must_use]
    pub fn shown_around(
        &self,
        workspace: Option<&Workspace>,
        action: &ShownAction,
    ) -> Vec<ShownAction> {
        let Some(workspace) = workspace else {
            return Vec::new();
        };
        let mut around = vec![action.declared()];
        self.with_authority_in(workspace, |authority| {
            around.extend(
                authority
                    .nodes()
                    .into_iter()
                    .filter_map(|node| action.at_node(&node.within_workspace())),
            );
        });
        around.sort();
        around.dedup();
        around.retain(|around| around != action);
        around
    }
}
