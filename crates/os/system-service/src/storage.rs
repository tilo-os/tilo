//! The `storage` resource: what [`tilo_system_resources::storage`] says it is, published with
//! the actions of [`StorageAction`].
//!
//! The scan reads the same roots the disk's storage policy does ([`tilo_storage_scan::scan_storage`])
//! and previews what clearing the apps' cache frees. A scan runs when the service starts and
//! whenever the resource is asked to scan again, never on every read of the machine.

use std::path::PathBuf;

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_storage_scan::{StorageCategory, StorageRoots};
use tilo_system_resources::storage::{
    CategoryUsage, DiskUsage, STORAGE, Storage, StorageAction, StoredItem,
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
    Storage {
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
