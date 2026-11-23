//! The `storage` resource: what this computer's disk holds, how its space is used, its largest
//! items and its cache, with the actions that scan it again and free the cache.
//!
//! The scan covers the same roots the disk's storage policy does: installed apps, the apps'
//! data, the apps' cache and the synced replica. What a workspace holds on its servers is the
//! workspace service's reading, not this one.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const STORAGE: &str = "storage";

/// How full the disk the person's files live on is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl DiskUsage {
    /// The bytes in use.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// What one kind of item takes: installed apps, app data, cache or synced files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct CategoryUsage {
    /// What the kind is called: "Installed apps", "App data", "Cache", "Synced files".
    pub category: String,
    pub bytes: u64,
    pub files: u64,
    /// Whether all of it can be freed without losing anything: cache, never data.
    pub reclaimable: bool,
}

/// One of the largest items the scan found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct StoredItem {
    /// The kind it is: "Installed apps", "App data", "Cache", "Synced files".
    pub category: String,
    /// What it is called: an app's id or name, a file's name.
    pub owner: String,
    pub bytes: u64,
    pub files: u64,
    pub reclaimable: bool,
}

/// What this computer's storage reads as now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Storage {
    /// The disk the person's home lives on, where it could be measured.
    #[serde(default)]
    pub disk: Option<DiskUsage>,
    /// The kinds found, largest first.
    pub categories: Vec<CategoryUsage>,
    /// The largest items found, largest first.
    pub largest: Vec<StoredItem>,
    /// What clearing the apps' cache would free.
    #[serde(default)]
    pub cache_bytes: u64,
    /// Whether a scan has answered since the service started.
    #[serde(default)]
    pub scanned: bool,
    /// Why the scan could not read something, when it could not.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `storage` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.storage")]
pub enum StorageAction {
    /// Read what the disk holds again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "scan storage, verwendeten speicher neu lesen"
    )]
    Scan,
    /// Remove the apps' caches, which download again when they are needed.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "clear cache, free space, cache leeren"
    )]
    ClearCache,
}
