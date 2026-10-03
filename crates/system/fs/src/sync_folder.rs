//! A plain folder mirroring the workspace both ways, for hosts where the
//! workspace is not mounted: stock macOS, or a Linux desktop without FUSE.
//!
//! The folder is an ordinary directory any program on the host can use.
//! The workspace stays the authority. A change made in the folder enters it
//! through the path-level operations the mount and the native data plane
//! use ([`QuickFS::write_path`], [`QuickFS::apply_local_move`], ...), so it
//! becomes a version in the file's history like any other write, and a
//! document is imported through its format's codec rather than stored as
//! opaque bytes. A change in the workspace reaches the folder as a complete
//! file renamed into place, a document exported in its legacy format.
//!
//! A pass ([`SyncFolder::reconcile`]) compares both sides with what the
//! last pass left them as (the baseline) and carries each change across:
//!
//! - a path changed on one side only is copied to the other side, or
//!   removed there;
//! - a path changed on both sides keeps the folder's version, as the newest
//!   version in the workspace; the workspace's version stays in its history;
//! - an edit wins over a removal on the other side, so nothing is lost;
//! - a rename in the folder (the same inode at a new path) is a rename in
//!   the workspace, so the record keeps its identity and history; a rename
//!   in the workspace (the same record at a new path) is one in the folder.
//!
//! An empty workspace never empties the folder: when the workspace lost
//! everything the baseline knew, the folder is imported again instead.
//!
//! [`SyncFolder::watch`] runs passes on change: `FSEvents` on macOS, `inotify`
//! on Linux, and a periodic pass that picks up workspace changes.

mod baseline;
mod local;
mod watch;

#[cfg(test)]
mod tests;

pub use local::STAGING_PREFIX;
pub use watch::Watch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use baseline::{Baseline, Mark, Synced};
use local::LocalEntry;

use crate::fs::QuickFS;

/// Where a folder keeps its baseline, in its state directory.
const BASELINE: &str = "sync-folder.json";
/// Where an import stages the file it reads, in the state directory.
const STAGING: &str = "sync-folder.staging";

/// What one pass carried across.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    /// Paths written from the folder into the workspace.
    pub imported: usize,
    /// Paths written from the workspace into the folder.
    pub exported: usize,
    /// Paths removed on one side because they were removed on the other.
    pub removed: usize,
    /// Renames carried across.
    pub renamed: usize,
}

/// Persists what a pass changed in the workspace; runs before the baseline
/// is saved, so a baseline never describes workspace changes a crash lost.
pub type Commit = Box<dyn Fn() -> io::Result<()> + Send>;

/// One workspace path as the folder sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteEntry {
    mark: Mark,
    record: Option<[u8; 16]>,
    mtime_ms: u64,
}

impl RemoteEntry {
    const fn dir(&self) -> bool {
        matches!(self.mark, Mark::Dir)
    }
}

/// What a pass does with one path.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Keep,
    Import,
    Export,
    RemoveLocal,
    RemoveRemote,
    Forget,
}

fn decide(local: Option<&LocalEntry>, remote: Option<&RemoteEntry>, base: Option<&Synced>) -> Step {
    let local_changed = match (local, base) {
        (Some(l), Some(b)) => l.dir != b.dir || (!l.dir && l.stamp != b.local),
        (None, None) => false,
        _ => true,
    };
    let remote_changed = match (remote, base) {
        (Some(r), Some(b)) => r.mark != b.remote,
        (None, None) => false,
        _ => true,
    };
    match (local, remote) {
        (None, None) => Step::Forget,
        (Some(_), None) if base.is_some() && !local_changed => Step::RemoveLocal,
        (Some(_), None) => Step::Import,
        (None, Some(_)) if base.is_some() && !remote_changed => Step::RemoveRemote,
        (None, Some(_)) => Step::Export,
        (Some(l), Some(r)) => match (local_changed, remote_changed) {
            (false, false) => Step::Keep,
            (false, true) => Step::Export,
            (true, true) if l.dir && r.dir() => Step::Keep,
            (true, _) => Step::Import,
        },
    }
}

fn rel(path: &str) -> &Path {
    Path::new(path)
}

fn errno(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// A folder kept in step with a workspace.
pub struct SyncFolder {
    fs: Arc<QuickFS>,
    root: PathBuf,
    staging: PathBuf,
    baseline: Baseline,
    commit: Commit,
}

impl SyncFolder {
    /// The folder at `root` (created when missing) over the workspace `fs`,
    /// keeping its baseline in `state`. `commit` persists the workspace.
    ///
    /// # Errors
    ///
    /// The folder cannot be created, or the baseline cannot be read.
    pub fn open(fs: Arc<QuickFS>, root: &Path, state: &Path, commit: Commit) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        std::fs::create_dir_all(state)?;
        Ok(Self {
            fs,
            root: root.to_path_buf(),
            staging: state.join(STAGING),
            baseline: Baseline::open(&state.join(BASELINE))?,
            commit,
        })
    }

    /// The folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Carries every change since the last pass across, in both directions.
    ///
    /// # Errors
    ///
    /// The folder cannot be read, or the workspace or the baseline cannot
    /// be persisted. A path that cannot be carried across is left for the
    /// next pass and does not fail this one.
    pub fn reconcile(&mut self) -> io::Result<Pass> {
        let mut pass = Pass::default();
        let mut local = local::scan(&self.root)?;
        let mut remote = self.remote_view();
        if remote.is_empty() && !self.baseline.entries.is_empty() {
            tracing::warn!(
                folder = %self.root.display(),
                "the workspace lost every synced file; importing the folder again rather than emptying it"
            );
            self.baseline.entries.clear();
        }
        if self.follow_renames(&local, &remote, &mut pass)? {
            local = local::scan(&self.root)?;
            remote = self.remote_view();
        }
        let paths: BTreeSet<String> = local
            .keys()
            .chain(remote.keys())
            .chain(self.baseline.entries.keys())
            .cloned()
            .collect();
        let mut next = BTreeMap::new();
        let mut removals = Vec::new();
        for path in paths {
            let (l, r) = (local.get(&path), remote.get(&path));
            let base = self.baseline.entries.get(&path).copied();
            let synced = match (decide(l, r, base.as_ref()), l, r) {
                (Step::Keep, Some(l), Some(r)) => Some(Synced {
                    dir: l.dir,
                    record: r.record,
                    remote: r.mark,
                    local: l.stamp,
                }),
                (Step::Import, Some(l), _) => {
                    let imported = self.import(&path, l);
                    self.carried(&path, imported, &mut pass.imported).or(base)
                }
                (Step::Export, _, Some(r)) => {
                    let exported = self.export(&path, l, r);
                    self.carried(&path, exported, &mut pass.exported).or(base)
                }
                (Step::RemoveLocal | Step::RemoveRemote, ..) => {
                    removals.extend(base.map(|base| (path.clone(), l.is_some(), base)));
                    None
                }
                _ => None,
            };
            if let Some(synced) = synced {
                next.insert(path, synced);
            }
        }
        removals.sort_by_key(|(path, _, _)| std::cmp::Reverse(depth(path)));
        let removed_remotely: BTreeSet<&str> = removals
            .iter()
            .filter(|(_, in_folder, _)| !in_folder)
            .map(|(path, _, _)| path.as_str())
            .collect();
        for (path, in_folder, base) in &removals {
            let removed = if *in_folder {
                self.remove_local(path, base)
            } else {
                self.remove_remote(path, &remote, &removed_remotely)
            };
            match removed {
                Ok(true) => pass.removed = pass.removed.saturating_add(1),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, path, "could not carry a removal across; retrying");
                    next.insert(path.clone(), *base);
                }
            }
        }
        self.baseline.entries = next;
        (self.commit)()?;
        self.baseline.save()?;
        Ok(pass)
    }

    /// Counts a path carried across; a failure is logged and leaves the
    /// path to the next pass.
    fn carried(
        &self,
        path: &str,
        outcome: io::Result<Option<Synced>>,
        count: &mut usize,
    ) -> Option<Synced> {
        match outcome {
            Ok(Some(synced)) => {
                *count = count.saturating_add(1);
                Some(synced)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, path, folder = %self.root.display(), "could not sync; retrying");
                None
            }
        }
    }

    /// Every workspace path the folder can hold, with its current mark.
    /// Content this device does not know yet, and symbolic links, are left
    /// out until they can be written as files.
    fn remote_view(&self) -> BTreeMap<String, RemoteEntry> {
        let mut out = BTreeMap::new();
        for entry in self.fs.tree_snapshot() {
            if !entry.known || entry.posix.symlink {
                continue;
            }
            let Ok(names) = entry
                .path
                .iter()
                .map(|name| std::str::from_utf8(name))
                .collect::<Result<Vec<_>, _>>()
            else {
                continue;
            };
            let path = names.join("/");
            let mark = if entry.dir {
                Mark::Dir
            } else if self.fs.document_at(rel(&path)) {
                match self.fs.read_path(rel(&path)) {
                    Ok(bytes) => Mark::Document {
                        digest: *blake3::hash(&bytes).as_bytes(),
                    },
                    Err(_) => continue,
                }
            } else {
                match entry.hash {
                    Some(hash) => Mark::Content {
                        hash: *hash.as_bytes(),
                        size: entry.size,
                    },
                    None => Mark::Empty,
                }
            };
            out.insert(
                path,
                RemoteEntry {
                    mark,
                    record: entry.record.map(|record| *record.0.as_bytes()),
                    mtime_ms: entry.mtime_ms,
                },
            );
        }
        out
    }

    fn remote_entry(&self, path: &str) -> Option<RemoteEntry> {
        let entry = self.fs.entry_at(rel(path))?;
        let mark = if entry.dir {
            Mark::Dir
        } else if self.fs.document_at(rel(path)) {
            Mark::Document {
                digest: *blake3::hash(&self.fs.read_path(rel(path)).ok()?).as_bytes(),
            }
        } else {
            entry.hash.map_or(Mark::Empty, |hash| Mark::Content {
                hash: *hash.as_bytes(),
                size: entry.size,
            })
        };
        Some(RemoteEntry {
            mark,
            record: entry.record.map(|record| *record.0.as_bytes()),
            mtime_ms: entry.mtime_ms,
        })
    }

    /// Carries renames across before anything else, so a renamed file
    /// keeps its record rather than being removed and created again.
    /// Returns whether any was.
    fn follow_renames(
        &mut self,
        local: &BTreeMap<String, LocalEntry>,
        remote: &BTreeMap<String, RemoteEntry>,
        pass: &mut Pass,
    ) -> io::Result<bool> {
        let fresh_local: HashMap<(u64, u64), &str> = local
            .iter()
            .filter(|(path, _)| !self.baseline.entries.contains_key(*path))
            .map(|(path, entry)| ((entry.stamp.dev, entry.stamp.ino), path.as_str()))
            .collect();
        let remote_by_record: HashMap<[u8; 16], &str> = remote
            .iter()
            .filter_map(|(path, entry)| Some((entry.record?, path.as_str())))
            .collect();
        let before = pass.renamed;
        let known: Vec<String> = self.baseline.entries.keys().cloned().collect();
        for from in known {
            // Gone when a directory above it was renamed with its subtree.
            let Some(base) = self.baseline.entries.get(&from).copied() else {
                continue;
            };
            if local.contains_key(&from) {
                if let Some(to) = renamed_remotely(&from, &base, local, remote, &remote_by_record) {
                    let target = self.root.join(to);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(self.root.join(&from), target)?;
                    self.baseline.rename(&from, to);
                    pass.renamed = pass.renamed.saturating_add(1);
                }
                continue;
            }
            let Some(&to) = fresh_local.get(&(base.local.dev, base.local.ino)) else {
                continue;
            };
            let unchanged_remotely = remote.get(&from).is_some_and(|r| r.mark == base.remote);
            if local.get(to).is_some_and(|l| l.dir == base.dir)
                && unchanged_remotely
                && !remote.contains_key(to)
            {
                match self.fs.apply_local_move(rel(&from), rel(to)) {
                    Ok(()) => {
                        self.baseline.rename(&from, to);
                        pass.renamed = pass.renamed.saturating_add(1);
                    }
                    // Carried as a removal and a creation instead, which
                    // loses the record's identity but never its content.
                    Err(code) => tracing::warn!(error = %errno(code), from, to, "rename not kept"),
                }
            }
        }
        Ok(pass.renamed > before)
    }

    /// Writes the folder's `path` into the workspace. `Ok(None)` when it
    /// changed while being read; the next pass takes it again.
    fn import(&self, path: &str, local: &LocalEntry) -> io::Result<Option<Synced>> {
        if local.dir {
            self.fs.apply_local_dir(rel(path)).map_err(errno)?;
            return self.synced_with(path, true, local.stamp);
        }
        let _ = std::fs::remove_file(&self.staging);
        let written = self.import_file(path);
        let _ = std::fs::remove_file(&self.staging);
        match written? {
            Some(stamp) => self.synced_with(path, false, stamp),
            None => Ok(None),
        }
    }

    /// Writes one version of the folder's file `path` into the workspace
    /// and returns the stamp of that version.
    fn import_file(&self, path: &str) -> io::Result<Option<local::Stamp>> {
        let Some(stamp) = local::snapshot(&self.root.join(path), &self.staging)? else {
            return Ok(None);
        };
        match self
            .fs
            .write_path(rel(path), &self.staging, stamp.mtime_ms())
        {
            Ok(()) => Ok(Some(stamp)),
            Err(libc::EINVAL) => {
                // The workspace keeps its version and the folder the bytes
                // its format refused, until either changes again.
                tracing::warn!(path, "the document's format refused the folder's version");
                Ok(Some(stamp))
            }
            Err(code) => Err(errno(code)),
        }
    }

    fn synced_with(
        &self,
        path: &str,
        dir: bool,
        stamp: local::Stamp,
    ) -> io::Result<Option<Synced>> {
        let remote = self.remote_entry(path).ok_or_else(|| errno(libc::ENOENT))?;
        Ok(Some(Synced {
            dir,
            record: remote.record,
            remote: remote.mark,
            local: stamp,
        }))
    }

    /// Writes the workspace's `path` into the folder, unless the folder's
    /// copy changed since it was scanned. `Ok(None)` when it did.
    fn export(
        &self,
        path: &str,
        scanned: Option<&LocalEntry>,
        remote: &RemoteEntry,
    ) -> io::Result<Option<Synced>> {
        let target = self.root.join(path);
        let now = local::stat(&target)?;
        if now.as_ref() != scanned {
            return Ok(None);
        }
        if let Some(current) = now
            && current.dir != remote.dir()
        {
            // A kind change replaces only what holds nothing.
            if current.dir {
                std::fs::remove_dir(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
        }
        let stamp = if remote.dir() {
            local::create_dir(&target)?
        } else {
            let bytes = self.fs.read_path(rel(path)).map_err(errno)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            local::write_atomically(&target, &bytes, remote.mtime_ms)?
        };
        Ok(Some(Synced {
            dir: remote.dir(),
            record: remote.record,
            remote: remote.mark,
            local: stamp,
        }))
    }

    /// Removes the folder's `path`, which the workspace removed, unless it
    /// changed since the last pass. A directory goes only once empty.
    fn remove_local(&self, path: &str, base: &Synced) -> io::Result<bool> {
        let target = self.root.join(path);
        let Some(now) = local::stat(&target)? else {
            return Ok(false);
        };
        if now.dir != base.dir || (!now.dir && now.stamp != base.local) {
            return Ok(false);
        }
        if !now.dir {
            std::fs::remove_file(&target)?;
            return Ok(true);
        }
        local::remove_host_files(&target)?;
        match std::fs::remove_dir(&target) {
            Ok(()) => Ok(true),
            // It holds what the folder added since; the next pass imports it.
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Removes the workspace's `path`, which the folder removed. A
    /// directory goes only when everything below it goes in this pass too.
    fn remove_remote(
        &self,
        path: &str,
        remote: &BTreeMap<String, RemoteEntry>,
        removed: &BTreeSet<&str>,
    ) -> io::Result<bool> {
        let keeps_children = remote
            .keys()
            .any(|child| Synced::below(child, path) && !removed.contains(child.as_str()));
        if keeps_children {
            return Ok(false);
        }
        self.fs.apply_local_remove(rel(path)).map_err(errno)?;
        Ok(true)
    }
}

/// Where the workspace moved the record synced at `from`, when the
/// folder can follow: the folder's copy is unchanged and nothing stands
/// at the new path.
fn renamed_remotely<'a>(
    from: &str,
    base: &Synced,
    local: &BTreeMap<String, LocalEntry>,
    remote: &BTreeMap<String, RemoteEntry>,
    remote_by_record: &HashMap<[u8; 16], &'a str>,
) -> Option<&'a str> {
    let to = *remote_by_record.get(&base.record?)?;
    let unchanged_locally = local
        .get(from)
        .is_some_and(|l| l.dir == base.dir && (l.dir || l.stamp == base.local));
    (to != from && !remote.contains_key(from) && unchanged_locally && !local.contains_key(to))
        .then_some(to)
}
