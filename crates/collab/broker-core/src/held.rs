//! What the workspace holding a record says of an action on it, for a window open in another
//! workspace ([`runtime_collab::held_elsewhere`]): every host decides it this way.

use runtime_collab::held_elsewhere::{HeldRecord, HeldVerdict};
use tilo_action_policy::Answer;
use tilo_desktop_model::Workspace;

use crate::BrokerActor;

/// What a person reads when the workspace holding a record is not held on this host.
pub const NOT_HELD: &str =
    "This is kept in a workspace this device doesn't have, so it can't be changed here.";
/// What a person reads when the holding workspace's grants refuse the action.
pub const REFUSED: &str = "You aren't allowed to change this.";

impl BrokerActor {
    /// What `workspace`, which holds `record` (`None` where this host does not hold it), says
    /// of this actor running its action at `now`: the actor's ordinary answer there.
    #[must_use]
    pub fn held_verdict(
        &self,
        workspace: Option<&Workspace>,
        record: &HeldRecord,
        now: u64,
    ) -> HeldVerdict {
        let Some(workspace) = workspace else {
            return HeldVerdict::Refused(NOT_HELD.into());
        };
        match self.action_answer_in(
            workspace,
            &record.app,
            (&record.kind, &record.resource, &record.action),
            &record.need(),
            now,
        ) {
            Answer::Allow => HeldVerdict::Allowed,
            Answer::Ask => HeldVerdict::AsksFirst,
            Answer::Deny => HeldVerdict::Refused(REFUSED.into()),
        }
    }
}
