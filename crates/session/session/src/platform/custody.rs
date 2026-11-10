//! Whether this device still holds custody of a workspace, as this desktop checks it.
//!
//! The owner of a workspace commits only what this device still has custody of: a device the
//! workspace removed, or whose certificate ran out, finds out before it commits. The check itself
//! is the client's seam (`tilo_session_client::custody::Custody`); this desktop proves it over the
//! collaboration plane.

use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::owner::NativeWorkspaceOwner;

pub use tilo_session_client::custody::Custody;

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

/// This desktop's custody, as its engine asks it before every commit to a workspace.
#[cfg(target_os = "linux")]
pub(crate) fn of_this_desk() -> std::sync::Arc<dyn Custody> {
    std::sync::Arc::new(Signed)
}
