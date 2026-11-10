//! A host's content, one store per workspace it serves.
//!
//! A host that serves workspaces keeps the bytes their records reference
//! and do not carry: a file a caller uploaded, what an app made in a record
//! the host runs, the files the workspace's members wrote, what a bot fetched
//! from outside. [`WorkspaceStores`] is the one place a host keeps them: a
//! directory per workspace, each a [`ContentStore`] bounded by the same
//! [`ContentLimits`], so one workspace's store never answers for another's.
//!
//! What bounds them all is the disk. A write is refused, with its reason
//! ([`ContentError::DiskLow`]), while the disk holding the stores has less
//! free than [`keep_free`] of its size: the host's own state (its records,
//! journals and keys) needs that room, and refusing new content keeps them
//! writable. Nothing kept is dropped for room. The host logs an error the
//! moment its disk runs low, and again when it has room once more.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::{
    Collected, ContentBackend, ContentError, ContentLimits, ContentReader, ContentState,
    ContentStore, DirBackend, Stored,
};

/// The most a host keeps free on a small disk for its own state: a tenth of
/// the disk, up to this.
const KEEP_FREE_SMALL_DISK: u64 = 1024 * 1024 * 1024;
/// The share of the disk, in hundredths, a host keeps free when that is more.
const KEEP_FREE_PERCENT: u64 = 2;

/// Each workspace's content store under one directory, by the workspace's
/// id.
pub struct WorkspaceStores {
    dir: PathBuf,
    limits: ContentLimits,
    stores: Mutex<BTreeMap<String, Kept>>,
    /// Whether the disk was found low at the last write: the error is logged
    /// once when it runs low, not at every refusal.
    low: Arc<AtomicBool>,
}

impl WorkspaceStores {
    /// The stores under `dir`, each bounded by `limits`.
    #[must_use]
    pub fn in_dir(dir: impl Into<PathBuf>, limits: ContentLimits) -> Self {
        Self {
            dir: dir.into(),
            limits,
            stores: Mutex::default(),
            low: Arc::default(),
        }
    }

    /// The content store of `workspace`: one store per workspace, however
    /// often it is asked for, so its limits hold across everyone who keeps
    /// content there.
    ///
    /// # Errors
    ///
    /// `workspace` is not a workspace's id (a UUID).
    pub fn get(&self, workspace: &str) -> Result<ContentStore, String> {
        Ok(self.kept(workspace)?.store)
    }

    /// Remove from `workspace`'s store what nothing in `live` names and was
    /// last written before `before`: content the workspace no longer
    /// references, once whoever wrote it had time to reference it.
    ///
    /// # Errors
    ///
    /// `workspace` is not a workspace's id, or its store could not be read or
    /// changed.
    pub fn collect(
        &self,
        workspace: &str,
        live: &BTreeSet<String>,
        before: SystemTime,
    ) -> Result<Collected, String> {
        self.kept(workspace)?
            .dir
            .collect_unreferenced_before(live, before)
            .map_err(|error| error.to_string())
    }

    /// The workspaces this host keeps content of, by id.
    #[must_use]
    pub fn workspaces(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (workspace_dir(&name).as_deref() == Some(name.as_str())).then_some(name)
            })
            .collect()
    }

    fn kept(&self, workspace: &str) -> Result<Kept, String> {
        let workspace = workspace_dir(workspace).ok_or("the workspace is not a uuid")?;
        let kept = self
            .stores
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace)
            .or_insert_with_key(|workspace| {
                let dir = Arc::new(DirBackend::with_limits(
                    self.dir.join(workspace),
                    self.limits,
                ));
                Kept {
                    store: ContentStore::new(RoomChecked {
                        inner: Arc::clone(&dir),
                        disk: self.dir.clone(),
                        low: Arc::clone(&self.low),
                    }),
                    dir,
                }
            })
            .clone();
        Ok(kept)
    }

    /// The limits every workspace's store is bounded by.
    #[must_use]
    pub const fn limits(&self) -> ContentLimits {
        self.limits
    }
}

/// One workspace's store, and the directory it keeps its content in.
#[derive(Clone)]
struct Kept {
    store: ContentStore,
    dir: Arc<DirBackend>,
}

/// What a host keeps free on a disk of `total` bytes: a tenth of a small
/// disk, a gigabyte of a larger one, and a fiftieth of one so large that is
/// more.
#[must_use]
pub fn keep_free(total: u64) -> u64 {
    let share = (total / 100).saturating_mul(KEEP_FREE_PERCENT);
    share.max((total / 10).min(KEEP_FREE_SMALL_DISK))
}

/// The free and total bytes of the disk holding `path`, or of the nearest
/// directory above it that exists; `None` where that cannot be read.
fn disk_of(path: &Path) -> Option<(u64, u64)> {
    let existing = path.ancestors().find(|dir| dir.exists())?;
    room_at(existing)
}

#[cfg(unix)]
fn room_at(path: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let block = stat.f_frsize;
    Some((
        stat.f_bavail.saturating_mul(block),
        stat.f_blocks.saturating_mul(block),
    ))
}

#[cfg(not(unix))]
fn room_at(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// A workspace's store that keeps nothing new while the disk is low.
struct RoomChecked {
    inner: Arc<DirBackend>,
    /// Where the stores are: the disk whose room counts.
    disk: PathBuf,
    low: Arc<AtomicBool>,
}

impl RoomChecked {
    /// Whether the disk has room for new content, saying so in the log as it
    /// runs low and as it has room again.
    fn room(&self) -> Result<(), ContentError> {
        // A disk whose room cannot be read is not refused for it: the write
        // itself says when there is no room at all.
        let Some((free, total)) = disk_of(&self.disk) else {
            return Ok(());
        };
        let keep_free = keep_free(total);
        if free < keep_free {
            if !self.low.swap(true, Ordering::AcqRel) {
                tracing::error!(
                    disk = %self.disk.display(),
                    free,
                    keep_free,
                    "the disk holding the host's content is nearly full: \
                     no new content is kept until there is room, and nothing kept is dropped"
                );
            }
            return Err(ContentError::DiskLow { free, keep_free });
        }
        if self.low.swap(false, Ordering::AcqRel) {
            tracing::warn!(
                disk = %self.disk.display(),
                free,
                "the disk holding the host's content has room again"
            );
        }
        Ok(())
    }
}

impl ContentBackend for RoomChecked {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        self.room()?;
        self.inner.put(reader, mime)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        self.inner.open(hash)
    }

    fn has(&self, hash: &str) -> bool {
        self.inner.has(hash)
    }

    fn state(&self, hash: &str) -> ContentState {
        self.inner.state(hash)
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        self.inner.collect_unreferenced(live)
    }

    fn release(&self, hash: &str) -> Result<Collected, ContentError> {
        self.inner.release(hash)
    }
}

/// `workspace` as the name of its directory: a UUID, lower-case and
/// hyphenated, whichever way it was written. Anything else names no
/// directory.
fn workspace_dir(workspace: &str) -> Option<String> {
    let digits: String = workspace.chars().filter(|c| *c != '-').collect();
    if digits.len() != 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits = digits.to_ascii_lowercase();
    let part = |from: usize, to: usize| digits.get(from..to).unwrap_or_default();
    Some(format!(
        "{}-{}-{}-{}-{}",
        part(0, 8),
        part(8, 12),
        part(12, 16),
        part(16, 20),
        part(20, 32)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

    #[test]
    fn each_workspace_keeps_its_own_content() {
        let dir = std::env::temp_dir().join(format!("tilo-content-ws-{}", std::process::id()));
        let stores = WorkspaceStores::in_dir(&dir, ContentLimits::default());
        let hash = stores.get(WORKSPACE).unwrap().put(b"the report").unwrap();
        assert_eq!(
            stores
                .get(&WORKSPACE.to_uppercase().replace('-', ""))
                .unwrap()
                .read(&hash),
            Some(b"the report".to_vec()),
            "a workspace is one store however its id is written"
        );
        let other = stores.get("00000000-0000-4000-8000-000000000000").unwrap();
        assert_eq!(
            other.read(&hash),
            None,
            "one workspace's store answers for no other"
        );
        assert!(stores.get("../elsewhere").is_err());
        stores.get(WORKSPACE).unwrap().release(&hash).unwrap();
        assert_eq!(
            stores.get(WORKSPACE).unwrap().read(&hash),
            None,
            "a released item is gone"
        );
        assert!(stores.get("").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_workspace_lets_go_of_what_it_no_longer_references_once_its_writer_had_time() {
        let dir =
            std::env::temp_dir().join(format!("tilo-content-ws-collect-{}", std::process::id()));
        let stores = WorkspaceStores::in_dir(&dir, ContentLimits::default());
        let store = stores.get(WORKSPACE).unwrap();
        let named = store.put(b"a file the tree names").unwrap();
        let replaced = store.put(b"the version it replaced").unwrap();
        let live = BTreeSet::from([named.clone()]);

        let long_ago = SystemTime::UNIX_EPOCH;
        let collected = stores.collect(WORKSPACE, &live, long_ago).unwrap();
        assert!(
            collected.hashes.is_empty(),
            "content written since is spared: its writer may not have named it yet"
        );
        assert!(store.has(&replaced));

        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        let collected = stores.collect(WORKSPACE, &live, later).unwrap();
        assert_eq!(collected.hashes, std::slice::from_ref(&replaced));
        assert!(store.has(&named), "what the workspace names stays");
        assert!(!store.has(&replaced));
        assert_eq!(stores.workspaces(), [WORKSPACE]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writing_content_already_kept_spares_it_from_collection() {
        let dir =
            std::env::temp_dir().join(format!("tilo-content-ws-rewrite-{}", std::process::id()));
        let stores = WorkspaceStores::in_dir(&dir, ContentLimits::default());
        let store = stores.get(WORKSPACE).unwrap();
        let hash = store.put(b"uploaded again").unwrap();
        let path = dir.join(WORKSPACE).join(hash.get(..2).unwrap()).join(&hash);
        std::fs::File::options()
            .append(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        let a_while_ago = SystemTime::now() - std::time::Duration::from_secs(60);
        assert_eq!(store.put(b"uploaded again").unwrap(), hash);
        let collected = stores
            .collect(WORKSPACE, &BTreeSet::new(), a_while_ago)
            .unwrap();
        assert!(
            collected.hashes.is_empty(),
            "an upload of content already here is a fresh write: its record may name it soon"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_host_keeps_room_free_in_proportion_to_its_disk() {
        assert_eq!(keep_free(256 * 1024 * 1024), 256 * 1024 * 1024 / 10);
        assert_eq!(keep_free(20 * 1024 * 1024 * 1024), KEEP_FREE_SMALL_DISK);
        assert_eq!(keep_free(1_000_000_000_000), 20_000_000_000);
    }

    #[test]
    fn a_workspace_is_bounded_by_its_limits() {
        let dir =
            std::env::temp_dir().join(format!("tilo-content-ws-limit-{}", std::process::id()));
        let limits = ContentLimits {
            max_total_bytes: 16,
            ..ContentLimits::default()
        };
        let stores = WorkspaceStores::in_dir(&dir, limits);
        let store = stores.get(WORKSPACE).unwrap();
        store.put(b"twelve bytes").unwrap();
        assert!(
            store.put(b"and twelve more").is_err(),
            "past its limit a workspace keeps no more"
        );
        assert!(
            stores
                .get("00000000-0000-4000-8000-000000000000")
                .unwrap()
                .put(b"twelve bytes")
                .is_ok(),
            "another workspace has its own room"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
