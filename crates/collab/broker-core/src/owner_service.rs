//! What the session holding this device's owner answers for its commits: which workspaces it commits,
//! where the owner is, whether this device still owns a workspace, and how a commit
//! reaches the workspace's peers. Every host that holds an owner serves it.

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::NativeWorkspaceOwner;

/// What the session holding the native owner answers for its commits.
pub trait OwnerService: collab_types::signer::MaybeSendSync + 'static {
    /// Whether `workspace`'s replica is projected here: a workspace held here,
    /// with its governance.
    fn projects_workspace(&self, workspace: WorkspaceId) -> bool;
    /// The owner if it is open, never waiting: `unavailable` when there is
    /// none.
    fn opened_owner(&self, unavailable: &str) -> Result<NativeWorkspaceOwner, String>;
    /// The owner, waiting for its open: for a commit's blocking thread.
    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String>;
    /// Whether this device still owns `workspace` in `owner`, as
    /// its current identity for the workspace says.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check_ownership(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<(), String>;
    /// This device's ownership of its key, which signs its own changes of `workspace`.
    ///
    /// # Errors
    /// This device holds no ownership of `workspace`.
    fn signer(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<std::sync::Arc<dyn collab_types::signer::DeviceSigner>, String>;
    /// Send the owner's outgoing ownership of `workspace` to its peers soon:
    /// what was just committed leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace: WorkspaceId);
}
