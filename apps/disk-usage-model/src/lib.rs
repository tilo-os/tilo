//! Disk Usage Analyzer's action resource and the previous record of local folders.
//! Devices scan their own disks and keep the recent folders in device settings.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_resource::{self, AbsolutePath, Model, Recents, resource};

/// The folders one device analyzed lately, newest first.
pub type Folders = Recents<PathBuf, 5>;

/// The folders analyzed lately, by the device whose disk they are on: a resource Disk Usage
/// keeps beside the app's resource in the workspace, each device reading and writing only its
/// own list.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct RecentFolders {
    pub devices: BTreeMap<String, Folders>,
}

impl RecentFolders {
    /// What the resource holds: the folders analyzed lately, by device.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "disk-usage.recent-folders",
    read_content = RecentFolders::read_document
)]
impl RecentFolders {
    /// `folder` on `device` was analyzed: it moves to the front.
    #[action(housekeeping)]
    pub fn remember(&mut self, device: String, folder: AbsolutePath) {
        self.devices
            .entry(device)
            .or_default()
            .remember(folder.into_path_buf());
    }
}

impl RecentFolders {
    /// The folders `device` analyzed lately, newest first.
    #[must_use]
    pub fn on(&self, device: &str) -> Folders {
        self.devices.get(device).cloned().unwrap_or_default()
    }
}

/// The disk-usage analyzer: the resource its effects and live rows are published on.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DiskUsage {}

impl DiskUsage {
    /// What the resource holds: nothing — what it scans is this device's disk, and the
    /// folders it remembers are kept in the resource beside it.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(kind = "disk-usage", read_content = DiskUsage::read_document)]
impl DiskUsage {}
