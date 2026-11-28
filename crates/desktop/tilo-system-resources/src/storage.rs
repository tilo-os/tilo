//! The `storage` resource: what this computer's disk holds, how its space is used, its largest
//! items and its cache, with the actions that scan it again and free the cache.
//!
//! The scan covers the same roots the disk's storage policy does: installed apps, the apps'
//! data, the apps' cache and the synced replica. What a workspace holds on its servers is the
//! workspace service's reading, not this one.
//!
//! How the filesystem daemon fetches and keeps the workspace files it syncs is here too: the
//! prefetch switch and the space fetched files may take, and the synced files whose copy on this
//! device can be dropped without losing anything.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const STORAGE: &str = "storage";

/// How full the disk the person's files live on is.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
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

/// Where one workspace's bytes on this device stand, by what losing this device would cost. The
/// parts are disjoint and add up to the bytes the workspace holds here. Counts only: nothing here
/// names a file.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct WorkspaceHold {
    /// The workspace, as its mount names it.
    pub workspace: String,
    /// Whether the person keeps the whole workspace on this device.
    pub kept_here: bool,
    /// Kept here because the person asked for it ("Keep offline"), and held by another device too.
    pub pinned_bytes: u64,
    /// Kept here, and another device acknowledged holding the same version.
    pub replicated_bytes: u64,
    /// Held by no other device this one knows of: authored here and not yet acknowledged
    /// elsewhere, or changed and not yet saved. Wiping this device loses it.
    pub only_here_bytes: u64,
    /// Files with bytes only here.
    pub only_here_files: u64,
    /// Fetched from another device and dropped again when space is short: safe to free, it
    /// downloads again when opened.
    pub cache_bytes: u64,
}

impl WorkspaceHold {
    /// Every byte the workspace holds here.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.pinned_bytes
            .saturating_add(self.replicated_bytes)
            .saturating_add(self.only_here_bytes)
            .saturating_add(self.cache_bytes)
    }
}

/// How this device fetches and keeps the workspace files it syncs: whether it fetches the files
/// a person is likely to open next before they open them, and how many bytes of fetched files it
/// keeps.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct CachePolicy {
    /// Fetch predicted files ahead of their open. Network, battery and disk checks still apply.
    pub prefetch: bool,
    /// Bytes of fetched files to keep; `None` is the share of the disk the daemon chooses,
    /// which a person reads as "Automatic". Kept and only-here files are kept regardless.
    #[serde(default)]
    pub budget_bytes: Option<u64>,
}

/// The space fetched files may take, in the words the action takes them.
pub const CACHE_SIZES: [&str; 6] = ["Automatic", "5 GB", "10 GB", "25 GB", "50 GB", "100 GB"];

/// The bytes `size` names, as [`CACHE_SIZES`] spells it: `Some(None)` is the daemon's own share
/// of the disk, and `None` a size it does not offer.
#[must_use]
pub fn cache_budget(size: &str) -> Option<Option<u64>> {
    if size == "Automatic" {
        return Some(None);
    }
    CACHE_SIZES
        .iter()
        .filter(|offered| **offered == size)
        .find_map(|offered| offered.strip_suffix(" GB"))
        .and_then(|gigabytes| gigabytes.parse::<u64>().ok())
        .map(|gigabytes| Some(gigabytes.saturating_mul(1_000_000_000)))
}

/// A synced workspace file whose copy on this device can be freed without losing anything: the
/// workspace has it elsewhere, or it downloads again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct ReclaimableFile {
    /// The workspace the file belongs to, as the reading names its workspaces.
    pub workspace: String,
    /// The file's path in that workspace.
    pub path: String,
    /// The bytes its copy here takes.
    pub bytes: u64,
}

/// Where one synced file's bytes on this device stand: what a person asks about a file of a
/// workspace before opening it ("Kept on this device", "Kept offline", "Kept on another device").
/// Only files that are not plainly local and kept elsewhere are listed, so a workspace with
/// thousands of files lists the ones the person kept and the ones this device fetched and may
/// free.
#[allow(
    clippy::struct_excessive_bools,
    reason = "a reading's facts: whether it is pinned, on another device, local and unsaved are each read on their own, not a state machine"
)]
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct KeptFile {
    /// The workspace the file belongs to, as the reading names its workspaces.
    pub workspace: String,
    /// The file's path in that workspace.
    pub path: String,
    /// Whether the person keeps this file offline ("Keep offline").
    pub pinned: bool,
    /// Whether another device the person's acknowledged holding this version.
    pub elsewhere: bool,
    /// Whether this device holds the file's bytes whole.
    pub local: bool,
    /// The bytes of the file, and how many of them are on this device.
    pub bytes: u64,
    pub local_bytes: u64,
    /// The bytes freeing this device's copy would give back, when it may be freed safely.
    pub reclaimable_bytes: u64,
    /// What fetch or save of it is doing: "in flight", or "idle".
    pub transfer: String,
    /// Whether this device holds changes of it no other device has saved yet.
    pub unsaved: bool,
}

/// The most files [`Storage::kept`] lists: the ones a person kept come first, then those this
/// device fetched and may free.
pub const KEPT_SHOWN: usize = 256;

/// What this computer's storage reads as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Storage {
    /// The disk the person's home lives on, where it could be measured.
    #[serde(default)]
    pub disk: Option<DiskUsage>,
    /// What this device holds of each workspace it mounts, by what losing this device would cost.
    #[serde(default)]
    pub residency: Vec<WorkspaceHold>,
    /// Why what this device holds of its workspaces could not be read, when it could not.
    #[serde(default)]
    pub residency_error: Option<String>,
    /// How this device fetches and keeps the workspace files it syncs, when its files can be
    /// reached.
    #[serde(default)]
    pub cache_policy: Option<CachePolicy>,
    /// Where each synced file that is not plainly local and kept elsewhere stands: the ones the
    /// person keeps offline, and the ones this device fetched and may free.
    #[serde(default)]
    pub kept: Vec<KeptFile>,
    /// Synced files whose copy here can be freed without losing anything, largest first.
    #[serde(default)]
    pub reclaimable: Vec<ReclaimableFile>,
    /// Every byte freeing [`Self::reclaimable`] gives back.
    #[serde(default)]
    pub reclaimable_bytes: u64,
    /// Why the synced files this device holds could not be read, when they could not.
    #[serde(default)]
    pub sync_error: Option<String>,
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
    /// Keep a whole workspace on this device, or stop keeping it. Keeping it fetches every file
    /// and holds it, so the workspace opens with no network.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "keep offline, keep on this device, offline behalten, auf diesem gerät behalten"
    )]
    KeepHere {
        /// The workspace, as the reading lists it.
        #[param("The workspace to keep, as the reading lists it")]
        workspace: String,
        /// Whether to keep it here.
        #[param("Whether to keep it on this device")]
        keep: bool,
    },
    /// Free a workspace's fetched cache: the files this device downloaded from another one, which
    /// download again when they are opened. Nothing only this device holds is removed.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "free up, free cache, speicher freigeben"
    )]
    FreeWorkspaceCache {
        /// The workspace, as the reading lists it.
        #[param("The workspace whose fetched files to free, as the reading lists it")]
        workspace: String,
    },
    /// Fetch the workspace files likely to be opened next before they are opened, or only when
    /// they are opened.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "download",
        keywords = "prefetch, download ahead, vorausladen"
    )]
    SetPrefetch {
        /// Whether to fetch files ahead of their open.
        #[param("Whether to fetch files ahead of their open")]
        on: bool,
    },
    /// Set how much space files fetched from another device may take on this device.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "hard-drive",
        keywords = "cache size, space for fetched files, cache-größe"
    )]
    SetCacheSize {
        /// The space to keep, as `CACHE_SIZES` names it.
        #[param("The space to keep", choices = CACHE_SIZES)]
        size: String,
    },
    /// Keep one file of a workspace on this device, so it opens with no network, or stop keeping
    /// it. A file kept offline is never freed to make room.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "pin",
        keywords = "keep offline, keep on this device, offline behalten, datei offline"
    )]
    KeepFile {
        /// The file's workspace, as the reading lists it.
        #[param("The file's workspace, as the reading lists it")]
        workspace: String,
        /// The file's path in that workspace.
        #[param("The file's path in its workspace, as the reading lists it")]
        path: String,
        /// Whether to keep it on this device.
        #[param("Whether to keep it on this device")]
        keep: bool,
    },
    /// Free one synced file's copy on this device. The workspace keeps it elsewhere, and opening
    /// it here fetches it again.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "free a file, remove local copy, datei freigeben"
    )]
    FreeFile {
        /// The file's workspace, as the reading lists it.
        #[param("The file's workspace, as the reading lists it")]
        workspace: String,
        /// The file's path in that workspace.
        #[param("The file's path in its workspace, as the reading lists it")]
        path: String,
    },
    /// Free every synced file whose copy here can be freed without losing anything.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "free synced files, free space, synchronisierte dateien freigeben"
    )]
    FreeSyncedFiles,
}
