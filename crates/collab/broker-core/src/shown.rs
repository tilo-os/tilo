//! What a host says of an action a window shows ([`runtime_collab::shown_actions`]): every host
//! decides it this way, with the actor's ordinary answer in the workspace holding the record.

use runtime_collab::shown_actions::{ActionVerdict, ShownAction};
use tilo_action_policy::Answer;
use tilo_desktop_model::Workspace;

use crate::BrokerActor;

/// What a person reads when the workspace holding a record is not held on this host.
pub const NOT_HERE: &str =
    "This is kept in a workspace this device doesn't have, so it can't be changed here.";
/// What a person reads when the workspace's grants refuse the action.
pub const REFUSED: &str = "You aren't allowed to change this.";

impl BrokerActor {
    /// What `workspace`, which holds the record `action` acts on (`None` where this host does
    /// not hold it), says of this actor running the action at `now`: the actor's ordinary answer
    /// there.
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
        let node = action.node(&workspace.node());
        match self
            .verdict_in(workspace, &node, action.kind_needed, action.privileged, now)
            .0
        {
            Answer::Allow => ActionVerdict::Allowed,
            Answer::Ask => ActionVerdict::AsksFirst,
            Answer::Deny => ActionVerdict::Refused(REFUSED.into()),
        }
    }
}
