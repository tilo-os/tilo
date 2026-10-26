//! A workspace's actions on one node of its published tree: sharing a
//! record or an item of it with the workspace's members, and asking
//! whether the caller may change it.

use super::{ActionAddress, RemoteActionTarget, workspace_provider};

/// Resource prefix of a workspace's actions on one node of its published
/// tree (a record, an item of it): the rest is the node's text form
/// (`room:…/app:…/kind:…/resource:…/item:…`). See [`node_target`].
pub const NODE_RESOURCE_PREFIX: &str = "node:";

/// Share a node with the workspace's members: grant them, on that node
/// only, what argument `access` names (`view` or `edit`), as much as the
/// person sharing holds there, under the share's id (argument `share`).
pub const SHARE_NODE_ACTION: &str = "share";

/// Take back what the share named by argument `share` granted.
pub const UNSHARE_NODE_ACTION: &str = "unshare";

/// Whether the caller may change the node: output `{"undoable": bool}`,
/// as the workspace decides their writes there.
pub const NODE_PERMITS_ACTION: &str = "permits";

/// The target of `action` (one of [`SHARE_NODE_ACTION`],
/// [`UNSHARE_NODE_ACTION`], [`NODE_PERMITS_ACTION`]) on `node`, the text
/// form of a node of workspace `room`'s tree.
#[must_use]
pub fn node_target(room: &str, node: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(room),
        address: ActionAddress::new(format!("{NODE_RESOURCE_PREFIX}{node}"), action),
    }
}
