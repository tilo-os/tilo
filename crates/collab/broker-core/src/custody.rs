//! What the session holding this device's owner answers for its commits: which workspaces it commits,
//! where the owner is, whether this device still holds custody of a workspace, and how a commit
//! reaches the workspace's peers. Every host that holds an owner serves it.

use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::NativeWorkspaceOwner;

/// What the session holding the native owner answers for its commits.
pub trait OwnerCustody: collab_types::signer::MaybeSendSync + 'static {
    /// Whether `workspace`'s app records are documents the owner commits:
    /// a workspace held here whose host takes part in no channels.
    fn commits_documents(&self, workspace: WorkspaceId) -> bool;
    /// Whether `workspace`'s replica is projected here, its records committed
    /// documents or sealed channels: a workspace held here, with its
    /// governance.
    fn projects_room(&self, workspace: WorkspaceId) -> bool;
    /// The owner if it is open, never waiting: `unavailable` when there is
    /// none.
    fn opened_owner(&self, unavailable: &str) -> Result<NativeWorkspaceOwner, String>;
    /// The owner, waiting for its open: for a commit's blocking thread.
    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String>;
    /// Whether this device still holds custody of `workspace` in `owner`, as
    /// its current identity for the workspace says.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check_custody(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<(), String>;
    /// The key this device signs its own changes of `workspace` with.
    ///
    /// # Errors
    /// This device holds no custody of `workspace`, or has no key.
    fn signing_key(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<SigningKey, String>;
    /// Send the owner's outgoing custody of `workspace` to its peers soon:
    /// what was just committed leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace: WorkspaceId);
}
