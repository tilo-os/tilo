//! Who may read and write a workspace's files: the one place the workspace's people and grants
//! are asked about them. The mount enforces what this decides for every member, and a member's
//! device is handed the files' bytes only when this lets its person read them.
//!
//! A file change is decided at the node a receiver of the change decides it at
//! ([`fs_protocol::file_action::verb_node`]), and reading at the tree's own records' node, so
//! the mount, the service and every receiver answer the same question.

use collab_types::caps::Access;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath, WorkspacePolicy};

/// The nodes a member's access to `workspace`'s files is decided at: the tree's records, listing
/// them and writing a file.
#[must_use]
pub fn nodes(workspace: &str) -> [TreePath; 3] {
    let file_action = |verb| {
        TreePath::workspace(workspace).joined(&fs_protocol::file_action::verb_node(None, verb))
    };
    [
        collab_types::document_node(
            workspace,
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        ),
        file_action(fs_protocol::tree::LIST),
        file_action(fs_protocol::file_action::verbs::WRITE),
    ]
}

/// What `member` may do to `workspace`'s files under `policy` at `context`'s time: write them,
/// only read them, or nothing.
#[must_use]
pub fn decide(
    policy: &WorkspacePolicy,
    workspace: &str,
    member: &str,
    context: &Context<'_>,
) -> Option<Access> {
    let [records, listing, writing] = nodes(workspace);
    let party = policy.party(member);
    let allows = |node: &TreePath, kind| party.decide(node, kind, context).answer == Answer::Allow;
    if !allows(&records, ActionKind::Read) || !allows(&listing, ActionKind::Read) {
        return None;
    }
    Some(
        if allows(&records, ActionKind::Undoable) && allows(&writing, ActionKind::Undoable) {
            Access::Write
        } else {
            Access::Read
        },
    )
}
