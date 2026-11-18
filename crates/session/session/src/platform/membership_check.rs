//! Whether this device still owns a workspace, as this desktop checks it.
//!
//! The owner of a workspace commits only what this device still owns: a device the
//! workspace removed, or whose certificate ran out, finds out before it commits. The check itself
//! is the client's service (`tilo_session_client::membership_check::MembershipCheck`); this
//! desktop proves it over the peer network.

#[cfg(target_os = "linux")]
use tilo_desktop_model::WorkspaceId;
#[cfg(target_os = "linux")]
use tilo_identity::identity_store::DeviceIdentity;
#[cfg(target_os = "linux")]
use tilo_peer_state::owner::NativeWorkspaceOwner;

pub use tilo_session_client::membership_check::MembershipCheck;

/// This desktop's ownership: the device's own key proves it to the workspace's host.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Signed;

#[cfg(target_os = "linux")]
impl MembershipCheck for Signed {
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &DeviceIdentity,
    ) -> Result<(), String> {
        tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace)?
            .ownership(identity)
            .map(drop)
    }
}

/// This desktop's ownership, as its engine asks it before every commit to a workspace.
#[cfg(target_os = "linux")]
pub(crate) fn of_this_session() -> std::sync::Arc<dyn MembershipCheck> {
    std::sync::Arc::new(Signed)
}
