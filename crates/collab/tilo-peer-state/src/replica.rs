//! One workspace's replica: the one copy every part of the engine decides on.

use crate::StatePeer;
use parking_lot::{Mutex, MutexGuard};
use std::sync::Arc;

/// One workspace's replica, held by the device whose key signs its changes.
///
/// Every part of the engine that decides something about the workspace reads
/// the same replica: the direct link admits frames into it, and the action
/// plane authorizes the devices asking for its actions against its roster.
/// The guard a replica's state is read or changed through.
pub type PeerGuard<'a> = MutexGuard<'a, StatePeer>;
#[derive(Clone)]
pub struct Replica(Arc<Mutex<StatePeer>>);

impl Replica {
    /// Hold `peer` as a replica.
    #[must_use]
    pub fn new(peer: StatePeer) -> Self {
        Self(Arc::new(Mutex::new(peer)))
    }

    /// Hold a replica other parts of the engine keep already.
    #[must_use]
    pub fn over(peer: Arc<Mutex<StatePeer>>) -> Self {
        Self(peer)
    }

    /// The replica, for another part of the engine to share.
    #[must_use]
    pub fn shared(&self) -> Arc<Mutex<StatePeer>> {
        Arc::clone(&self.0)
    }

    /// Read the replica.
    pub fn read<R>(&self, read: impl FnOnce(&StatePeer) -> R) -> R {
        read(&self.0.lock())
    }

    /// Change the replica: a governance change, or anything else the plane's
    /// authorization has to see at once.
    pub fn write<R>(&self, write: impl FnOnce(&mut StatePeer) -> R) -> R {
        write(&mut self.0.lock())
    }

    /// The replica, held: a caller that reads and changes it as one step.
    /// Never hold it across an await.
    pub fn lock(&self) -> MutexGuard<'_, StatePeer> {
        self.0.lock()
    }

    /// The workspace this replica is, as its id.
    #[must_use]
    pub fn workspace(&self) -> String {
        self.read(|peer| peer.workspace_id().to_owned())
    }

    /// The workspace this replica is, as the number a store names it by.
    ///
    /// # Errors
    ///
    /// The replica's workspace id is not a workspace id.
    pub fn number(&self) -> Result<u64, String> {
        self.read(|peer| workspace_number(peer.workspace_id()))
    }
}

fn workspace_number(workspace: &str) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(workspace)
        .map_err(|_| format!("{workspace} is not a workspace id"))
}
