//! What a governed workspace is read through without its lock, for every read of its replica:
//! policy reads, document reads and views alike.

use super::{GovernedWorkspace, Workspaces};
use std::collections::HashMap;
use std::sync::Arc;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

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

/// What a governed workspace is read through without its lock: its replica,
/// which every change is made in and which is only ever held for a moment, the
/// workspace's name and the keys of the device it is held on. The workspace's
/// lock is held across what takes long: asking the key service, which settles a
/// newcomer's access first, and a commit's writes. A read waiting for those made
/// every call of an agent wait up to a minute while a person joined.
#[derive(Clone)]
pub(super) struct Reading {
    pub(super) replica: super::Replica,
    pub(super) workspace: String,
    pub(super) device: Option<super::DeviceKeys>,
}

type Governed = std::sync::Weak<tokio::sync::Mutex<GovernedWorkspace>>;

/// The [`Reading`] of each workspace governed here, once one was read.
#[derive(Default)]
pub(super) struct Readings(std::sync::Mutex<HashMap<WorkspaceId, (Governed, Reading)>>);

impl Readings {
    fn of(
        &self,
        workspace: WorkspaceId,
        governed: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
    ) -> Option<Reading> {
        let (of, reading) = self.0.lock_recover().get(&workspace)?.clone();
        // A workspace opened again is read through its new replica.
        (of.as_ptr() == Arc::as_ptr(governed)).then_some(reading)
    }

    fn keep(
        &self,
        workspace: WorkspaceId,
        governed: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
        reading: Reading,
    ) {
        let mut known = self.0.lock_recover();
        known.retain(|_, (of, _)| of.strong_count() > 0);
        known.insert(workspace, (Arc::downgrade(governed), reading));
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
