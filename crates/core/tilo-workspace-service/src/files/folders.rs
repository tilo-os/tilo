//! Where each folder the files daemon holds stands, as the daemon last said it: published as this
//! device's resource [`FOLDERS_APP`], so a person's shell (`device.folders`) shows whether what
//! they put in a folder has reached the workspace before they rely on it being there.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, Caller, Pending, RemoteActionMessage, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake,
};
use tilo_fs::tree_sync::Status;
use tilo_lock::MutexRecover as _;

/// The resource this device's folders are read at, in the person's own workspace (`record`
/// empty): a reading of [`FolderList`], answered to this device's own person on this device
/// alone.
pub const FOLDERS_APP: &str = "tilo.folders";

/// One folder as it is shown: where its sync stands, and the names a person knows its workspace
/// and the server keeping it by.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub workspace: String,
    pub name: String,
    /// The name of the device that keeps the workspace's content, empty while none is named.
    pub server: String,
    pub status: Status,
}

/// The reading of [`FOLDERS_APP`]: every folder this device holds, by workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FolderList {
    pub folders: Vec<Folder>,
}

/// The folders the files daemon holds, each as it last said.
#[derive(Clone, Default)]
pub struct Folders(Arc<Shown>);

#[derive(Default)]
struct Shown {
    folders: Mutex<BTreeMap<String, Folder>>,
    revision: AtomicU64,
    /// The reads waiting for a folder to move.
    waiting: Mutex<Vec<ResourceWake>>,
}

impl Folders {
    /// Show `folder` as it stands now.
    pub(crate) fn show(&self, folder: Folder) {
        let mut folders = self.0.folders.lock_recover();
        if folders.get(&folder.workspace) == Some(&folder) {
            return;
        }
        folders.insert(folder.workspace.clone(), folder);
        drop(folders);
        self.moved();
    }

    /// The daemon no longer holds `workspace`'s folder.
    pub(crate) fn gone(&self, workspace: &str) {
        if self.0.folders.lock_recover().remove(workspace).is_some() {
            self.moved();
        }
    }

    fn moved(&self) {
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        for wake in std::mem::take(&mut *self.0.waiting.lock_recover()) {
            wake.wake();
        }
    }

    /// Every folder, by its workspace.
    #[must_use]
    pub fn all(&self) -> Vec<Folder> {
        self.0.folders.lock_recover().values().cloned().collect()
    }
}

impl ResourceReads for Folders {
    fn read(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if address.app != FOLDERS_APP || !address.record.is_empty() {
            return Ok(ResourceCopy::Absent);
        }
        // The revision first: a folder that moves while they are read moves it past this one.
        let revision = self.0.revision.load(Ordering::Relaxed).to_string();
        serde_json::to_value(FolderList {
            folders: self.all(),
        })
        .map(|value| ResourceCopy::reading(value, Some(revision)))
        .map_err(|error| ActionCallError::Unavailable {
            reason: error.to_string(),
        })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        _caller: &Caller,
        _address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        let mut waiting = self.0.waiting.lock_recover();
        waiting.retain(ResourceWake::waiting);
        waiting.push(wake.clone());
        Ok(())
    }
}
