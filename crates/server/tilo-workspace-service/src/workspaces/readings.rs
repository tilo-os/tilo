//! A governed workspace as it is read without holding its lock: the replica
//! every change is made in, the workspace's name and the keys of the device it
//! is held on. A workspace's lock is held across what takes long — asking the
//! key service, which settles a newcomer's access first, and a commit's writes
//! — so every read that can answer from the replica alone reads it here.

use std::collections::HashMap;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use tilo_peer_state::direct::Replica;

use super::{DeviceKeys, GovernedWorkspace, Workspaces};

/// What a governed workspace is read through without its lock.
#[derive(Clone)]
pub(super) struct Reading {
    pub(super) replica: Replica,
    pub(super) workspace: String,
    pub(super) device: Option<DeviceKeys>,
}

type Governed = std::sync::Weak<tokio::sync::Mutex<GovernedWorkspace>>;

/// The [`Reading`] of each workspace governed here, once one was read.
#[derive(Default)]
pub(super) struct Readings(pub(super) std::sync::Mutex<HashMap<WorkspaceId, (Governed, Reading)>>);

impl Readings {
    pub(super) fn of(
        &self,
        workspace: WorkspaceId,
        governed: &std::sync::Arc<tokio::sync::Mutex<GovernedWorkspace>>,
    ) -> Option<Reading> {
        let (of, reading) = self.0.lock_recover().get(&workspace)?.clone();
        // A workspace opened again is read through its new replica.
        (of.as_ptr() == std::sync::Arc::as_ptr(governed)).then_some(reading)
    }

    pub(super) fn keep(
        &self,
        workspace: WorkspaceId,
        governed: &std::sync::Arc<tokio::sync::Mutex<GovernedWorkspace>>,
        reading: Reading,
    ) {
        let mut known = self.0.lock_recover();
        known.retain(|_, (of, _)| of.strong_count() > 0);
        known.insert(workspace, (std::sync::Arc::downgrade(governed), reading));
    }
}

impl GovernedWorkspace {
    /// What this workspace is read through without its lock.
    pub(super) fn reading(&self) -> Reading {
        Reading {
            replica: self.peer.clone(),
            workspace: self.workspace().to_owned(),
            device: self.device.clone(),
        }
    }
}

impl Workspaces {
    /// What the workspace `workspace` governed here is read through without
    /// its lock; `None` for a workspace not governed here. Only the first read
    /// of a workspace opened here waits for its lock.
    pub(super) async fn reading(&self, workspace: WorkspaceId) -> Option<Reading> {
        let governed = self.held(workspace).await?;
        if let Some(reading) = self.inner.readings.of(workspace, &governed) {
            return Some(reading);
        }
        let reading = governed.lock().await.reading();
        self.inner
            .readings
            .keep(workspace, &governed, reading.clone());
        Some(reading)
    }
}
