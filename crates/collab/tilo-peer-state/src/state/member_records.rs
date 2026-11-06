//! A record a member creates: empty, where the workspace lets its app author
//! records and the member change the record.
use super::{AutoCommit, MemberAuthority, Route, StatePeer, SystemTime};
use tilo_action_policy::ActionKind;

impl StatePeer {
    /// Whether `member` may create `route`'s record empty: the workspace lets
    /// its app author records and lets them change that record.
    pub(super) fn may_create_record(
        &self,
        member: &MemberAuthority,
        route: &Route,
        at: SystemTime,
    ) -> bool {
        member
            .policy
            .app_authors(&route.app, &tilo_action_policy::Context::at_time(at))
            && member.allows(
                &collab_types::document_node(&self.core.workspace, &route.app, &route.record),
                ActionKind::Undoable,
                at,
            )
    }
}

/// Whether `bytes`, a record's snapshot, is the empty record: a document
/// holding no change at all.
pub(super) fn empty_genesis(bytes: &[u8]) -> bool {
    AutoCommit::load(bytes).is_ok_and(|mut doc| doc.get_heads().is_empty())
}
