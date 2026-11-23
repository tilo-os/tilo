//! Where each folder the files daemon holds stands, as the daemon last said it: what the session
//! shows of them (`device.folders`), so a person knows whether what they put in a folder has
//! reached the workspace before they rely on it being there.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_fs::files_sync::Status;
use tilo_lock::MutexRecover as _;

/// One folder as it is shown: where its sync stands, and the names a person knows its workspace
/// and the server keeping it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub workspace: String,
    pub name: String,
    /// The name of the device that keeps the workspace's content, empty while none is named.
    pub server: String,
    pub status: Status,
}

/// The folders the files daemon holds, each as it last said.
#[derive(Clone)]
pub struct Folders(Arc<Shown>);

struct Shown {
    folders: Mutex<BTreeMap<String, Folder>>,
    revision: AtomicU64,
    /// Wakes whoever shows the folders when one moved.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Folders {
    /// No folder yet; `wake` is called each time one moves.
    #[must_use]
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self(Arc::new(Shown {
            folders: Mutex::new(BTreeMap::new()),
            revision: AtomicU64::new(0),
            wake,
        }))
    }

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
        (self.0.wake)();
    }

    /// Moves each time a folder does.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Relaxed)
    }

    /// Every folder, by its workspace.
    #[must_use]
    pub fn all(&self) -> Vec<Folder> {
        self.0.folders.lock_recover().values().cloned().collect()
    }
}
