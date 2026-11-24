//! The toolkit's part of what the store keeps with each open resource: the action open on it, the
//! undo status each window was answered, and the invocation evidence its writes carry.

use std::collections::HashMap;

use automerge::AutoCommit;

use super::PendingResourceAction;
use crate::permissions::MemberId;

/// The toolkit's part of a resource's action state ([`crate::resource::ResourceActions::toolkit`]): the
/// action open on the resource, the undo status each window was answered, and the invocation
/// evidence its writes carry.
#[derive(Default)]
pub(crate) struct ResourceActionState {
    pub(super) pending: Option<PendingResourceAction>,
    pub(super) statuses:
        HashMap<(MemberId, crate::application::InstanceId), crate::undo::UndoStatus>,
    /// The status last answered per author's window, with where the document, the history and the
    /// pending action stood: a frame asks every time, and it changes only when one of them moves.
    pub(super) answered: HashMap<
        (MemberId, crate::application::InstanceId),
        (
            crate::state::HistoryMoment,
            bool,
            u64,
            crate::undo::UndoStatus,
        ),
    >,
}
impl ResourceActionState {
    #[cfg(test)]
    pub(crate) fn statuses_for_test(
        &self,
    ) -> Vec<(
        tilo_actions::MemberId,
        crate::application::InstanceId,
        crate::undo::UndoStatus,
    )> {
        let mut statuses = self
            .statuses
            .iter()
            .map(|((author, instance), status)| (author.clone(), *instance, status.clone()))
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        statuses
    }
}
/// The toolkit's part of a resource's action state, reached from the whole.
pub(crate) trait ToolkitPart {
    fn part(&mut self) -> &mut ResourceActionState;
    /// The action the toolkit has open on the resource, if it made its part.
    fn pending(&self) -> Option<&PendingResourceAction>;
}

impl ToolkitPart for crate::resource::ResourceActions {
    fn part(&mut self) -> &mut ResourceActionState {
        self.toolkit()
    }

    fn pending(&self) -> Option<&PendingResourceAction> {
        self.toolkit_ref::<ResourceActionState>()
            .and_then(|part| part.pending.as_ref())
    }
}

impl crate::resource::ToolkitActions for ResourceActionState {
    /// Persistence may close a storage transaction while retaining the logical undo burst; preserve
    /// its original provenance before serialization does so.
    fn commit_for_save(&self, doc: &mut AutoCommit) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        tilo_resource::history::commit_stamped_with_message(
            doc,
            pending.context.clone().in_step().change_message(),
        );
        true
    }

    fn finish_external(
        &mut self,
        doc: &mut AutoCommit,
        history: &mut collaboration::history::UndoLog,
    ) {
        if let Some(pending) = self.pending.take() {
            tilo_resource::history::commit_stamped_with_message(
                doc,
                pending.context.clone().in_step().change_message(),
            );
            let after = doc.get_heads();
            history.record_committed_span(
                doc,
                pending.before,
                after,
                None,
                pending.metadata,
                Some(pending.author),
                Some(pending.instance_id),
            );
        }
        self.statuses.clear();
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
