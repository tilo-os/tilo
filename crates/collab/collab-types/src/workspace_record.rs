//! Records of a workspace's room that every app of the workspace opens
//! beside its own: one per kind and room, kept by one app (its owner), read
//! and changed from any app's window of the room.
//!
//! An app's launch otherwise reaches only its own routes. These records are
//! the exception, named here one by one: every app of the room may join
//! and write the route `(owner, room, workspace_record_id(kind))` of its
//! launch room, and nothing else of the owner's. The room's grants on the
//! owner's resource decide every change, wherever it is made, as they
//! decide the owner's own.

/// A record every app of a workspace opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceRecord {
    /// The app that keeps it: whose surface and grants decide its changes.
    pub app: &'static str,
    /// Its resource kind.
    pub kind: &'static str,
}

/// Every record every app of a workspace opens.
pub const WORKSPACE_RECORDS: &[WorkspaceRecord] = &[
    // The workspace's own emoji, which any view draws for `:name:`
    // (`tilo-workspace-emoji`).
    WorkspaceRecord {
        app: "so.tilo.chat",
        kind: "tilo.workspace.emoji",
    },
];

/// The id of the workspace record of `kind` in any room: one per kind, the
/// same on every device, derived as the person's record of a kind is.
#[must_use]
pub fn workspace_record_id(kind: &str) -> String {
    crate::person_record_id(kind)
}

/// The workspace record `app` keeps under `record`, when it is one.
#[must_use]
pub fn workspace_record(app: &str, record: &str) -> Option<&'static WorkspaceRecord> {
    let kind = crate::person_record_kind(record)?;
    WORKSPACE_RECORDS
        .iter()
        .find(|known| known.app == app && known.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_listed_record_of_its_owner_is_a_workspace_record() {
        let emoji = workspace_record_id("tilo.workspace.emoji");
        assert_eq!(
            workspace_record("so.tilo.chat", &emoji).map(|record| record.kind),
            Some("tilo.workspace.emoji")
        );
        assert!(
            workspace_record("so.tilo.notes", &emoji).is_none(),
            "another app's record of the kind"
        );
        assert!(workspace_record("so.tilo.chat", &workspace_record_id("chat.workspace")).is_none());
        assert!(workspace_record("so.tilo.chat", "tab:0000000000000000").is_none());
    }
}
