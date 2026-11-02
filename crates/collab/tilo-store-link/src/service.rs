//! A device's store service: the workspaces it keeps, retained for their
//! members and served on the endpoint it already speaks on.
//!
//! A store device is a member that keeps a workspace's sealed history for the
//! others while they are away, without any key to open it. This is where such
//! a device says so: the appetite it was given decides how much it keeps
//! ([`appetite`]), the engine it runs decides who may sync it
//! ([`MembershipAccess`]), and the handler answers the store's protocol on the
//! embedder's endpoint — one accept loop serves everything the device answers.
//!
//! What the store service is not: a second door. It holds no state of its own
//! beyond the frames, reads membership through the same engine a member acts
//! on, and a member reaches it through the same [`StoreSession`] it uses with
//! any store.

use crate::access::MembershipAccess;
use std::path::PathBuf;
use std::sync::Arc;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_relay_protocol::{DeviceMeshProfile, ReplicationIntent};
use tilo_store_node::{Appetite, Store, StoreError, StoreNode};

/// The frame bytes a store that takes work keeps before it retires the oldest
/// frames: the share of a device that is not set aside for its workspaces.
pub const WORK_BYTES: u64 = 1024 * 1024 * 1024;

/// How much history this device keeps for its workspaces, as its mesh policy
/// says; `None` when it keeps none, i.e. it is not a store device.
///
/// A device that pins a workspace whole ([`ReplicationIntent::FullWorkspace`])
/// stores everything it is given there, until the authors retire their own
/// frames. A device that only offers to keep state
/// ([`ServiceCapabilities::workspace_replication`]) takes work instead: it
/// keeps up to [`WORK_BYTES`] and retires the oldest frames first.
///
/// [`ServiceCapabilities::workspace_replication`]: tilo_relay_protocol::ServiceCapabilities::workspace_replication
#[must_use]
pub fn appetite(profile: &DeviceMeshProfile) -> Option<Appetite> {
    if !profile.services.workspace_replication {
        return None;
    }
    let keeps_whole = profile
        .replication
        .values()
        .any(|intent| matches!(intent, ReplicationIntent::FullWorkspace));
    Some(if keeps_whole {
        Appetite::StoreAll
    } else {
        Appetite::Bounded {
            max_bytes: WORK_BYTES,
        }
    })
}

/// A device's store: the frames it keeps for its workspaces, and the
/// handler that serves them to the workspace's members.
pub struct StoreService {
    store: Arc<Store>,
    node: Arc<StoreNode>,
}

impl std::fmt::Debug for StoreService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreService")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl StoreService {
    /// Take on being a store device at `root` for the workspaces `owner`
    /// holds, keeping what `profile` says this device keeps.
    ///
    /// Returns `None` when the device is no store ([`appetite`]): nothing is
    /// opened and nothing is served.
    ///
    /// # Errors
    ///
    /// The root cannot be created or read back ([`StoreError`]).
    pub fn open(
        root: impl Into<PathBuf>,
        profile: &DeviceMeshProfile,
        owner: Arc<NativeWorkspaceOwner>,
    ) -> Result<Option<Self>, StoreError> {
        let Some(appetite) = appetite(profile) else {
            return Ok(None);
        };
        let store = Arc::new(Store::open(root, appetite)?);
        let node = Arc::new(StoreNode::new(
            Arc::clone(&store),
            Arc::new(MembershipAccess::new(owner)),
        ));
        Ok(Some(Self { store, node }))
    }

    /// The store this device keeps.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// What answers the store's protocol
    /// ([`ALPN`](tilo_store_node::ALPN)): the embedder adds it to the router
    /// that serves its endpoint, so one accept loop answers everything the
    /// device serves.
    #[must_use]
    pub fn handler(&self) -> Arc<StoreNode> {
        Arc::clone(&self.node)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use std::collections::BTreeMap;
    use tilo_relay_protocol::{DeviceRole, ServiceCapabilities};

    fn profile(
        services: ServiceCapabilities,
        replication: BTreeMap<u64, ReplicationIntent>,
    ) -> DeviceMeshProfile {
        DeviceMeshProfile {
            device: [1; 32],
            role: DeviceRole::AlwaysOnServer,
            services,
            replication,
            filesystem_endpoints: Vec::new(),
        }
    }

    #[test]
    fn a_device_that_stores_a_workspace_whole_keeps_everything() {
        let profile = profile(
            ServiceCapabilities {
                workspace_replication: true,
                ..ServiceCapabilities::default()
            },
            BTreeMap::from([(7, ReplicationIntent::FullWorkspace)]),
        );
        assert_eq!(appetite(&profile), Some(Appetite::StoreAll));
    }

    #[test]
    fn a_device_that_takes_work_keeps_a_bounded_share() {
        let profile = profile(
            ServiceCapabilities {
                workspace_replication: true,
                ..ServiceCapabilities::default()
            },
            BTreeMap::from([(7, ReplicationIntent::OnDemand)]),
        );
        assert_eq!(
            appetite(&profile),
            Some(Appetite::Bounded {
                max_bytes: WORK_BYTES
            })
        );
    }

    #[test]
    fn a_device_that_does_not_pin_state_is_no_store() {
        let profile = profile(
            ServiceCapabilities::default(),
            BTreeMap::from([(7, ReplicationIntent::FullWorkspace)]),
        );
        assert_eq!(appetite(&profile), None);
    }
}
