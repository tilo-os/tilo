//! The `storage` resource: what [`tilo_system_resources::storage`] says it is, published with
//! the actions of [`StorageAction`].
//!
//! The scan reads the same roots the disk's storage policy does ([`tilo_storage_scan::scan_storage`])
//! and previews what clearing the apps' cache frees. A scan runs when the service starts and
//! whenever the resource is asked to scan again, never on every read of the machine.
//!
//! What this device holds of each workspace it mounts, and what keeping one here or freeing its
//! fetched files does, is the filesystem daemon's: the service is a client of the storage socket it
//! serves (`tilo_fs_client::storage`), never a second accounting of the same bytes.

use std::path::PathBuf;

use tilo_actions::ResourceSpec;
use tilo_fs_client::storage::ResidencySummary;
use tilo_service::offer;
use tilo_storage_scan::{StorageCategory, StorageRoots};
use tilo_system_resources::storage::{
    CategoryUsage, DiskUsage, STORAGE, Storage, StorageAction, StoredItem, WorkspaceHold,
};

use crate::i18n::name;

/// How many of the largest items the reading keeps.
const LARGEST_SHOWN: usize = 20;

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

/// What this device holds of each workspace it mounts, as the filesystem daemon splits it.
///
/// # Errors
/// The socket is not there (no workspace is mounted), or the daemon refused or could not read.
fn residency() -> Result<ResidencySummary, String> {
    #[cfg(unix)]
    {
        connect()?
            .residency()
            .map_err(|error| format!("What this device holds cannot be read: {error}"))
    }
    #[cfg(not(unix))]
    {
        Err("What this device holds of its workspaces cannot be read here".to_owned())
    }
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
    let (residency, residency_error) = match self::residency() {
        Ok(summary) => (holds(summary), None),
        Err(error) => (Vec::new(), Some(error)),
    };
    Storage {
        disk,
        categories,
        largest,
        cache_bytes,
        residency,
        residency_error,
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
    }
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
