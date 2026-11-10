//! Whether this device still holds custody of a workspace, as its host checks it.
//!
//! The owner of a workspace commits only what this device still has custody of: a device the
//! workspace removed, or whose certificate ran out, finds out before it commits. Each host keeps
//! its own proof of that — this desktop's signed custody over the collaboration plane, a page's
//! over its own transport.

use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::owner::NativeWorkspaceOwner;

/// The check a host makes before this device commits to a workspace it holds.
pub trait Custody: collab_types::signer::MaybeSendSync {
    /// Whether this device still holds custody of `workspace` in `owner`.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &HostIdentity,
    ) -> Result<(), String>;
}

/// This desktop's custody: the device's own key proves it to the workspace's host.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Signed;

#[cfg(target_os = "linux")]
impl Custody for Signed {
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &HostIdentity,
    ) -> Result<(), String> {
        tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace)?
            .custody(identity)
            .map(drop)
    }
}
