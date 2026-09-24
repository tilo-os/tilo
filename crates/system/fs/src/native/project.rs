//! Materialising the record plane into the live tree.
//!
//! Remote versions reach the live tree only through here, and only in ways
//! a reader can never observe half-done (`storage.md` §6.5, §10):
//!
//! - a new or changed small file is written to a staging file in the same
//!   directory and renamed over its path, so an open reader keeps the old
//!   inode and a new open sees the complete new version;
//! - a large file becomes a placeholder: exact size and mtime, no bytes,
//!   filled range by range on first access through the pre-content group;
//! - directories are created before anything inside them, files follow in
//!   the order their versions were made (the author's commit time), and
//!   removals come last, deepest first.
//!
//! A path the local user changed since the daemon last wrote it is never
//! overwritten: the change is captured and published as a local version,
//! and the record plane's conflict handling takes it from there.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use fs_native::hydrate::{Hydrator as Placeholders, PlaceholderSpec, STAGING_PREFIX};

use super::state::{LocalStat, Materialized};
use crate::fs::native_ops::TreeEntry;
use crate::hydration::Hydrator;

const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

pub fn rel_path(components: &[Vec<u8>]) -> PathBuf {
    components
        .iter()
        .map(|c| OsString::from_vec(c.clone()))
        .collect()
}

/// What one projection pass did.
#[derive(Debug, Default)]
pub struct Projection {
    /// Paths written or created (files, placeholders, directories).
    pub written: Vec<PathBuf>,
    /// Paths removed from the live tree.
    pub removed: Vec<PathBuf>,
    /// Paths left alone because they changed locally since the daemon wrote
    /// them.
    pub skipped_dirty: Vec<PathBuf>,
    pub updates: Vec<(PathBuf, Option<Materialized>)>,
    pub files: usize,
    /// A version this device cannot verify yet stopped the pass; the rest
    /// follows once it arrives.
    pub waiting: bool,
    pub waiting_for: Option<PathBuf>,
    pub placeholders: usize,
    pub errors: Vec<String>,
}

pub struct Projector {
    live: PathBuf,
    placeholders: Arc<Placeholders>,
    content: Arc<Hydrator>,
    runtime: tokio::runtime::Handle,
    /// Files up to this size are fetched whole and written before they
    /// appear; larger ones start as placeholders.
    pub eager_bytes: u64,
    /// How long an eager fetch may take before the file falls back to a
    /// placeholder.
    pub eager_deadline: Duration,
    pub parallel_chunks: usize,
    owner: Option<(u32, u32)>,
    seq: AtomicU64,
    /// Files whose writers are refused (another device holds the lease).
    /// A new version of one is gated before it is renamed into place, so
    /// there is no instant at which a writer could get through.
    write_denied: Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    last_unavailable: std::sync::Mutex<Option<String>>,
}

fn mtime_of(ms: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_millis(ms))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

impl Projector {
    pub fn new(
        live: PathBuf,
        placeholders: Arc<Placeholders>,
        content: Arc<Hydrator>,
        runtime: tokio::runtime::Handle,
        owner: Option<(u32, u32)>,
        write_denied: Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    ) -> Self {
        Self {
            live,
            placeholders,
            content,
            runtime,
            eager_bytes: 1024 * 1024,
            eager_deadline: Duration::from_secs(10),
            parallel_chunks: super::fetch::DEFAULT_PARALLEL_CHUNKS,
            owner,
            seq: AtomicU64::new(0),
            write_denied,
            last_unavailable: std::sync::Mutex::new(None),
        }
    }

    fn chown(&self, path: &Path) -> io::Result<()> {
        if let Some((uid, gid)) = self.owner {
            std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
        }
        Ok(())
    }

    /// Brings the live tree to `tree`, the record plane's current state.
    pub fn project(
        &self,
        tree: &[TreeEntry],
        materialized: &HashMap<PathBuf, Materialized>,
    ) -> Projection {
        let mut out = Projection::default();
        let desired: HashSet<PathBuf> = tree.iter().map(|e| rel_path(&e.path)).collect();

        let mut dirs: Vec<&TreeEntry> = tree.iter().filter(|e| e.dir).collect();
        dirs.sort_by_key(|e| e.path.len());
        for e in dirs {
            let rel = rel_path(&e.path);
            if let Err(error) = self.project_dir(&rel, e, materialized, &mut out) {
                out.errors.push(format!("{}: {error}", rel.display()));
            }
        }

        let mut files: Vec<&TreeEntry> = tree.iter().filter(|e| !e.dir).collect();
        files.sort_by(|a, b| {
            a.mtime_ms
                .cmp(&b.mtime_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        for e in files {
            if !e.known {
                // Content revisions arrive per record, in no particular
                // order. Nothing made after a version this device cannot
                // verify yet is shown before it: the next pass continues.
                out.waiting = true;
                out.waiting_for = Some(rel_path(&e.path));
                break;
            }
            let rel = rel_path(&e.path);
            match self.project_file(&rel, e, materialized, &mut out) {
                Ok(true) => {}
                Ok(false) => {
                    // Its bytes are not reachable right now; what follows it
                    // waits, as for an unverified version.
                    out.waiting = true;
                    out.waiting_for = Some(rel);
                    break;
                }
                Err(error) => out.errors.push(format!("{}: {error}", rel.display())),
            }
        }
        if out.waiting {
            return out;
        }

        let mut gone: Vec<(&PathBuf, &Materialized)> = materialized
            .iter()
            .filter(|(p, _)| !desired.contains(*p))
            .collect();
        gone.sort_by_key(|(p, m)| (std::cmp::Reverse(p.components().count()), m.dir));
        for (rel, m) in gone {
            self.project_removal(rel, m, &mut out);
        }
        out
    }

    fn project_dir(
        &self,
        rel: &Path,
        e: &TreeEntry,
        materialized: &HashMap<PathBuf, Materialized>,
        out: &mut Projection,
    ) -> io::Result<()> {
        let abs = self.live.join(rel);
        let record = Materialized {
            dir: true,
            hash: None,
            size: 0,
            mtime_ms: e.mtime_ms,
            placeholder: false,
            local: None,
        };
        match fs::symlink_metadata(&abs) {
            Ok(meta) if meta.is_dir() => {
                if !materialized.contains_key(rel) {
                    out.updates.push((rel.to_path_buf(), Some(record)));
                }
                Ok(())
            }
            Ok(_) => {
                // A file stands where the record plane now has a directory.
                let ours = materialized
                    .get(rel)
                    .is_some_and(|m| m.local.is_some() && m.local == LocalStat::of(&abs));
                if !ours {
                    out.skipped_dirty.push(rel.to_path_buf());
                    return Ok(());
                }
                fs::remove_file(&abs)?;
                self.make_dir(&abs)?;
                out.written.push(rel.to_path_buf());
                out.updates.push((rel.to_path_buf(), Some(record)));
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)?;
                }
                self.make_dir(&abs)?;
                out.written.push(rel.to_path_buf());
                out.updates.push((rel.to_path_buf(), Some(record)));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn make_dir(&self, abs: &Path) -> io::Result<()> {
        match fs::create_dir(abs) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(e),
        }
        fs::set_permissions(abs, fs::Permissions::from_mode(DIR_MODE))?;
        self.chown(abs)
    }

    fn project_file(
        &self,
        rel: &Path,
        e: &TreeEntry,
        materialized: &HashMap<PathBuf, Materialized>,
        out: &mut Projection,
    ) -> io::Result<bool> {
        let hash = e.hash.map(|h| *h.as_bytes());
        let previous = materialized.get(rel);
        if previous.is_some_and(|m| !m.dir && m.hash == hash && m.size == e.size) {
            return Ok(true);
        }
        let abs = self.live.join(rel);
        let local = LocalStat::of(&abs);
        let dirty = match (local, previous) {
            (None, _) => false,
            (Some(now), Some(m)) => m.local != Some(now),
            // A local file the daemon never wrote: the capture publishes it
            // (or finds it identical) before anything replaces it.
            (Some(_), None) => true,
        };
        if dirty {
            out.skipped_dirty.push(rel.to_path_buf());
            return Ok(true);
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        let Some(placeholder) = self.materialize(&abs, hash, e, local.is_some())? else {
            return Ok(false);
        };
        if placeholder {
            out.placeholders = out.placeholders.saturating_add(1);
        } else {
            out.files = out.files.saturating_add(1);
        }
        out.written.push(rel.to_path_buf());
        out.updates.push((
            rel.to_path_buf(),
            Some(Materialized {
                dir: false,
                hash,
                size: e.size,
                mtime_ms: e.mtime_ms,
                placeholder,
                local: LocalStat::of(&abs),
            }),
        ));
        Ok(true)
    }

    /// Writes one version at `abs`. Returns whether it is a placeholder, or
    /// `None` when a small file's bytes could not be fetched: a small file
    /// only appears complete.
    fn materialize(
        &self,
        abs: &Path,
        hash: Option<[u8; 32]>,
        e: &TreeEntry,
        replace: bool,
    ) -> io::Result<Option<bool>> {
        let Some(content) = hash.filter(|_| e.size > 0) else {
            self.write_staged(abs, &[], e.mtime_ms)?;
            return Ok(Some(false));
        };
        if e.size <= self.eager_bytes {
            let hydrator = Arc::clone(&self.content);
            let parallel = self.parallel_chunks;
            let deadline = self.eager_deadline;
            let size = e.size;
            let fetched = self.runtime.block_on(async move {
                tokio::time::timeout(
                    deadline,
                    hydrator.read_parallel(iroh_blobs::Hash::from(content), 0, size, parallel),
                )
                .await
            });
            return match fetched {
                Ok(Ok(bytes)) if bytes.len() as u64 == e.size => {
                    self.write_staged(abs, &bytes, e.mtime_ms)?;
                    Ok(Some(false))
                }
                Ok(Ok(bytes)) => {
                    self.note_unavailable(format!("{} of {} bytes", bytes.len(), e.size));
                    Ok(None)
                }
                Ok(Err(error)) => {
                    self.note_unavailable(format!("{error:#}"));
                    Ok(None)
                }
                Err(_) => {
                    self.note_unavailable("timed out".into());
                    Ok(None)
                }
            };
        }
        let spec = PlaceholderSpec {
            content,
            size: e.size,
            mode: FILE_MODE,
            mtime: mtime_of(e.mtime_ms),
            owner: self.owner,
        };
        if replace {
            self.placeholders.replace_with_placeholder(abs, &spec)?;
        } else {
            self.placeholders.create_placeholder(abs, &spec)?;
        }
        Ok(Some(true))
    }

    fn note_unavailable(&self, why: String) {
        if let Ok(mut slot) = self.last_unavailable.lock() {
            *slot = Some(why);
        }
    }

    /// Why the last small file could not be fetched, once.
    pub fn take_unavailable(&self) -> Option<String> {
        self.last_unavailable.lock().ok()?.take()
    }

    fn write_staged(&self, abs: &Path, bytes: &[u8], mtime_ms: u64) -> io::Result<()> {
        let dir = abs.parent().unwrap_or_else(|| Path::new("."));
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let staging = dir.join(format!("{STAGING_PREFIX}{}-{n}", std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staging)?;
            file.write_all(bytes)?;
            file.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
            if let Some((uid, gid)) = self.owner {
                std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
            }
            file.set_modified(mtime_of(mtime_ms))?;
            drop(file);
            let denied = self.write_denied.lock().is_ok_and(|d| d.contains(abs));
            if denied {
                self.placeholders
                    .gate(&staging, fs_native::hydrate::WriteGate::Deny(libc::EBUSY))?;
            }
            fs::rename(&staging, abs)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result
    }

    fn project_removal(&self, rel: &Path, m: &Materialized, out: &mut Projection) {
        let abs = self.live.join(rel);
        let local = LocalStat::of(&abs);
        if m.dir {
            match fs::remove_dir(&abs) {
                Ok(()) => out.removed.push(rel.to_path_buf()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // Local-only or not-yet-captured entries remain; the
                // directory stays for them.
                Err(_) => {}
            }
            out.updates.push((rel.to_path_buf(), None));
            return;
        }
        match local {
            None => {}
            Some(now) if m.local == Some(now) => match fs::remove_file(&abs) {
                Ok(()) => out.removed.push(rel.to_path_buf()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    out.errors.push(format!("{}: {e}", rel.display()));
                    return;
                }
            },
            // Changed locally after the daemon wrote it: the local version
            // survives the remote delete and is captured as a new record.
            Some(_) => out.skipped_dirty.push(rel.to_path_buf()),
        }
        out.updates.push((rel.to_path_buf(), None));
    }
}

/// Removes staging files a crash left in the live tree.
pub fn remove_stale_staging(live: &Path) -> io::Result<usize> {
    let mut removed = 0usize;
    let mut stack = vec![live.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(STAGING_PREFIX))
            {
                fs::remove_file(entry.path())?;
                removed = removed.saturating_add(1);
            }
        }
    }
    Ok(removed)
}

/// Every placeholder left by a previous run, from its residency xattr.
pub fn find_placeholders(live: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![live.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() && Placeholders::is_placeholder(&entry.path()) {
                out.push(entry.path());
            }
        }
    }
    Ok(out)
}
