//! Truthful storage accounting and a preview-before-delete cache cleaner.
//!
//! Trees are measured with `symlink_metadata`; symbolic links are counted but
//! never followed. Cleanup can only target direct children of the configured
//! cache root and revalidates identity, metadata, and measured contents
//! immediately before removal.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageRoots {
    /// The package store root (`tilo-apps`); installed apps are read from
    /// its registry.
    pub app_payloads: Option<PathBuf>,
    pub room_data: Option<PathBuf>,
    pub app_cache: Option<PathBuf>,
    pub sync_replica: Option<PathBuf>,
}

impl StorageRoots {
    pub fn discover() -> Self {
        let data = xdg_paths::xdg::data_home();
        let cache = xdg_paths::xdg::cache_home();
        let payload_root = std::env::var_os("TILO_APPS_DATA_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| data.as_ref().map(|p| p.join("tilo-apps")));
        Self {
            app_payloads: payload_root,
            room_data: data.as_ref().map(|p| p.join("tilo-rooms")),
            app_cache: cache.map(|p| p.join("tilo-apps")),
            // The sync filesystem owns this directory. Until it publishes
            // hydration state, all bytes are truthfully labelled replica data,
            // never guessed as cache or reclaimable.
            sync_replica: data.map(|p| p.join("tilo-sync")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageCategory {
    InstalledApps,
    AppData,
    Cache,
    SyncedFiles,
}

impl StorageCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::InstalledApps => "Installed apps",
            Self::AppData => "App data",
            Self::Cache => "Cache",
            Self::SyncedFiles => "Synced files",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageEntry {
    pub category: StorageCategory,
    pub owner: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub reclaimable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageSnapshot {
    pub entries: Vec<StorageEntry>,
    pub unavailable: Vec<StorageCategory>,
}

impl StorageSnapshot {
    pub fn totals(&self) -> BTreeMap<StorageCategory, u64> {
        let mut out: BTreeMap<StorageCategory, u64> = BTreeMap::new();
        for entry in &self.entries {
            let slot = out.entry(entry.category).or_default();
            *slot = slot.saturating_add(entry.bytes);
        }
        out
    }

    pub fn reclaimable_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.reclaimable)
            .map(|e| e.bytes)
            .sum()
    }
}

pub fn scan_storage(roots: &StorageRoots) -> StorageSnapshot {
    let mut snapshot = StorageSnapshot::default();
    scan_installed_apps(&mut snapshot, roots.app_payloads.as_deref());
    scan_room_apps(&mut snapshot, roots.room_data.as_deref());
    scan_children(
        &mut snapshot,
        roots.app_cache.as_deref(),
        StorageCategory::Cache,
        true,
    );
    scan_children(
        &mut snapshot,
        roots.sync_replica.as_deref(),
        StorageCategory::SyncedFiles,
        false,
    );
    snapshot.entries.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.owner.cmp(&b.owner))
    });
    snapshot.unavailable.sort();
    snapshot.unavailable.dedup();
    snapshot
}

fn scan_children(
    snapshot: &mut StorageSnapshot,
    root: Option<&Path>,
    category: StorageCategory,
    reclaimable: bool,
) {
    let Some(root) = root else {
        snapshot.unavailable.push(category);
        return;
    };
    let Ok(children) = std::fs::read_dir(root) else {
        snapshot.unavailable.push(category);
        return;
    };
    for child in children.flatten() {
        let path = child.path();
        let (bytes, files) = measure(&path);
        snapshot.entries.push(StorageEntry {
            category,
            owner: child.file_name().to_string_lossy().into_owned(),
            path,
            bytes,
            files,
            reclaimable,
        });
    }
}

/// One entry per installed package, attributed by the package store's
/// registry: a tree is named by its package id, so only the registry knows
/// whose it is. Payload bytes shared between packages live once in the
/// store's content; each tree is measured as the app sees it.
fn scan_installed_apps(snapshot: &mut StorageSnapshot, root: Option<&Path>) {
    let Some(root) = root else {
        snapshot.unavailable.push(StorageCategory::InstalledApps);
        return;
    };
    let registry = root.join(package_store::layout::REGISTRY_FILE);
    let Ok(registry) = package_store::Registry::load(&registry) else {
        snapshot.unavailable.push(StorageCategory::InstalledApps);
        return;
    };
    for row in registry.into_entries() {
        let (bytes, files) = measure(&row.payload_root);
        snapshot.entries.push(StorageEntry {
            category: StorageCategory::InstalledApps,
            owner: row.id,
            path: row.payload_root,
            bytes,
            files,
            reclaimable: false,
        });
    }
}

fn scan_room_apps(snapshot: &mut StorageSnapshot, root: Option<&Path>) {
    let Some(root) = root else {
        snapshot.unavailable.push(StorageCategory::AppData);
        return;
    };
    let Ok(rooms) = std::fs::read_dir(root) else {
        snapshot.unavailable.push(StorageCategory::AppData);
        return;
    };
    let mut totals: BTreeMap<String, (u64, u64, PathBuf)> = BTreeMap::new();
    for room in rooms.flatten().filter(|e| e.path().is_dir()) {
        let Ok(apps) = std::fs::read_dir(room.path()) else {
            continue;
        };
        for app in apps.flatten() {
            let (bytes, files) = measure(&app.path());
            let owner = app.file_name().to_string_lossy().into_owned();
            let entry = totals.entry(owner).or_insert_with(|| (0, 0, app.path()));
            entry.0 = entry.0.saturating_add(bytes);
            entry.1 = entry.1.saturating_add(files);
        }
    }
    for (owner, (bytes, files, path)) in totals {
        snapshot.entries.push(StorageEntry {
            category: StorageCategory::AppData,
            owner,
            path,
            bytes,
            files,
            reclaimable: false,
        });
    }
}

fn measure(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (meta.len(), 1);
    }
    let Ok(children) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    children
        .flatten()
        .fold((0u64, 0u64), |(bytes, files), child| {
            let (b, f) = measure(&child.path());
            (bytes.saturating_add(b), files.saturating_add(f))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupItem {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub kind: CleanupKind,
    device: u64,
    inode: u64,
    modified_ns: i128,
    changed_ns: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupPreview {
    root: PathBuf,
    pub items: Vec<CleanupItem>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: usize,
    pub freed_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("cache root is unavailable")]
    Unavailable,
    #[error("refusing a cache root that is a symlink or not a directory")]
    UnsafeRoot,
    #[error("cleanup target changed after preview: {0}")]
    Changed(PathBuf),
    #[error("cleanup target escaped its cache root: {0}")]
    Escaped(PathBuf),
    #[error("cleanup failed for {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// # Errors
///
/// Returns [`CleanupError::Unavailable`] if `cache_root` can't be stat'd or
/// canonicalized, [`CleanupError::UnsafeRoot`] if it is not a plain
/// directory, or [`CleanupError::Io`] if listing its children or stat'ing
/// one fails.
pub fn preview_cache_cleanup(cache_root: &Path) -> Result<CleanupPreview, CleanupError> {
    let root_meta = std::fs::symlink_metadata(cache_root).map_err(|_| CleanupError::Unavailable)?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(CleanupError::UnsafeRoot);
    }
    let root = cache_root
        .canonicalize()
        .map_err(|_| CleanupError::Unavailable)?;
    let children = std::fs::read_dir(&root).map_err(|e| CleanupError::Io {
        path: root.clone(),
        source: e,
    })?;
    let mut items = Vec::new();
    for child in children {
        let child = child.map_err(|e| CleanupError::Io {
            path: root.clone(),
            source: e,
        })?;
        let path = child.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| CleanupError::Io {
            path: path.clone(),
            source: e,
        })?;
        let (bytes, files) = measure(&path);
        let kind = if meta.is_dir() {
            CleanupKind::Directory
        } else {
            CleanupKind::File
        };
        items.push(CleanupItem {
            path,
            bytes,
            files,
            kind,
            device: meta.dev(),
            inode: meta.ino(),
            modified_ns: i128::from(meta.mtime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(meta.mtime_nsec())),
            changed_ns: i128::from(meta.ctime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(meta.ctime_nsec())),
        });
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    let total_bytes = items.iter().map(|i| i.bytes).sum();
    Ok(CleanupPreview {
        root,
        items,
        total_bytes,
    })
}

/// # Errors
///
/// Returns [`CleanupError::UnsafeRoot`] if the cache root moved since the
/// preview, [`CleanupError::Escaped`] if a selected item is no longer a
/// direct child of it, [`CleanupError::Changed`] if a selected item's
/// identity or mtime no longer matches the preview, or [`CleanupError::Io`]
/// if removing it fails.
pub fn apply_cleanup(
    preview: &CleanupPreview,
    selected: &[usize],
) -> Result<CleanupReport, CleanupError> {
    let root_now = preview
        .root
        .canonicalize()
        .map_err(|_| CleanupError::UnsafeRoot)?;
    if root_now != preview.root {
        return Err(CleanupError::UnsafeRoot);
    }
    let mut report = CleanupReport::default();
    let mut unique = selected.to_vec();
    unique.sort_unstable();
    unique.dedup();
    for index in unique {
        let Some(item) = preview.items.get(index) else {
            continue;
        };
        if item.path.parent() != Some(preview.root.as_path()) {
            return Err(CleanupError::Escaped(item.path.clone()));
        }
        let meta = std::fs::symlink_metadata(&item.path)
            .map_err(|_| CleanupError::Changed(item.path.clone()))?;
        let modified_ns = i128::from(meta.mtime())
            .saturating_mul(1_000_000_000)
            .saturating_add(i128::from(meta.mtime_nsec()));
        let changed_ns = i128::from(meta.ctime())
            .saturating_mul(1_000_000_000)
            .saturating_add(i128::from(meta.ctime_nsec()));
        let (bytes, files) = measure(&item.path);
        if meta.dev() != item.device
            || meta.ino() != item.inode
            || modified_ns != item.modified_ns
            || changed_ns != item.changed_ns
            || (meta.is_dir()) != (item.kind == CleanupKind::Directory)
            || bytes != item.bytes
            || files != item.files
        {
            return Err(CleanupError::Changed(item.path.clone()));
        }
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(&item.path)
        } else {
            std::fs::remove_file(&item.path)
        };
        result.map_err(|e| CleanupError::Io {
            path: item.path.clone(),
            source: e,
        })?;
        report.removed = report.removed.saturating_add(1);
        report.freed_bytes = report.freed_bytes.saturating_add(item.bytes);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_is_per_app_and_does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("tilo-apps");
        let tree = store.join("pkgs").join("ab".repeat(32));
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("app"), vec![0u8; 20]).unwrap();
        std::fs::write(
            store.join("registry.toml"),
            format!(
                "[[entry]]\nid = \"so.tilo.notes\"\npayload_root = {:?}\nmanifest_path = {:?}\n",
                tree.display().to_string(),
                tree.display().to_string()
            ),
        )
        .unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, vec![0u8; 50_000]).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("link")).unwrap();
        let snap = scan_storage(&StorageRoots {
            app_payloads: Some(store),
            room_data: None,
            app_cache: None,
            sync_replica: None,
        });
        let app = snap
            .entries
            .iter()
            .find(|e| e.category == StorageCategory::InstalledApps)
            .unwrap();
        assert!(app.bytes < 1_000, "symlink target must not be counted");
        assert_eq!(app.owner, "so.tilo.notes");
    }

    #[test]
    fn cleanup_removes_only_selected_items_whose_preview_still_matches() {
        // Phase `cleanup_requires_preview_and_rejects_replaced_target`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir_all(cache.join("so.tilo.notes")).unwrap();
            std::fs::write(cache.join("so.tilo.notes/a"), b"cache").unwrap();
            let preview = preview_cache_cleanup(&cache).unwrap();
            std::fs::remove_dir_all(cache.join("so.tilo.notes")).unwrap();
            std::fs::create_dir(cache.join("so.tilo.notes")).unwrap();
            assert!(matches!(
                apply_cleanup(&preview, &[0]),
                Err(CleanupError::Changed(_))
            ));
        }
        // Phase `cleanup_rejects_directory_payload_changed_after_preview`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            let target = cache.join("so.tilo.notes");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("a"), b"cache").unwrap();
            let preview = preview_cache_cleanup(&cache).unwrap();

            std::fs::write(target.join("a"), b"larger cache payload").unwrap();

            assert!(matches!(
                apply_cleanup(&preview, &[0]),
                Err(CleanupError::Changed(_))
            ));
            assert!(target.exists(), "changed cache must not be deleted");
        }
        // Phase `cleanup_rejects_directory_file_count_changed_after_preview`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            let target = cache.join("so.tilo.notes");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("a"), b"cache").unwrap();
            let preview = preview_cache_cleanup(&cache).unwrap();

            std::fs::write(target.join("empty"), []).unwrap();

            assert!(matches!(
                apply_cleanup(&preview, &[0]),
                Err(CleanupError::Changed(_))
            ));
            assert!(target.exists(), "changed cache must not be deleted");
        }
        // Phase `only_selected_preview_items_are_removed`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(cache.join("a"), b"aaaa").unwrap();
            std::fs::write(cache.join("b"), b"bb").unwrap();
            let preview = preview_cache_cleanup(&cache).unwrap();
            let a = preview
                .items
                .iter()
                .position(|i| i.path.ends_with("a"))
                .unwrap();
            let report = apply_cleanup(&preview, &[a]).unwrap();
            assert_eq!(report.freed_bytes, 4);
            assert!(!cache.join("a").exists());
            assert!(cache.join("b").exists());
        }
    }
}
