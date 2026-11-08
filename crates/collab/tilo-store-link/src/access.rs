//! Who a store admits, answered from the engine's membership.
//!
//! A store device is an ordinary member of the workspaces it keeps: it holds
//! their governance in its own engine (its roster, its grants, its removals)
//! and none of their content keys, so the same membership every member acts
//! on is the only thing it can decide with. This module is that answer
//! for [`tilo_store_node::Access`]: a device may sync a workspace here when the
//! engine's copy of that workspace admits the device now.

use std::sync::Arc;
use std::time::SystemTime;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_store_node::Access;

/// The engine's membership, as a store's admission.
///
/// The owner is the store device's own: the workspaces it holds are the ones
/// it keeps, and a workspace it holds not at all is one it has nothing to
/// serve.
pub struct MembershipAccess {
    owner: Arc<NativeWorkspaceOwner>,
}

impl std::fmt::Debug for MembershipAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipAccess")
            .field("workspaces", &self.owner.workspaces().len())
            .finish_non_exhaustive()
    }
}

impl MembershipAccess {
    /// Answer from `owner`, the engine this store device runs.
    #[must_use]
    pub fn new(owner: Arc<NativeWorkspaceOwner>) -> Self {
        Self { owner }
    }

    /// The workspace of `owner`'s a store names by `workspace`.
    #[must_use]
    pub fn held(&self, workspace: u64) -> Option<WorkspaceId> {
        held_workspace(&self.owner, workspace)
    }
}

impl Access for MembershipAccess {
    fn admits(&self, workspace: u64, device: [u8; 32]) -> bool {
        let Some(workspace) = self.held(workspace) else {
            return false;
        };
        let now = SystemTime::now();
        // A workspace this device holds and that admits the device's own
        // certificate right now: a member's device, not a stranger, a
        // removed device or an expired one.
        self.owner
            .with_current(workspace, |view| {
                Ok(view.peer().permissions(device, now).is_ok())
            })
            .unwrap_or(false)
    }
}

/// The workspace of `owner`'s that a store names by `workspace`.
///
/// A store names a workspace by the number its id hashes to
/// ([`workspace_of`](crate::workspace_of)), which is one way: the workspaces
/// the engine holds are the ones to compare against.
#[must_use]
pub fn held_workspace(owner: &NativeWorkspaceOwner, workspace: u64) -> Option<WorkspaceId> {
    owner
        .workspaces()
        .into_iter()
        .find(|id| crate::workspace_of(&id.to_string()).ok() == Some(workspace))
}
