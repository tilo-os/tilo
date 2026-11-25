//! The `storage` resource: what [`tilo_system_resources::storage`] says it is, published with
//! the actions of [`StorageAction`].
//!
//! The scan reads the same roots the disk's storage policy does ([`tilo_storage_scan::scan_storage`])
//! and previews what clearing the apps' cache frees. A scan runs when the service starts and
//! whenever the resource is asked to scan again, never on every read of the machine.
//!
//! What this device holds of each workspace it mounts, and what keeping one here or freeing its
//! fetched files does, is the filesystem daemon's: the service is a client of the storage socket it
//! serves (`tilo_fs_client::storage`), never a second accounting of the same bytes. How the daemon
//! fetches and keeps the files it syncs is the same socket's: the prefetch switch, the space
//! fetched files may take, and freeing one file's copy here.

use std::path::PathBuf;

use tilo_actions::ResourceSpec;
use tilo_fs_client::storage::{
    CachePolicy as SocketPolicy, EvictionSelection, ResidencySummary, SyncStorageSnapshot,
};
use tilo_service::offer;
use tilo_storage_scan::{StorageCategory, StorageRoots};
use tilo_system_resources::storage::{
    CACHE_SIZES, CachePolicy, CategoryUsage, DiskUsage, ReclaimableFile, STORAGE, Storage,
    StorageAction, StoredItem, WorkspaceHold,
};

use crate::i18n::name;

/// How many of the largest items the reading keeps.
const LARGEST_SHOWN: usize = 20;

/// How many of the synced files that can be freed the reading lists.
const RECLAIMABLE_SHOWN: usize = 20;

/// The `storage` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.storage", label = "Storage")]
pub struct StorageReading;

/// The `storage` resource reading `storage`.
#[must_use]
pub fn resource(_storage: &Storage) -> ResourceSpec {
    offer::<StorageAction>(tilo_resource::typed_resource::<StorageReading>(
        STORAGE,
        name("so.tilo.system.storage", "Storage"),
    ))
}

/// This device's filesystem daemon, as a client of the socket it serves.
///
/// # Errors
/// The socket is not there (no workspace is mounted), or it refuses the connection.
#[cfg(unix)]
fn connect() -> Result<tilo_fs_client::storage::StorageClient, String> {
    tilo_fs_client::storage::StorageClient::connect(tilo_fs_client::storage::storage_socket())
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "No workspace is mounted on this device".to_owned()
            } else {
                format!("This device's files cannot be reached: {error}")
            }
        })
}

/// [`ResidencySummary`] as the reading publishes it.
fn holds(summary: ResidencySummary) -> Vec<WorkspaceHold> {
    summary
        .workspaces
        .into_iter()
        .map(|workspace| WorkspaceHold {
            workspace: workspace.workspace,
            kept_here: workspace.kept_here,
            pinned_bytes: workspace.split.pinned_bytes,
            replicated_bytes: workspace.split.replicated_bytes,
            only_here_bytes: workspace.split.only_here_bytes,
            only_here_files: workspace.split.only_here_files,
            cache_bytes: workspace.split.cache_bytes,
        })
        .collect()
}

/// [`SocketPolicy`] as the reading publishes it.
fn policy(policy: SocketPolicy) -> CachePolicy {
    CachePolicy {
        prefetch: policy.prefetch,
        budget_bytes: policy.budget_bytes,
    }
}

/// The synced files `snapshot` says this device may free, largest first, and the bytes the listed
/// ones give back.
fn reclaimable(snapshot: &SyncStorageSnapshot) -> (Vec<ReclaimableFile>, u64) {
    let mut files: Vec<ReclaimableFile> = snapshot
        .files
        .iter()
        .filter(|file| file.reclaimable_bytes > 0)
        .map(|file| ReclaimableFile {
            workspace: file.workspace.clone(),
            path: file.logical_path.clone(),
            bytes: file.reclaimable_bytes,
        })
        .collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.bytes));
    files.truncate(RECLAIMABLE_SHOWN);
    let bytes = files.iter().map(|file| file.bytes).sum();
    (files, bytes)
}

/// What the filesystem daemon says of the workspace files this device syncs, into `reading`: what
/// it holds of each mounted workspace, how it fetches and keeps them, and what can be freed. Each
/// answer lands on its own, so one the daemon refuses leaves the others readable.
fn fill_synced(reading: &mut Storage) {
    #[cfg(unix)]
    {
        let mut client = match connect() {
            Ok(client) => client,
            Err(error) => {
                reading.residency_error = Some(error.clone());
                reading.sync_error = Some(error);
                return;
            }
        };
        match client.residency() {
            Ok(summary) => reading.residency = holds(summary),
            Err(error) => reading.residency_error = Some(unread("What this device holds", &error)),
        }
        match client.cache_policy() {
            Ok(policy) => reading.cache_policy = Some(self::policy(policy)),
            Err(error) => {
                reading.sync_error = Some(unread("How this device keeps workspace files", &error));
            }
        }
        match client.snapshot() {
            Ok(snapshot) => {
                let (files, bytes) = reclaimable(&snapshot);
                reading.reclaimable = files;
                reading.reclaimable_bytes = bytes;
            }
            Err(error) => {
                reading.sync_error = Some(unread("The synced files this device holds", &error));
            }
        }
    }
    #[cfg(not(unix))]
    {
        reading.residency_error =
            Some("What this device holds of its workspaces cannot be read here".to_owned());
        reading.sync_error =
            Some("The workspace files this device holds cannot be read here".to_owned());
    }
}

/// `what` could not be read, in one line, with the daemon's own words.
#[cfg(unix)]
fn unread(what: &str, error: &std::io::Error) -> String {
    format!("{what} cannot be read: {error}")
}

/// Read what this computer's disk holds now.
#[must_use]
pub fn scan() -> Storage {
    let roots = StorageRoots::discover();
    let snapshot = tilo_storage_scan::scan_storage(&roots);
    let cache_bytes = roots
        .app_cache
        .as_deref()
        .and_then(|root| tilo_storage_scan::preview_cache_cleanup(root).ok())
        .map_or(0, |preview| preview.total_bytes);
    let disk = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| os_actions::disk_capacity(&home).ok())
        .map(|capacity| DiskUsage {
            total_bytes: capacity.total_bytes,
            free_bytes: capacity.available_bytes,
        });
    let categories = snapshot
        .totals()
        .into_iter()
        .map(|(category, bytes)| CategoryUsage {
            category: category.label().to_owned(),
            bytes,
            files: snapshot
                .entries
                .iter()
                .filter(|entry| entry.category == category)
                .map(|entry| entry.files)
                .sum(),
            reclaimable: snapshot
                .entries
                .iter()
                .filter(|entry| entry.category == category)
                .all(|entry| entry.reclaimable),
        })
        .collect();
    let mut largest: Vec<StoredItem> = snapshot
        .entries
        .iter()
        .map(|entry| StoredItem {
            category: entry.category.label().to_owned(),
            owner: entry.owner.clone(),
            bytes: entry.bytes,
            files: entry.files,
            reclaimable: entry.reclaimable,
        })
        .collect();
    largest.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
    largest.truncate(LARGEST_SHOWN);
    let mut storage = Storage {
        disk,
        categories,
        largest,
        cache_bytes,
        scanned: true,
        read_error: (!snapshot.unavailable.is_empty()).then(|| {
            let kinds: Vec<&str> = snapshot
                .unavailable
                .iter()
                .copied()
                .map(StorageCategory::label)
                .collect();
            format!("Some storage could not be read: {}", kinds.join(", "))
        }),
        ..Storage::default()
    };
    fill_synced(&mut storage);
    storage
}

/// Remove the apps' caches.
///
/// # Errors
/// There is no cache to clear, or one of its items changed since it was previewed.
pub fn clear_cache() -> Result<(), String> {
    let root = StorageRoots::discover()
        .app_cache
        .ok_or_else(|| "This computer keeps no app cache".to_owned())?;
    let preview =
        tilo_storage_scan::preview_cache_cleanup(&root).map_err(|error| error.to_string())?;
    let selected: Vec<usize> = (0..preview.items.len()).collect();
    tilo_storage_scan::apply_cleanup(&preview, &selected).map_err(|error| error.to_string())?;
    Ok(())
}

/// Keep `workspace` on this device, or stop keeping it.
///
/// # Errors
/// No workspace is mounted, the filesystem daemon refuses the change, or it cannot record it.
pub fn keep_here(workspace: &str, keep: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        connect()?
            .keep_here(workspace, keep)
            .map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, keep);
        Err("What this device holds of its workspaces cannot be changed here".to_owned())
    }
}

/// Free the cache `workspace` fetched here; nothing only this device holds goes.
///
/// # Errors
/// No workspace is mounted, or the filesystem daemon refuses the change.
pub fn free_cache(workspace: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        connect()?
            .free_cache(workspace)
            .map(drop)
            .map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = workspace;
        Err("What this device holds of its workspaces cannot be changed here".to_owned())
    }
}

/// Replace the cache policy with `change` made on the one in force, so changing the prefetch
/// switch keeps the space chosen and the other way round.
///
/// # Errors
/// No workspace is mounted, the daemon refuses the change, or it cannot record it.
#[cfg(unix)]
fn change_policy(change: impl FnOnce(&mut SocketPolicy)) -> Result<(), String> {
    let mut client = connect()?;
    let mut policy = client
        .cache_policy()
        .map_err(|error| unread("How this device keeps workspace files", &error))?;
    change(&mut policy);
    client.set_cache_policy(policy).map(drop).map_err(|error| {
        format!("How this device keeps workspace files cannot be changed: {error}")
    })
}

/// Fetch predicted workspace files ahead of their open, or only when they are opened.
///
/// # Errors
/// No workspace is mounted, or the filesystem daemon refuses the change.
pub fn set_prefetch(on: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        change_policy(|policy| policy.prefetch = on)
    }
    #[cfg(not(unix))]
    {
        let _ = on;
        Err("How this device keeps workspace files cannot be changed here".to_owned())
    }
}

/// Set the bytes of fetched workspace files this device keeps; `None` is the daemon's own share
/// of the disk. A smaller space frees the files over it at once.
///
/// # Errors
/// No workspace is mounted, or the filesystem daemon refuses the change.
pub fn set_cache_size(bytes: Option<u64>) -> Result<(), String> {
    #[cfg(unix)]
    {
        change_policy(|policy| policy.budget_bytes = bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
        Err("How this device keeps workspace files cannot be changed here".to_owned())
    }
}

/// The bytes `size` names, as `CACHE_SIZES` spells it.
///
/// # Errors
/// `size` is none of the sizes the person can choose.
pub fn cache_budget(size: &str) -> Result<Option<u64>, String> {
    tilo_system_resources::storage::cache_budget(size).ok_or_else(|| {
        format!(
            "{size} is not a space fetched files may take; one of {} is",
            CACHE_SIZES.join(", ")
        )
    })
}

/// Free one synced file's copy on this device, asking the daemon for its current version so the
/// file it drops is the one the person saw.
///
/// # Errors
/// No workspace is mounted, `workspace` holds no such file, the file cannot be freed, or the
/// filesystem daemon refuses the change.
pub fn free_file(workspace: &str, path: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        evict(
            workspace,
            |file| file.logical_path == path,
            "That file cannot be freed: the workspace keeps no safe copy of it elsewhere yet",
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, path);
        Err("The workspace files this device holds cannot be freed here".to_owned())
    }
}

/// Free every synced file whose copy here can be freed without losing anything.
///
/// # Errors
/// No workspace is mounted, nothing can be freed, or the filesystem daemon refuses the change.
pub fn free_synced_files() -> Result<(), String> {
    #[cfg(unix)]
    {
        evict(
            "",
            |_| true,
            "No synced file can be freed: no other device is known to hold a copy yet",
        )
    }
    #[cfg(not(unix))]
    {
        Err("The workspace files this device holds cannot be freed here".to_owned())
    }
}

/// Evict the files the daemon's own snapshot says can be freed, those `keep` names in `workspace`
/// when it names one; `nothing` says why, when there are none.
///
/// # Errors
/// As [`free_file`].
#[cfg(unix)]
fn evict(
    workspace: &str,
    keep: impl Fn(&tilo_fs_client::storage::SyncedFileState) -> bool,
    nothing: &str,
) -> Result<(), String> {
    let mut client = connect()?;
    let snapshot = client
        .snapshot()
        .map_err(|error| unread("The synced files this device holds", &error))?;
    let selected: Vec<EvictionSelection> = snapshot
        .files
        .iter()
        .filter(|file| {
            file.reclaimable_bytes > 0
                && (workspace.is_empty() || file.workspace == workspace)
                && keep(file)
        })
        .map(EvictionSelection::from)
        .collect();
    if selected.is_empty() {
        return Err(nothing.to_owned());
    }
    let report = client
        .evict(selected)
        .map_err(|error| format!("The files cannot be freed: {error}"))?;
    if !report.skipped.is_empty() && report.evicted_files == 0 && report.queued_files == 0 {
        return Err(format!("Nothing was freed: {}", report.skipped.join("; ")));
    }
    Ok(())
}
