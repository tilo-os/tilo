//! Where each folder the files daemon holds stands, as the daemon last said it: published as this
//! device's resource [`FOLDERS_APP`], so a person's shell (`device.folders`) shows whether what
//! they put in a folder has reached the workspace before they rely on it being there.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads, ResourceWake,
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
    /// Where this workspace's files stand on this computer, empty while they are not mounted
    /// here: the place a person opens to browse them.
    #[serde(default)]
    pub path: String,
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
        let waiting = std::mem::take(&mut *self.0.waiting.lock_recover());
        for wake in waiting {
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
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer
    where
        Self: 'static,
    {
        Box::pin(tilo_action_calls::answer_read(
            self,
            caller.clone(),
            correlation,
            read,
        ))
    }

    fn read<'a>(
        &'a self,
        _caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ResourceCopy> {
        tilo_actions::remote::reads_answer(async move {
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
        })
    }

    fn watch<'a>(
        &'a self,
        _caller: &'a Caller,
        _address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        tilo_actions::remote::reads_answer(async move {
            let mut waiting = self.0.waiting.lock_recover();
            waiting.retain(ResourceWake::waiting);
            waiting.push(wake.clone());
            drop(waiting);
            Ok(())
        })
    }
}
