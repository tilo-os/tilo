//! A record a member creates: empty, where the member may change the record.
use super::{
    AutoCommit, DocumentMutation, MemberAuthority, Route, SignedOp, StatePeer, SystemTime,
};
use tilo_action_policy::ActionKind;

impl StatePeer {
    /// Whether `member` may create `route`'s record empty: they may change
    /// that record.
    pub(super) fn may_create_record(
        &self,
        member: &MemberAuthority,
        route: &Route,
        at: SystemTime,
    ) -> bool {
        member.allows(
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

/// Whether `envelope` starts `app`'s record `record` in `workspace`: the
/// record's genesis, cleartext and holding no change at all.
pub(super) fn empty_genesis_op(
    app: &str,
    record: &str,
    workspace: &str,
    envelope: &SignedOp,
) -> bool {
    collab_types::validate_whole_document_payload_route(app, workspace, record, envelope).is_ok_and(
        |payload| {
            matches!(
                payload,
                collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Snapshot(bytes))
                    if empty_genesis(&bytes)
            )
        },
    )
}
