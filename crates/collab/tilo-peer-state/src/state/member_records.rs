//! A record a member creates: empty, where the room lets its app author
//! records and the member change the record.
use super::{AutoCommit, MemberAuthority, Route, StatePeer, SystemTime};
use tilo_action_policy::ActionKind;

impl StatePeer {
    /// Check that `author`, not one of the owner's devices, may sign
    /// `bytes` as `route`'s first snapshot: a member creates a record empty,
    /// and nothing more, so there is nothing in it for a checkpoint to cover.
    pub(super) fn check_member_genesis(
        &self,
        route: &Route,
        bytes: &[u8],
        author: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        if self.core.documents.contains_key(route) || !empty_genesis(bytes) {
            return Err("only the owner's device may publish a retained checkpoint".into());
        }
        let member = self.membership(author, now)?;
        if !self.may_create_record(&member, route, now) {
            return Err("the room does not let this member create the record".into());
        }
        Ok(())
    }

    /// Whether `member` may create `route`'s record empty: the room lets
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
                &collab_types::document_node(&self.core.room, &route.app, &route.record),
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
